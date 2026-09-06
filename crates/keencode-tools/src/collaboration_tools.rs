//! 单层异步 Agent 协作工具。

use std::sync::Arc;
use std::time::Duration;

use keencode_agent::{
    AgentCapabilities, AgentId, AgentProfile, AgentTemplateSnapshot, AgentTool as RuntimeAgentTool,
    CollaborationAgentStatus, CollaborationCoordinator, CollaborationError, ContextInheritance,
    SpawnAgentRequest, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    ToolRegistry, ToolRegistryError, WaitAgentOutcome,
};
use keencode_model::{Message, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};

/// Agent 初始任务允许的最大 UTF-8 字节数。
pub(super) const MAX_INITIAL_TASK_BYTES: usize = 256 * 1024;
/// Agent 间单条消息允许的最大 UTF-8 字节数。
pub(super) const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// 不透明 Agent 标识允许的最大 UTF-8 字节数。
pub(super) const MAX_AGENT_ID_BYTES: usize = 256;
/// WaitAgent 单次等待允许的最大毫秒数。
pub(super) const MAX_WAIT_TIMEOUT_MILLISECONDS: u64 = 300_000;
/// RecentTurns 允许继承的最大 Turn 数量。
const MAX_RECENT_TURNS: u32 = 10_000;
/// 单次协作工具 JSON 文本结果允许的最大 UTF-8 字节数。
const MAX_TOOL_OUTPUT_BYTES: usize = 16 * 1024;
/// 只能由根 Agent 使用、不得冻结进子 Agent Profile 的工具名称。
const ROOT_ONLY_AGENT_TOOL_NAMES: [&str; 5] =
    ["spawn_agent", "AskUser", "TodoWrite", "Goal", "Plan"];

/// 从待持久化或恢复的工具快照中移除全部根 Agent 专用工具。
pub fn retain_child_agent_tool_snapshot(tool_names: &mut Vec<String>) {
    tool_names.retain(|name| {
        !ROOT_ONLY_AGENT_TOOL_NAMES
            .iter()
            .any(|root_only| name == root_only)
    });
}

/// 显式 Agent 模板解析时只允许使用的可信父 Turn 上下文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnAgentTemplateContext {
    /// 当前根 Session 标识。
    pub session_id: keencode_agent::SessionId,
    /// 发起 spawn 的根 Agent 标识。
    pub parent_agent_id: AgentId,
    /// 当前 Agent 树的根 Turn 标识。
    pub root_turn_id: keencode_agent::TurnId,
}

/// 扩展候选为一次 spawn 冻结的完整 Agent 模板结果。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSpawnAgentTemplate {
    /// 需要进入 AgentDefinition 冷恢复快照的非模型模板字段。
    pub snapshot: AgentTemplateSnapshot,
    /// 模板可选的精确模型覆盖。
    pub model: Option<String>,
    /// 模板显式工具集合；为空表示继承父 Agent 的冻结工具表。
    pub tool_names: Option<Vec<String>>,
    /// 从继承或显式集合中移除的工具名称。
    pub disallowed_tool_names: Vec<String>,
}

/// 从当前项目已经原子发布的扩展候选解析显式 Agent 模板。
pub trait SpawnAgentTemplateResolver: Send + Sync {
    /// 未知名称返回 `None`，解析或候选错误必须返回安全且稳定的工具错误。
    fn resolve(
        &self,
        name: &str,
        context: &SpawnAgentTemplateContext,
    ) -> Result<Option<ResolvedSpawnAgentTemplate>, ToolError>;
}

/// 父 Transcript 中一个已经形成唯一终态的 Turn 消息组。
#[derive(Clone, Debug, PartialEq)]
pub struct CompletedTurnContext {
    /// 该 Turn 按 Provider 实际消费顺序保存的中立消息。
    pub messages: Vec<Message>,
}

/// 在 spawn 提交前读取可信父 Transcript 的同步快照端口。
pub trait SpawnAgentContextSource: Send + Sync {
    /// 返回当前来源 Turn 之前按时间顺序排列的全部已完成 Turn；不得包含运行中 Turn。
    fn completed_turns(
        &self,
        context: &SpawnAgentTemplateContext,
    ) -> Result<Vec<CompletedTurnContext>, ToolError>;
}

/// 按当前 Agent 能力加入 V2 协作工具；子 Agent 不暴露递归创建入口。
pub fn register_collaboration_tools(
    registry: &mut ToolRegistry,
    coordinator: Arc<CollaborationCoordinator>,
    child_profile: AgentProfile,
    capabilities: AgentCapabilities,
    context_source: Arc<dyn SpawnAgentContextSource>,
) -> Result<(), ToolRegistryError> {
    register_collaboration_tools_inner(
        registry,
        coordinator,
        child_profile,
        capabilities,
        context_source,
        None,
    )
}

