//! 生产状态控制器的崩溃恢复、幂等与隔离测试。

use std::sync::{Arc, Barrier};

use keencode_agent::{
    AgentId, GoalController, GoalDraft, GoalPatch, GoalStatus, GoalTransition, GoalUsageDelta,
    PlanController, RuntimeStateError, SessionId, TodoController, TodoItem, TodoStatus,
};
use keencode_resources::{PlanFileStore, SessionEvent};
use tempfile::TempDir;

use crate::{
    CreateSessionRequest, OpenSessionResult, PersistentAgentState, RuntimeConfig, RuntimeSession,
};

/// 创建绑定指定项目目录的全新权威 Session。
fn create_session(
    storage_root: &TempDir,
    session_id: &str,
    project_root: &TempDir,
) -> RuntimeSession {
    RuntimeSession::create_session(
        RuntimeConfig::new(storage_root.path()),
        CreateSessionRequest {
            session_id: session_id.to_owned(),
            title: "状态持久化测试".to_owned(),
            project_root: project_root.path().to_string_lossy().into_owned(),
        },
    )
    .expect("测试 Session 应创建")
}

/// 重新打开一个已经释放 lease 的健康 Session。
fn reopen_session(storage_root: &TempDir, session_id: &str) -> RuntimeSession {
    match RuntimeSession::open_session(RuntimeConfig::new(storage_root.path()), session_id)
        .expect("测试 Session 应重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(_) => panic!("测试 Session 不应损坏"),
    }
}

/// 创建一个有效 Todo 条目。
fn todo(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem {
        content: content.to_owned(),
        status,
        active_form: format!("正在{content}"),
    }
}

/// TodoWrite 必须进入 Session Journal、跨重启恢复且保持 operationId 幂等冲突语义。
#[test]
fn todo_is_authoritative_restart_safe_and_operation_idempotent() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let session = create_session(&storage_root, "session-todo-restart", &project_root);
    let state = PersistentAgentState::open(session.clone()).expect("持久状态应打开");
    let submitted = vec![
        todo("实现持久化", TodoStatus::InProgress),
        todo("验证恢复", TodoStatus::Pending),
    ];
    let first = state
        .replace_todos("todo-call-one", submitted.clone())
        .expect("Todo 应首次保存");
    assert!(first.changed);
    assert_eq!(first.current.revision, 1);
    assert_eq!(first.current.items, submitted);
    let replay = session.replay(None, 16).expect("Session 事件应重放");
    assert!(matches!(
        replay.records.last().map(|record| &record.event),
        Some(SessionEvent::TodoReplaced { items, .. }) if items.len() == 2
    ));

    drop(state);
    drop(session);
    let reopened = reopen_session(&storage_root, "session-todo-restart");
    let reopened_state = PersistentAgentState::open(reopened.clone()).expect("持久状态应恢复");
    assert_eq!(
        reopened_state.todo_snapshot().expect("Todo 应恢复"),
        first.current
    );
    let repeated = reopened_state
        .replace_todos("todo-call-one", submitted.clone())
        .expect("相同 operationId 与正文应幂等命中");
    assert!(!repeated.changed);
    assert_eq!(
        reopened.snapshot().expect("快照应读取").state.last_sequence,
        2
    );

    let conflict = reopened_state
        .replace_todos(
            "todo-call-one",
            vec![todo("不同正文", TodoStatus::InProgress)],
        )
        .expect_err("相同 operationId 不得绑定不同正文");
    assert!(matches!(conflict, RuntimeStateError::Conflict { .. }));
    assert_eq!(
        reopened_state.todo_snapshot().expect("冲突后 Todo 应保持"),
        first.current
    );

    reopened_state
        .replace_todos(
            "todo-completed-collision",
            vec![todo("完成甲", TodoStatus::Completed)],
        )
        .expect("首次完成列表应清空 Todo");
    assert!(matches!(
        reopened_state.replace_todos(
            "todo-completed-collision",
            vec![todo("完成乙", TodoStatus::Completed)],
        ),
        Err(RuntimeStateError::Conflict { .. })
    ));
    assert!(
        reopened_state
            .todo_snapshot()
            .expect("完成列表冲突后 Todo 应读取")
            .items
            .is_empty()
    );
}

