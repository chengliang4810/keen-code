use std::fs;
use std::sync::{Arc, Barrier};

use keencode_resources::{
    AgentId, ArtifactLimits, ArtifactStore, DocumentLimits, DocumentOperationOutcome,
    MAX_DOCUMENT_OPERATION_RECEIPTS, PlanDocument, PlanFileStore, PlanState, ResourceError,
    ScopeId, SessionEvent, SessionEventId, SessionId, SessionJournal, SessionOpen,
};
use tempfile::TempDir;

/// 创建一个带正文和稳定测试时间戳的计划候选文档。
fn plan(
    scope: &ScopeId,
    session_id: &SessionId,
    agent_id: &AgentId,
    content: &str,
) -> PlanDocument {
    let mut document = PlanDocument::new(scope.clone(), session_id.clone(), agent_id.clone());
    document.content = Some(content.to_owned());
    document.updated_at_unix_ms = Some(1_000);
    document
}

/// 计划文档必须跨重新打开恢复，并按项目、Session 与 Agent 三层隔离。
#[test]
fn plan_round_trip_reopens_in_three_level_sandbox() {
    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let user_project = TempDir::new().expect("用户项目临时目录应创建");
    let scope = ScopeId::new("project-alpha").expect("项目作用域应有效");
    let other_scope = ScopeId::new("project-beta").expect("其他项目作用域应有效");
    let session_id = SessionId::new("session-one").expect("Session ID 应有效");
    let other_session = SessionId::new("session-two").expect("其他 Session ID 应有效");
    let root_agent = AgentId::new("root").expect("根 Agent ID 应有效");
    let child_agent = AgentId::new("child-one").expect("子 Agent ID 应有效");
    let store = PlanFileStore::open(app_data.path()).expect("Plan Store 应打开");

    let operation = ("plan_replace_v1", "# 方案");
    let saved = store
        .compare_and_swap(
            "plan-round-trip",
            &operation,
            0,
            plan(&scope, &session_id, &root_agent, "# 方案"),
        )
        .expect("计划应原子保存")
        .into_document();
    assert_eq!(saved.revision, 1);
    assert_eq!(saved.content.as_deref(), Some("# 方案"));
    assert!(
        app_data
            .path()
            .join("plans")
            .join(scope.as_str())
            .join(session_id.as_str())
            .join(root_agent.as_str())
            .join("plan.json")
            .is_file()
    );
    assert!(!user_project.path().join("plans").exists());
    assert!(
        store
            .read(&scope, &session_id, &child_agent)
            .expect("其他 Agent 计划应读取")
            .is_none()
    );
    assert!(
        store
            .read(&scope, &other_session, &root_agent)
            .expect("其他 Session 计划应读取")
            .is_none()
    );
    assert!(
        store
            .read(&other_scope, &session_id, &root_agent)
            .expect("其他项目计划应读取")
            .is_none()
    );

    drop(store);
    let reopened = PlanFileStore::open(app_data.path()).expect("Plan Store 应重新打开");
    assert_eq!(
        reopened
            .read(&scope, &session_id, &root_agent)
            .expect("重新打开后计划应读取"),
        Some(saved)
    );
    assert!(matches!(
        reopened
            .compare_and_swap(
                "plan-round-trip",
                &operation,
                0,
                plan(&scope, &session_id, &root_agent, "# 方案"),
            )
            .expect("重新打开后相同操作应去重"),
        DocumentOperationOutcome::Deduplicated(_)
    ));
}

/// 计划比较交换必须只允许一个并发写入者提交相同 revision。
#[test]
fn plan_compare_and_swap_rejects_concurrent_stale_revision() {
    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let store = Arc::new(PlanFileStore::open(app_data.path()).expect("Plan Store 应打开"));
    let scope = ScopeId::new("project-concurrent").expect("项目作用域应有效");
    let session_id = SessionId::new("session-concurrent").expect("Session ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["# 方案 A", "# 方案 B"].map(|content| {
        let store = store.clone();
        let scope = scope.clone();
        let session_id = session_id.clone();
        let agent_id = agent_id.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            store.compare_and_swap(
                &format!("plan-concurrent-{content}"),
                &("plan_replace_v1", content),
                0,
                plan(&scope, &session_id, &agent_id, content),
            )
        })
    });
    let results = handles.map(|handle| handle.join().expect("并发计划线程不应 panic"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(ResourceError::RevisionConflict { .. })))
            .count(),
        1
    );
    let current = store
        .read(&scope, &session_id, &agent_id)
        .expect("并发后的计划应读取")
        .expect("并发后应有一个获胜计划");
    assert_eq!(current.revision, 1);
}

