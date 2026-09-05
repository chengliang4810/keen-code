use super::*;
use peri_acp_types::goal::InMemoryGoalStore;
use std::sync::Arc;

fn make_state() -> GoalState {
    GoalState::new(
        Arc::new(InMemoryGoalStore::new()),
        "test-thread".to_string(),
    )
}

#[tokio::test]
async fn test_set_goal_写入_store_并触发_objective_updated() {
    let state = make_state();
    state
        .set_goal("完成模块重构".to_string(), Some(200_000))
        .await
        .unwrap();

    let snap = state.snapshot();
    assert_eq!(snap.objective.as_deref(), Some("完成模块重构"));
    assert_eq!(snap.token_budget, Some(200_000));
    assert_eq!(snap.status, Some(GoalStatus::Active));
    assert!(snap.objective_just_updated);
}

#[tokio::test]
async fn test_clear_清空_goal() {
    let state = make_state();
    state.set_goal("临时目标".to_string(), None).await.unwrap();
    state.clear().await.unwrap();

    let snap = state.snapshot();
    assert!(snap.objective.is_none());
    assert!(!snap.objective_just_updated);
}

#[tokio::test]
async fn test_set_goal_覆盖旧_goal_生成新_goal_id() {
    let state = make_state();
    state.set_goal("目标 A".to_string(), None).await.unwrap();
    let id_a = state.snapshot().goal_id.clone().unwrap();

    state.set_goal("目标 B".to_string(), None).await.unwrap();
    let id_b = state.snapshot().goal_id.clone().unwrap();

    assert_ne!(id_a, id_b);
    assert_eq!(state.snapshot().objective.as_deref(), Some("目标 B"));
}

#[tokio::test]
async fn test_store_写入失败_内存镜像仍可读() {
    use async_trait::async_trait;
    use peri_acp_types::goal::{GoalStore, GoalStoreError, ThreadGoal};

    struct FailingStore;
    #[async_trait]
    impl GoalStore for FailingStore {
        async fn save(&self, _: &str, _: ThreadGoal) -> Result<(), GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }
        async fn load(&self, _: &str) -> Result<Option<ThreadGoal>, GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }
        async fn delete(&self, _: &str) -> Result<(), GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }
    }

    let state = GoalState::new(Arc::new(FailingStore), "test-thread".to_string());
    // set_goal 即使 store 失败也不 panic（内存镜像更新成功）
    let result = state.set_goal("fallback".to_string(), None).await;
    // store 失败返回 Err，但内存镜像已更新
    assert!(result.is_err());
    assert_eq!(state.snapshot().objective.as_deref(), Some("fallback"));
}

#[tokio::test]
async fn test_set_status_合法转换_active_to_blocked() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Active));

    state
        .set_status_with_reason(GoalStatus::Blocked, "缺少依赖".to_string())
        .await
        .unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Blocked));
}

#[tokio::test]
async fn test_set_status_非法转换_complete_to_active_返回错误() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.set_status(GoalStatus::Complete).await.unwrap();

    let result = state.set_status(GoalStatus::Active).await;
    assert!(result.is_err());
    // 状态未改变
    assert_eq!(state.snapshot().status, Some(GoalStatus::Complete));
}

#[tokio::test]
async fn test_set_status_blocked_必须附带_reason() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();

    let result = state.set_status(GoalStatus::Blocked).await;
    assert!(result.is_err(), "Blocked 必须附带 reason");

    state
        .set_status_with_reason(GoalStatus::Blocked, "缺少依赖".to_string())
        .await
        .unwrap();
    assert_eq!(state.snapshot().status, Some(GoalStatus::Blocked));
}

#[tokio::test]
async fn test_set_status_无_goal_返回错误() {
    let state = make_state();
    let result = state.set_status(GoalStatus::Complete).await;
    assert!(result.is_err(), "无 goal 时 set_status 应失败");
}

#[tokio::test]
async fn test_resume_from_complete_返回错误() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.set_status(GoalStatus::Complete).await.unwrap();

    // Complete 是终态，不能 resume
    let result = state.set_status(GoalStatus::Active).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_put_pending_user_message_覆盖旧值() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();

    state.put_pending_user_message("第一条".to_string());
    state.put_pending_user_message("第二条".to_string());

    let taken = state.take_pending_user_message();
    assert_eq!(taken.as_deref(), Some("第二条"));
    // take 后清空
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_clear_goal_清零_pending_user_message() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.put_pending_user_message("待清空".to_string());

    state.clear().await.unwrap();
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_set_status_complete_清零_pending_user_message() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.put_pending_user_message("待清空".to_string());

    state.set_status(GoalStatus::Complete).await.unwrap();
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_set_status_blocked_清零_pending_user_message() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();
    state.put_pending_user_message("待清空".to_string());

    state
        .set_status_with_reason(GoalStatus::Blocked, "阻塞原因".to_string())
        .await
        .unwrap();
    assert!(state.take_pending_user_message().is_none());
}

