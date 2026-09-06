//! Session Todo、项目 Goal 与计划沙箱文档工具。

use std::collections::BTreeSet;
use std::sync::Arc;

use keencode_agent::{
    AgentTool, GoalController, GoalDraft, GoalPatch, GoalStatus, GoalTransition,
    MAX_GOAL_EVIDENCE_CHARS, MAX_PLAN_CONTENT_CHARS, PlanController, RuntimeStateError,
    TodoController, TodoItem, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture,
    ToolOutput, ToolRegistry, ToolRegistryError,
};
use keencode_model::ToolDefinition;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::environment::invalid_input;

/// 把 TodoWrite、Goal 与 Plan 加入统一工具注册表。
pub fn register_state_tools(
    registry: &mut ToolRegistry,
    todo_controller: Arc<dyn TodoController>,
    goal_controller: Arc<dyn GoalController>,
    plan_controller: Arc<dyn PlanController>,
) -> Result<(), ToolRegistryError> {
    registry.register(Arc::new(TodoWriteTool::new(todo_controller)))?;
    registry.register(Arc::new(GoalTool::new(goal_controller)))?;
    registry.register(Arc::new(PlanTool::new(plan_controller)))?;
    Ok(())
}

/// 使用全量列表维护根 Session 唯一权威 Todo 状态的工具。
pub struct TodoWriteTool {
    /// 绑定根 Session 唯一 Todo 的状态控制器。
    controller: Arc<dyn TodoController>,
}

impl TodoWriteTool {
    /// 创建绑定到指定 Todo 状态控制器的工具。
    pub fn new(controller: Arc<dyn TodoController>) -> Self {
        Self { controller }
    }
}

impl AgentTool for TodoWriteTool {
    /// 返回 Todo 完整列表的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TodoWrite",
            "用一个完整列表替换当前根 Session 唯一 Todo。复杂任务保持最多一个 in_progress 条目；任务全部完成时提交 completed 列表会自动收起当前 Todo。",
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string", "minLength": 1, "maxLength": 500 },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                },
                                "active_form": { "type": "string", "minLength": 1, "maxLength": 500 }
                            },
                            "required": ["content", "status", "active_form"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["todos"],
                "additionalProperties": false
            }),
        )
    }

    /// Todo 只改变 Runtime 内部 Session 状态，不写入用户项目。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_todo_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// Todo 替换必须与同一 Round 的其他状态更新顺序执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在阻塞状态事务边界中幂等替换根 Session 的完整 Todo 列表。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let controller = self.controller.clone();
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(cancelled_state_error());
            }
            let input = parse_todo_input(&input)?;
            let operation_id = state_operation_id(&context, StateOperationKind::Todo);
            let change = tokio::task::spawn_blocking(move || {
                controller.replace_todos(&operation_id, input.todos)
            })
            .await
            .map_err(state_join_error)?
            .map_err(normalize_state_error)?;
            json_output(json!({
                "message": if change.current.items.is_empty() && !change.submitted.is_empty() {
                    "Todo 已全部完成并清空当前列表"
                } else if change.changed {
                    "Todo 列表已更新"
                } else {
                    "Todo 列表未变化"
                },
                "revision": change.current.revision,
                "todos": change.submitted,
                "current_todos": change.current.items,
                "changed": change.changed
            }))
        })
    }
}

/// 状态 mutation 的资源类别，用于操作标识域隔离。
#[derive(Clone, Copy)]
enum StateOperationKind {
    /// Session 级 Todo 全量替换。
    Todo,
    /// 项目级 Goal 生命周期变化。
    Goal,
    /// Session/Agent 隔离的 Plan 变化。
    Plan,
}

impl StateOperationKind {
    /// 返回稳定摘要域和便于诊断的无敏感前缀。
    const fn labels(self) -> (&'static [u8], &'static str) {
        match self {
            Self::Todo => (b"todo", "todo-operation-"),
            Self::Goal => (b"goal", "goal-operation-"),
            Self::Plan => (b"plan", "plan-operation-"),
        }
    }
}