/// 按当前 Agent 能力加入 V2 协作工具，并为根 Agent 绑定项目级模板解析器。
pub fn register_collaboration_tools_with_template_resolver(
    registry: &mut ToolRegistry,
    coordinator: Arc<CollaborationCoordinator>,
    child_profile: AgentProfile,
    capabilities: AgentCapabilities,
    context_source: Arc<dyn SpawnAgentContextSource>,
    template_resolver: Arc<dyn SpawnAgentTemplateResolver>,
) -> Result<(), ToolRegistryError> {
    register_collaboration_tools_inner(
        registry,
        coordinator,
        child_profile,
        capabilities,
        context_source,
        Some(template_resolver),
    )
}

/// 统一注册通用或模板感知的协作工具集合。
fn register_collaboration_tools_inner(
    registry: &mut ToolRegistry,
    coordinator: Arc<CollaborationCoordinator>,
    child_profile: AgentProfile,
    capabilities: AgentCapabilities,
    context_source: Arc<dyn SpawnAgentContextSource>,
    template_resolver: Option<Arc<dyn SpawnAgentTemplateResolver>>,
) -> Result<(), ToolRegistryError> {
    if capabilities.can_spawn_agent {
        let mut spawn = SpawnAgentTool::new(coordinator.clone(), child_profile, context_source);
        if let Some(template_resolver) = template_resolver {
            spawn = spawn.with_template_resolver(template_resolver);
        }
        registry.register(Arc::new(spawn))?;
    }
    registry.register(Arc::new(SendMessageTool::new(coordinator.clone())))?;
    registry.register(Arc::new(FollowupTaskTool::new(coordinator.clone())))?;
    registry.register(Arc::new(InterruptAgentTool::new(coordinator.clone())))?;
    registry.register(Arc::new(RetryAgentTool::new(coordinator.clone())))?;
    registry.register(Arc::new(ListAgentsTool::new(coordinator.clone())))?;
    registry.register(Arc::new(WaitAgentTool::new(coordinator)))?;
    Ok(())
}

/// 创建一个单层异步子 Agent，并立即返回已持久化身份的工具。
pub struct SpawnAgentTool {
    /// 唯一负责 Agent 树、Turn 和 mailbox 状态的协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
    /// 由可信运行时冻结、模型输入不能覆盖的子 Agent 配置。
    child_profile: AgentProfile,
    /// spawn 提交前唯一允许读取父 Transcript 的可信快照端口。
    context_source: Arc<dyn SpawnAgentContextSource>,
    /// 当前项目候选的显式 Agent 模板解析器；通用子 Agent 不需要该端口。
    template_resolver: Option<Arc<dyn SpawnAgentTemplateResolver>>,
}

impl SpawnAgentTool {
    /// 创建绑定协作协调器和可信子 Agent 配置的工具。
    pub fn new(
        coordinator: Arc<CollaborationCoordinator>,
        child_profile: AgentProfile,
        context_source: Arc<dyn SpawnAgentContextSource>,
    ) -> Self {
        Self {
            coordinator,
            child_profile,
            context_source,
            template_resolver: None,
        }
    }

    /// 为显式 `agent` 输入绑定当前项目候选解析器。
    pub fn with_template_resolver(mut self, resolver: Arc<dyn SpawnAgentTemplateResolver>) -> Self {
        self.template_resolver = Some(resolver);
        self
    }
}

