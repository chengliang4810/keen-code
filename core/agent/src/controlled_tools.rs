//! Workflow 等非模型入口使用的受控工具调用边界。
//!
//! 该模块故意不复用 [`AgentCommitEvent`]：资源层的模型工具事件必须绑定真实
//! `model_round`，Workflow 节点没有模型 Round，不能通过伪造模型响应绕过该约束。
//! Runner 仍负责所有工具输入、Hook、计划守卫、取消和输出边界；本模块只定义
//! 受控入口所需的独立持久化生命周期。

use std::sync::Arc;

use keencode_model::{ToolCall, ToolResult};

use crate::{
    AgentCommitSinkError, AgentId, PlanGuard, SessionId, ToolCallId, ToolCompletionStatus,
    ToolEffect, TurnCancellation, TurnId,
};

/// 受控工具调用的操作身份上限，防止工作流定义把未界定文本送入生命周期事件。
pub const MAX_CONTROLLED_OPERATION_ID_BYTES: usize = 512;

/// Workflow 或其他宿主受控调用使用的稳定身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlledToolIdentity {
    /// 受控调用所属 Session。
    pub session_id: SessionId,
    /// 受控调用所属 Turn。
    pub turn_id: TurnId,
    /// 发起调用的根 Agent 或单层子 Agent。
    pub source_agent_id: AgentId,
    /// 宿主分配的稳定操作身份，例如 workflow run、node 和 iteration 的摘要。
    pub operation_id: String,
}

impl ControlledToolIdentity {
    /// 校验不依赖模型协议的受控操作身份。
    pub fn validate(&self) -> Result<(), ControlledToolError> {
        if self.operation_id.trim().is_empty()
            || self.operation_id.len() > MAX_CONTROLLED_OPERATION_ID_BYTES
        {
            return Err(ControlledToolError::InvalidRequest {
                message: "受控工具操作身份为空或超过长度上限".to_owned(),
            });
        }
        Ok(())
    }
}

/// 受控工具调用请求。
#[derive(Clone)]
pub struct ControlledToolRequest {
    /// 受控调用的 Session、Turn、Agent 与宿主操作身份。
    pub identity: ControlledToolIdentity,
    /// 待执行的 Provider 中立工具调用；Runner 会在 Hook 修改后重新冻结它。
    pub call: ToolCall,
    /// 在工具 effect 判定后生效的计划只读守卫。
    pub plan_guard: PlanGuard,
    /// 由宿主持有并必须传播给 Hook 和真实工具的取消令牌。
    pub cancellation: TurnCancellation,
    /// 工具生命周期预检与持久提交出口。
    pub lifecycle: Arc<dyn ControlledToolLifecycleSink>,
}

impl std::fmt::Debug for ControlledToolRequest {
    /// 调试信息只包含稳定身份与工具名，不打印参数正文。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlledToolRequest")
            .field("identity", &self.identity)
            .field("tool_name", &self.call.name)
            .field("tool_call_id", &self.call.id)
            .field("plan_guard", &self.plan_guard)
            .finish_non_exhaustive()
    }
}

/// 受控工具调用预检候选。
///
/// Sink 必须在返回 reservation 前完成它自己的持久容量和恢复检查；Runner
/// 只有在预检成功后才会提交 Requested 并允许真实工具开始。
#[derive(Clone, Debug, PartialEq)]
pub struct ControlledToolPreflight {
    /// 受控调用身份。
    pub identity: ControlledToolIdentity,
    /// Hook 修改后、Schema 重新校验过的最终调用。
    pub call: ToolCall,
    /// 最终输入对应的 effect 分类。
    pub effect: ToolEffect,
}

