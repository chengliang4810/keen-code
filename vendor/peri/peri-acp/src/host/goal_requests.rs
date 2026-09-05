//! KeenCode Goal ACP 方法处理。
//!
//! 为桌面端提供 `session/goal-get`、`session/goal-upsert`、
//! `session/goal-transition` 与 `session/goal-clear`。revision 是 Session 级
//! 单调集合版本；带 requestNonce 的写入按完整请求身份幂等。

use peri_acp_types::goal::GoalStatus;
use serde_json::{json, Value};

use crate::session::goal_state::{GoalMutationError, GoalMutationResult, GoalSnapshot};
use crate::transport::types::AcpError;

use super::AcpServerConfig;

/// Goal 乐观并发冲突使用稳定的应用级错误码，客户端可据此刷新快照。
const GOAL_CONFLICT_CODE: i64 = -32009;

/// 将 Goal 状态错误映射为 ACP JSON-RPC 错误，并保留冲突诊断数据。
fn map_goal_error(error: GoalMutationError) -> AcpError {
    match error {
        GoalMutationError::RevisionConflict { expected, actual } => AcpError::new(
            GOAL_CONFLICT_CODE,
            format!("revision 冲突：期望 {expected}，当前 {actual}"),
        )
        .with_data(json!({
            "kind": "revision_conflict",
            "expectedRevision": expected,
            "currentRevision": actual,
        })),
        GoalMutationError::GoalIdMismatch { expected, actual } => AcpError::new(
            GOAL_CONFLICT_CODE,
            format!("goalId 不匹配：请求 {expected}，当前 {actual}"),
        )
        .with_data(json!({
            "kind": "goal_id_conflict",
            "expectedGoalId": expected,
            "currentGoalId": actual,
        })),
        GoalMutationError::NonceConflict { nonce } => AcpError::new(
            GOAL_CONFLICT_CODE,
            format!("requestNonce 已用于不同的 Goal 请求：{nonce}"),
        )
        .with_data(json!({
            "kind": "request_nonce_conflict",
            "requestNonce": nonce,
        })),
        GoalMutationError::Store(message) => AcpError::new(-32603, message),
        other => AcpError::new(-32602, other.to_string()),
    }
}

/// 解析可选的 expectedRevision；字段存在但类型错误时不能静默降级为 None。
fn optional_revision(params: &Value) -> Result<Option<u64>, AcpError> {
    match params.get("expectedRevision") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| AcpError::new(-32602, "expectedRevision 必须是非负整数")),
    }
}

/// 解析迁移操作必需的 expectedRevision。
fn required_revision(params: &Value) -> Result<u64, AcpError> {
    optional_revision(params)?.ok_or_else(|| AcpError::new(-32602, "缺少 expectedRevision"))
}

/// 解析可选 requestNonce；空白值不能冒充幂等 key。
fn optional_request_nonce(params: &Value) -> Result<Option<String>, AcpError> {
    match params.get("requestNonce") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Err(AcpError::new(-32602, "requestNonce 不能为空")),
        Some(_) => Err(AcpError::new(-32602, "requestNonce 必须是非空字符串")),
    }
}

/// 解析可选 Goal 身份；clear 允许省略，transition 不允许省略。
fn optional_goal_id(params: &Value) -> Result<Option<String>, AcpError> {
    match params.get("goalId") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Err(AcpError::new(-32602, "goalId 不能为空")),
        Some(_) => Err(AcpError::new(-32602, "goalId 必须是非空字符串")),
    }
}

/// 将 Goal 写操作结果映射为前端所需的响应字段。
fn goal_mutation_response(session_id: &str, result: &GoalMutationResult) -> Value {
    json!({
        "sessionId": session_id,
        "revision": result.revision,
        "goal": goal_dto_from_snapshot(&result.snapshot),
        "deduplicated": result.deduplicated,
    })
}