impl RuntimeAgentTool for SpawnAgentTool {
    /// 返回不接受来源 Session、Turn 或 Agent 身份的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "spawn_agent",
            "创建一个并发运行的单层子 Agent，并立即返回稳定身份和初始 Turn 标识。fork_turns 可选 none、all 或正整数文本；完整历史继承时固定继承父模型配置，子 Agent 不允许继续创建 Agent。",
            json!({
                "type": "object",
                "properties": {
                    "task_name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 64,
                        "description": "用于稳定 /root/{task_name} 路径的小写字母、数字或下划线名称"
                    },
                    "message": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_INITIAL_TASK_BYTES
                    },
                    "fork_turns": {
                        "type": "string",
                        "description": "none、all 或 1..=10000 的十进制正整数；缺省为 all"
                    },
                    "agent": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 1024,
                        "description": "可选的 Agent catalog 稳定名称；显式指定后未知或无效模板不会回退"
                    },
                    "model": { "type": "string", "minLength": 1, "maxLength": 256 },
                    "reasoning_effort": { "type": "string", "minLength": 1, "maxLength": 64 }
                },
                "required": ["task_name", "message"],
                "additionalProperties": false
            }),
        )
    }

    /// 创建子 Agent 只改变运行时内部协作状态，不直接修改用户项目。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_spawn_agent_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 身份创建和 Turn 入队必须保持模型工具调用的原始顺序。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 使用可信 ToolContext 中的来源身份创建子 Agent。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let coordinator = self.coordinator.clone();
        let child_profile = self.child_profile.clone();
        let context_source = self.context_source.clone();
        let template_resolver = self.template_resolver.clone();
        Box::pin(async move {
            ensure_not_cancelled(&context)?;
            let input = parse_spawn_agent_input(&input)?;
            let mut child_profile = child_profile;
            if let Some(model) = input.model {
                child_profile.model = model;
            }
            if let Some(reasoning_effort) = input.reasoning_effort {
                child_profile.reasoning_effort = Some(reasoning_effort);
            }
            let agent_template = if let Some(agent_name) = input.agent.as_deref() {
                let resolver = template_resolver.ok_or_else(|| {
                    ToolError::permanent(
                        "agent_template_unavailable",
                        "当前 Session 没有可用的 Agent 模板候选",
                    )
                })?;
                let template = resolver
                    .resolve(
                        agent_name,
                        &SpawnAgentTemplateContext {
                            session_id: context.session_id.clone(),
                            parent_agent_id: context.source_agent_id.clone(),
                            root_turn_id: context.turn_id.clone(),
                        },
                    )?
                    .ok_or_else(|| {
                        ToolError::permanent(
                            "agent_template_not_found",
                            "指定的 Agent 模板不存在、未启用或不适用于当前项目",
                        )
                    })?;
                if matches!(&input.context_inheritance, ContextInheritance::All)
                    && template.model.is_some()
                {
                    return Err(ToolError::permanent(
                        "invalid_input",
                        "fork_turns=all 必须沿用父 Agent 模型，不能选择带模型覆盖的 Agent 模板",
                    ));
                }
                apply_resolved_agent_template(&mut child_profile, template)?
            } else {
                None
            };
            // 模板可以重选父工具，因此必须在全部覆盖完成后统一收紧最终持久快照。
            retain_child_agent_tool_snapshot(&mut child_profile.tool_snapshot);
            let context_snapshot = freeze_parent_context(
                context_source.as_ref(),
                &SpawnAgentTemplateContext {
                    session_id: context.session_id.clone(),
                    parent_agent_id: context.source_agent_id.clone(),
                    root_turn_id: context.turn_id.clone(),
                },
                &input.context_inheritance,
            )?;
            let spawned = coordinator
                .spawn_agent(
                    &context.source_agent_id,
                    &context.turn_id,
                    &context.tool_call_id,
                    SpawnAgentRequest {
                        task_name: input.task_name,
                        initial_task: input.message,
                        context_inheritance: input.context_inheritance,
                        context_snapshot,
                        agent_template,
                        profile: child_profile,
                    },
                )
                .map_err(normalize_collaboration_error)?;
            json_output(json!({
                "outcome": "created",
                "agent_id": spawned.agent.agent_id.as_str(),
                "session_id": spawned.agent.session_id.as_str(),
                "path": spawned.agent.path.as_str(),
                "initial_turn_id": spawned.initial_turn_id.as_str()
            }))
        })
    }
}

/// 等待当前可信 Turn 的 mailbox 或用户 steer 活动，但不消费正文的工具。
pub struct WaitAgentTool {
    /// 唯一负责等待活动版本和 Turn 生命周期的协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
}

impl WaitAgentTool {
    /// 创建绑定协作协调器的等待工具。
    pub fn new(coordinator: Arc<CollaborationCoordinator>) -> Self {
        Self { coordinator }
    }
}

impl RuntimeAgentTool for WaitAgentTool {
    /// 返回只接受有界硬超时且不能伪造等待身份的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "wait_agent",
            "等待当前 Agent 的当前 Turn 出现 mailbox 活动、用户 steer、Turn 结束或硬超时。该工具只返回数量和最新序号，不读取或消费任何消息正文。",
            json!({
                "type": "object",
                "properties": {
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_WAIT_TIMEOUT_MILLISECONDS
                    }
                },
                "required": ["timeout_ms"],
                "additionalProperties": false
            }),
        )
    }

    /// 等待只观察运行时内部活动，不改变 mailbox 或用户项目。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_wait_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 同一 Agent 的等待必须与相邻协作命令保持明确顺序。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 使用可信 ToolContext 等待，并让 Turn 取消优先终止长等待。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            ensure_not_cancelled(&context)?;
            let input = parse_wait_input(&input)?;
            let wait = coordinator.wait_agent(
                &context.source_agent_id,
                &context.turn_id,
                Duration::from_millis(input.timeout_ms),
            );
            let outcome = tokio::select! {
                biased;
                _ = context.cancellation.cancelled() => {
                    return Err(cancelled_error());
                }
                result = wait => result.map_err(normalize_collaboration_error)?,
            };
            match outcome {
                WaitAgentOutcome::MailboxActivity(activity) => json_output(json!({
                    "outcome": "mailbox_activity",
                    "pending_count": activity.pending_count,
                    "latest_sequence": activity.latest_sequence
                })),
                WaitAgentOutcome::UserSteer(activity) => json_output(json!({
                    "outcome": "user_steer_activity",
                    "pending_count": activity.pending_count,
                    "latest_sequence": activity.latest_sequence
                })),
                WaitAgentOutcome::TimedOut => json_output(json!({
                    "outcome": "timed_out"
                })),
                WaitAgentOutcome::TurnEnded => json_output(json!({
                    "outcome": "turn_ended"
                })),
            }
        })
    }
}

/// 幂等中断同一根树内目标子 Agent 当前 Turn 的工具。
pub struct InterruptAgentTool {
    /// 唯一负责目标校验、因果校验和取消状态的协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
}

impl InterruptAgentTool {
    /// 创建绑定协作协调器的中断工具。
    pub fn new(coordinator: Arc<CollaborationCoordinator>) -> Self {
        Self { coordinator }
    }
}

