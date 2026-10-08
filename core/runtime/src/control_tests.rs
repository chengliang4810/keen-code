use keencode_resources::{
    AgentId, CommandReceiptStatus, FollowupMode, MailboxMessage, MailboxMessageId, MailboxState,
    PlanState, SessionEvent, SessionEventId, SessionInputDelivery, SessionInputDispatch,
    SessionInputKind, SessionInputQueueItem, SessionPermissionMode, SubAgentState, SubAgentStatus,
    TitleSource, TurnId,
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

/// 创建只供输入队列控制面测试使用的普通文本意图。
fn queue_item(source_command_id: &str, text: &str) -> SessionInputQueueItem {
    SessionInputQueueItem {
        queue_item_id: format!("queue:{source_command_id}"),
        source_command_id: source_command_id.to_owned(),
        client_id: Some("test-client".to_owned()),
        kind: SessionInputKind::SendText,
        text: text.to_owned(),
        attachments: Vec::new(),
        model_selection: None,
        mode: None,
        plan_enabled: false,
        requested_delivery: SessionInputDelivery::Queue,
        admitted_delivery: SessionInputDelivery::Queue,
        admission_seq: 0,
        reserve_attempt: 0,
        dispatch: SessionInputDispatch::Queued,
        promoted_turn_id: None,
        admitted_at_unix_ms: 1,
    }
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
        .rename("operation-1", "新标题", None)
        .expect("首次重命名应提交");
    let retried = session
        .rename("operation-1", "新标题", None)
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

/// 手动改名是 Journal 事实；Runtime 冷恢复后标题来源仍必须阻止自动标题覆盖。
#[test]
fn manual_rename_survives_runtime_cold_recovery() {
    let root = TempDir::new().expect("冷恢复测试根目录应创建");
    let session_id = "control-manual-rename-cold";
    let session = create_session(&root, session_id);

    let renamed = session
        .rename(
            "manual-rename-operation",
            "冷恢复手动标题",
            Some(TitleSource::Manual),
        )
        .expect("手动改名应提交");
    assert_eq!(renamed.title, "冷恢复手动标题");
    assert_eq!(renamed.title_source, TitleSource::Manual);
    drop(session);

    let reopened = match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
        .expect("手动改名 Session 应重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("手动改名 Journal 不应损坏：{report:?}")
        }
    };
    let snapshot = reopened.snapshot().expect("冷恢复快照应读取");
    assert_eq!(snapshot.state.title, "冷恢复手动标题");
    assert_eq!(snapshot.state.title_source, TitleSource::Manual);
}

/// 权限模式和视觉输入开关由 Session Journal 持久化，冷恢复不得依赖 Host 缓存。
#[test]
fn session_execution_preferences_survive_cold_recovery() {
    let root = TempDir::new().expect("偏好测试根目录应创建");
    let session_id = "control-execution-preferences-cold";
    let session = create_session(&root, session_id);
    let updated = session
        .set_permission_mode("permission-mode-operation", SessionPermissionMode::Yolo)
        .expect("权限模式应写入 Journal");
    assert_eq!(updated.permission_mode, SessionPermissionMode::Yolo);
    let updated = session
        .set_vision_enabled("vision-operation", true)
        .expect("视觉输入开关应写入 Journal");
    assert!(updated.vision_enabled);
    drop(session);

    let reopened = match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
        .expect("偏好 Session 应重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("偏好 Journal 不应损坏：{report:?}")
        }
    };
    let state = reopened.snapshot().expect("偏好冷恢复快照应读取").state;
    assert_eq!(state.permission_mode, SessionPermissionMode::Yolo);
    assert!(state.vision_enabled);
}