/// 从可信工具调用上下文派生不回显 Provider 标识的状态幂等操作标识。
fn state_operation_id(context: &ToolContext, kind: StateOperationKind) -> String {
    let (domain, prefix) = kind.labels();
    let mut hasher = Sha256::new();
    hasher.update(b"keencode/state-operation/v1\0");
    hasher.update(
        u64::try_from(domain.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    hasher.update(domain);
    for value in [
        context.session_id.as_str(),
        context.turn_id.as_str(),
        context.source_agent_id.as_str(),
        context.tool_call_id.as_str(),
    ] {
        hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    let digest = hasher.finalize();
    let mut output = String::with_capacity(prefix.len() + digest.len() * 2);
    output.push_str(prefix);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("写入 String 不会失败");
    }
    output
}

/// 创建、查询、更新和结束项目持久 Goal 的单一工具。
pub struct GoalTool {
    /// 绑定当前项目单例 Goal 的状态控制器。
    controller: Arc<dyn GoalController>,
}

impl GoalTool {
    /// 创建绑定到指定 Goal 状态控制器的工具。
    pub fn new(controller: Arc<dyn GoalController>) -> Self {
        Self { controller }
    }
}

impl AgentTool for GoalTool {
    /// 返回 Goal 动作和条件字段的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Goal",
            "维护当前项目唯一的长期 Goal。支持 get、create、update、complete、block、clear；complete 必须提供覆盖目标各项要求的具体 evidence，block 必须说明无法自行解决的 reason。",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["get", "create", "update", "complete", "block", "clear"]
                    },
                    "title": { "type": "string", "minLength": 1, "maxLength": 200 },
                    "objective": { "type": "string", "minLength": 1, "maxLength": 20000 },
                    "description": { "type": ["string", "null"], "maxLength": 20000 },
                    "token_budget": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "仅在用户明确要求预算时设置；null 表示取消预算"
                    },
                    "progress_percent": {
                        "type": ["integer", "null"],
                        "minimum": 0,
                        "maximum": 100
                    },
                    "reason": { "type": "string", "minLength": 1, "maxLength": 4000 },
                    "evidence": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_GOAL_EVIDENCE_CHARS,
                        "description": "complete 必填：逐项说明目标要求及其可验证证据"
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )
    }

    /// Goal 只改变 Runtime 管理的项目状态，不写入用户项目。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_goal_action(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// Goal 的读取和修改必须按模型原始调用顺序观察单例状态。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在阻塞状态事务边界中执行一次严格 Goal 动作。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let controller = self.controller.clone();
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(cancelled_state_error());
            }
            let action = parse_goal_action(&input)?;
            let operation_id = state_operation_id(&context, StateOperationKind::Goal);
            let result = tokio::task::spawn_blocking(move || {
                execute_goal_action(&controller, &operation_id, action)
            })
            .await
            .map_err(state_join_error)?
            .map_err(normalize_state_error)?;
            json_output(result)
        })
    }
}

/// 读取、完整替换或清除应用数据沙箱计划文档的工具。
pub struct PlanTool {
    /// 按 Session 与来源 Agent 隔离计划正文的控制器。
    controller: Arc<dyn PlanController>,
}

impl PlanTool {
    /// 创建绑定到指定计划状态控制器的工具。
    pub fn new(controller: Arc<dyn PlanController>) -> Self {
        Self { controller }
    }
}

impl AgentTool for PlanTool {
    /// 返回计划读取、完整写入和清除动作的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Plan",
            "在 KeenCode 应用数据沙箱中读取或完整替换当前 Session 的 Markdown 计划/报告。该工具不会写入用户项目，因此可在只读 Plan 模式使用。",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["get", "write", "clear"]
                    },
                    "content": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_PLAN_CONTENT_CHARS
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        )
    }

    /// Plan 仅改变应用数据沙箱，不改变用户项目或外部系统。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_plan_action(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 计划读取和替换必须按模型原始调用顺序执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在阻塞状态事务边界中执行当前 Session 与 Agent 的计划动作。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let controller = self.controller.clone();
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(cancelled_state_error());
            }
            let action = parse_plan_action(&input)?;
            let operation_id = state_operation_id(&context, StateOperationKind::Plan);
            let session_id = context.session_id;
            let agent_id = context.source_agent_id;
            let result = tokio::task::spawn_blocking(move || match action {
                PlanAction::Get => controller
                    .plan_snapshot(&session_id, &agent_id)
                    .map(|snapshot| json!({ "action": "get", "plan": snapshot })),
                PlanAction::Write { content } => controller
                    .replace_plan(&operation_id, &session_id, &agent_id, content)
                    .map(|change| json!({ "action": "write", "change": change })),
                PlanAction::Clear => controller
                    .clear_plan(&operation_id, &session_id, &agent_id)
                    .map(|change| json!({ "action": "clear", "change": change })),
            })
            .await
            .map_err(state_join_error)?
            .map_err(normalize_state_error)?;
            json_output(result)
        })
    }
}