/// 同一 operationId 的并发不同 Todo 正文必须只有一个提交者获胜。
#[test]
fn concurrent_todo_operation_rejects_conflicting_payload() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let session = create_session(&storage_root, "session-todo-concurrent", &project_root);
    let state = Arc::new(PersistentAgentState::open(session.clone()).expect("持久状态应打开"));
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["方案 A", "方案 B"].map(|content| {
        let state = state.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            state.replace_todos(
                "todo-shared-operation",
                vec![todo(content, TodoStatus::InProgress)],
            )
        })
    });
    let results = handles.map(|handle| handle.join().expect("Todo 并发线程不应 panic"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(RuntimeStateError::Conflict { .. })))
            .count(),
        1
    );
    let snapshot = state.todo_snapshot().expect("并发后 Todo 应读取");
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(
        session
            .snapshot()
            .expect("Session 快照应读取")
            .state
            .last_sequence,
        2
    );
}

/// 同一项目的两个 Session 并发创建不同 Goal 时必须只接受一个项目单例。
#[test]
fn concurrent_project_goal_creation_has_one_winner() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let first_session = create_session(&storage_root, "session-goal-race-one", &project_root);
    let second_session = create_session(&storage_root, "session-goal-race-two", &project_root);
    let first = Arc::new(PersistentAgentState::open(first_session).expect("首个状态应打开"));
    let second = Arc::new(PersistentAgentState::open(second_session).expect("第二状态应打开"));
    let barrier = Arc::new(Barrier::new(2));
    let handles = [
        (first.clone(), "goal-race-a", "目标 A"),
        (second.clone(), "goal-race-b", "目标 B"),
    ]
    .map(|(state, operation_id, title)| {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            state.create_goal(
                operation_id,
                GoalDraft {
                    title: title.to_owned(),
                    objective: format!("并发创建{title}"),
                    description: None,
                    token_budget: None,
                    progress_percent: None,
                },
            )
        })
    });
    let results = handles.map(|handle| handle.join().expect("Goal 并发线程不应 panic"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(RuntimeStateError::Conflict { .. })))
            .count(),
        1
    );
    let first_snapshot = first.goal_snapshot().expect("首个 Goal 快照应读取");
    let second_snapshot = second.goal_snapshot().expect("第二 Goal 快照应读取");
    assert_eq!(first_snapshot, second_snapshot);
    assert_eq!(first_snapshot.revision, 1);
    assert!(first_snapshot.goal.is_some());
}