/// 自动标题和稳定操作身份都由 Journal 冷恢复；竞争结果只允许首次提交。
#[test]
fn automatic_title_commits_once_and_survives_cold_recovery() {
    let root = TempDir::new().unwrap();
    let session_id = "control-automatic-title-cold";
    let session = create_session(&root, session_id);
    let original = session.snapshot().unwrap().state;
    assert!(
        session
            .rename_generated_title(
                "auto-title-first",
                &original.title,
                original.title_source,
                "自动标题"
            )
            .unwrap()
    );
    let renamed = session.snapshot().unwrap().state;
    assert_eq!(renamed.title_source, TitleSource::Automatic);
    assert!(
        !session
            .rename_generated_title(
                "auto-title-first",
                &original.title,
                original.title_source,
                "自动标题"
            )
            .unwrap()
    );
    assert!(
        !session
            .rename_generated_title(
                "auto-title-competing",
                &original.title,
                original.title_source,
                "迟到标题"
            )
            .unwrap()
    );
    assert!(matches!(
        session.rename_generated_title(
            "auto-title-first",
            &original.title,
            original.title_source,
            "冲突标题"
        ),
        Err(RuntimeError::ControlOperationConflict)
    ));
    assert_eq!(
        session.snapshot().unwrap().state.last_sequence,
        renamed.last_sequence
    );
    drop(session);
    let OpenSessionResult::Ready(reopened) =
        RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id).unwrap()
    else {
        panic!("标题 Journal 不应损坏")
    };
    let state = reopened.snapshot().unwrap().state;
    assert_eq!(state.title, "自动标题");
    assert_eq!(state.title_source, TitleSource::Automatic);
}

/// 标题网络请求期间手工改名后，迟到的生成结果与冷恢复重试均不得覆盖。
#[test]
fn automatic_title_cannot_overwrite_manual_rename() {
    let root = TempDir::new().unwrap();
    let session_id = "control-automatic-manual-race";
    let session = create_session(&root, session_id);
    let original = session.snapshot().unwrap().state;
    let manual = session
        .rename("manual-title", "用户标题", Some(TitleSource::Manual))
        .unwrap();
    assert!(
        !session
            .rename_generated_title(
                "auto-title-first",
                &original.title,
                original.title_source,
                "迟到标题"
            )
            .unwrap()
    );
    assert_eq!(
        session.snapshot().unwrap().state.last_sequence,
        manual.last_sequence
    );
    drop(session);
    let OpenSessionResult::Ready(reopened) =
        RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id).unwrap()
    else {
        panic!("标题 Journal 不应损坏")
    };
    assert!(
        !reopened
            .rename_generated_title(
                "auto-title-first",
                &original.title,
                original.title_source,
                "迟到标题"
            )
            .unwrap()
    );
    assert_eq!(reopened.snapshot().unwrap().state.title, "用户标题");
}

/// 自动标题已提交后仍允许手工改名；旧自动操作重试不能还原旧标题。
#[test]
fn automatic_title_retry_preserves_subsequent_manual_title() {
    let root = TempDir::new().unwrap();
    let session = create_session(&root, "control-automatic-manual-retry");
    let original = session.snapshot().unwrap().state;
    assert!(
        session
            .rename_generated_title(
                "auto-title-first",
                &original.title,
                original.title_source,
                "自动标题"
            )
            .unwrap()
    );
    let manual = session
        .rename("manual-title", "最终用户标题", Some(TitleSource::Manual))
        .unwrap();
    assert!(
        !session
            .rename_generated_title(
                "auto-title-first",
                &original.title,
                original.title_source,
                "自动标题"
            )
            .unwrap()
    );
    let state = session.snapshot().unwrap().state;
    assert_eq!(state.title, "最终用户标题");
    assert_eq!(state.title_source, TitleSource::Manual);
    assert_eq!(state.last_sequence, manual.last_sequence);
}

/// 相同操作标识绑定不同正文时返回显式冲突且不冻结 Session。
#[test]
fn control_retry_with_different_payload_conflicts_without_freezing_session() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-conflict");

    session
        .rename("operation-1", "标题一", None)
        .expect("首次重命名应提交");
    assert!(matches!(
        session.rename("operation-1", "标题二", None),
        Err(RuntimeError::ControlOperationConflict)
    ));

    let snapshot = session.snapshot().expect("冲突后快照应读取");
    assert_eq!(snapshot.state.title, "标题一");
    assert!(!snapshot.recovery_required);
    let recovered = session
        .rename("operation-2", "标题三", None)
        .expect("独立控制操作不应被冲突冻结");
    assert_eq!(recovered.title, "标题三");
}

