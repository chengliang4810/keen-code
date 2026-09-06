use keencode_resources::{
    AgentId, MailboxMessage, MailboxMessageId, MailboxState, PlanState, SessionEvent,
    SessionEventId, SubAgentState, SubAgentStatus, TurnId,
};
use tempfile::TempDir;

use super::{
    CreateSessionRequest, OpenSessionResult, RuntimeConfig, RuntimeError, RuntimeSession,
    append_resource_event, inject_runtime_lifecycle_visible_indeterminate,
    runtime_control_event_id,
};

/// 创建只供控制面幂等测试使用的隔离 Session。
fn create_session(root: &TempDir, session_id: &str) -> RuntimeSession {
    RuntimeSession::create_session(
        RuntimeConfig::new(root.path()),
        CreateSessionRequest {
            session_id: session_id.to_owned(),
            title: "控制面测试".to_owned(),
            project_root: root.path().display().to_string(),
        },
    )
    .expect("控制面测试 Session 应创建")
}

/// 为邮箱控制入口创建根 Turn、单层子 Agent 和子 Turn 的合法路由状态。
fn register_mailbox_route(session: &RuntimeSession) -> (AgentId, AgentId, TurnId) {
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let child_agent = AgentId::new("mailbox-child").expect("子 Agent ID 应有效");
    let root_turn = TurnId::new("turn-mailbox-root").expect("根 Turn ID 应有效");
    let child_turn = TurnId::new("turn-mailbox-child").expect("子 Turn ID 应有效");
    for (event_id, event) in [
        (
            "mailbox-root-start",
            SessionEvent::TurnStarted {
                turn_id: root_turn.clone(),
                source_agent_id: root_agent.clone(),
                root_turn_id: root_turn.clone(),
                parent_turn_id: None,
                prompt_summary: "邮箱根任务".to_owned(),
            },
        ),
        (
            "mailbox-child-spawn",
            SessionEvent::SubAgentSpawned {
                agent: SubAgentState {
                    agent_id: child_agent.clone(),
                    parent_agent_id: root_agent.clone(),
                    agent_path: "/root/mailbox_child".to_owned(),
                    task: "邮箱子任务".to_owned(),
                    status: SubAgentStatus::Pending,
                    current_turn_id: None,
                    result_summary: None,
                },
            },
        ),
        (
            "mailbox-child-start",
            SessionEvent::AtomicBatch {
                events: vec![
                    SessionEvent::TurnStarted {
                        turn_id: child_turn.clone(),
                        source_agent_id: child_agent.clone(),
                        root_turn_id: root_turn.clone(),
                        parent_turn_id: Some(root_turn),
                        prompt_summary: "邮箱子任务开始".to_owned(),
                    },
                    SessionEvent::SubAgentStatusChanged {
                        agent_id: child_agent.clone(),
                        turn_id: Some(child_turn.clone()),
                        status: SubAgentStatus::Running,
                        result_summary: None,
                    },
                ],
            },
        ),
    ] {
        append_resource_event(
            &session.inner.journal,
            SessionEventId::new(event_id).expect("邮箱夹具事件 ID 应有效"),
            event,
        )
        .expect("邮箱路由夹具应提交");
    }
    (root_agent, child_agent, child_turn)
}

/// 相同操作标识与相同正文重试时只保留一条权威事件。
#[test]
fn control_retry_with_identical_payload_is_idempotent() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-identical");

    let first = session
        .rename("operation-1", "新标题")
        .expect("首次重命名应提交");
    let retried = session
        .rename("operation-1", "新标题")
        .expect("相同操作重试应幂等命中");

    assert_eq!(retried.title, "新标题");
    assert_eq!(retried.last_sequence, first.last_sequence);
    assert!(
        !session
            .snapshot()
            .expect("Runtime 快照应读取")
            .recovery_required
    );
}

/// 相同操作标识绑定不同正文时返回显式冲突且不冻结 Session。
#[test]
fn control_retry_with_different_payload_conflicts_without_freezing_session() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-conflict");

    session
        .rename("operation-1", "标题一")
        .expect("首次重命名应提交");
    assert!(matches!(
        session.rename("operation-1", "标题二"),
        Err(RuntimeError::ControlOperationConflict)
    ));

    let snapshot = session.snapshot().expect("冲突后快照应读取");
    assert_eq!(snapshot.state.title, "标题一");
    assert!(!snapshot.recovery_required);
    let recovered = session
        .rename("operation-2", "标题三")
        .expect("独立控制操作不应被冲突冻结");
    assert_eq!(recovered.title, "标题三");
}

/// 相同操作标识不能跨控制方法复用。
#[test]
fn control_operation_id_is_bound_across_methods() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-method-conflict");

    session
        .rename("operation-shared", "新标题")
        .expect("首次控制操作应提交");
    assert!(matches!(
        session.set_plan(
            "operation-shared",
            PlanState {
                enabled: true,
                plan_artifact: None,
            },
        ),
        Err(RuntimeError::ControlOperationConflict)
    ));
    assert!(
        !session
            .snapshot()
            .expect("跨方法冲突后快照应读取")
            .recovery_required
    );
}

