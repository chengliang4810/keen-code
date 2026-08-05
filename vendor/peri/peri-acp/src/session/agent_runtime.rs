use peri_agent::thread::ThreadId;
use tokio_util::sync::CancellationToken;

/// agent 取消策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelPolicy {
    /// 同步子 agent：跟随父 agent 取消
    Cascade,
    /// Background 子 agent：仅跟随 session 根取消
    Independent,
}

impl CancelPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cascade => "cascade",
            Self::Independent => "independent",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "independent" => Self::Independent,
            _ => Self::Cascade,
        }
    }
}

/// agent 运行时状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Active,
    Done,
    Cancelled,
    Error,
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "done" => Self::Done,
            "cancelled" => Self::Cancelled,
            "error" => Self::Error,
            _ => Self::Active,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// 运行时 agent 实例
pub struct AgentRuntime {
    pub thread_id: ThreadId,
    pub cancel_token: CancellationToken,
    pub cancel_policy: CancelPolicy,
    pub status: AgentStatus,
}

impl AgentRuntime {
    pub fn new(thread_id: ThreadId, cancel_policy: CancelPolicy) -> Self {
        Self {
            thread_id,
            cancel_token: CancellationToken::new(),
            cancel_policy,
            status: AgentStatus::Active,
        }
    }
}