/// 相同操作标识不能跨控制方法复用。
#[test]
fn control_operation_id_is_bound_across_methods() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-method-conflict");

    session
        .rename("operation-shared", "新标题", None)
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
        session.rename("operation-1", "已写入标题", None),
        Err(RuntimeError::RecoveryRequired)
    ));
    let uncertain = session.snapshot().expect("不确定提交快照应读取");
    assert_eq!(uncertain.state.title, "已写入标题");
    assert!(uncertain.recovery_required);
    assert_eq!(uncertain.pending_indeterminate_events, 1);
    assert!(matches!(
        session.rename("operation-2", "不应写入", None),
        Err(RuntimeError::RecoveryRequired)
    ));

    let reconciled = session
        .rename("operation-1", "已写入标题", None)
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
            session.rename(operation_id, "标题", None),
            Err(RuntimeError::InvalidControlOperation)
        ));
    }
    let oversized = "x".repeat(129);
    assert!(matches!(
        session.rename(&oversized, "标题", None),
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

/// 后续模式、队列顺序和编辑删除均由 Journal 归约；重复 operationId 不增加 admission。
#[test]
fn input_queue_mode_order_and_mutation_are_restart_safe() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "control-input-queue";
    let session = create_session(&root, session_id);

    let first_mode = session
        .set_followup_mode("mode-1", FollowupMode::Guide)
        .expect("后续模式应持久化");
    let retried_mode = session
        .set_followup_mode("mode-1", FollowupMode::Guide)
        .expect("相同模式重试应幂等");
    assert_eq!(first_mode.last_sequence, retried_mode.last_sequence);
    assert_eq!(
        session.input_queue_state().expect("队列状态应读取").0,
        FollowupMode::Guide
    );

    session
        .enqueue_input("input-a", queue_item("input-a", "第一条"))
        .expect("第一条输入应入队");
    session
        .enqueue_input("input-b", queue_item("input-b", "第二条"))
        .expect("第二条输入应入队");
    let (_, queue) = session.input_queue_state().expect("队列应读取");
    assert_eq!(
        queue
            .items
            .iter()
            .map(|item| item.source_command_id.as_str())
            .collect::<Vec<_>>(),
        ["input-a", "input-b"]
    );
    assert_eq!(queue.items[0].admission_seq, 1);
    assert_eq!(queue.items[1].admission_seq, 2);

    session
        .edit_queued_input("edit-a", "queue:input-a", "编辑后的第一条".to_owned())
        .expect("队列正文应可编辑");
    session
        .reorder_queued_input("move-b", "queue:input-b", Some("queue:input-a"))
        .expect("队列顺序应可调整");
    let (_, queue) = session.input_queue_state().expect("重排后队列应读取");
    assert_eq!(queue.items[0].source_command_id, "input-b");
    assert_eq!(queue.items[1].text, "编辑后的第一条");

    session
        .delete_queued_input("delete-b", "queue:input-b")
        .expect("队列项应可删除");
    let (_, queue) = session.input_queue_state().expect("删除后队列应读取");
    assert_eq!(queue.items.len(), 1);
    drop(session);

    let OpenSessionResult::Ready(reopened) =
        RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
            .expect("队列 Session 应冷恢复")
    else {
        panic!("队列 Session 不应损坏");
    };
    let (_, queue) = reopened.input_queue_state().expect("冷恢复队列应读取");
    assert_eq!(queue.items[0].text, "编辑后的第一条");
    assert_eq!(queue.next_admission_seq, 3);
}

/// admission command 重试只忽略连接和时间等临时字段；同一 commandId 改正文必须冲突。
#[test]
fn input_queue_command_retry_is_stable_but_conflicting_payload_is_rejected() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-input-command-dedup");
    session
        .enqueue_input("input-a", queue_item("input-a", "原始正文"))
        .expect("输入应入队");

    let mut retry = queue_item("input-a", "原始正文");
    retry.client_id = Some("reconnected-client".to_owned());
    retry.admitted_at_unix_ms = 999;
    let retried = session
        .enqueue_input("input-a", retry)
        .expect("跨连接重试应命中同一 admission");
    assert_eq!(retried.last_sequence, 2);

    let conflict = session.enqueue_input("retry-command", queue_item("input-a", "篡改正文"));
    assert!(matches!(
        conflict,
        Err(RuntimeError::ControlOperationConflict)
    ));
}