/// 追加已经可见但调用方丢失响应时，相同请求重试会先对账并解除恢复栅栏。
#[test]
fn visible_indeterminate_control_retry_reconciles_before_new_work() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-visible-indeterminate");
    let event_id =
        runtime_control_event_id(session.session_id(), "operation-1").expect("控制事件标识应派生");
    inject_runtime_lifecycle_visible_indeterminate(&event_id);

    assert!(matches!(
        session.rename("operation-1", "已写入标题"),
        Err(RuntimeError::RecoveryRequired)
    ));
    let uncertain = session.snapshot().expect("不确定提交快照应读取");
    assert_eq!(uncertain.state.title, "已写入标题");
    assert!(uncertain.recovery_required);
    assert_eq!(uncertain.pending_indeterminate_events, 1);
    assert!(matches!(
        session.rename("operation-2", "不应写入"),
        Err(RuntimeError::RecoveryRequired)
    ));

    let reconciled = session
        .rename("operation-1", "已写入标题")
        .expect("原操作重试应对账成功");
    assert_eq!(reconciled.title, "已写入标题");
    let healthy = session.snapshot().expect("对账后快照应读取");
    assert!(!healthy.recovery_required);
    assert_eq!(healthy.pending_indeterminate_events, 0);
}

/// 控制操作标识拒绝空值、首尾空白、控制字符和无界输入。
#[test]
fn control_operation_id_validation_is_bounded_and_unambiguous() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-invalid-id");

    for operation_id in ["", " operation", "operation ", "operation\n"] {
        assert!(matches!(
            session.rename(operation_id, "标题"),
            Err(RuntimeError::InvalidControlOperation)
        ));
    }
    let oversized = "x".repeat(129);
    assert!(matches!(
        session.rename(&oversized, "标题"),
        Err(RuntimeError::InvalidControlOperation)
    ));
}

/// 标题结果必须按 operationId 和输入摘要幂等保存，并在冷恢复后继续复用。
#[test]
fn generated_title_cache_is_persistent_and_rejects_conflicting_input() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "control-title-cache";
    let input_sha256 = "a".repeat(64);
    let conflicting_sha256 = "b".repeat(64);
    let session = create_session(&root, session_id);

    assert_eq!(
        session
            .cached_generated_title("title-operation", &input_sha256)
            .expect("首次查询应成功"),
        None
    );
    assert_eq!(
        session
            .cache_generated_title("title-operation", &input_sha256, "持久标题")
            .expect("标题结果应提交"),
        "持久标题"
    );
    assert_eq!(
        session
            .cache_generated_title("title-operation", &input_sha256, "持久标题")
            .expect("相同结果重试应幂等"),
        "持久标题"
    );
    assert!(matches!(
        session.cached_generated_title("title-operation", &conflicting_sha256),
        Err(RuntimeError::ControlOperationConflict)
    ));
    drop(session);

    let reopened = match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
        .expect("标题 Session 应重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("标题 Session 不应损坏：{report:?}")
        }
    };
    assert_eq!(
        reopened
            .cached_generated_title("title-operation", &input_sha256)
            .expect("冷恢复后标题缓存应读取"),
        Some("持久标题".to_owned())
    );
}

/// 邮箱强类型入口以消息 ID 和动作幂等提交，并在同 ID 不同正文时明确冲突。
#[test]
fn mailbox_control_entrypoints_are_idempotent_conflict_safe_and_restart_stable() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "control-mailbox-idempotent";
    let session = create_session(&root, session_id);
    let (root_agent, child_agent, child_turn) = register_mailbox_route(&session);
    let message_id = MailboxMessageId::new("mailbox-idempotent").expect("邮箱消息 ID 应有效");
    let message = MailboxMessage {
        message_id: message_id.clone(),
        from: child_agent,
        to: root_agent,
        related_turn_id: child_turn,
        body: "子任务完成".to_owned(),
        artifact: None,
        state: MailboxState::Queued,
    };

    let queued = session
        .queue_mailbox_message(message.clone())
        .expect("邮箱消息应排队");
    let retried = session
        .queue_mailbox_message(message.clone())
        .expect("相同邮箱消息应幂等重试");
    assert_eq!(retried.last_sequence, queued.last_sequence);
    let mut conflicting = message.clone();
    conflicting.body = "冲突正文".to_owned();
    assert!(matches!(
        session.queue_mailbox_message(conflicting),
        Err(RuntimeError::ControlOperationConflict)
    ));
    assert!(
        !session
            .snapshot()
            .expect("冲突后快照应读取")
            .recovery_required
    );

    let delivered = session
        .deliver_mailbox_message(message_id.clone())
        .expect("邮箱消息应投递");
    let delivery_retried = session
        .deliver_mailbox_message(message_id.clone())
        .expect("相同投递确认应幂等重试");
    assert_eq!(delivery_retried.last_sequence, delivered.last_sequence);
    assert_eq!(
        delivery_retried
            .mailbox
            .get(&message_id)
            .map(|message| &message.state),
        Some(&MailboxState::Delivered)
    );
    drop(session);

    let reopened = match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
        .expect("邮箱 Session 应重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => panic!("邮箱 Session 不应损坏：{report:?}"),
    };
    let before_retry = reopened
        .snapshot()
        .expect("重启快照应读取")
        .state
        .last_sequence;
    assert_eq!(
        reopened
            .queue_mailbox_message(message)
            .expect("重启后相同排队应幂等")
            .last_sequence,
        before_retry
    );
    assert_eq!(
        reopened
            .deliver_mailbox_message(message_id)
            .expect("重启后相同投递应幂等")
            .last_sequence,
        before_retry
    );
}