/// TodoWrite 的严格反序列化输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TodoInput {
    /// 模型提交的完整 Todo 列表。
    todos: Vec<TodoItem>,
}

/// 解析 Todo 输入并提前运行与状态控制器相同的条目级校验。
fn parse_todo_input(input: &Value) -> Result<TodoInput, ToolError> {
    let mut input: TodoInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.todos.len() > 100 {
        return Err(ToolError::permanent(
            "invalid_input",
            "Todo 条目不能超过 100 项",
        ));
    }
    let mut in_progress = 0_usize;
    let mut contents = BTreeSet::new();
    for item in &mut input.todos {
        *item = item.clone().normalized().map_err(normalize_state_error)?;
        if item.status == keencode_agent::TodoStatus::InProgress {
            in_progress = in_progress.saturating_add(1);
        }
        if !contents.insert(item.content.clone()) {
            return Err(ToolError::permanent("invalid_input", "Todo 内容不能重复"));
        }
    }
    if in_progress > 1 {
        return Err(ToolError::permanent(
            "invalid_input",
            "Todo 列表最多只能有一个 in_progress 条目",
        ));
    }
    Ok(input)
}

/// 已完成条件校验的 Goal 动作。
enum GoalAction {
    /// 返回当前 Goal 快照。
    Get,
    /// 创建项目单例 Goal。
    Create(GoalDraft),
    /// 更新活跃 Goal 字段。
    Update(GoalPatch),
    /// 使用具体证据宣告完成。
    Complete {
        /// 规范化后保留到工具结果和 Transcript 的完成证据。
        evidence: String,
    },
    /// 使用无法自行解决的原因宣告阻塞。
    Block {
        /// 规范化后的阻塞原因。
        reason: String,
    },
    /// 清除当前 Goal。
    Clear,
}

/// 按 action 严格校验 Goal 条件字段并构造领域动作。
fn parse_goal_action(input: &Value) -> Result<GoalAction, ToolError> {
    let object = strict_object(
        input,
        &[
            "action",
            "title",
            "objective",
            "description",
            "token_budget",
            "progress_percent",
            "reason",
            "evidence",
        ],
    )?;
    let action = required_string(object, "action", 32)?;
    match action.as_str() {
        "get" => {
            require_only_fields(object, &["action"])?;
            Ok(GoalAction::Get)
        }
        "create" => {
            require_only_fields(
                object,
                &[
                    "action",
                    "title",
                    "objective",
                    "description",
                    "token_budget",
                    "progress_percent",
                ],
            )?;
            let objective = required_string(object, "objective", 20_000)?;
            let title = optional_non_null_string(object, "title", 200)?
                .unwrap_or_else(|| derive_goal_title(&objective));
            let draft = GoalDraft {
                title,
                objective,
                description: nullable_string_value(object, "description", 20_000)?.unwrap_or(None),
                token_budget: nullable_u64_value(object, "token_budget")?.unwrap_or(None),
                progress_percent: nullable_u8_value(object, "progress_percent")?.unwrap_or(None),
            }
            .normalized()
            .map_err(normalize_state_error)?;
            Ok(GoalAction::Create(draft))
        }
        "update" => {
            require_only_fields(
                object,
                &[
                    "action",
                    "title",
                    "objective",
                    "description",
                    "token_budget",
                    "progress_percent",
                ],
            )?;
            let patch = GoalPatch {
                title: optional_non_null_string(object, "title", 200)?,
                objective: optional_non_null_string(object, "objective", 20_000)?,
                description: nullable_string_value(object, "description", 20_000)?,
                token_budget: nullable_u64_value(object, "token_budget")?,
                progress_percent: nullable_u8_value(object, "progress_percent")?,
            }
            .normalized()
            .map_err(normalize_state_error)?;
            Ok(GoalAction::Update(patch))
        }
        "complete" => {
            require_only_fields(object, &["action", "evidence"])?;
            Ok(GoalAction::Complete {
                evidence: required_string(object, "evidence", MAX_GOAL_EVIDENCE_CHARS)?,
            })
        }
        "block" => {
            require_only_fields(object, &["action", "reason"])?;
            Ok(GoalAction::Block {
                reason: required_string(object, "reason", 4_000)?,
            })
        }
        "clear" => {
            require_only_fields(object, &["action"])?;
            Ok(GoalAction::Clear)
        }
        _ => Err(ToolError::permanent(
            "invalid_input",
            "Goal action 必须是 get、create、update、complete、block 或 clear",
        )),
    }
}