/// 计划 CAS 必须保存空初态收据，并拒绝伪造更新时间和时间倒退。
#[test]
fn plan_compare_and_swap_rejects_duplicate_content_and_time_regression() {
    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let store = PlanFileStore::open(app_data.path()).expect("Plan Store 应打开");
    let scope = ScopeId::new("project-transition").expect("项目作用域应有效");
    let session_id = SessionId::new("session-transition").expect("Session ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let empty = PlanDocument::new(scope.clone(), session_id.clone(), agent_id.clone());
    let empty = store
        .compare_and_swap("plan-clear-empty", &"plan_clear_v1", 0, empty)
        .expect("不存在计划时也应持久化清除收据")
        .into_document();
    assert_eq!(empty.revision, 0);
    assert_eq!(empty.operation_receipts.len(), 1);
    let saved = store
        .compare_and_swap(
            "plan-initial",
            &("plan_replace_v1", "# 初始方案"),
            empty.revision,
            plan(&scope, &session_id, &agent_id, "# 初始方案"),
        )
        .expect("初始计划应保存")
        .into_document();

    let mut duplicate = plan(&scope, &session_id, &agent_id, "# 初始方案");
    duplicate.revision = saved.revision;
    duplicate.updated_at_unix_ms = Some(2_000);
    let error = store
        .compare_and_swap(
            "plan-forged-time",
            &("plan_replace_v1", "# 初始方案"),
            saved.revision,
            duplicate,
        )
        .expect_err("相同正文不能制造新 revision");
    assert!(matches!(error, ResourceError::InvalidPlanTransition(_)));

    let mut regressed = plan(&scope, &session_id, &agent_id, "# 较旧方案");
    regressed.revision = saved.revision;
    regressed.updated_at_unix_ms = Some(999);
    let error = store
        .compare_and_swap(
            "plan-regressed-time",
            &("plan_replace_v1", "# 较旧方案"),
            saved.revision,
            regressed,
        )
        .expect_err("倒退时间不能覆盖现有计划");
    assert!(matches!(error, ResourceError::InvalidPlanTransition(_)));
    assert_eq!(
        store
            .read(&scope, &session_id, &agent_id)
            .expect("拒绝迁移后计划应读取"),
        Some(saved)
    );
}

/// 计划文档必须拒绝超限正文、路径段穿越和非目录边界。
#[test]
fn plan_store_enforces_size_and_path_boundaries() {
    assert!(ScopeId::new("../outside").is_err());
    assert!(SessionId::new("session/escape").is_err());
    assert!(AgentId::new("agent\\escape").is_err());

    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let scope = ScopeId::new("project-bounded").expect("项目作用域应有效");
    let session_id = SessionId::new("session-bounded").expect("Session ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let bounded = PlanFileStore::open_with_limits(
        app_data.path(),
        DocumentLimits {
            max_document_bytes: 256,
        },
    )
    .expect("有限 Plan Store 应打开");
    let error = bounded
        .compare_and_swap(
            "plan-too-large",
            &("plan_replace_v1", "x".repeat(512)),
            0,
            plan(&scope, &session_id, &agent_id, &"x".repeat(512)),
        )
        .expect_err("超限计划必须拒绝");
    assert!(matches!(error, ResourceError::DocumentTooLarge { .. }));

    let other_root = TempDir::new().expect("其他应用数据目录应创建");
    let store = PlanFileStore::open(other_root.path()).expect("Plan Store 应打开");
    fs::write(
        other_root.path().join("plans").join(scope.as_str()),
        b"not-a-directory",
    )
    .expect("非目录边界应创建");
    let error = store
        .read(&scope, &session_id, &agent_id)
        .expect_err("非目录路径边界必须拒绝");
    assert!(matches!(error, ResourceError::UnsafePath(_)));
}

/// 计划收据必须在后续变化后仍去重原请求，并拒绝同标识绑定不同载荷。
#[test]
fn plan_receipts_survive_later_changes_and_detect_payload_conflicts() {
    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let store = PlanFileStore::open(app_data.path()).expect("Plan Store 应打开");
    let scope = ScopeId::new("project-receipt").expect("项目作用域应有效");
    let session_id = SessionId::new("session-receipt").expect("Session ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let first_operation = ("plan_replace_v1", "第一版");
    let first = store
        .compare_and_swap(
            "plan-receipt-original",
            &first_operation,
            0,
            plan(&scope, &session_id, &agent_id, "第一版"),
        )
        .expect("第一版计划应保存")
        .into_document();
    let mut second_candidate = plan(&scope, &session_id, &agent_id, "第二版");
    second_candidate.revision = first.revision;
    second_candidate.updated_at_unix_ms = Some(2_000);
    let second = store
        .compare_and_swap(
            "plan-receipt-later",
            &("plan_replace_v1", "第二版"),
            first.revision,
            second_candidate,
        )
        .expect("第二版计划应保存")
        .into_document();

    let replay = store
        .compare_and_swap(
            "plan-receipt-original",
            &first_operation,
            first.revision,
            plan(&scope, &session_id, &agent_id, "第一版"),
        )
        .expect("原始操作在后续变化后仍应去重");
    assert!(replay.deduplicated());
    assert_eq!(replay.document(), &second);
    assert!(matches!(
        store.compare_and_swap(
            "plan-receipt-original",
            &("plan_replace_v1", "冲突正文"),
            second.revision,
            plan(&scope, &session_id, &agent_id, "冲突正文"),
        ),
        Err(ResourceError::OperationConflict)
    ));
}

/// 计划收据达到固定上限后只淘汰最旧项，保留其余操作的去重能力。
#[test]
fn plan_receipt_ledger_evicts_only_the_oldest_operation() {
    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let store = PlanFileStore::open(app_data.path()).expect("Plan Store 应打开");
    let scope = ScopeId::new("project-receipt-limit").expect("项目作用域应有效");
    let session_id = SessionId::new("session-receipt-limit").expect("Session ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let mut revision = 0;
    for index in 0..=MAX_DOCUMENT_OPERATION_RECEIPTS {
        let content = format!("计划-{index}");
        let mut candidate = plan(&scope, &session_id, &agent_id, &content);
        candidate.revision = revision;
        candidate.updated_at_unix_ms = Some(1_000 + index as u64);
        revision = store
            .compare_and_swap(
                &format!("plan-operation-{index}"),
                &("plan_replace_v1", &content),
                revision,
                candidate,
            )
            .expect("边界内计划操作应保存")
            .into_document()
            .revision;
    }
    let current = store
        .read(&scope, &session_id, &agent_id)
        .expect("计划应读取")
        .expect("计划应存在");
    assert_eq!(
        current.operation_receipts.len(),
        MAX_DOCUMENT_OPERATION_RECEIPTS
    );
    assert_eq!(
        current
            .operation_receipts
            .first()
            .map(|receipt| receipt.operation_id.as_str()),
        Some("plan-operation-1")
    );
    assert_eq!(
        current
            .operation_receipts
            .last()
            .map(|receipt| receipt.operation_id.as_str()),
        Some("plan-operation-256")
    );
    assert!(
        current
            .applied_operation_revision("plan-operation-1", &("plan_replace_v1", "计划-1"),)
            .expect("保留收据应查询")
            .is_some()
    );
    assert!(
        current
            .applied_operation_revision("plan-operation-0", &("plan_replace_v1", "计划-0"),)
            .expect("淘汰收据应查询")
            .is_none()
    );
}

/// 最终计划写入必须关联内容寻址 Artifact，并能被 Journal 的权威 PlanChanged 校验和恢复。
#[test]
fn final_plan_artifact_is_recoverable_and_authoritatively_associated() {
    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let scope = ScopeId::new("project-plan-artifact").expect("项目作用域应有效");
    let session_id = SessionId::new("session-plan-artifact").expect("Session ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let content = "# 最终方案\n\n- 保持项目目录不写入";
    let plans = PlanFileStore::open(app_data.path()).expect("Plan Store 应打开");
    let artifacts = Arc::new(
        ArtifactStore::open(
            app_data.path(),
            session_id.clone(),
            ArtifactLimits::default(),
        )
        .expect("Artifact Store 应打开"),
    );
    let mut candidate = plan(&scope, &session_id, &agent_id, content);
    candidate.updated_at_unix_ms = Some(2_000);
    let saved = plans
        .compare_and_swap_with_artifact(artifacts.as_ref(), "plan-artifact-write", 0, candidate)
        .expect("最终计划和 Artifact 应原子准备")
        .into_document();
    let artifact = saved
        .plan_artifact
        .clone()
        .expect("最终计划必须包含 Artifact 引用");
    artifacts
        .validate_use(&artifact)
        .expect("计划 Artifact 引用必须指向真实 pair");
    assert_eq!(
        artifacts.read_use(&artifact).expect("Artifact 应可读取"),
        content.as_bytes()
    );
    assert!(
        app_data
            .path()
            .join("plans")
            .join(scope.as_str())
            .join(session_id.as_str())
            .join(agent_id.as_str())
            .join("plan.json")
            .is_file()
    );

    // 以同一引用提交权威 PlanChanged；Journal 校验器不得接受伪造或缺失 Artifact。
    let journal = match SessionJournal::open_with_artifact_validator(
        app_data.path(),
        session_id.clone(),
        Default::default(),
        artifacts.clone(),
    )
    .expect("Journal 应打开")
    {
        SessionOpen::Ready(journal) => journal,
        SessionOpen::Corrupt(report) => panic!("测试 Journal 不应损坏：{:?}", report.issues),
    };
    let created_sequence = journal.state().expect("初始状态应读取").last_sequence;
    journal
        .append_idempotent(
            SessionEventId::new("event-plan-artifact-create").expect("事件 ID 应有效"),
            created_sequence,
            SessionEvent::SessionCreated {
                title: "计划 Artifact 测试".to_owned(),
                project_root: "D:/workspace".to_owned(),
            },
        )
        .expect("SessionCreated 应提交");
    let sequence = journal.state().expect("Session 状态应读取").last_sequence;
    journal
        .append_idempotent(
            SessionEventId::new("event-plan-artifact-change").expect("事件 ID 应有效"),
            sequence,
            SessionEvent::PlanChanged {
                plan: PlanState {
                    enabled: true,
                    plan_artifact: Some(artifact.clone()),
                },
            },
        )
        .expect("PlanChanged 应验证并提交 Artifact 引用");
    assert_eq!(
        journal
            .state()
            .expect("权威 Plan 状态应读取")
            .plan
            .plan_artifact,
        Some(artifact.clone())
    );

    drop(journal);
    drop(artifacts);
    let reopened_plans = PlanFileStore::open(app_data.path()).expect("Plan Store 应重开");
    assert_eq!(
        reopened_plans
            .read(&scope, &session_id, &agent_id)
            .expect("计划应可恢复")
            .and_then(|document| document.plan_artifact),
        Some(artifact)
    );
}

/// Artifact 内容与最终计划不一致时必须拒绝关联，并保持计划沙箱没有半成品文档。
#[test]
fn final_plan_artifact_mismatch_leaves_plan_document_unchanged() {
    let app_data = TempDir::new().expect("应用数据临时目录应创建");
    let scope = ScopeId::new("project-plan-artifact-failure").expect("项目作用域应有效");
    let session_id = SessionId::new("session-plan-artifact-failure").expect("Session ID 应有效");
    let agent_id = AgentId::new("root").expect("Agent ID 应有效");
    let plans = PlanFileStore::open(app_data.path()).expect("Plan Store 应打开");
    let artifacts = ArtifactStore::open(
        app_data.path(),
        session_id.clone(),
        ArtifactLimits::default(),
    )
    .expect("Artifact Store 应打开");
    let wrong_artifact = artifacts
        .put("另一份报告".as_bytes(), Some("text/markdown".to_owned()))
        .expect("测试 Artifact 应写入");
    let error = plans
        .compare_and_swap_with_artifact_ref(
            &wrong_artifact,
            "plan-artifact-mismatch",
            0,
            plan(&scope, &session_id, &agent_id, "# 目标计划"),
        )
        .expect_err("不匹配 Artifact 不得关联到计划");
    assert!(matches!(error, ResourceError::ArtifactHashMismatch));
    assert!(
        plans
            .read(&scope, &session_id, &agent_id)
            .expect("拒绝后计划应可读取")
            .is_none()
    );
}
