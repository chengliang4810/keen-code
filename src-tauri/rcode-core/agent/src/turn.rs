//! Turn 状态迁移与计数不变量。

use crate::{AgentId, TurnId};
use std::error::Error;
use std::fmt;

/// Turn 在任一时刻唯一所处的运行阶段。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnPhase {
    /// Turn 已创建但尚未准备上下文。
    Created,
    /// 正在准备 Provider 中立上下文。
    PreparingContext,
    /// 正在压缩或重建上下文。
    Compacting,
    /// 正在发起本 Round 的模型请求。
    RequestingModel,
    /// 正在消费本 Round 的模型流事件。
    StreamingModel,
    /// 正在校验和调度模型产生的工具调用。
    SchedulingTools,
    /// 正在执行一个或多个已通过运行时守卫的工具调用。
    ExecutingTools,
    /// 正在按原始顺序提交本 Round 的消息和工具结果。
    CommittingRound,
    /// 正在运行决定完成或继续的停止 Hook。
    RunningStopHooks,
    /// 正在等待单层子 Agent 的 mailbox 活动。
    WaitingSubagent,
    /// 正在传播取消并等待活动工作安全停止。
    Cancelling,
    /// Turn 已进入唯一终态，具体原因由 `TerminalReason` 保存。
    Terminal,
}

/// Turn 进入唯一终态时记录的原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalReason {
    /// Agent 正常完成用户任务。
    Completed,
    /// 用户或上层 Session 取消了 Turn。
    Cancelled,
    /// 模型、工具、Hook 或内部运行失败。
    Failed,
    /// Round、Step 或其他硬上限已达到。
    LimitReached,
    /// 上下文达到阻断限制且无法安全恢复。
    ContextBlocked,
    /// 模型响应达到输出 Token 上限，无法形成完整的正常结果。
    ModelOutputLimit,
    /// 模型因内容安全策略或拒答结束，无法形成完整的正常结果。
    ModelRefusal,
}

/// 发生溢出时标识具体计数器。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterKind {
    /// 模型请求 Round 计数器。
    Round,
    /// 已实际执行工具的 Step 计数器。
    Step,
}

/// 一个用户 Turn 的纯领域状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnState {
    id: TurnId,
    source_agent_id: AgentId,
    phase: TurnPhase,
    terminal_reason: Option<TerminalReason>,
    round_count: u32,
    step_count: u32,
}

impl TurnState {
    /// 创建处于 `Created` 阶段且计数为零的 Turn。
    pub fn new(id: TurnId, source_agent_id: AgentId) -> Self {
        Self {
            id,
            source_agent_id,
            phase: TurnPhase::Created,
            terminal_reason: None,
            round_count: 0,
            step_count: 0,
        }
    }

    /// 返回稳定 Turn 标识。
    pub const fn id(&self) -> &TurnId {
        &self.id
    }

    /// 返回执行当前 Turn 的来源 Agent 标识。
    pub const fn source_agent_id(&self) -> &AgentId {
        &self.source_agent_id
    }

    /// 返回当前运行阶段。
    pub const fn phase(&self) -> TurnPhase {
        self.phase
    }

    /// 返回已经记录的唯一终态原因。
    pub const fn terminal_reason(&self) -> Option<TerminalReason> {
        self.terminal_reason
    }

    /// 返回已经开始的模型请求 Round 数量。
    pub const fn round_count(&self) -> u32 {
        self.round_count
    }

    /// 返回已经获准并进入实际执行的工具 Step 数量。
    pub const fn step_count(&self) -> u32 {
        self.step_count
    }

    /// 判断 Turn 是否已经进入不可逆的唯一终态。
    pub const fn is_terminal(&self) -> bool {
        matches!(self.phase, TurnPhase::Terminal)
    }