impl RuntimeAgentTool for InterruptAgentTool {
    /// 返回只接受目标身份且不能伪造来源因果的严格输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "interrupt_agent",
            "幂等请求中断同一根 Agent 树内目标子 Agent 的当前 Turn，保留其身份和 mailbox。不能中断根 Agent 或调用者自身；来源 Agent 和 Turn 由运行时提供。",
            target_only_schema(),
        )
    }

    /// 停止请求只改变运行时内部 Turn 状态，不直接修改用户项目。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_target_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 停止请求必须按原始工具调用顺序观察目标状态。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 使用可信 ToolContext 的来源 Agent 与 Turn 提交停止请求。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            ensure_not_cancelled(&context)?;
            let input = parse_target_input(&input)?;
            let stopped_turn_id = coordinator
                .stop_agent(
                    &context.source_agent_id,
                    &context.turn_id,
                    &context.tool_call_id,
                    &input.target_agent_id,
                )
                .map_err(normalize_collaboration_error)?;
            json_output(json!({
                "outcome": "interrupt_requested",
                "target_agent_id": input.target_agent_id.as_str(),
                "turn_id": stopped_turn_id.as_str()
            }))
        })
    }
}

/// 以当前可信来源 Turn 重试同一根树内失败或中断 Agent 的工具。
pub struct RetryAgentTool {
    /// 唯一负责目标状态、Turn 分配和幂等提交的协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
}

impl RetryAgentTool {
    /// 创建绑定协作协调器的重试工具。
    pub fn new(coordinator: Arc<CollaborationCoordinator>) -> Self {
        Self { coordinator }
    }
}

impl RuntimeAgentTool for RetryAgentTool {
    /// 返回只接受目标身份且不能伪造来源 Turn 或 operationId 的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "retry_agent",
            "为同一根 Agent 树内最近一次失败或中断的 Agent 创建新 Turn。来源 Agent、来源 Turn 和幂等 operationId 均由运行时提供。",
            target_only_schema(),
        )
    }

    /// 重试只改变内部 Agent Turn 调度状态，不直接修改用户项目。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_target_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 目标状态检查与新 Turn 分配必须保持模型工具调用原始顺序。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 使用 ToolContext 的可信 Agent、Turn 与 ToolCall 身份提交可恢复重试。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            ensure_not_cancelled(&context)?;
            let input = parse_target_input(&input)?;
            let retry_turn_id = coordinator
                .retry_agent_with_operation(
                    &context.source_agent_id,
                    &context.turn_id,
                    &context.tool_call_id,
                    &input.target_agent_id,
                )
                .map_err(normalize_collaboration_error)?;
            json_output(json!({
                "outcome": "retry_queued",
                "target_agent_id": input.target_agent_id.as_str(),
                "turn_id": retry_turn_id.as_str()
            }))
        })
    }
}

/// 只向目标 mailbox 投递消息且不唤醒空闲 Agent 的工具。
pub struct SendMessageTool {
    /// 唯一负责 mailbox 持久化和因果校验的协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
}

impl SendMessageTool {
    /// 创建绑定协作协调器的消息工具。
    pub fn new(coordinator: Arc<CollaborationCoordinator>) -> Self {
        Self { coordinator }
    }
}

impl RuntimeAgentTool for SendMessageTool {
    /// 返回不允许模型伪造投递模式或来源身份的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "send_message",
            "向同一根 Agent 树内目标发送有界文本。消息只进入 mailbox；目标空闲时不会创建新 Turn，目标运行时也不会额外触发执行。来源 Agent 和 Turn 由运行时提供。",
            message_schema(),
        )
    }

    /// 消息只改变运行时内部 mailbox，不直接修改用户项目。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_send_message_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// mailbox 顺序必须保持模型调用原始顺序。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 使用可信 ToolContext 的来源 Agent 与 Turn 持久发送消息。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            ensure_not_cancelled(&context)?;
            let input = parse_send_message_input(&input)?;
            let message_id = coordinator
                .send_message(
                    &context.source_agent_id,
                    &context.turn_id,
                    &context.tool_call_id,
                    &input.target_agent_id,
                    input.message,
                )
                .map_err(normalize_collaboration_error)?;
            json_output(json!({
                "outcome": "queued",
                "target_agent_id": input.target_agent_id.as_str(),
                "message_id": message_id.as_str(),
                "delivery": "queue_only"
            }))
        })
    }
}

/// 向目标 mailbox 投递消息，并在目标空闲时显式触发新 Turn 的工具。
pub struct FollowupTaskTool {
    /// 唯一负责 mailbox 持久化、因果校验和目标 Turn 触发的协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
}

impl FollowupTaskTool {
    /// 创建绑定协作协调器的显式继续工具。
    pub fn new(coordinator: Arc<CollaborationCoordinator>) -> Self {
        Self { coordinator }
    }
}

