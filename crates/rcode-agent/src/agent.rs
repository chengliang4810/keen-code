//! 单层 Agent 和 mailbox 投递的领域规则。

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Agent 在根树中的深度，只允许根层和一层子 Agent。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AgentDepth(u8);

impl AgentDepth {
    /// 根 Agent 的固定深度。
    pub const ROOT: Self = Self(0);
    /// 单层子 Agent 的固定深度。
    pub const CHILD: Self = Self(1);

    /// 从数值创建深度，并拒绝超过单层边界的值。
    pub const fn new(depth: u8) -> Result<Self, AgentDepthError> {
        if depth <= Self::CHILD.0 {
            Ok(Self(depth))
        } else {
            Err(AgentDepthError::ExceedsSingleLayer { requested: depth })
        }
    }

    /// 返回深度数值。
    pub const fn value(self) -> u8 {
        self.0
    }

    /// 返回下一层子 Agent 深度，并在当前已经是子 Agent 时拒绝。
    pub const fn child(self) -> Result<Self, AgentDepthError> {
        match self.0 {
            0 => Ok(Self::CHILD),
            _ => Err(AgentDepthError::ExceedsSingleLayer {
                requested: self.0.saturating_add(1),
            }),
        }
    }

    /// 判断当前深度是否允许创建子 Agent。
    pub const fn can_spawn_child(self) -> bool {
        self.0 == Self::ROOT.0
    }
}

impl Serialize for AgentDepth {
    /// 将 Agent 深度序列化为受限数值。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for AgentDepth {
    /// 反序列化时重新执行单层深度校验。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let depth = u8::deserialize(deserializer)?;
        Self::new(depth).map_err(serde::de::Error::custom)
    }
}

/// Agent 深度超过产品单层限制时返回的错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentDepthError {
    /// 请求的深度超过根 Agent 加一层子 Agent的上限。
    ExceedsSingleLayer {
        /// 被拒绝的深度数值。
        requested: u8,
    },
}

impl fmt::Display for AgentDepthError {
    /// 输出单层限制错误和被拒绝的深度。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExceedsSingleLayer { requested } => {
                write!(formatter, "Agent 深度 {requested} 超过单层限制")
            }
        }
    }
}

impl Error for AgentDepthError {}

/// mailbox 消息对空闲目标 Agent 的投递语义。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MailboxDelivery {
    /// 只加入 mailbox，不启动空闲 Agent 的新 Turn。
    QueueOnly,
    /// 加入 mailbox，并在目标空闲时触发一个新 Turn。
    TriggerTurn,
}

impl MailboxDelivery {
    /// 判断该投递方式是否应唤醒空闲 Agent。
    pub const fn wakes_idle_agent(self) -> bool {
        matches!(self, Self::TriggerTurn)
    }
}

/// Agent 最近一次 Turn 的生命周期状态。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentStatus {
    /// Agent 已创建但尚未开始第一个 Turn。
    PendingInit,
    /// Agent 当前正在执行一个 Turn。
    Running,
    /// Agent 的当前 Turn 被中断，但身份和 mailbox 仍保留。
    Interrupted,
    /// Agent 的最近一次 Turn 已正常完成。
    Completed {
        /// 最近一次 Turn 的最终文本；没有文本时为 `None`。
        final_message: Option<String>,
    },
    /// Agent 的最近一次 Turn 失败。
    Failed {
        /// 已归一化且可展示的失败原因。
        message: String,
    },
    /// Agent 已永久停止，不能再接收任务。
    Stopped,
}

impl AgentStatus {
    /// 判断当前状态是否表示最近一次 Turn 已经结束。
    pub const fn is_turn_final(&self) -> bool {
        matches!(
            self,
            Self::Interrupted | Self::Completed { .. } | Self::Failed { .. } | Self::Stopped
        )
    }

    /// 判断 Agent 身份是否仍可接收 mailbox 消息或后续任务。
    pub const fn can_receive_messages(&self) -> bool {
        !matches!(self, Self::Stopped)
    }
}