/// Goal 必须按项目共享，Plan 必须按项目、Session 与 Agent 隔离并全部跨重启恢复。
#[test]
fn goal_and_plan_recover_with_required_scopes() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let first_session = create_session(&storage_root, "session-state-one", &project_root);
    let second_session = create_session(&storage_root, "session-state-two", &project_root);
    let first = PersistentAgentState::open(first_session.clone()).expect("首个状态应打开");
    let second = PersistentAgentState::open(second_session.clone()).expect("第二状态应打开");

    let created = first
        .create_goal(
            "goal-create-shared",
            GoalDraft {
                title: "完整交付".to_owned(),
                objective: "实现并验证持久状态".to_owned(),
                description: Some("项目级共享".to_owned()),
                token_budget: Some(10_000),
                progress_percent: Some(20),
            },
        )
        .expect("Goal 应创建");
    first
        .record_goal_usage(
            "goal-usage-shared",
            GoalUsageDelta {
                tokens: 321,
                elapsed_seconds: 7,
            },
        )
        .expect("Goal 用量应累计");
    assert_eq!(
        second
            .goal_snapshot()
            .expect("同项目另一 Session 应读取 Goal")
            .goal
            .as_ref()
            .map(|goal| goal.id.as_str()),
        created.current.goal.as_ref().map(|goal| goal.id.as_str())
    );

    let first_id = SessionId::new("session-state-one").expect("Session ID 应有效");
    let second_id = SessionId::new("session-state-two").expect("Session ID 应有效");
    let root = AgentId::new("root").expect("根 Agent ID 应有效");
    let child = AgentId::new("child-one").expect("子 Agent ID 应有效");
    first
        .replace_plan("plan-root-shared", &first_id, &root, "# 根计划".to_owned())
        .expect("根计划应保存");
    let root_plan_artifact = first_session
        .snapshot()
        .expect("根计划写入后 Session 快照应读取")
        .state
        .plan
        .plan_artifact
        .expect("根计划应关联权威 Artifact");
    assert_eq!(
        root_plan_artifact.media_type.as_deref(),
        Some("text/markdown")
    );
    assert!(
        storage_root
            .path()
            .join("sessions")
            .join(first_id.as_str())
            .join("artifacts")
            .join(format!(
                "{}.artifact",
                root_plan_artifact.artifact_id.as_str()
            ))
            .is_file()
    );
    first
        .replace_plan(
            "plan-child-shared",
            &first_id,
            &child,
            "# 子计划".to_owned(),
        )
        .expect("子计划应保存");
    assert!(
        second
            .plan_snapshot(&second_id, &root)
            .expect("第二 Session 根计划应读取")
            .content
            .is_none()
    );
    let plan_root = storage_root
        .path()
        .join("plans")
        .join(first.project_scope().as_str())
        .join(first_id.as_str());
    assert!(plan_root.join(root.as_str()).join("plan.json").is_file());
    assert!(plan_root.join(child.as_str()).join("plan.json").is_file());
    assert!(!project_root.path().join("plans").exists());

    drop(first);
    drop(second);
    drop(first_session);
    drop(second_session);
    let reopened = reopen_session(&storage_root, "session-state-one");
    let recovered = PersistentAgentState::open(reopened.clone()).expect("持久状态应重新打开");
    let goal = recovered
        .goal_snapshot()
        .expect("Goal 应恢复")
        .goal
        .expect("Goal 应存在");
    assert_eq!(goal.tokens_used, 321);
    assert_eq!(goal.time_used_seconds, 7);
    assert_eq!(
        recovered
            .plan_snapshot(&first_id, &root)
            .expect("根计划应恢复")
            .content
            .as_deref(),
        Some("# 根计划")
    );
    assert_eq!(
        reopened
            .snapshot()
            .expect("恢复后 Session 快照应读取")
            .state
            .plan
            .plan_artifact,
        Some(root_plan_artifact)
    );
    assert_eq!(
        recovered
            .plan_snapshot(&first_id, &child)
            .expect("子计划应恢复")
            .content
            .as_deref(),
        Some("# 子计划")
    );
    recovered
        .transition_goal(
            "goal-complete-shared",
            GoalTransition {
                status: GoalStatus::Completed,
                blocked_reason: None,
                completion_evidence: Some("恢复、隔离和用量断言均通过".to_owned()),
            },
        )
        .expect("Goal 应进入完成态");
    assert!(matches!(
        recovered.record_goal_usage(
            "goal-usage-terminal",
            GoalUsageDelta {
                tokens: 1,
                elapsed_seconds: 1,
            }
        ),
        Err(RuntimeStateError::Terminal { entity: "Goal" })
    ));
    let cleared = recovered
        .clear_goal("goal-clear-shared")
        .expect("终态 Goal 应清除");
    assert!(cleared.current.goal.is_none());
}