/// 将 Goal 快照映射为 KeenCode 前端使用的 Goal 记录。
fn goal_dto_from_snapshot(snapshot: &GoalSnapshot) -> Value {
    let status = snapshot
        .status
        .map(|status| match status {
            GoalStatus::Active => "active",
            GoalStatus::Complete => "completed",
            GoalStatus::Blocked => "blocked",
        })
        .unwrap_or("pending");
    json!({
        "id": snapshot.goal_id.clone().unwrap_or_default(),
        "title": snapshot.objective.clone().unwrap_or_default(),
        "objective": snapshot.objective.clone().unwrap_or_default(),
        "scope": "project",
        "status": status,
        "description": "",
        "progress_percent": snapshot.usage_pct().map(|value| value * 100.0),
        "token_budget": snapshot.token_budget,
        "tokens_used": snapshot.tokens_used,
        "time_used_seconds": snapshot.time_used_seconds,
        "blocked_reason": snapshot.blocked_reason,
        "created_at": "",
        "updated_at": "",
    })
}

/// 查询当前 Session 的 Goal。
pub(crate) async fn handle_goal_get(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "缺少 sessionId"))?;
    let goal_state = cfg
        .session_manager
        .goal_state_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session 不存在: {session_id}")))?;
    let snapshot = goal_state.snapshot();
    let goal = snapshot
        .goal_id
        .as_ref()
        .map(|_| goal_dto_from_snapshot(&snapshot));
    Ok(json!({
        "sessionId": session_id,
        "revision": snapshot.revision,
        "goals": goal.clone().map(|value| vec![value]).unwrap_or_default(),
        "activeGoalId": snapshot.goal_id,
    }))
}

/// 创建或完整更新当前 Session 的 Goal。
pub(crate) async fn handle_goal_upsert(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "缺少 sessionId"))?;
    let goal = params
        .get("goal")
        .ok_or_else(|| AcpError::new(-32602, "缺少 goal"))?;
    let title = goal
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AcpError::new(-32602, "goal.title 必填"))?;
    let token_budget = params.get("tokenBudget").and_then(Value::as_u64);
    let expected_revision = optional_revision(params)?;
    let request_nonce = optional_request_nonce(params)?;

    let goal_state = cfg
        .session_manager
        .goal_state_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session 不存在: {session_id}")))?;
    let result = goal_state
        .upsert_goal(
            title.to_string(),
            token_budget,
            expected_revision,
            request_nonce.as_deref(),
        )
        .await
        .map_err(map_goal_error)?;
    Ok(goal_mutation_response(session_id, &result))
}

/// 迁移当前 Goal 的生命周期状态。
pub(crate) async fn handle_goal_transition(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "缺少 sessionId"))?;
    let goal_id = params
        .get("goalId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AcpError::new(-32602, "缺少 goalId"))?;
    let expected_revision = required_revision(params)?;
    let request_nonce = optional_request_nonce(params)?;
    let status = params
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "缺少 status"))?;
    let target = match status {
        "active" => GoalStatus::Active,
        "completed" => GoalStatus::Complete,
        "blocked" => GoalStatus::Blocked,
        _ => {
            return Err(AcpError::new(
                -32602,
                format!("非法 status: {status}（可选值: active, completed, blocked）"),
            ));
        }
    };
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let goal_state = cfg
        .session_manager
        .goal_state_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session 不存在: {session_id}")))?;
    let result = goal_state
        .transition_goal(
            Some(goal_id),
            Some(expected_revision),
            target,
            reason,
            request_nonce.as_deref(),
        )
        .await
        .map_err(map_goal_error)?;
    Ok(goal_mutation_response(session_id, &result))
}

/// 清除当前 Session 的 Goal。
pub(crate) async fn handle_goal_clear(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| AcpError::new(-32602, "缺少 sessionId"))?;
    let expected_goal_id = optional_goal_id(params)?;
    let expected_revision = optional_revision(params)?;
    let request_nonce = optional_request_nonce(params)?;
    let goal_state = cfg
        .session_manager
        .goal_state_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session 不存在: {session_id}")))?;
    let result = goal_state
        .clear_with_preconditions(
            expected_goal_id.as_deref(),
            expected_revision,
            request_nonce.as_deref(),
        )
        .await
        .map_err(map_goal_error)?;
    Ok(json!({
        "sessionId": session_id,
        "revision": result.revision,
        "cleared": true,
        "deduplicated": result.deduplicated,
    }))
}