    /// 执行一次合法的非终态阶段迁移。
    pub fn transition_to(&mut self, next: TurnPhase) -> Result<(), TurnTransitionError> {
        if let Some(reason) = self.terminal_reason {
            return Err(TurnTransitionError::AlreadyTerminal { reason });
        }
        if matches!(next, TurnPhase::Terminal) {
            return Err(TurnTransitionError::TerminalRequiresFinish);
        }
        if !is_allowed_transition(self.phase, next) {
            return Err(TurnTransitionError::InvalidTransition {
                from: self.phase,
                to: next,
            });
        }
        self.phase = next;
        Ok(())
    }

    /// 开始一个新的模型请求 Round，并进入 `RequestingModel` 阶段。
    pub fn begin_round(&mut self) -> Result<u32, TurnTransitionError> {
        let next_count =
            self.round_count
                .checked_add(1)
                .ok_or(TurnTransitionError::CounterOverflow {
                    counter: CounterKind::Round,
                })?;
        self.transition_to(TurnPhase::RequestingModel)?;
        self.round_count = next_count;
        Ok(next_count)
    }

    /// 在 `ExecutingTools` 阶段记录一个已经进入实际执行的工具 Step。
    pub fn record_step(&mut self) -> Result<u32, TurnTransitionError> {
        if let Some(reason) = self.terminal_reason {
            return Err(TurnTransitionError::AlreadyTerminal { reason });
        }
        if !matches!(self.phase, TurnPhase::ExecutingTools) {
            return Err(TurnTransitionError::InvalidCounterPhase {
                counter: CounterKind::Step,
                phase: self.phase,
            });
        }
        let next_count =
            self.step_count
                .checked_add(1)
                .ok_or(TurnTransitionError::CounterOverflow {
                    counter: CounterKind::Step,
                })?;
        self.step_count = next_count;
        Ok(next_count)
    }

    /// 从允许的阶段写入唯一终态原因，并不可逆地进入 `Terminal`。
    pub fn finish(&mut self, reason: TerminalReason) -> Result<(), TurnTransitionError> {
        if let Some(existing) = self.terminal_reason {
            return Err(TurnTransitionError::AlreadyTerminal { reason: existing });
        }
        if !is_allowed_terminal_transition(self.phase, reason) {
            return Err(TurnTransitionError::InvalidTerminalTransition {
                from: self.phase,
                reason,
            });
        }
        self.phase = TurnPhase::Terminal;
        self.terminal_reason = Some(reason);
        Ok(())
    }
}

/// Turn 阶段、终态或计数规则被违反时返回的错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnTransitionError {
    /// 当前阶段不允许进入目标阶段。
    InvalidTransition {
        /// 当前阶段。
        from: TurnPhase,
        /// 被拒绝的目标阶段。
        to: TurnPhase,
    },
    /// 必须通过携带原因的 `finish` 方法进入终态。
    TerminalRequiresFinish,
    /// 当前阶段不能使用指定原因完成 Turn。
    InvalidTerminalTransition {
        /// 当前阶段。
        from: TurnPhase,
        /// 被拒绝的终态原因。
        reason: TerminalReason,
    },
    /// Turn 已经有唯一终态，不能再次迁移或完成。
    AlreadyTerminal {
        /// 首次写入且保持不变的终态原因。
        reason: TerminalReason,
    },
    /// 当前阶段不允许修改指定计数器。
    InvalidCounterPhase {
        /// 被拒绝的计数器。
        counter: CounterKind,
        /// 发起计数时的阶段。
        phase: TurnPhase,
    },
    /// 指定计数器超过 `u32` 可表达范围。
    CounterOverflow {
        /// 发生溢出的计数器。
        counter: CounterKind,
    },
}