/// Goal 用量收据必须跨重启只累计一次，并拒绝同操作标识改绑不同增量。
#[test]
fn goal_usage_is_restart_idempotent_and_rejects_conflicting_delta() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let session = create_session(&storage_root, "session-goal-usage", &project_root);
    let state = PersistentAgentState::open(session.clone()).expect("持久状态应打开");
    state
        .create_goal(
            "goal-usage-create",
            GoalDraft {
                title: "用量幂等".to_owned(),
                objective: "确保模型轮次重试不会重复累计".to_owned(),
                description: None,
                token_budget: Some(1_000),
                progress_percent: None,
            },
        )
        .expect("Goal 应创建");
    let delta = GoalUsageDelta {
        tokens: 42,
        elapsed_seconds: 9,
    };
    let first = state
        .record_goal_usage("goal-usage-round-one", delta)
        .expect("首次用量应累计");
    assert!(first.changed);
    assert_eq!(first.current.revision, 2);

    drop(state);
    drop(session);
    let reopened = reopen_session(&storage_root, "session-goal-usage");
    let recovered = PersistentAgentState::open(reopened).expect("持久状态应恢复");
    let retry = recovered
        .record_goal_usage("goal-usage-round-one", delta)
        .expect("重启后相同用量应去重");
    assert!(!retry.changed);
    assert_eq!(retry.current.revision, 2);
    let goal = retry.current.goal.expect("Goal 应存在");
    assert_eq!(goal.tokens_used, 42);
    assert_eq!(goal.time_used_seconds, 9);
    assert!(matches!(
        recovered.record_goal_usage(
            "goal-usage-round-one",
            GoalUsageDelta {
                tokens: 43,
                elapsed_seconds: 9,
            },
        ),
        Err(RuntimeStateError::Conflict { .. })
    ));
}

/// Goal 创建、更新、终态和清除收据必须在后续状态变化后继续识别原始重试。
#[test]
fn goal_lifecycle_receipts_survive_later_state_changes() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let session_name = "session-goal-receipts";
    let session = create_session(&storage_root, session_name, &project_root);
    let state = PersistentAgentState::open(session).expect("持久状态应打开");
    let draft = GoalDraft {
        title: "生命周期幂等".to_owned(),
        objective: "验证所有 Goal mutation 收据".to_owned(),
        description: None,
        token_budget: None,
        progress_percent: None,
    };
    state
        .create_goal("goal-create-original", draft.clone())
        .expect("Goal 应创建");
    let patch = GoalPatch {
        progress_percent: Some(Some(60)),
        ..GoalPatch::default()
    };
    state
        .update_goal("goal-update-original", patch.clone())
        .expect("Goal 应更新");
    let create_retry = state
        .create_goal("goal-create-original", draft)
        .expect("后续更新后创建重试应去重");
    assert!(!create_retry.changed);
    assert_eq!(create_retry.current.revision, 2);
    let completion = GoalTransition {
        status: GoalStatus::Completed,
        blocked_reason: None,
        completion_evidence: Some("Goal 生命周期回归测试通过".to_owned()),
    };
    let transitioned = state
        .transition_goal("goal-transition-original", completion.clone())
        .expect("Goal 应完成");
    assert_eq!(
        transitioned
            .current
            .goal
            .as_ref()
            .and_then(|goal| goal.completion_evidence.as_deref()),
        Some("Goal 生命周期回归测试通过")
    );
    drop(state);
    let reopened = reopen_session(&storage_root, session_name);
    let state = PersistentAgentState::open(reopened).expect("完成证据应跨重启恢复");
    assert_eq!(
        state
            .goal_snapshot()
            .expect("重启后 Goal 应读取")
            .goal
            .as_ref()
            .and_then(|goal| goal.completion_evidence.as_deref()),
        Some("Goal 生命周期回归测试通过")
    );
    let update_retry = state
        .update_goal("goal-update-original", patch)
        .expect("进入终态后更新重试应去重");
    assert!(!update_retry.changed);
    assert_eq!(update_retry.current.revision, 3);
    state
        .clear_goal("goal-clear-original")
        .expect("Goal 应清除");
    drop(state);
    let reopened = reopen_session(&storage_root, session_name);
    let state = PersistentAgentState::open(reopened).expect("Goal 收据应跨重启恢复");
    let transition_retry = state
        .transition_goal("goal-transition-original", completion)
        .expect("清除后终态重试应去重");
    assert!(!transition_retry.changed);
    assert!(transition_retry.current.goal.is_none());
    state
        .create_goal(
            "goal-create-next",
            GoalDraft {
                title: "后续目标".to_owned(),
                objective: "验证清除收据不会因重新创建丢失".to_owned(),
                description: None,
                token_budget: None,
                progress_percent: None,
            },
        )
        .expect("新 Goal 应创建");
    let clear_retry = state
        .clear_goal("goal-clear-original")
        .expect("重新创建后原清除操作仍应去重");
    assert!(!clear_retry.changed);
    assert_eq!(clear_retry.current.revision, 5);
    assert_eq!(
        clear_retry
            .current
            .goal
            .as_ref()
            .map(|goal| goal.title.as_str()),
        Some("后续目标")
    );
    assert!(matches!(
        state.transition_goal(
            "goal-transition-original",
            GoalTransition {
                status: GoalStatus::Completed,
                blocked_reason: None,
                completion_evidence: Some("不同完成证据".to_owned()),
            },
        ),
        Err(RuntimeStateError::Conflict { .. })
    ));
    assert!(matches!(
        state.create_goal(
            "goal-create-original",
            GoalDraft {
                title: "冲突目标".to_owned(),
                objective: "相同操作标识不得改绑".to_owned(),
                description: None,
                token_budget: None,
                progress_percent: None,
            },
        ),
        Err(RuntimeStateError::Conflict { .. })
    ));
}