#[tokio::test]
async fn test_record_token_usage_累积到_pending() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(1000);
    state.record_token_usage(500);

    // pending 累积 1500，但 snapshot 还没 flush
    // snapshot 读取的是已 flush 的值，所以仍是 0
    assert_eq!(state.snapshot().tokens_used, 0);
}

#[tokio::test]
async fn test_flush_progress_写入_goal_accounting() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(1500);
    state.flush_progress().await.unwrap();

    assert_eq!(state.snapshot().tokens_used, 1500);
}

#[tokio::test]
async fn test_flush_progress_多次累加() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(1000);
    state.flush_progress().await.unwrap();
    state.record_token_usage(500);
    state.flush_progress().await.unwrap();

    assert_eq!(state.snapshot().tokens_used, 1500);
}

#[tokio::test]
async fn test_record_time_usage_累积并_flush() {
    let state = make_state();
    state.set_goal("测试".to_string(), None).await.unwrap();

    state.record_time_usage(30);
    state.record_time_usage(15);
    state.flush_progress().await.unwrap();

    assert_eq!(state.snapshot().time_used_seconds, 45);
}

#[tokio::test]
async fn test_usage_pct_基于_flushed_值() {
    let state = make_state();
    state
        .set_goal("测试".to_string(), Some(200_000))
        .await
        .unwrap();

    state.record_token_usage(160_000);
    state.flush_progress().await.unwrap();

    let snap = state.snapshot();
    assert!((snap.usage_pct().unwrap() - 0.8).abs() < 0.01);
}

// ---- GoalController trait 实现测试 ----

use peri_acp_types::goal::{GoalController, GoalStore};

#[tokio::test]
async fn test_goal_controller_create_duplicate_errors() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    // 第一次 create 成功
    state.create_goal("测试目标".to_string()).await.unwrap();

    // 第二次 create 报错
    let result = state.create_goal("另一个目标".to_string()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("已存在"));
}

#[tokio::test]
async fn test_goal_controller_complete() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    state.create_goal("测试目标".to_string()).await.unwrap();
    state.complete_goal().await.unwrap();

    let snap = state.snapshot();
    assert_eq!(snap.status, Some(GoalStatus::Complete));
}

#[tokio::test]
async fn test_goal_controller_block_requires_reason() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    state.create_goal("测试目标".to_string()).await.unwrap();
    state.block_goal("缺权限".to_string()).await.unwrap();

    let snap = state.snapshot();
    assert_eq!(snap.status, Some(GoalStatus::Blocked));
}

#[tokio::test]
async fn test_goal_controller_terminal_cannot_transition() {
    let store = Arc::new(InMemoryGoalStore::new()) as Arc<dyn GoalStore>;
    let state = GoalState::new(store, "thread-1".to_string());

    state.create_goal("测试目标".to_string()).await.unwrap();
    state.complete_goal().await.unwrap();

    // 终态不能再转
    let result = state.complete_goal().await;
    assert!(result.is_err());
}

