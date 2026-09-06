//! Todo、Goal 与计划状态工具的确定性集成测试。

use std::sync::Arc;

use keencode_agent::{
    AgentId, AgentTool, GoalController, InMemoryRuntimeState, PlanController, SessionId,
    TodoController, ToolCallId, ToolConcurrency, ToolContext, ToolEffect, ToolRegistry,
    TurnCancellation, TurnId,
};
use keencode_model::ToolResultContent;
use serde_json::{Value, json};

use super::{
    GoalTool, PlanTool, StateOperationKind, TodoWriteTool, register_state_tools, state_operation_id,
};

/// 创建每次测试独立使用的根 Agent 工具上下文。
fn tool_context() -> ToolContext {
    tool_context_with_call("call-state-tools")
}

/// 创建绑定指定真实工具调用标识的根 Agent 工具上下文。
fn tool_context_with_call(tool_call_id: &str) -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-state-tools").expect("测试 Session ID 应有效"),
        turn_id: TurnId::new("turn-state-tools").expect("测试 Turn ID 应有效"),
        source_agent_id: AgentId::new("root").expect("测试 Agent ID 应有效"),
        tool_call_id: ToolCallId::new(tool_call_id).expect("测试 ToolCall ID 应有效"),
        cancellation: TurnCancellation::new(),
    }
}

/// 提取只含一个文本块的工具输出。
fn output_text(output: &keencode_agent::ToolOutput) -> &str {
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("测试输出应只包含一个文本块");
    };
    text
}

/// 把状态工具文本结果解析回 JSON 以验证稳定字段。
fn output_json(output: &keencode_agent::ToolOutput) -> Value {
    serde_json::from_str(output_text(output)).expect("状态工具输出应为 JSON")
}

/// 三类状态操作标识必须稳定绑定可信调用身份，并彼此保持摘要域隔离。
#[test]
fn state_operation_id_is_stable_context_scoped_and_domain_separated() {
    let first = tool_context();
    let repeated = tool_context();
    let todo = state_operation_id(&first, StateOperationKind::Todo);
    assert_eq!(
        todo,
        state_operation_id(&repeated, StateOperationKind::Todo)
    );
    let different = tool_context_with_call("call-state-tools-2");
    assert_ne!(
        todo,
        state_operation_id(&different, StateOperationKind::Todo)
    );
    let goal = state_operation_id(&first, StateOperationKind::Goal);
    let plan = state_operation_id(&first, StateOperationKind::Plan);
    assert_ne!(todo, goal);
    assert_ne!(goal, plan);
    assert!(todo.starts_with("todo-operation-"));
    assert!(goal.starts_with("goal-operation-"));
    assert!(plan.starts_with("plan-operation-"));
    assert!([todo, goal, plan].iter().all(|value| value.len() <= 128));
}

/// 三个状态工具必须以稳定名称加入统一注册表。
#[test]
fn state_tool_registration_is_stable() {
    let state = Arc::new(InMemoryRuntimeState::new());
    let todo: Arc<dyn TodoController> = state.clone();
    let goal: Arc<dyn GoalController> = state.clone();
    let plan: Arc<dyn PlanController> = state;
    let mut registry = ToolRegistry::new();
    register_state_tools(&mut registry, todo, goal, plan).expect("状态工具应注册");
    let names = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["Goal", "Plan", "TodoWrite"]);
}