/// Plan 收据必须跨重启保留，并在后续正文变化后返回当前权威快照。
#[test]
fn plan_receipts_are_restart_safe_and_payload_bound() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let session_name = "session-plan-receipts";
    let session = create_session(&storage_root, session_name, &project_root);
    let state = PersistentAgentState::open(session.clone()).expect("持久状态应打开");
    let session_id = SessionId::new(session_name).expect("Session ID 应有效");
    let root = AgentId::new("root").expect("根 Agent ID 应有效");
    state
        .replace_plan("plan-original", &session_id, &root, "第一版计划".to_owned())
        .expect("第一版计划应保存");
    state
        .replace_plan("plan-later", &session_id, &root, "第二版计划".to_owned())
        .expect("第二版计划应保存");

    drop(state);
    drop(session);
    let reopened = reopen_session(&storage_root, session_name);
    let recovered = PersistentAgentState::open(reopened).expect("持久状态应恢复");
    let retry = recovered
        .replace_plan("plan-original", &session_id, &root, "第一版计划".to_owned())
        .expect("原始 Plan 重试应去重");
    assert!(!retry.changed);
    assert_eq!(retry.current.revision, 2);
    assert_eq!(retry.current.content.as_deref(), Some("第二版计划"));
    assert!(matches!(
        recovered.replace_plan("plan-original", &session_id, &root, "冲突计划".to_owned(),),
        Err(RuntimeStateError::Conflict { .. })
    ));
    let cleared = recovered
        .clear_plan("plan-clear", &session_id, &root)
        .expect("Plan 应清除");
    assert!(cleared.changed);
    let clear_retry = recovered
        .clear_plan("plan-clear", &session_id, &root)
        .expect("Plan 清除重试应去重");
    assert!(!clear_retry.changed);
    assert_eq!(clear_retry.current.revision, 3);
}