/// reserve 与 Turn 启动之间退出时保留项恢复为 queued；真实 Turn 存在时才确认消费。
#[test]
fn input_queue_reservation_requires_bound_turn_and_recovers_conservatively() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "control-input-recovery";
    let session = create_session(&root, session_id);
    session
        .enqueue_input("input-a", queue_item("input-a", "待恢复"))
        .expect("输入应入队");
    let (_, reserved) = session
        .reserve_queued_input("send-a", "queue:input-a", "queue-turn-send-a")
        .expect("输入应先保留");
    assert_eq!(
        reserved.promoted_turn_id.as_ref().map(TurnId::as_str),
        Some("queue-turn-send-a")
    );
    drop(session);

    let OpenSessionResult::Ready(reopened) =
        RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
            .expect("保留态 Session 应冷恢复")
    else {
        panic!("保留态 Session 不应损坏");
    };
    let (_, queue) = reopened.input_queue_state().expect("恢复队列应读取");
    assert_eq!(queue.items[0].dispatch, SessionInputDispatch::Queued);
    assert!(queue.items[0].promoted_turn_id.is_none());

    let (_, reserved) = reopened
        .reserve_queued_input("send-b", "queue:input-a", "queue-turn-send-b")
        .expect("输入应可再次保留");
    assert_eq!(
        reserved.promoted_turn_id.as_ref().map(TurnId::as_str),
        Some("queue-turn-send-b")
    );
    append_resource_event(
        &reopened.inner.journal,
        SessionEventId::new("queue-turn-start").expect("Turn 事件标识应有效"),
        SessionEvent::TurnStarted {
            turn_id: TurnId::new("queue-turn-send-b").expect("目标 Turn 标识应有效"),
            source_agent_id: AgentId::new("root").expect("根 Agent 标识应有效"),
            root_turn_id: TurnId::new("queue-turn-send-b").expect("根 Turn 标识应有效"),
            parent_turn_id: None,
            prompt_summary: "待恢复".to_owned(),
        },
    )
    .expect("真实 Turn 应提交");
    drop(reopened);

    let OpenSessionResult::Ready(recovered) =
        RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
            .expect("真实 Turn 恢复应成功")
    else {
        panic!("真实 Turn Session 不应损坏");
    };
    let (_, queue) = recovered.input_queue_state().expect("最终队列应读取");
    assert!(queue.items.is_empty());
    assert_eq!(queue.completions.len(), 1);
    assert!(
        queue.completions[0]
            .completion_operation_id
            .starts_with("recover-input-")
    );
    let conflict = recovered.enqueue_input("input-retry", queue_item("input-a", "篡改正文"));
    assert!(matches!(
        conflict,
        Err(RuntimeError::ControlOperationConflict)
    ));
}

/// reserve 失败后 release 再重试必须取得新的持久尝试身份，不能读回旧的 queued 状态。
#[test]
fn input_queue_reserve_release_retry_is_idempotent_and_progresses() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-input-retry");
    session
        .enqueue_input("input-a", queue_item("input-a", "重试输入"))
        .expect("输入应入队");
    session
        .reserve_queued_input("send-a", "queue:input-a", "queue-turn-a")
        .expect("第一次 reserve 应成功");
    session
        .release_queued_input("send-a-release", "queue:input-a")
        .expect("启动失败后应 release");
    let (_, retried) = session
        .reserve_queued_input("send-a", "queue:input-a", "queue-turn-b")
        .expect("相同 sendQueuedNow 重试应取得新 reserve");
    assert_eq!(
        retried.promoted_turn_id.as_ref().map(TurnId::as_str),
        Some("queue-turn-b")
    );
    assert_eq!(retried.reserve_attempt, 3);
}

