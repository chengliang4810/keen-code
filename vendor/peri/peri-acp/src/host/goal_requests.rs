//! KeenCode Goal ACP 方法处理。
//!
//! 为桌面端提供 `session/goal-get`、`session/goal-upsert`、
//! `session/goal-transition` 与 `session/goal-clear`。上游 GoalState 不使用
//! revision 与去重语义，因此响应中的 revision 固定为 0，deduplicated 固定为 false。

use peri_acp_types::goal::GoalStatus;
use serde_json::{json, Value};

use crate::session::goal_state::GoalSnapshot;
use crate::transport::types::AcpError;

use super::AcpServerConfig;

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
        "progress_percent": 0,
        "tokens_used": snapshot.tokens_used,
        "time_used_seconds": snapshot.time_used_seconds,
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
        "revision": 0,
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

    let goal_state = cfg
        .session_manager
        .goal_state_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session 不存在: {session_id}")))?;
    goal_state
        .set_goal(title.to_string(), token_budget)
        .await
        .map_err(|error| AcpError::new(-32603, format!("goal 保存失败: {error}")))?;
    let snapshot = goal_state.snapshot();
    Ok(json!({
        "sessionId": session_id,
        "revision": 0,
        "goal": goal_dto_from_snapshot(&snapshot),
        "deduplicated": false,
    }))
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
    goal_state
        .set_status_with_reason(target, reason)
        .await
        .map_err(|message| AcpError::new(-32602, message))?;
    let snapshot = goal_state.snapshot();
    Ok(json!({
        "sessionId": session_id,
        "revision": 0,
        "goal": goal_dto_from_snapshot(&snapshot),
        "deduplicated": false,
    }))
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
    let goal_state = cfg
        .session_manager
        .goal_state_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session 不存在: {session_id}")))?;
    goal_state
        .clear()
        .await
        .map_err(|error| AcpError::new(-32603, format!("goal 清除失败: {error}")))?;
    Ok(json!({
        "sessionId": session_id,
        "revision": 0,
        "cleared": true,
    }))
}