/// 新版 mutation API 应在创建、迁移和清除之间保持单调 revision。
#[tokio::test]
async fn test_goal_mutation_revision_连续递增() {
    let state = make_state();
    assert_eq!(state.snapshot().revision, 0);

    let created = state
        .upsert_goal("目标".to_string(), Some(100), Some(0), None)
        .await
        .unwrap();
    assert_eq!(created.revision, 1);
    let goal_id = created.snapshot.goal_id.clone().unwrap();

    let completed = state
        .transition_goal(
            Some(&goal_id),
            Some(created.revision),
            GoalStatus::Complete,
            String::new(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(completed.revision, 2);
    assert_eq!(completed.snapshot.status, Some(GoalStatus::Complete));

    let cleared = state
        .clear_with_preconditions(Some(&goal_id), Some(completed.revision), None)
        .await
        .unwrap();
    assert_eq!(cleared.revision, 3);
    assert!(cleared.snapshot.goal_id.is_none());

    // 空集合 clear 是幂等 no-op，不应制造虚假的版本变化。
    let empty_clear = state
        .clear_with_preconditions(None, Some(cleared.revision), None)
        .await
        .unwrap();
    assert_eq!(empty_clear.revision, 3);
}

/// Goal mutation 应拒绝过期 revision，并保留当前状态不变。
#[tokio::test]
async fn test_goal_mutation_过期revision返回冲突() {
    let state = make_state();
    let created = state
        .upsert_goal("目标".to_string(), None, None, None)
        .await
        .unwrap();
    let goal_id = created.snapshot.goal_id.clone().unwrap();

    let error = state
        .transition_goal(
            Some(&goal_id),
            Some(0),
            GoalStatus::Complete,
            String::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error,
        GoalMutationError::RevisionConflict {
            expected: 0,
            actual: 1,
        }
    );
    assert_eq!(state.snapshot().status, Some(GoalStatus::Active));
    assert_eq!(state.snapshot().revision, 1);
}

/// 同一个 requestNonce 重放应返回第一次提交的快照，复用到不同请求则冲突。
#[tokio::test]
async fn test_goal_mutation_request_nonce_幂等与冲突() {
    let state = make_state();
    let first = state
        .upsert_goal("目标".to_string(), None, None, Some("nonce-1"))
        .await
        .unwrap();
    let replay = state
        .upsert_goal("目标".to_string(), None, None, Some("nonce-1"))
        .await
        .unwrap();
    assert_eq!(replay.revision, first.revision);
    assert_eq!(replay.snapshot.goal_id, first.snapshot.goal_id);
    assert!(replay.deduplicated);

    let error = state
        .upsert_goal("另一个目标".to_string(), None, None, Some("nonce-1"))
        .await
        .unwrap_err();
    assert_eq!(
        error,
        GoalMutationError::NonceConflict {
            nonce: "nonce-1".to_string(),
        }
    );
    assert_eq!(state.snapshot().objective.as_deref(), Some("目标"));
    assert_eq!(state.snapshot().revision, 1);
}

/// 状态机应覆盖 blocked reason、Goal ID 校验和两个终态的不可恢复性。
#[tokio::test]
async fn test_goal_mutation_状态机边界() {
    let state = make_state();
    let created = state
        .upsert_goal("目标".to_string(), None, None, None)
        .await
        .unwrap();
    let goal_id = created.snapshot.goal_id.clone().unwrap();

    let missing_reason = state
        .transition_goal(
            Some(&goal_id),
            Some(1),
            GoalStatus::Blocked,
            "   ".to_string(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(missing_reason, GoalMutationError::BlockedReasonRequired);
    assert_eq!(state.snapshot().revision, 1);

    let blocked = state
        .transition_goal(
            Some(&goal_id),
            Some(1),
            GoalStatus::Blocked,
            "缺少依赖".to_string(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(blocked.snapshot.blocked_reason.as_deref(), Some("缺少依赖"));
    assert_eq!(blocked.revision, 2);

    let resume = state
        .transition_goal(
            Some(&goal_id),
            Some(2),
            GoalStatus::Active,
            String::new(),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        resume,
        GoalMutationError::InvalidTransition { .. }
    ));

    let wrong_id = state
        .clear_with_preconditions(Some("wrong-goal"), Some(2), None)
        .await
        .unwrap_err();
    assert_eq!(
        wrong_id,
        GoalMutationError::GoalIdMismatch {
            expected: "wrong-goal".to_string(),
            actual: goal_id,
        }
    );
}

/// 两个并发请求使用同一旧 revision 时只能有一个完成迁移。
#[tokio::test]
async fn test_goal_mutation_并发revision只允许一个成功() {
    let state = make_state();
    let created = state
        .upsert_goal("目标".to_string(), None, None, None)
        .await
        .unwrap();
    let goal_id = created.snapshot.goal_id.clone().unwrap();

    let (complete, block) = tokio::join!(
        state.transition_goal(
            Some(goal_id.as_str()),
            Some(created.revision),
            GoalStatus::Complete,
            String::new(),
            None,
        ),
        state.transition_goal(
            Some(goal_id.as_str()),
            Some(created.revision),
            GoalStatus::Blocked,
            "并发请求失败".to_string(),
            None,
        ),
    );

    assert_eq!(
        usize::from(complete.is_ok()) + usize::from(block.is_ok()),
        1
    );
    let conflict = if let Err(error) = complete {
        error
    } else {
        block.expect_err("第二个并发迁移应因旧 revision 失败")
    };
    assert!(matches!(
        conflict,
        GoalMutationError::RevisionConflict {
            expected: 1,
            actual: 2
        }
    ));
    assert_eq!(state.snapshot().revision, 2);
    assert!(state
        .snapshot()
        .status
        .is_some_and(|status| status.is_terminal()));
}

/// 构造轻量回执，供有界缓存自身的容量和顺序测试使用。
fn test_receipt(objective: &str) -> GoalMutationReceipt {
    GoalMutationReceipt {
        request: GoalMutationRequest::Upsert {
            objective: objective.to_string(),
            token_budget: None,
            expected_revision: None,
        },
        result: GoalMutationResult {
            revision: 1,
            snapshot: GoalSnapshot {
                objective: Some(objective.to_string()),
                ..GoalSnapshot::default()
            },
            deduplicated: false,
        },
    }
}

/// 回执缓存达到容量后保留最近请求；更新同一 nonce 不应重复占用顺序槽位。
#[test]
fn test_goal_mutation_receipts_容量边界与重复更新顺序() {
    let mut receipts = GoalMutationReceipts::new();
    for index in 0..GOAL_REQUEST_RECEIPT_CAPACITY {
        let nonce = format!("nonce-{index}");
        receipts.insert(nonce, test_receipt(&format!("目标-{index}")));
    }
    assert_eq!(receipts.len(), GOAL_REQUEST_RECEIPT_CAPACITY);
    assert!(receipts.contains("nonce-0"));

    // 更新已有项应移动到最近位置，但缓存总数不能增加或出现重复顺序项。
    receipts.insert("nonce-0".to_string(), test_receipt("目标-0-更新"));
    assert_eq!(receipts.len(), GOAL_REQUEST_RECEIPT_CAPACITY);

    receipts.insert("nonce-new".to_string(), test_receipt("目标-new"));
    assert_eq!(receipts.len(), GOAL_REQUEST_RECEIPT_CAPACITY);
    assert!(
        receipts.contains("nonce-0"),
        "更新后的最近 nonce 不应被淘汰"
    );
    assert!(!receipts.contains("nonce-1"), "最旧的未访问 nonce 应被淘汰");
}

/// 超过容量后，已淘汰 nonce 应视为新请求；仍保留的 nonce 继续检测冲突。
#[tokio::test]
async fn test_goal_mutation_receipts_淘汰后重用与保留项冲突() {
    let state = make_state();
    for index in 0..=GOAL_REQUEST_RECEIPT_CAPACITY {
        let nonce = format!("nonce-{index}");
        state
            .upsert_goal(format!("目标-{index}"), None, None, Some(nonce.as_str()))
            .await
            .unwrap();
    }

    // nonce-0 是最早的一项，已在插入第 CAPACITY+1 项时淘汰。
    let reused = state
        .upsert_goal("淘汰后重用".to_string(), None, None, Some("nonce-0"))
        .await
        .unwrap();
    assert!(!reused.deduplicated);
    assert_eq!(reused.revision, (GOAL_REQUEST_RECEIPT_CAPACITY + 2) as u64);
    assert_eq!(reused.snapshot.objective.as_deref(), Some("淘汰后重用"));

    // nonce-CAPACITY 仍在缓存中，复用到不同请求必须保持冲突语义。
    let conflict = state
        .upsert_goal(
            "不应写入".to_string(),
            None,
            None,
            Some(format!("nonce-{}", GOAL_REQUEST_RECEIPT_CAPACITY).as_str()),
        )
        .await
        .unwrap_err();
    assert_eq!(
        conflict,
        GoalMutationError::NonceConflict {
            nonce: format!("nonce-{}", GOAL_REQUEST_RECEIPT_CAPACITY),
        }
    );
    assert_eq!(state.snapshot().objective.as_deref(), Some("淘汰后重用"));
}

/// store 保存失败时，内存 mutation 与 receipt 已提交；同 nonce 重试只重放内存结果，
/// 不再次递增 revision。首次调用仍返回存储错误，deduplicated 只表示内存幂等重放。
#[tokio::test]
async fn test_goal_mutation_store失败仍保留nonce_receipt语义() {
    use async_trait::async_trait;
    use peri_acp_types::goal::{GoalStore, GoalStoreError, ThreadGoal};

    struct FailingStore;
    #[async_trait]
    impl GoalStore for FailingStore {
        async fn save(&self, _: &str, _: ThreadGoal) -> Result<(), GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }

        async fn load(&self, _: &str) -> Result<Option<ThreadGoal>, GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }

        async fn delete(&self, _: &str) -> Result<(), GoalStoreError> {
            Err(GoalStoreError::Io("simulated".to_string()))
        }
    }

    let state = GoalState::new(Arc::new(FailingStore), "test-thread".to_string());
    let first = state
        .upsert_goal("只提交内存".to_string(), None, None, Some("store-failure"))
        .await
        .unwrap_err();
    assert!(matches!(
        first,
        GoalMutationError::Store(message) if message.contains("simulated")
    ));
    assert_eq!(state.snapshot().revision, 1);

    let replay = state
        .upsert_goal("只提交内存".to_string(), None, None, Some("store-failure"))
        .await
        .unwrap();
    assert!(replay.deduplicated);
    assert_eq!(replay.revision, 1);
}