/// TodoWrite 必须全量替换、拒绝歧义活动项并在全部完成后清空当前列表。
#[tokio::test]
async fn todo_write_is_strict_and_clears_completed_list() {
    let state = Arc::new(InMemoryRuntimeState::new());
    let controller: Arc<dyn TodoController> = state.clone();
    let tool = TodoWriteTool::new(controller);
    let input = json!({
        "todos": [
            { "content": "实现状态", "status": "in_progress", "active_form": "正在实现状态" },
            { "content": "运行测试", "status": "pending", "active_form": "正在运行测试" }
        ]
    });
    assert_eq!(tool.effect(&input), Ok(ToolEffect::ReadOnly));
    assert_eq!(tool.concurrency(), ToolConcurrency::Exclusive);
    let output = tool
        .execute(tool_context(), input)
        .await
        .expect("Todo 应更新");
    let value = output_json(&output);
    assert_eq!(value["revision"], 1);
    assert_eq!(value["current_todos"].as_array().map(Vec::len), Some(2));

    let invalid = json!({
        "todos": [
            { "content": "A", "status": "in_progress", "active_form": "A 中" },
            { "content": "B", "status": "in_progress", "active_form": "B 中" }
        ]
    });
    assert!(tool.effect(&invalid).is_err());
    assert_eq!(state.todo_snapshot().expect("Todo 快照应读取").revision, 1);

    let mut completed_context = tool_context();
    completed_context.tool_call_id =
        ToolCallId::new("call-state-tools-completed").expect("ToolCall ID 应有效");
    let completed = tool
        .execute(
            completed_context,
            json!({
                "todos": [
                    { "content": "实现状态", "status": "completed", "active_form": "正在实现状态" },
                    { "content": "运行测试", "status": "completed", "active_form": "正在运行测试" }
                ]
            }),
        )
        .await
        .expect("完成 Todo 应成功");
    let value = output_json(&completed);
    assert_eq!(value["revision"], 2);
    assert_eq!(value["todos"].as_array().map(Vec::len), Some(2));
    assert_eq!(value["current_todos"].as_array().map(Vec::len), Some(0));
}

/// Goal 必须严格执行创建、更新、带证据完成、终态拒绝和清除顺序。
#[tokio::test]
async fn goal_tool_enforces_action_contract_and_terminal_state() {
    let state = Arc::new(InMemoryRuntimeState::new());
    let controller: Arc<dyn GoalController> = state.clone();
    let tool = GoalTool::new(controller);
    let create = json!({
        "action": "create",
        "objective": "完成 Runtime 状态工具并通过测试",
        "token_budget": null,
        "progress_percent": 10
    });
    assert_eq!(tool.effect(&create), Ok(ToolEffect::ReadOnly));
    assert_eq!(tool.concurrency(), ToolConcurrency::Exclusive);
    let created = tool
        .execute(tool_context(), create)
        .await
        .expect("Goal 应创建");
    let created = output_json(&created);
    assert_eq!(created["change"]["current"]["revision"], 1);
    assert_eq!(
        created["change"]["current"]["goal"]["title"],
        "完成 Runtime 状态工具并通过测试"
    );
    assert!(created["change"]["current"]["goal"]["token_budget"].is_null());

    let updated = tool
        .execute(
            tool_context_with_call("call-goal-update"),
            json!({
                "action": "update",
                "description": "已完成领域层",
                "progress_percent": 70
            }),
        )
        .await
        .expect("Goal 应更新");
    assert_eq!(
        output_json(&updated)["change"]["current"]["goal"]["progress_percent"],
        70
    );
    let active_clear_error = tool
        .execute(
            tool_context_with_call("call-goal-clear-active"),
            json!({ "action": "clear" }),
        )
        .await
        .expect_err("活跃 Goal 不能跳过终态直接清除");
    assert_eq!(active_clear_error.code, "invalid_input");
    assert!(
        tool.effect(&json!({ "action": "complete" })).is_err(),
        "complete 缺少 evidence 必须在执行前失败"
    );
    let completed = tool
        .execute(
            tool_context_with_call("call-goal-complete"),
            json!({
                "action": "complete",
                "evidence": "领域测试与工具集成测试均通过"
            }),
        )
        .await
        .expect("带证据完成应成功");
    let completed = output_json(&completed);
    assert_eq!(
        completed["change"]["current"]["goal"]["status"],
        "completed"
    );
    assert_eq!(completed["evidence"], "领域测试与工具集成测试均通过");
    let completion_retry = tool
        .execute(
            tool_context_with_call("call-goal-complete"),
            json!({
                "action": "complete",
                "evidence": "领域测试与工具集成测试均通过"
            }),
        )
        .await
        .expect("相同完成证据重试应去重");
    assert_eq!(output_json(&completion_retry)["change"]["changed"], false);
    let evidence_conflict = tool
        .execute(
            tool_context_with_call("call-goal-complete"),
            json!({
                "action": "complete",
                "evidence": "不同完成证据"
            }),
        )
        .await
        .expect_err("同一调用标识不得改绑完成证据");
    assert_eq!(evidence_conflict.code, "state_conflict");
    let terminal_error = tool
        .execute(
            tool_context_with_call("call-goal-update-terminal"),
            json!({ "action": "update", "progress_percent": 100 }),
        )
        .await
        .expect_err("终态 Goal 不能更新");
    assert_eq!(terminal_error.code, "state_terminal");

    let cleared = tool
        .execute(
            tool_context_with_call("call-goal-clear-terminal"),
            json!({ "action": "clear" }),
        )
        .await
        .expect("终态 Goal 应清除");
    assert!(output_json(&cleared)["change"]["current"]["goal"].is_null());
}

