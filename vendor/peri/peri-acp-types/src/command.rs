//! Slash 命令契约（L5：命令执行体迁入 Agent 层的边界端口）。
//!
//! 自 `peri-acp/src/session/command/mod.rs` 与 `peri-acp/src/host/exec/`
//! `compact_pipeline` 命令执行体迁入：命令定义（[`AgentCommand`]）、
//! 执行上下文与命令/执行终态（[`CommandResult`] / [`PromptStopReason`]）
//! 归本层，Agent 层命令实现经本契约执行，ACP 保留协议化薄壳与装配面
//! （命令注册表 / EventSink 实现）。
//!
//! 依赖反转说明：
//! - `peri_config`（ACP provider 配置）不进入本契约——`CommandContext` 以
//!   `compact_config`（compact 管线输入）投影；
//! - 事件发射经 [`crate::event::EventSink`] 端口（ACP 实现，协议序列化面）。

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::compact::CompactConfig;
use crate::event::EventSink;
use crate::messages::BaseMessage;
use crate::store::ThreadStore;

/// 命令执行停止原因（`executor::PromptStopReason` 契约化，ACP re-export）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptStopReason {
    /// 正常完成——agent 完成本轮。
    EndTurn,
    /// 用户经 `session/cancel` 取消。
    Cancelled,
    /// agent 达到最大迭代次数。
    MaxTurnRequests,
}

impl PromptStopReason {
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::Cancelled => "cancelled",
            Self::MaxTurnRequests => "max_turn_requests",
        }
    }
}

/// 命令执行方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandKind {
    /// 直接执行，不构建 agent（如 compact、clear）。
    Immediate,
    /// 透传到正常 agent 管线（如 skills）。
    Passthrough,
    /// [预留] 变换 prompt 内容后传给 agent。
    Transform,
}

/// 命令执行上下文（L5 契约化：原 ACP `session::command::CommandContext`）。
///
/// `peri_config`（ACP provider 配置）不进入本结构：
/// - compact 管线使用 [`CommandContext::compact_config`]（ACP 装配点按
///   `load_compact_config` 语义预填，env overrides 每轮重新应用）；
pub struct CommandContext {
    pub session_id: String,
    pub history: Vec<BaseMessage>,
    pub cwd: String,
    /// compact 管线配置（ACP 装配点按 `load_compact_config` 语义预填：
    /// unwrap_or_default + env overrides，每次 CommandContext 构造 = 每轮）。
    pub compact_config: CompactConfig,
    /// 辅助 LLM（v2 stages/compact.rs 摘要 + Goal 工具验证共用）。
    pub auxiliary_model: Option<Arc<dyn peri_model::Model>>,
    pub event_sink: Arc<dyn EventSink>,
    /// 命令参数（命令名之后的文本）。
    pub args: String,
    /// 取消令牌，用于 Ctrl+C 打断长时间运行的命令（如 compact 的 LLM 调用）。
    pub cancel_token: CancellationToken,
    /// 持久化存储，用于 rewind 等需要删除消息的命令。
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 当前会话的 thread ID，配合 thread_store 使用。
    pub thread_id: Option<String>,
}

/// 命令执行结果。
pub struct CommandResult {
    /// 执行后的消息历史。
    pub messages: Vec<BaseMessage>,
    /// 停止原因。
    pub stop_reason: PromptStopReason,
}

/// Agent 侧命令 trait（L5：命令实现迁入 Agent 层，经本契约执行）。
#[async_trait]
pub trait AgentCommand: Send + Sync {
    /// 命令名（不含 `/` 前缀）。
    fn name(&self) -> &str;
    /// 别名列表。
    fn aliases(&self) -> Vec<&str> {
        vec![]
    }
    /// 命令描述。
    fn description(&self) -> &str;
    /// 命令类型。
    fn kind(&self) -> CommandKind;
    /// 执行命令。
    async fn execute(&self, ctx: CommandContext) -> CommandResult;
}