/// 在 Goal 控制器上执行一个已校验动作并生成稳定 JSON 输出。
fn execute_goal_action(
    controller: &Arc<dyn GoalController>,
    operation_id: &str,
    action: GoalAction,
) -> Result<Value, RuntimeStateError> {
    match action {
        GoalAction::Get => controller
            .goal_snapshot()
            .map(|snapshot| json!({ "action": "get", "snapshot": snapshot })),
        GoalAction::Create(draft) => controller
            .create_goal(operation_id, draft)
            .map(|change| json!({ "action": "create", "change": change })),
        GoalAction::Update(patch) => controller
            .update_goal(operation_id, patch)
            .map(|change| json!({ "action": "update", "change": change })),
        GoalAction::Complete { evidence } => controller
            .transition_goal(
                operation_id,
                GoalTransition {
                    status: GoalStatus::Completed,
                    blocked_reason: None,
                    completion_evidence: Some(evidence.clone()),
                },
            )
            .map(|change| {
                json!({
                    "action": "complete",
                    "evidence": evidence,
                    "change": change
                })
            }),
        GoalAction::Block { reason } => controller
            .transition_goal(
                operation_id,
                GoalTransition {
                    status: GoalStatus::Blocked,
                    blocked_reason: Some(reason.clone()),
                    completion_evidence: None,
                },
            )
            .map(|change| json!({ "action": "block", "reason": reason, "change": change })),
        GoalAction::Clear => controller
            .clear_goal(operation_id)
            .map(|change| json!({ "action": "clear", "change": change })),
    }
}

/// 已完成条件校验的计划文档动作。
enum PlanAction {
    /// 返回当前计划快照。
    Get,
    /// 完整替换当前计划正文。
    Write {
        /// 规范化后的非空 Markdown 正文。
        content: String,
    },
    /// 清除当前计划正文。
    Clear,
}

/// 按 action 严格校验计划文档条件字段。
fn parse_plan_action(input: &Value) -> Result<PlanAction, ToolError> {
    let object = strict_object(input, &["action", "content"])?;
    let action = required_string(object, "action", 32)?;
    match action.as_str() {
        "get" => {
            require_only_fields(object, &["action"])?;
            Ok(PlanAction::Get)
        }
        "write" => {
            require_only_fields(object, &["action", "content"])?;
            Ok(PlanAction::Write {
                content: required_string(object, "content", MAX_PLAN_CONTENT_CHARS)?,
            })
        }
        "clear" => {
            require_only_fields(object, &["action"])?;
            Ok(PlanAction::Clear)
        }
        _ => Err(ToolError::permanent(
            "invalid_input",
            "Plan action 必须是 get、write 或 clear",
        )),
    }
}

/// 将 JSON 值格式化为模型可消费的文本工具结果。
fn json_output(value: Value) -> Result<ToolOutput, ToolError> {
    serde_json::to_string_pretty(&value)
        .map(ToolOutput::text)
        .map_err(|error| {
            ToolError::permanent(
                "state_output_failed",
                format!("无法序列化状态工具结果：{error}"),
            )
        })
}

/// 将领域状态错误映射为稳定工具错误码。
fn normalize_state_error(error: RuntimeStateError) -> ToolError {
    match error {
        RuntimeStateError::Invalid { message } => ToolError::permanent("invalid_input", message),
        RuntimeStateError::Conflict { message } => ToolError::permanent("state_conflict", message),
        RuntimeStateError::NotFound { .. } => {
            ToolError::permanent("state_not_found", error.to_string())
        }
        RuntimeStateError::Terminal { .. } => {
            ToolError::permanent("state_terminal", error.to_string())
        }
        RuntimeStateError::CounterOverflow { .. } | RuntimeStateError::LockPoisoned => {
            ToolError::permanent("state_internal_error", error.to_string())
        }
        RuntimeStateError::Storage { .. } => {
            ToolError::retryable("state_storage_error", error.to_string())
        }
    }
}