impl RuntimeAgentTool for FollowupTaskTool {
    /// 返回不允许模型伪造来源身份的严格目标与消息 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "followup_task",
            "向同一根 Agent 树内目标发送有界文本。目标空闲时创建一个新 Turn；目标正在运行时在安全消息边界提示其处理 mailbox。来源 Agent 和 Turn 由运行时提供。",
            message_schema(),
        )
    }

    /// 显式继续只改变运行时内部 mailbox 与 Turn 调度状态。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_send_message_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// mailbox 与新 Turn 的因果顺序必须保持模型调用原始顺序。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 使用可信 ToolContext 投递消息，并按目标状态决定是否触发新 Turn。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            ensure_not_cancelled(&context)?;
            let input = parse_send_message_input(&input)?;
            let (message_id, triggered_turn_id) = coordinator
                .followup_agent(
                    &context.source_agent_id,
                    &context.turn_id,
                    &context.tool_call_id,
                    &input.target_agent_id,
                    input.message,
                )
                .map_err(normalize_collaboration_error)?;
            json_output(json!({
                "outcome": "queued",
                "target_agent_id": input.target_agent_id.as_str(),
                "message_id": message_id.as_str(),
                "delivery": "trigger_turn",
                "triggered_turn_id": triggered_turn_id.as_ref().map(|turn_id| turn_id.as_str())
            }))
        })
    }
}

/// 查询当前来源所属根树全部 Agent 身份和生命周期状态的工具。
pub struct ListAgentsTool {
    /// 唯一负责同根树身份解析和冷状态加载的协作协调器。
    coordinator: Arc<CollaborationCoordinator>,
}

impl ListAgentsTool {
    /// 创建绑定协作协调器的 Agent 列表工具。
    pub fn new(coordinator: Arc<CollaborationCoordinator>) -> Self {
        Self { coordinator }
    }
}

impl RuntimeAgentTool for ListAgentsTool {
    /// 返回不接受目标或来源身份的严格空对象 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "list_agents",
            "列出当前 Agent 所属根树的全部已知 Agent、稳定路径和当前生命周期状态。不会返回模型配置、工作目录、工具快照或消息正文。",
            empty_schema(),
        )
    }

    /// 列表查询不改变 Agent 或用户项目状态。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_empty_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 列表查询可与其他只读工具并行，但冷加载由协调器状态锁串行化。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 使用可信来源身份列出同根树 Agent，并将状态投影为有界 JSON。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let coordinator = self.coordinator.clone();
        Box::pin(async move {
            ensure_not_cancelled(&context)?;
            parse_empty_input(&input)?;
            let agents = coordinator
                .list_agents(&context.source_agent_id, &context.turn_id)
                .map_err(normalize_collaboration_error)?
                .into_iter()
                .map(|summary| {
                    let (status, turn_id) = status_projection(&summary.status);
                    json!({
                        "agent_id": summary.agent.agent_id.as_str(),
                        "session_id": summary.agent.session_id.as_str(),
                        "parent_agent_id": summary.parent_agent_id.as_ref().map(|agent_id| agent_id.as_str()),
                        "path": summary.agent.path.as_str(),
                        "status": status,
                        "turn_id": turn_id
                    })
                })
                .collect::<Vec<_>>();
            json_output(json!({ "agents": agents }))
        })
    }
}

/// spawn_agent 完成严格反序列化后的输入。
struct ParsedSpawnAgentInput {
    /// 稳定子 Agent 路径名。
    task_name: String,
    /// 第一个子 Agent Turn 的完整任务。
    message: String,
    /// 由有限枚举构造的父上下文继承范围。
    context_inheritance: ContextInheritance,
    /// 可选的 Agent catalog 稳定名称。
    agent: Option<String>,
    /// 可选的精确模型覆盖。
    model: Option<String>,
    /// 可选的推理强度覆盖。
    reasoning_effort: Option<String>,
}

/// spawn_agent 的严格顶层输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnAgentInput {
    /// 稳定子 Agent 路径名。
    task_name: String,
    /// 第一个子 Agent Turn 的完整任务。
    message: String,
    /// none、all 或正整数文本；缺失时继承全部历史。
    fork_turns: Option<String>,
    /// 可选的 Agent catalog 稳定名称。
    agent: Option<String>,
    /// 可选的精确模型覆盖。
    model: Option<String>,
    /// 可选的推理强度覆盖。
    reasoning_effort: Option<String>,
}

/// WaitAgent 的严格输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitInput {
    /// 本次调用的硬超时毫秒数。
    timeout_ms: u64,
}

/// interrupt_agent 的严格输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetInput {
    /// 需要停止的同树子 Agent 标识。
    target_agent_id: String,
}

/// 已解析并校验的不透明目标 Agent 标识。
struct ParsedTargetInput {
    /// 只可作为目标、不能替代 ToolContext 来源身份的 Agent 标识。
    target_agent_id: AgentId,
}

/// send_message 与 followup_task 共享的严格输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendMessageInput {
    /// 接收消息的同树 Agent 标识。
    target_agent_id: String,
    /// 需要持久化到 mailbox 的完整正文。
    message: String,
}

/// 已完成边界校验的 SendMessage 输入。
struct ParsedSendMessageInput {
    /// 只可作为目标、不能替代 ToolContext 来源身份的 Agent 标识。
    target_agent_id: AgentId,
    /// 已校验非空和 UTF-8 字节上限的正文。
    message: String,
}

/// list_agents 的严格空对象输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