/// 受控工具生命周期事件。
#[derive(Clone, Debug, PartialEq)]
pub enum ControlledToolEvent {
    /// 预检成功后、任何真实副作用前提交的请求事实。
    Requested {
        /// 受控调用身份。
        identity: ControlledToolIdentity,
        /// Hook 修改后最终冻结的工具调用。
        call: ToolCall,
        /// 最终输入对应的 effect 分类。
        effect: ToolEffect,
    },
    /// 工具已经越过全部执行前守卫，即将调用真实实现。
    ExecutionStarted {
        /// 受控调用身份。
        identity: ControlledToolIdentity,
        /// 已冻结的可信工具调用标识。
        tool_call_id: ToolCallId,
    },
    /// 工具形成唯一终态后的完成事实。
    Completed {
        /// 受控调用身份。
        identity: ControlledToolIdentity,
        /// 已冻结的可信工具调用标识。
        tool_call_id: ToolCallId,
        /// 工具完成、失败或取消分类。
        status: ToolCompletionStatus,
        /// 已通过 Agent Runtime 输出边界的结果。
        result: ToolResult,
    },
}

/// 受控工具生命周期持久化出口。
pub trait ControlledToolLifecycleSink: Send + Sync {
    /// 在真实工具执行前验证并保留本次受控调用的持久容量。
    fn preflight(
        &self,
        request: &ControlledToolPreflight,
    ) -> Result<Box<dyn ControlledToolReservation>, AgentCommitSinkError>;
}

/// 一次受控工具调用持有的生命周期 reservation。
pub trait ControlledToolReservation: Send {
    /// 同步提交一个生命周期事件；实现必须在返回前确认或明确失败。
    fn commit(&mut self, event: ControlledToolEvent) -> Result<(), AgentCommitSinkError>;

    /// 所有生命周期事件确认后消费预留。
    fn consume(self: Box<Self>);

    /// 调用未启动或明确失败且没有不确定进度时释放预留。
    fn release(self: Box<Self>);

    /// 提交结果不确定时保留完整事件，等待宿主恢复对账。
    fn retain_indeterminate(self: Box<Self>, event: ControlledToolEvent);
}

/// 没有独立持久化出口时供测试和纯内存宿主显式注入的 reservation。
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopControlledToolLifecycleSink;

/// Noop 受控生命周期 reservation。
#[derive(Clone, Copy, Debug, Default)]
struct NoopControlledToolReservation;

impl ControlledToolReservation for NoopControlledToolReservation {
    fn commit(&mut self, _event: ControlledToolEvent) -> Result<(), AgentCommitSinkError> {
        Ok(())
    }

    fn consume(self: Box<Self>) {}

    fn release(self: Box<Self>) {}

    fn retain_indeterminate(self: Box<Self>, _event: ControlledToolEvent) {}
}

impl ControlledToolLifecycleSink for NoopControlledToolLifecycleSink {
    fn preflight(
        &self,
        _request: &ControlledToolPreflight,
    ) -> Result<Box<dyn ControlledToolReservation>, AgentCommitSinkError> {
        Ok(Box::new(NoopControlledToolReservation))
    }
}

/// 受控工具执行成功、失败或被取消后的有界结果。
#[derive(Clone, Debug, PartialEq)]
pub struct ControlledToolResult {
    /// Hook 修改后最终实际执行或拒绝的调用。
    pub call: ToolCall,
    /// 最终 effect；输入在工具解析前失败时为空。
    pub effect: Option<ToolEffect>,
    /// 工具生命周期终态分类。
    pub status: ToolCompletionStatus,
    /// 已通过统一输出校验的模型中立结果。
    pub result: ToolResult,
    /// 是否已经提交 ExecutionStarted 并进入真实工具边界。
    pub execution_started: bool,
    /// 真实执行或取消收尾的墙钟毫秒数。
    pub duration_ms: u64,
}

/// 受控工具请求本身无效时的稳定错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlledToolError {
    /// 请求缺少有效的操作、调用 ID 或工具输入。
    InvalidRequest {
        /// 不包含调用参数正文的安全说明。
        message: String,
    },
}

impl std::fmt::Display for ControlledToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ControlledToolError {}
