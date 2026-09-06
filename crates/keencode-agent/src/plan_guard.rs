//! 计划只读守卫的领域规则。

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

/// 工具调用对外部状态的影响类别。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolEffect {
    /// 工具只读取状态，不产生可观察副作用。
    ReadOnly,
    /// 工具可能修改文件、进程、网络远端或其他外部状态。
    ChangesState,
}

/// 计划守卫当前采用的运行状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanGuardState {
    /// 普通执行状态，工具按自身副作用分类直接执行。
    Inactive,
    /// 计划状态，任何可能产生副作用的工具调用都被拒绝。
    ReadOnly,
}

/// 在工具执行之前强制执行的计划只读守卫。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct PlanGuard {
    /// 当前 Turn 已冻结的普通执行或只读计划状态。
    state: PlanGuardState,
}

impl PlanGuard {
    /// 创建普通执行状态的守卫。
    pub const fn inactive() -> Self {
        Self {
            state: PlanGuardState::Inactive,
        }
    }

    /// 创建计划只读状态的守卫。
    pub const fn read_only() -> Self {
        Self {
            state: PlanGuardState::ReadOnly,
        }
    }

    /// 返回守卫当前状态。
    pub const fn state(self) -> PlanGuardState {
        self.state
    }

    /// 校验当前工具副作用是否满足计划只读约束。
    pub const fn authorize(self, effect: ToolEffect) -> Result<(), PlanGuardError> {
        if matches!(self.state, PlanGuardState::ReadOnly)
            && matches!(effect, ToolEffect::ChangesState)
        {
            return Err(PlanGuardError::StateChangeDenied);
        }
        Ok(())
    }
}

/// 计划守卫拒绝执行时返回的错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanGuardError {
    /// 计划状态禁止产生外部状态变更。
    StateChangeDenied,
}

impl fmt::Display for PlanGuardError {
    /// 输出计划守卫拒绝原因。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StateChangeDenied => formatter.write_str("计划模式禁止产生状态变更"),
        }
    }
}

impl Error for PlanGuardError {}

/// 规范化工具输入的固定长度摘要。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ToolInputHash(
    /// 规范化工具输入的 SHA-256 原始字节。
    [u8; 32],
);

impl ToolInputHash {
    /// 从上层已计算的 32 字节摘要创建输入摘要。
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// 返回输入摘要的字节引用。
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