/// 通用命令收据在同一 Runtime 控制锁内只允许一次执行 admission。
#[test]
fn command_receipt_admission_is_atomic_and_payload_bound() {
    let root = TempDir::new().expect("临时目录应创建");
    let session = create_session(&root, "control-command-receipt");
    let digest = "a".repeat(64);
    let first = session
        .admit_command_receipt(
            "session:control-command-receipt",
            "command-1",
            "sendText",
            &digest,
        )
        .expect("首次收据 admission 应成功");
    assert!(matches!(first, super::CommandReceiptAdmission::Execute(_)));

    let duplicate = session
        .admit_command_receipt(
            "session:control-command-receipt",
            "command-1",
            "sendText",
            &digest,
        )
        .expect("相同收据重试应返回既有记录");
    assert!(matches!(
        duplicate,
        super::CommandReceiptAdmission::Existing(_)
    ));

    let conflict = session.admit_command_receipt(
        "session:control-command-receipt",
        "command-1",
        "sendText",
        &"b".repeat(64),
    );
    assert!(matches!(
        conflict,
        Err(RuntimeError::ControlOperationConflict)
    ));
}

/// 已完成、已拒绝和未知收据均可从冷恢复 Journal 读取；未知状态不能再次 admission。
#[test]
fn command_receipt_terminal_states_survive_cold_reopen() {
    let root = TempDir::new().expect("临时目录应创建");
    let session_id = "control-command-receipt-cold";
    let session = create_session(&root, session_id);
    let digest = "c".repeat(64);
    session
        .admit_command_receipt(
            "session:control-command-receipt-cold",
            "done",
            "sendText",
            &digest,
        )
        .expect("完成收据 admission 应成功");
    session
        .finish_command_receipt(
            "session:control-command-receipt-cold",
            "done",
            "sendText",
            &digest,
            CommandReceiptStatus::Completed {
                ack: serde_json::json!({"commandId":"done","status":"accepted","revisionAtDecision":1}),
            },
        )
        .expect("完成收据应持久化");

    session
        .admit_command_receipt(
            "session:control-command-receipt-cold",
            "reject",
            "sendText",
            &digest,
        )
        .expect("拒绝收据 admission 应成功");
    session
        .finish_command_receipt(
            "session:control-command-receipt-cold",
            "reject",
            "sendText",
            &digest,
            CommandReceiptStatus::Rejected {
                ack: serde_json::json!({"commandId":"reject","status":"rejected","revisionAtDecision":1}),
            },
        )
        .expect("拒绝收据应持久化");

    session
        .admit_command_receipt(
            "session:control-command-receipt-cold",
            "unknown",
            "sendText",
            &digest,
        )
        .expect("未知收据 admission 应成功");
    session
        .finish_command_receipt(
            "session:control-command-receipt-cold",
            "unknown",
            "sendText",
            &digest,
            CommandReceiptStatus::Unknown {
                reason_code: "fault.command.resultUnknown".to_owned(),
            },
        )
        .expect("未知收据应持久化");
    drop(session);

    let OpenSessionResult::Ready(reopened) =
        RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
            .expect("收据 Session 应可冷恢复")
    else {
        panic!("收据 Session 不应损坏");
    };
    let done = reopened
        .command_receipt("session:control-command-receipt-cold", "done")
        .expect("完成收据应可读取")
        .expect("完成收据应存在");
    assert!(matches!(
        done.status,
        CommandReceiptStatus::Completed { .. }
    ));
    let unknown = reopened
        .admit_command_receipt(
            "session:control-command-receipt-cold",
            "unknown",
            "sendText",
            &digest,
        )
        .expect("未知收据重试应返回既有记录");
    assert!(
        matches!(unknown, super::CommandReceiptAdmission::Existing(record) if matches!(record.status, CommandReceiptStatus::Unknown { .. }))
    );
}