/// 解析 spawn_agent 输入并执行 Schema 之外的继承与覆盖约束。
fn parse_spawn_agent_input(input: &Value) -> Result<ParsedSpawnAgentInput, ToolError> {
    let input: SpawnAgentInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.task_name.is_empty()
        || input.task_name.len() > 64
        || !input
            .task_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ToolError::permanent(
            "invalid_input",
            "task_name 必须是 1..=64 字节的小写 ASCII 字母、数字或下划线",
        ));
    }
    validate_required_text(&input.message, MAX_INITIAL_TASK_BYTES, "message")?;
    let fork_turns = input.fork_turns.as_deref().unwrap_or("all");
    let context_inheritance = match fork_turns {
        "none" => ContextInheritance::None,
        "all" => ContextInheritance::All,
        count => {
            let count = count.parse::<u32>().map_err(|_error| {
                ToolError::permanent(
                    "invalid_input",
                    "fork_turns 必须是 none、all 或 1..=10000 的十进制正整数",
                )
            })?;
            if !(1..=MAX_RECENT_TURNS).contains(&count) || count.to_string() != fork_turns {
                return Err(ToolError::permanent(
                    "invalid_input",
                    "fork_turns 必须是 none、all 或 1..=10000 的规范十进制正整数",
                ));
            }
            ContextInheritance::RecentTurns { count }
        }
    };
    if let Some(model) = input.model.as_deref() {
        validate_required_text(model, 256, "model")?;
    }
    if let Some(reasoning_effort) = input.reasoning_effort.as_deref() {
        validate_required_text(reasoning_effort, 64, "reasoning_effort")?;
    }
    if let Some(agent) = input.agent.as_deref() {
        validate_required_text(agent, 1_024, "agent")?;
        if agent.trim() != agent || agent.chars().any(char::is_control) {
            return Err(ToolError::permanent(
                "invalid_input",
                "agent 不能包含首尾空白或控制字符",
            ));
        }
    }
    if context_inheritance == ContextInheritance::All
        && (input.model.is_some() || input.reasoning_effort.is_some())
    {
        return Err(ToolError::permanent(
            "invalid_input",
            "fork_turns=all 时必须继承父 Agent 的模型与推理强度",
        ));
    }
    Ok(ParsedSpawnAgentInput {
        task_name: input.task_name,
        message: input.message,
        context_inheritance,
        agent: input.agent,
        model: input.model,
        reasoning_effort: input.reasoning_effort,
    })
}

/// 将可信模板的模型和工具约束冻结进子 Agent Profile，并返回持久模板快照。
fn apply_resolved_agent_template(
    profile: &mut AgentProfile,
    template: ResolvedSpawnAgentTemplate,
) -> Result<Option<AgentTemplateSnapshot>, ToolError> {
    if let Some(model) = template.model {
        validate_required_text(&model, 1_024, "Agent 模板 model")?;
        profile.model = model;
    }
    let inherited = profile
        .tool_snapshot
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let mut selected = template
        .tool_names
        .unwrap_or_else(|| profile.tool_snapshot.clone());
    let mut unique = std::collections::HashSet::new();
    if selected.iter().any(|name| {
        name.trim().is_empty()
            || !inherited.contains(name.as_str())
            || !unique.insert(name.as_str())
    }) {
        return Err(ToolError::permanent(
            "agent_template_invalid",
            "Agent 模板包含父 Agent 未提供、空值或重复工具",
        ));
    }
    let disallowed = template
        .disallowed_tool_names
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    if template
        .disallowed_tool_names
        .iter()
        .any(|name| name.trim().is_empty())
    {
        return Err(ToolError::permanent(
            "agent_template_invalid",
            "Agent 模板禁用工具列表包含空值",
        ));
    }
    selected.retain(|name| !disallowed.contains(name.as_str()));
    profile.tool_snapshot = selected;
    Ok(Some(template.snapshot))
}

/// 按 none、all 或最近 N 个已完成 Turn 冻结规范 Provider 中立消息 JSON。
fn freeze_parent_context(
    source: &dyn SpawnAgentContextSource,
    context: &SpawnAgentTemplateContext,
    inheritance: &ContextInheritance,
) -> Result<Vec<String>, ToolError> {
    if matches!(inheritance, ContextInheritance::None) {
        return Ok(Vec::new());
    }
    let completed = source.completed_turns(context)?;
    let first = match inheritance {
        ContextInheritance::None => completed.len(),
        ContextInheritance::All => 0,
        ContextInheritance::RecentTurns { count } => {
            completed.len().saturating_sub(*count as usize)
        }
    };
    completed[first..]
        .iter()
        .flat_map(|turn| turn.messages.iter())
        .map(|message| {
            message.validate().map_err(|_| {
                ToolError::permanent(
                    "agent_context_invalid",
                    "父 Transcript 包含无效的 Provider 中立消息",
                )
            })?;
            serde_json::to_string(message).map_err(|_| {
                ToolError::permanent("agent_context_invalid", "父 Transcript 消息无法规范序列化")
            })
        })
        .collect()
}

