//! (KeenCode) Goal ACP 方法处理 —— session/goal-get / session/goal-upsert /
//! session/goal-transition / session/goal-clear。
//!
//! 桌面端所需的 Goal 查询与写入；上游 GoalState 已取消 revision 语义与
//! change emitter，因此 revision 固定为 0、deduplicated 固定为 false，
//! 前端在回合结束时通过 goal-get 重新拉取状态。

use peri_agent::goal::GoalStatus;
use serde_json::{json, Value};

use crate::session::goal_state::GoalSnapshot;
use crate::transport::types::AcpError;

use super::AcpServerConfig;

/// 将上游 GoalSnapshot 映射为桌面端 GoalRecordDto。
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

/// session/goal-get —— 查询当前 Session 所属项目的共享 Goal。
pub(crate) async fn handle_goal_get(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
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
        "goals": goal.clone().map(|g| vec![g]).unwrap_or_default(),
        "activeGoalId": snapshot.goal_id,
    }))
}

/// session/goal-upsert —— 创建或完整更新 Goal。
pub(crate) async fn handle_goal_upsert(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "缺少 sessionId"))?;
    let goal = params
        .get("goal")
        .ok_or_else(|| AcpError::new(-32602, "缺少 goal"))?;
    let title = goal
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| AcpError::new(-32602, "goal.title 必填"))?;
    let token_budget = params.get("tokenBudget").and_then(|v| v.as_u64());

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

/// session/goal-transition —— 只改变 Goal 生命周期状态。
///
/// status 合法值："active" | "completed" | "blocked"（映射 peri 内部
/// GoalStatus；"completed" 是前端契约值，内部序列化为 "complete"）。
pub(crate) async fn handle_goal_transition(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "缺少 sessionId"))?;
    let status = params
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "缺少 status"))?;
    // "completed" 是前端契约值；peri 内部 GoalStatus 序列化为 "complete"。
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
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let goal_state = cfg
        .session_manager
        .goal_state_for(session_id)
        .ok_or_else(|| AcpError::new(-32602, format!("session 不存在: {session_id}")))?;
    goal_state
        .set_status_with_reason(target, reason.unwrap_or_default())
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

/// session/goal-clear —— 清除当前 Session 的 Goal，并释放单例槽位。
pub(crate) async fn handle_goal_clear(
    cfg: &AcpServerConfig,
    params: &Value,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .and_then(|value| value.as_str())
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