impl fmt::Display for TurnTransitionError {
    /// 输出不包含用户内容的状态机错误信息。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { from, to } => {
                write!(formatter, "Turn 阶段不能从 {from:?} 迁移到 {to:?}")
            }
            Self::TerminalRequiresFinish => {
                formatter.write_str("Turn 必须通过 finish 写入终态原因")
            }
            Self::InvalidTerminalTransition { from, reason } => {
                write!(formatter, "Turn 阶段 {from:?} 不能以 {reason:?} 结束")
            }
            Self::AlreadyTerminal { reason } => {
                write!(formatter, "Turn 已经以 {reason:?} 结束")
            }
            Self::InvalidCounterPhase { counter, phase } => {
                write!(formatter, "{counter:?} 计数器不能在 {phase:?} 阶段更新")
            }
            Self::CounterOverflow { counter } => {
                write!(formatter, "{counter:?} 计数器溢出")
            }
        }
    }
}

impl Error for TurnTransitionError {}

/// 判断两个非终态阶段之间是否允许迁移。
const fn is_allowed_transition(from: TurnPhase, to: TurnPhase) -> bool {
    match from {
        TurnPhase::Created => matches!(to, TurnPhase::PreparingContext | TurnPhase::Cancelling),
        TurnPhase::PreparingContext => matches!(
            to,
            TurnPhase::Compacting | TurnPhase::RequestingModel | TurnPhase::Cancelling
        ),
        TurnPhase::Compacting => matches!(
            to,
            TurnPhase::PreparingContext | TurnPhase::RequestingModel | TurnPhase::Cancelling
        ),
        TurnPhase::RequestingModel => matches!(
            to,
            TurnPhase::StreamingModel | TurnPhase::Compacting | TurnPhase::Cancelling
        ),
        TurnPhase::StreamingModel => matches!(
            to,
            TurnPhase::SchedulingTools
                | TurnPhase::CommittingRound
                | TurnPhase::Compacting
                | TurnPhase::Cancelling
        ),
        TurnPhase::SchedulingTools => matches!(
            to,
            TurnPhase::ExecutingTools | TurnPhase::CommittingRound | TurnPhase::Cancelling
        ),
        TurnPhase::ExecutingTools => matches!(
            to,
            TurnPhase::SchedulingTools | TurnPhase::CommittingRound | TurnPhase::Cancelling
        ),
        TurnPhase::CommittingRound => matches!(
            to,
            TurnPhase::PreparingContext
                | TurnPhase::RunningStopHooks
                | TurnPhase::WaitingSubagent
                | TurnPhase::Cancelling
        ),
        TurnPhase::RunningStopHooks => matches!(
            to,
            TurnPhase::PreparingContext | TurnPhase::WaitingSubagent | TurnPhase::Cancelling
        ),
        TurnPhase::WaitingSubagent => {
            matches!(to, TurnPhase::PreparingContext | TurnPhase::Cancelling)
        }
        TurnPhase::Cancelling | TurnPhase::Terminal => false,
    }
}

/// 判断当前阶段能否以指定原因进入终态。
const fn is_allowed_terminal_transition(from: TurnPhase, reason: TerminalReason) -> bool {
    match reason {
        TerminalReason::Completed => matches!(
            from,
            TurnPhase::CommittingRound | TurnPhase::RunningStopHooks | TurnPhase::WaitingSubagent
        ),
        TerminalReason::Cancelled => matches!(from, TurnPhase::Cancelling),
        TerminalReason::Failed => !matches!(from, TurnPhase::Terminal),
        TerminalReason::LimitReached => !matches!(
            from,
            TurnPhase::Created | TurnPhase::Cancelling | TurnPhase::Terminal
        ),
        TerminalReason::ContextBlocked => matches!(
            from,
            TurnPhase::PreparingContext
                | TurnPhase::Compacting
                | TurnPhase::RequestingModel
                | TurnPhase::SchedulingTools
        ),
        TerminalReason::ModelOutputLimit | TerminalReason::ModelRefusal => {
            matches!(from, TurnPhase::StreamingModel | TurnPhase::CommittingRound)
        }
    }
}