/// 并发根 Agent 写计划时，三层计划文档与权威 Session Artifact 必须始终保持同一版本。
#[test]
fn concurrent_root_plan_writes_keep_authority_and_document_aligned() {
    let storage_root = TempDir::new().expect("应用数据临时目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let session = create_session(&storage_root, "session-plan-authority-race", &project_root);
    let first = Arc::new(PersistentAgentState::open(session.clone()).expect("首个持久状态应打开"));
    let second =
        Arc::new(PersistentAgentState::open(session.clone()).expect("第二个持久状态应打开"));
    let project_scope = first.project_scope().clone();
    let session_id = SessionId::new("session-plan-authority-race").expect("Session ID 应有效");
    let root = AgentId::new("root").expect("根 Agent ID 应有效");
    let resource_session_id =
        keencode_resources::SessionId::new(session_id.as_str()).expect("资源 Session ID 应有效");
    let resource_root =
        keencode_resources::AgentId::new(root.as_str()).expect("资源根 Agent ID 应有效");
    let barrier = Arc::new(Barrier::new(2));
    let handles = [
        (first, "plan-authority-a", "# 方案 A"),
        (second, "plan-authority-b", "# 方案 B"),
    ]
    .map(|(state, operation_id, content)| {
        let barrier = barrier.clone();
        let session_id = session_id.clone();
        let root = root.clone();
        std::thread::spawn(move || {
            barrier.wait();
            state.replace_plan(operation_id, &session_id, &root, content.to_owned())
        })
    });
    for handle in handles {
        handle
            .join()
            .expect("并发根计划线程不应 panic")
            .expect("并发根计划写入应成功");
    }

    let plan_store = PlanFileStore::open(storage_root.path()).expect("Plan Store 应打开");
    let document = plan_store
        .read(&project_scope, &resource_session_id, &resource_root)
        .expect("并发后计划文档应读取")
        .expect("并发后应存在根计划");
    assert_eq!(
        session
            .snapshot()
            .expect("并发后 Session 快照应读取")
            .state
            .plan
            .plan_artifact,
        document.plan_artifact,
        "权威 PlanChanged 必须引用计划文档当前 Artifact"
    );
    assert!(document.plan_artifact.is_some());
}

/// 清除或 CAS 冲突产生的未引用计划 Artifact 必须在 Session 冷恢复时回收，当前引用必须保留。
#[test]
fn plan_artifact_orphans_are_reclaimed_after_cold_recovery() {
    let storage_root = TempDir::new().expect("应用数据目录应创建");
    let project_root = TempDir::new().expect("用户项目目录应创建");
    let session_name = "session-plan-orphan-recovery";
    let session = create_session(&storage_root, session_name, &project_root);
    let state = PersistentAgentState::open(session.clone()).expect("持久状态应打开");
    let session_id = SessionId::new(session_name).expect("Session ID 应有效");
    let root = AgentId::new("root").expect("根 Agent ID 应有效");

    state
        .replace_plan(
            "plan-orphan-first",
            &session_id,
            &root,
            "# 第一版".to_owned(),
        )
        .expect("第一版计划应保存");
    let first_artifact = session
        .snapshot()
        .expect("第一版快照应读取")
        .state
        .plan
        .plan_artifact
        .expect("第一版应有关联 Artifact");
    state
        .clear_plan("plan-orphan-clear", &session_id, &root)
        .expect("第一版计划应清除");

    state
        .replace_plan(
            "plan-orphan-second",
            &session_id,
            &root,
            "# 第二版".to_owned(),
        )
        .expect("第二版计划应保存");
    let second_artifact = session
        .snapshot()
        .expect("第二版快照应读取")
        .state
        .plan
        .plan_artifact
        .expect("第二版应有关联 Artifact");
    let third_artifact = session
        .put_artifact("# 第三版".as_bytes(), Some("text/markdown".to_owned()))
        .expect("冲突测试 Artifact 应预先写入")
        .as_event_use();
    assert!(matches!(
        state.replace_plan(
            "plan-orphan-second",
            &session_id,
            &root,
            "# 第三版".to_owned(),
        ),
        Err(RuntimeStateError::Conflict { .. })
    ));

    let artifact_path = |artifact: &keencode_resources::ArtifactUse| {
        storage_root
            .path()
            .join("sessions")
            .join(session_name)
            .join("artifacts")
            .join(format!("{}.artifact", artifact.artifact_id.as_str()))
    };
    let first_path = artifact_path(&first_artifact);
    let second_path = artifact_path(&second_artifact);
    let third_path = artifact_path(&third_artifact);
    assert!(first_path.is_file(), "冷恢复前清除的 Artifact 应暂时存在");
    assert!(second_path.is_file(), "当前计划 Artifact 应存在");
    assert!(third_path.is_file(), "CAS 冲突产生的 Artifact 应暂时存在");

    drop(state);
    drop(session);
    let reopened = reopen_session(&storage_root, session_name);
    assert_eq!(
        reopened
            .snapshot()
            .expect("冷恢复后 Session 快照应读取")
            .state
            .plan
            .plan_artifact,
        Some(second_artifact)
    );
    drop(reopened);
    assert!(!first_path.exists(), "清除后的孤儿 Artifact 应被回收");
    assert!(second_path.is_file(), "权威当前 Artifact 不能被误回收");
    assert!(!third_path.exists(), "CAS 冲突后的孤儿 Artifact 应被回收");
}