/// 并发创建同一个项目 Goal 时只能有一个事务成功。
#[tokio::test]
async fn goal_create_is_atomic_under_concurrency() {
    let state = Arc::new(InMemoryRuntimeState::new());
    let controller: Arc<dyn GoalController> = state;
    let tool = GoalTool::new(controller);
    let first = tool.execute(
        tool_context_with_call("call-goal-concurrent-a"),
        json!({ "action": "create", "objective": "目标 A" }),
    );
    let second = tool.execute(
        tool_context_with_call("call-goal-concurrent-b"),
        json!({ "action": "create", "objective": "目标 B" }),
    );
    let (first, second) = tokio::join!(first, second);
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let error = first
        .err()
        .or_else(|| second.err())
        .expect("必须有一个冲突");
    assert_eq!(error.code, "state_conflict");
}

/// Plan 工具必须声明只读效果，并按 Session 与 Agent 隔离应用数据正文。
#[tokio::test]
async fn plan_tool_is_read_only_to_project_and_session_scoped() {
    let state = Arc::new(InMemoryRuntimeState::new());
    let controller: Arc<dyn PlanController> = state.clone();
    let tool = PlanTool::new(controller);
    let input = json!({ "action": "write", "content": "# 计划\n\n1. 分析\n2. 验证" });
    let effect = tool.effect(&input).expect("计划输入应有效");
    assert_eq!(effect, ToolEffect::ReadOnly);
    assert_eq!(tool.concurrency(), ToolConcurrency::Exclusive);
    let written = tool
        .execute(tool_context_with_call("call-plan-write"), input)
        .await
        .expect("计划应写入应用数据状态");
    assert_eq!(output_json(&written)["change"]["current"]["revision"], 1);

    let fetched = tool
        .execute(
            tool_context_with_call("call-plan-get"),
            json!({ "action": "get" }),
        )
        .await
        .expect("计划应读取");
    assert!(
        output_json(&fetched)["plan"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("分析"))
    );
    let other_session = SessionId::new("other-session").expect("测试 Session ID 应有效");
    let root = AgentId::new("root").expect("测试 Agent ID 应有效");
    assert!(
        state
            .plan_snapshot(&other_session, &root)
            .expect("其他 Session 快照应读取")
            .content
            .is_none()
    );
}

/// 预先取消的状态工具调用不得写入任何内部状态。
#[tokio::test]
async fn pre_cancelled_state_tool_has_no_effect() {
    let state = Arc::new(InMemoryRuntimeState::new());
    let controller: Arc<dyn TodoController> = state.clone();
    let tool = TodoWriteTool::new(controller);
    let context = tool_context();
    context.cancellation.cancel();
    let error = tool
        .execute(
            context,
            json!({
                "todos": [
                    { "content": "不应写入", "status": "pending", "active_form": "正在写入" }
                ]
            }),
        )
        .await
        .expect_err("预取消调用必须失败");
    assert_eq!(error.code, "cancelled");
    assert_eq!(state.todo_snapshot().expect("Todo 快照应读取").revision, 0);
}