/// 解析并限制 WaitAgent 硬超时。
fn parse_wait_input(input: &Value) -> Result<WaitInput, ToolError> {
    let input: WaitInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.timeout_ms > MAX_WAIT_TIMEOUT_MILLISECONDS {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("timeout_ms 不能超过 {MAX_WAIT_TIMEOUT_MILLISECONDS}"),
        ));
    }
    Ok(input)
}

/// 解析 interrupt_agent 目标，并拒绝控制字符、空白包围和超长标识。
fn parse_target_input(input: &Value) -> Result<ParsedTargetInput, ToolError> {
    let input: TargetInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    Ok(ParsedTargetInput {
        target_agent_id: parse_target_agent_id(input.target_agent_id)?,
    })
}

/// 解析消息输入并执行不透明身份和正文 UTF-8 字节校验。
fn parse_send_message_input(input: &Value) -> Result<ParsedSendMessageInput, ToolError> {
    let input: SendMessageInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    validate_required_text(&input.message, MAX_MESSAGE_BYTES, "message")?;
    Ok(ParsedSendMessageInput {
        target_agent_id: parse_target_agent_id(input.target_agent_id)?,
        message: input.message,
    })
}

/// 严格解析 list_agents 的空对象输入。
fn parse_empty_input(input: &Value) -> Result<(), ToolError> {
    serde_json::from_value::<EmptyInput>(input.clone())
        .map(|_input| ())
        .map_err(invalid_input)
}

/// 将内部 Agent 状态投影为稳定状态名和可选 Turn 标识，不回显结果正文。
fn status_projection(status: &CollaborationAgentStatus) -> (&'static str, Option<&str>) {
    match status {
        CollaborationAgentStatus::PendingInit => ("pending_init", None),
        CollaborationAgentStatus::Idle => ("idle", None),
        CollaborationAgentStatus::WaitingCapacity { turn_id } => {
            ("waiting_capacity", Some(turn_id.as_str()))
        }
        CollaborationAgentStatus::Running { turn_id } => ("running", Some(turn_id.as_str())),
        CollaborationAgentStatus::Cancelling { turn_id } => ("cancelling", Some(turn_id.as_str())),
        CollaborationAgentStatus::Completed { turn_id, .. } => {
            ("completed", Some(turn_id.as_str()))
        }
        CollaborationAgentStatus::Interrupted { turn_id } => {
            ("interrupted", Some(turn_id.as_str()))
        }
        CollaborationAgentStatus::Failed { turn_id, .. } => ("failed", Some(turn_id.as_str())),
        CollaborationAgentStatus::Stopped => ("stopped", None),
    }
}

/// 从有界、无控制字符且无首尾空白的文本创建不透明目标身份。
fn parse_target_agent_id(value: String) -> Result<AgentId, ToolError> {
    if value.is_empty()
        || value.len() > MAX_AGENT_ID_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ToolError::permanent(
            "invalid_input",
            "target_agent_id 必须是 1..=256 UTF-8 字节且不含首尾空白或控制字符",
        ));
    }
    AgentId::new(value).map_err(|_error| {
        ToolError::permanent("invalid_input", "target_agent_id 不是有效的 Agent 标识")
    })
}

/// 校验必须非空且受 UTF-8 字节上限约束的工具文本。
fn validate_required_text(
    value: &str,
    maximum_bytes: usize,
    field: &'static str,
) -> Result<(), ToolError> {
    if value.trim().is_empty() {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("{field} 不能为空"),
        ));
    }
    if value.len() > maximum_bytes {
        return Err(ToolError::permanent(
            "invalid_input",
            format!("{field} 超过最大 UTF-8 字节数 {maximum_bytes}"),
        ));
    }
    Ok(())
}

/// 返回 interrupt_agent 复用的严格目标对象 Schema。
fn target_only_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_agent_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_AGENT_ID_BYTES
            }
        },
        "required": ["target_agent_id"],
        "additionalProperties": false
    })
}

/// 返回 send_message 与 followup_task 共享的严格目标和正文 Schema。
fn message_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target_agent_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_AGENT_ID_BYTES,
                "description": "使用 spawn_agent/list_agents 返回的 agent_id，不接受任务名或路径"
            },
            "message": {
                "type": "string",
                "minLength": 1,
                "maxLength": MAX_MESSAGE_BYTES
            }
        },
        "required": ["target_agent_id", "message"],
        "additionalProperties": false
    })
}

/// 返回 list_agents 使用的严格空对象 Schema。
fn empty_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    })
}

/// 在所有领域副作用之前拒绝已经取消的工具调用。
fn ensure_not_cancelled(context: &ToolContext) -> Result<(), ToolError> {
    if context.cancellation.is_cancelled() {
        Err(cancelled_error())
    } else {
        Ok(())
    }
}

/// 创建不包含用户正文或 mailbox 内容的固定取消错误。
fn cancelled_error() -> ToolError {
    ToolError::permanent("turn_cancelled", "当前 Turn 已取消")
}

/// 将严格反序列化错误归一为不回显输入正文的固定错误。
fn invalid_input(_error: serde_json::Error) -> ToolError {
    ToolError::permanent("invalid_input", "协作工具输入不符合严格 Schema")
}