/// 将阻塞状态事务任务的 Join 失败归一为工具错误。
fn state_join_error(error: tokio::task::JoinError) -> ToolError {
    ToolError::permanent(
        "state_task_failed",
        format!("状态工具任务异常结束：{error}"),
    )
}

/// 创建预先取消状态工具调用的统一错误。
fn cancelled_state_error() -> ToolError {
    ToolError::permanent("cancelled", "状态工具调用已取消")
}

/// 要求输入是对象并拒绝不在白名单中的字段。
fn strict_object<'a>(
    input: &'a Value,
    allowed_fields: &[&str],
) -> Result<&'a Map<String, Value>, ToolError> {
    let object = input
        .as_object()
        .ok_or_else(|| ToolError::permanent("invalid_input", "状态工具输入必须是 JSON 对象"))?;
    let allowed = allowed_fields.iter().copied().collect::<BTreeSet<_>>();
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(field.as_str()))
    {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("状态工具包含未知字段：{field}"),
        ));
    }
    Ok(object)
}

/// 要求对象只出现当前 action 允许的字段。
fn require_only_fields(
    object: &Map<String, Value>,
    allowed_fields: &[&str],
) -> Result<(), ToolError> {
    let allowed = allowed_fields.iter().copied().collect::<BTreeSet<_>>();
    if let Some(field) = object
        .keys()
        .find(|field| !allowed.contains(field.as_str()))
    {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("当前 action 不接受字段：{field}"),
        ));
    }
    Ok(())
}

/// 读取必填非空字符串并应用字符上限。
fn required_string(
    object: &Map<String, Value>,
    field: &str,
    max_chars: usize,
) -> Result<String, ToolError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::permanent("invalid_input", format!("缺少字符串字段：{field}")))?;
    normalize_tool_text(field, value, max_chars)
}

/// 读取可选但不能为 null 的字符串字段。
fn optional_non_null_string(
    object: &Map<String, Value>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, ToolError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        ToolError::permanent("invalid_input", format!("字段 {field} 必须是字符串"))
    })?;
    normalize_tool_text(field, value, max_chars).map(Some)
}

/// 读取缺失、null 或字符串三态字段。
fn nullable_string_value(
    object: &Map<String, Value>,
    field: &str,
    max_chars: usize,
) -> Result<Option<Option<String>>, ToolError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let value = value.as_str().ok_or_else(|| {
        ToolError::permanent("invalid_input", format!("字段 {field} 必须是字符串或 null"))
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(Some(None));
    }
    normalize_tool_text(field, value, max_chars).map(|value| Some(Some(value)))
}

/// 读取缺失、null 或正整数三态字段。
fn nullable_u64_value(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<Option<u64>>, ToolError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let value = value.as_u64().ok_or_else(|| {
        ToolError::permanent("invalid_input", format!("字段 {field} 必须是正整数或 null"))
    })?;
    if value == 0 {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("字段 {field} 必须大于零或设为 null"),
        ));
    }
    Ok(Some(Some(value)))
}

/// 读取缺失、null 或 0..=100 整数三态字段。
fn nullable_u8_value(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<Option<u8>>, ToolError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(Some(None));
    }
    let value = value.as_u64().ok_or_else(|| {
        ToolError::permanent(
            "invalid_input",
            format!("字段 {field} 必须是 0..=100 整数或 null"),
        )
    })?;
    let value = u8::try_from(value).map_err(|_| {
        ToolError::permanent(
            "invalid_input",
            format!("字段 {field} 必须位于 0..=100 范围"),
        )
    })?;
    if value > 100 {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("字段 {field} 必须位于 0..=100 范围"),
        ));
    }
    Ok(Some(Some(value)))
}

/// 去除状态工具文本首尾空白并应用 Unicode 字符上限。
fn normalize_tool_text(field: &str, value: &str, max_chars: usize) -> Result<String, ToolError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("字段 {field} 不能为空"),
        ));
    }
    if value.chars().count() > max_chars {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("字段 {field} 超过 {max_chars} 个字符上限"),
        ));
    }
    Ok(value.to_owned())
}

/// 从目标首个非空行生成不超过八十字符的默认标题。
fn derive_goal_title(objective: &str) -> String {
    let first_line = objective
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("长期目标");
    first_line.chars().take(80).collect()
}

#[cfg(test)]
#[path = "state_tools_tests.rs"]
mod tests;