/// 把 Collaboration 领域错误映射为稳定、有界且不泄露 mailbox 正文的工具错误。
fn normalize_collaboration_error(error: CollaborationError) -> ToolError {
    match error {
        CollaborationError::InvalidTurnLimit
        | CollaborationError::InvalidAgentPath(_)
        | CollaborationError::InvalidMessageId
        | CollaborationError::EmptyMessage
        | CollaborationError::TextTooLarge { .. }
        | CollaborationError::InvalidAgentProfile { .. }
        | CollaborationError::InvalidContextInheritance
        | CollaborationError::InvalidMailboxBatch => {
            ToolError::permanent("invalid_input", "协作命令参数无效")
        }
        CollaborationError::ResourceLimitExceeded { .. } => ToolError::permanent(
            "collaboration_capacity_exceeded",
            "协作运行时已达到固定资源上限",
        ),
        CollaborationError::IdentifierCollision { .. }
        | CollaborationError::DuplicateAgent { .. } => ToolError::permanent(
            "collaboration_identifier_conflict",
            "协作运行时标识发生冲突",
        ),
        CollaborationError::AgentNotFound { .. } => {
            ToolError::permanent("agent_not_found", "目标 Agent 不存在")
        }
        CollaborationError::DuplicateAgentPath { .. } => {
            ToolError::permanent("duplicate_agent_path", "同一根 Agent 树已存在该任务路径")
        }
        CollaborationError::IdempotencyConflict { .. } => ToolError::permanent(
            "collaboration_idempotency_conflict",
            "同一可信工具调用标识已经提交不同的协作命令输入",
        ),
        CollaborationError::RecursiveSpawnForbidden { .. } => ToolError::permanent(
            "recursive_agent_forbidden",
            "单层子 Agent 不允许继续创建 Agent",
        ),
        CollaborationError::SourceAgentNotRunning { .. }
        | CollaborationError::TurnMismatch { .. } => {
            ToolError::permanent("stale_turn", "协作命令来源不是当前运行中的可信 Turn")
        }
        CollaborationError::CrossTreeOperation => ToolError::permanent(
            "cross_tree_operation_forbidden",
            "不允许跨根 Agent 树执行协作命令",
        ),
        CollaborationError::TargetStopped { .. } => {
            ToolError::permanent("agent_stopped", "目标 Agent 已永久停止")
        }
        CollaborationError::TargetNotIdle { .. } => {
            ToolError::permanent("agent_not_idle", "目标 Agent 当前不空闲")
        }
        CollaborationError::TargetNotRunning { .. } => {
            ToolError::permanent("agent_not_running", "目标 Agent 当前没有可停止的 Turn")
        }
        CollaborationError::CannotStopRoot => {
            ToolError::permanent("cannot_stop_root", "StopAgent 不能停止根 Agent")
        }
        CollaborationError::CannotStopSelf => {
            ToolError::permanent("cannot_stop_self", "StopAgent 不能停止调用者自身")
        }
        CollaborationError::RetryNotAllowed { .. } => {
            ToolError::permanent("retry_not_allowed", "目标 Agent 当前不能重试")
        }
        CollaborationError::PendingUserSteers { .. } => {
            ToolError::permanent("pending_user_steers", "当前 Turn 仍有未消费的用户 steer")
        }
        CollaborationError::InputClaimMismatch { .. } => ToolError::permanent(
            "collaboration_input_claim_mismatch",
            "协作输入批次与当前可信 Turn 不一致",
        ),
        CollaborationError::PendingInputClaim { .. } => ToolError::retryable(
            "collaboration_input_pending",
            "当前 Turn 仍有未确认的 Transcript 输入批次；Runner 应先提交并确认该批次，再重试结束 Turn",
        ),
        CollaborationError::TreeClosed { .. } => {
            ToolError::permanent("agent_tree_closed", "当前 Agent 树已关闭")
        }
        CollaborationError::InvalidRecovery { .. }
        | CollaborationError::SequenceExhausted
        | CollaborationError::StatePoisoned
        | CollaborationError::StoreRecoveryRequired { .. } => ToolError::permanent(
            "collaboration_recovery_required",
            "协作运行时需要从持久状态恢复",
        ),
        CollaborationError::Store { .. } => {
            ToolError::retryable("collaboration_store_unavailable", "协作存储暂时不可用")
        }
        CollaborationError::CommittedExecutionPending { .. } => ToolError::permanent(
            "collaboration_committed_pending",
            "协作命令已经提交，执行确认仍在收敛；不要重复发送原命令",
        ),
    }
}

/// 将有界 JSON 值序列化为单个文本工具结果。
fn json_output(value: Value) -> Result<ToolOutput, ToolError> {
    let text = serde_json::to_string(&value).map_err(|_error| {
        ToolError::permanent("collaboration_output_failed", "协作工具结果无法序列化")
    })?;
    if text.len() > MAX_TOOL_OUTPUT_BYTES {
        return Err(ToolError::permanent(
            "collaboration_output_too_large",
            "协作命令可能已经提交，但结果超过固定输出上限；不要重复发送原命令",
        ));
    }
    Ok(ToolOutput::text(text))
}
