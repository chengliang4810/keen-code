//! Collaboration v2 的单层异步子 Agent 领域核心。
//!
//! 本模块只管理稳定 Agent 树、Turn 容量、mailbox 和生命周期事件。
//! 持久化和真正的 Agent Loop 由端口接入，因此不依赖 Tauri、ACP 或具体磁盘实现。

use crate::{
    AgentDepth, AgentId, MailboxDelivery, PlanGuard, PlanGuardState, SessionId, ToolCallId,
    TurnCancellation, TurnId,
};
use keencode_model::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::thread;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{Instant, timeout_at};
use uuid::Uuid;

mod canonical;
mod recovery;
mod transitions;
mod validation;

pub use canonical::root_turn_prompt_digest;
use canonical::*;
use recovery::*;
use transitions::*;
use validation::*;

/// 一棵根树最多保留的根与单层子 Agent 总数。
pub(crate) const MAX_AGENTS_PER_ROOT: usize = 1_000;

/// 单个协调器冷启动时最多恢复的根 Agent 树数量。
const MAX_ROOT_TREES: usize = 1_024;

/// 单个协调器全部根树合计允许保留的 Agent 身份数量。
pub(crate) const MAX_AGENTS_PER_COORDINATOR: usize = 16_384;

/// 单个协调器全部根树合计允许保留的未消费 mailbox 消息数量。
const MAX_MAILBOX_MESSAGES_PER_COORDINATOR: usize = 131_072;

/// 单个协调器全部根树合计允许保留的 mailbox 正文字节数。
const MAX_MAILBOX_BYTES_PER_COORDINATOR: usize = 512 * 1024 * 1024;

/// 单个协调器全部根树合计允许保留的用户 steer 数量。
const MAX_PENDING_STEERS_PER_COORDINATOR: usize = 65_536;

/// 单个协调器全部根树合计允许保留的用户 steer 正文字节数。
const MAX_PENDING_STEER_BYTES_PER_COORDINATOR: usize = 256 * 1024 * 1024;

/// 单个协调器最多保留的协作工具幂等调用记录数量。
const MAX_COLLABORATION_INVOCATIONS_PER_COORDINATOR: usize = 131_072;

/// 单个协调器最多保留的外部根 Turn 幂等绑定数量。
const MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR: usize = 262_144;

/// 单个协调器全部身份、Turn、mailbox 与 steer 合计允许保留的文本字节数。
const MAX_RETAINED_TEXT_BYTES_PER_COORDINATOR: usize = 1024 * 1024 * 1024;

/// 单条任务、消息、Steer 或最终文本允许的最大 UTF-8 字节数。
const MAX_COLLABORATION_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// 单个子 Agent 生命周期职责允许持久化并向同树 Agent 展示的最大 UTF-8 字节数。
pub const MAX_AGENT_ASSIGNMENT_BYTES: usize = 512;

/// 单个 Agent mailbox 最多保留的未消费消息数量。
const MAX_MAILBOX_MESSAGES_PER_AGENT: usize = 4_096;

/// 单个 Agent mailbox 最多保留的未消费正文总字节数。
const MAX_MAILBOX_BYTES_PER_AGENT: usize = 32 * 1024 * 1024;

/// 单条子 Agent 完成通知最多保留的摘要字节数。
const MAX_COMPLETION_NOTIFICATION_BYTES: usize = 64 * 1024;

/// Agent 列表中当前 Turn 摘要允许保留的最大 UTF-8 字节数。
const MAX_CURRENT_TURN_SUMMARY_BYTES: usize = 4 * 1024;

/// 单棵树为子 Agent 完成通知保留的消息槽位数量。
const MAX_COMPLETION_MESSAGES_PER_TREE: usize = MAX_AGENTS_PER_ROOT - 1;

/// 单棵树为普通 Agent 消息开放的未消费消息数量。
const MAX_USER_MAILBOX_MESSAGES_PER_TREE: usize =
    MAX_MAILBOX_MESSAGES_PER_TREE - MAX_COMPLETION_MESSAGES_PER_TREE;

/// 单棵树为子 Agent 完成通知预留的正文字节数。
const MAX_COMPLETION_BYTES_PER_TREE: usize =
    MAX_COMPLETION_MESSAGES_PER_TREE * MAX_COMPLETION_NOTIFICATION_BYTES;

/// 单棵树为普通 Agent 消息开放的未消费正文字节数。
const MAX_USER_MAILBOX_BYTES_PER_TREE: usize =
    MAX_MAILBOX_BYTES_PER_TREE - MAX_COMPLETION_BYTES_PER_TREE;

/// 单棵恢复树最多接受的未消费 mailbox 消息总数。
const MAX_MAILBOX_MESSAGES_PER_TREE: usize = 32_768;

/// 单棵恢复树最多接受的未消费 mailbox 正文总字节数。
const MAX_MAILBOX_BYTES_PER_TREE: usize = 128 * 1024 * 1024;

/// 单个活跃 Turn 最多保留的未消费用户 Steer 数量。
const MAX_PENDING_STEERS_PER_AGENT: usize = 1_024;

/// 单个活跃 Turn 最多保留的未消费用户 Steer 总字节数。
const MAX_PENDING_STEER_BYTES_PER_AGENT: usize = 8 * 1024 * 1024;

/// 单个 Agent 配置最多冻结的工具名称数量。
const MAX_TOOL_SNAPSHOT_ENTRIES: usize = 512;

/// 模型标识、推理强度和工具名称共用的短字段最大字节数。
const MAX_PROFILE_FIELD_BYTES: usize = 1_024;

/// 系统签发的 Worktree lease 标识允许的最大 ASCII 字节数。
const MAX_WORKTREE_LEASE_BYTES: usize = 128;

/// 存储或执行端口错误允许进入领域状态的最大 UTF-8 字节数。
pub(crate) const MAX_PORT_ERROR_BYTES: usize = 64 * 1024;

/// RecentTurns 最多允许继承的父 Turn 数量。
const MAX_RECENT_TURNS: u32 = 10_000;

/// 单个子 Agent 创建时最多冻结的 Provider 中立消息数量。
const MAX_CONTEXT_SNAPSHOT_MESSAGES: usize = 65_536;

/// 单个子 Agent 创建时最多持久化的规范上下文 JSON 字节数。
const MAX_CONTEXT_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;

/// 单个扩展 Agent 模板最多允许冻结的额外写目录数量。
const MAX_AGENT_TEMPLATE_WRITE_DIRS: usize = 64;

/// 单个扩展 Agent 模板最多允许的模型轮次数量。
const MAX_AGENT_TEMPLATE_TURNS: u32 = 10_000;

/// Agent 路径不符合固定根路径或单层子路径规则。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentPathError {
    /// 路径不是固定的 `/root`。
    InvalidRoot,
    /// 子 Agent 名称为空或包含非法字符。
    InvalidChildName {
        /// 被拒绝的原始子 Agent 名称。
        name: String,
    },
    /// 已经是子 Agent 的路径尝试再创建一层。
    RecursiveChild,
}

impl fmt::Display for AgentPathError {
    /// 输出不包含机密数据的路径校验错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRoot => formatter.write_str("Agent 根路径必须是 /root"),
            Self::InvalidChildName { name } => {
                write!(formatter, "子 Agent 名称 {name:?} 不符合路径规则")
            }
            Self::RecursiveChild => formatter.write_str("单层 Agent 路径不允许继续创建子路径"),
        }
    }
}

impl Error for AgentPathError {}

/// 在一棵根 Agent 树内稳定寻址的 `/root/...` 路径。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AgentPath(String);

impl AgentPath {
    /// 返回根 Agent 的固定路径。
    pub fn root() -> Self {
        Self("/root".to_owned())
    }

    /// 从持久化字符串恢复并校验根或单层子 Agent 路径。
    pub fn parse(value: impl Into<String>) -> Result<Self, AgentPathError> {
        let value = value.into();
        if value == "/root" {
            return Ok(Self(value));
        }
        let Some(name) = value.strip_prefix("/root/") else {
            return Err(AgentPathError::InvalidRoot);
        };
        validate_child_name(name)?;
        Ok(Self(value))
    }

    /// 从当前根路径创建一层稳定子路径。
    pub fn child(&self, name: impl Into<String>) -> Result<Self, AgentPathError> {
        if self.0 != "/root" {
            return Err(AgentPathError::RecursiveChild);
        }
        let name = name.into();
        validate_child_name(&name)?;
        Ok(Self(format!("/root/{name}")))
    }

    /// 返回路径字符串视图。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 返回路径对应的根层或子层深度。
    pub fn depth(&self) -> AgentDepth {
        if self.0 == "/root" {
            AgentDepth::ROOT
        } else {
            AgentDepth::CHILD
        }
    }
}

impl fmt::Display for AgentPath {
    /// 将稳定路径原样写入格式化器。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for AgentPath {
    /// 将已校验路径序列化为单个字符串。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for AgentPath {
    /// 反序列化时重新执行根路径与单层子路径校验。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// 拒绝把同一系统 Worktree lease 同时绑定给多个 Agent 定义。
fn ensure_worktree_lease_available(
    state: &CoordinatorState,
    profile: &AgentProfile,
) -> Result<(), CollaborationError> {
    let Some(worktree_lease) = &profile.worktree_lease else {
        return Ok(());
    };
    if state.roots.values().any(|root| {
        root.known_agents.values().any(|definition| {
            definition
                .profile
                .worktree_lease
                .as_ref()
                .is_some_and(|known| known == worktree_lease)
        })
    }) {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "Worktree lease 已绑定到其他 Agent",
        });
    }
    Ok(())
}

/// 系统能力层签发的稳定、不可复用 Worktree 清理授权。
///
/// 该值只携带不透明 lease 标识，不携带也不接受文件系统路径。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorktreeLease(String);

impl WorktreeLease {
    /// 从系统签发的 ASCII 标识创建 Worktree lease。
    pub fn new(value: impl Into<String>) -> Result<Self, CollaborationError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_WORKTREE_LEASE_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if !valid {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "Worktree lease 必须是非空且不超过 128 字节的 ASCII 字母、数字、短横线或下划线",
            });
        }
        Ok(Self(value))
    }

    /// 返回不透明 Worktree lease 标识的字符串视图。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorktreeLease {
    /// 将不透明 Worktree lease 标识原样写入格式化器。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for WorktreeLease {
    /// 将不透明 lease 序列化为单个字符串。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WorktreeLease {
    /// 反序列化时重新执行 lease 字符集与长度校验。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// 持久 mailbox 消息的全局唯一标识。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MailboxMessageId(String);

impl MailboxMessageId {
    /// 从非空字符串创建 mailbox 消息标识。
    pub fn new(value: impl Into<String>) -> Result<Self, CollaborationError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(CollaborationError::InvalidMessageId);
        }
        if value.len() > MAX_PROFILE_FIELD_BYTES {
            return Err(CollaborationError::TextTooLarge {
                field: "mailbox 消息标识",
                maximum_bytes: MAX_PROFILE_FIELD_BYTES,
            });
        }
        Ok(Self(value))
    }

    /// 返回消息标识的字符串视图。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MailboxMessageId {
    /// 将消息标识原样写入格式化器。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for MailboxMessageId {
    /// 将 mailbox 消息标识序列化为单个字符串。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MailboxMessageId {
    /// 反序列化时重新执行消息标识校验。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// 子 Agent 创建时从父会话继承的上下文范围。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ContextInheritance {
    /// 不继承父 Agent 的历史。
    None,
    /// 继承父 Agent 的全部可用历史。
    All,
    /// 只继承父 Agent 最近若干个 Turn。
    RecentTurns {
        /// 需要继承的最近 Turn 数，必须大于零。
        count: u32,
    },
}

/// 一个独立 Agent Session 的运行快照配置。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentProfile {
    /// 该 Agent 固定使用的 Provider 中立模型标识。
    pub model: String,
    /// 该 Agent 固定使用的推理强度快照。
    pub reasoning_effort: Option<String>,
    /// 该 Agent 继承后不可放宽的计划只读守卫。
    pub plan_guard: PlanGuard,
    /// 该 Agent 独立的工作目录。
    pub cwd: PathBuf,
    /// 该 Agent 可选的系统 Worktree 清理授权，不包含实际目录路径。
    pub worktree_lease: Option<WorktreeLease>,
    /// 该 Agent 创建时固定的工具名称快照。
    pub tool_snapshot: Vec<String>,
}

/// spawn 提交前从当前项目扩展候选冻结的 Agent 模板非模型配置。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentTemplateSnapshot {
    /// 已由 Agent catalog 规范化的模板名称。
    pub name: String,
    /// 追加到 KeenCode 基础提示之后的冻结系统说明。
    pub system_prompt: String,
    /// 模板允许执行的最大模型轮数；为空时使用 Runtime 默认上限。
    pub max_turns: Option<u32>,
    /// 模板额外允许写入的项目内相对目录；Plan 只读守卫仍优先。
    pub allowed_write_dirs: Vec<PathBuf>,
}

/// 持久化的 Agent 身份、父子关系与独立 Session 定义。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentDefinition {
    /// Agent 的全局唯一标识。
    pub agent_id: AgentId,
    /// Agent 独立逻辑 Session 的标识。
    pub session_id: SessionId,
    /// 所属根 Agent 的标识。
    pub root_agent_id: AgentId,
    /// 所属根 Session 的所有者标识。
    pub root_session_id: SessionId,
    /// 直接父 Agent；根 Agent 固定为 `None`。
    pub parent_agent_id: Option<AgentId>,
    /// 在根树内稳定且可持久的 Agent 路径。
    pub path: AgentPath,
    /// 子 Agent 生命周期内稳定且对同树可见的职责；根 Agent 固定为 `None`。
    pub assignment: Option<String>,
    /// 只能是根层或一层子 Agent 的深度。
    pub depth: AgentDepth,
    /// 创建时固定的上下文继承方式。
    pub context_inheritance: ContextInheritance,
    /// 子 Agent 在 spawn 提交前已经冻结并规范编码的 Provider 中立消息；根 Agent 固定为空。
    pub context_snapshot: Vec<String>,
    /// 显式选择扩展 Agent 时冻结的模板；内置通用 Agent 与根 Agent 固定为空。
    pub agent_template: Option<AgentTemplateSnapshot>,
    /// 该 Agent 独立的模型、Plan、目录和工具快照。
    pub profile: AgentProfile,
}

/// 单个 Agent 最近 Turn 与调度器的组合状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CollaborationAgentStatus {
    /// Agent 身份已创建，但初始 Turn 还未入队。
    PendingInit,
    /// 根 Agent 刚注册或 Agent 已恢复，当前没有最近 Turn。
    Idle,
    /// Turn 已持久化入队，正在等待全局与根级容量。
    WaitingCapacity {
        /// 正在等待容量的 Turn 标识。
        turn_id: TurnId,
    },
    /// Agent 当前有一个正在执行的 Turn。
    Running {
        /// 当前正在执行的 Turn 标识。
        turn_id: TurnId,
    },
    /// 当前 Turn 已请求取消，正等待执行器收敛。
    Cancelling {
        /// 正在取消的 Turn 标识。
        turn_id: TurnId,
    },
    /// 最近 Turn 已正常完成，Agent 处于空闲状态。
    Completed {
        /// 已完成的 Turn 标识。
        turn_id: TurnId,
        /// 该 Turn 可选的最终文本。
        final_message: Option<String>,
    },
    /// 最近 Turn 已被中断，Agent 身份仍可重试。
    Interrupted {
        /// 已中断的 Turn 标识。
        turn_id: TurnId,
    },
    /// 最近 Turn 已失败，Agent 身份仍可重试。
    Failed {
        /// 已失败的 Turn 标识。
        turn_id: TurnId,
        /// 已归一化的失败原因。
        message: String,
    },
    /// 根 Session 关闭后的永久停止状态。
    Stopped,
}

impl CollaborationAgentStatus {
    /// 返回当前状态是否可以接受一个新 Turn。
    pub fn is_idle(&self) -> bool {
        matches!(
            self,
            Self::Idle | Self::Completed { .. } | Self::Interrupted { .. } | Self::Failed { .. }
        )
    }

    /// 返回当前正在执行或取消的 Turn 标识。
    pub fn active_turn_id(&self) -> Option<&TurnId> {
        match self {
            Self::Running { turn_id } | Self::Cancelling { turn_id } => Some(turn_id),
            _ => None,
        }
    }

    /// 返回 Agent 身份是否仍能接收 mailbox 消息。
    pub fn can_receive_messages(&self) -> bool {
        !matches!(self, Self::Stopped)
    }
}

/// Agent Turn 执行器回传的唯一终态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentTurnOutcome {
    /// Turn 正常完成。
    Completed {
        /// 可选的最终文本。
        final_message: Option<String>,
    },
    /// Turn 因显式取消或 StopAgent 而中断。
    Interrupted,
    /// Turn 因可展示的执行错误而失败。
    Failed {
        /// 已归一化的失败原因。
        message: String,
    },
}

/// 触发 Agent Turn 的持久化原因。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AgentTurnCause {
    /// 根 Agent 收到一次新用户任务。
    RootUser,
    /// 子 Agent 创建时的初始任务。
    InitialTask,
    /// `FollowupAgent` 在空闲目标上触发了新 Turn。
    Followup {
        /// 触发该 Turn 的 mailbox 消息标识。
        message_id: MailboxMessageId,
    },
    /// 重试一个失败或中断的旧 Turn。
    Retry {
        /// 被重试的旧 Turn 标识。
        previous_turn_id: TurnId,
    },
}

/// 执行器用于控制工具暴露的 Agent 能力快照。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentCapabilities {
    /// 当前 Agent 是否应暴露创建子 Agent 的工具。
    pub can_spawn_agent: bool,
}

/// 已原子预约容量、可交给 Agent Loop 的 Turn 启动请求。
#[derive(Clone, Debug)]
pub struct AgentTurnLaunch {
    /// 将要执行 Turn 的完整 Agent 定义。
    pub agent: AgentDefinition,
    /// 本次 Turn 的唯一标识。
    pub turn_id: TurnId,
    /// 创建子 Turn 的直接父 Turn；根用户 Turn 为 `None`。
    pub parent_turn_id: Option<TurnId>,
    /// 跨父子 Agent 关联的根 Turn 标识。
    pub root_turn_id: TurnId,
    /// 触发该 Turn 的原因。
    pub cause: AgentTurnCause,
    /// 初始任务或根用户输入；纯 mailbox Turn 可为 `None`。
    pub prompt: Option<String>,
    /// 只影响本 Turn 的独立取消令牌。
    pub cancellation: TurnCancellation,
    /// 本 Turn 继承并冻结的计划只读守卫。
    pub plan_guard: PlanGuard,
    /// 根据 Agent 深度生成的工具能力快照。
    pub capabilities: AgentCapabilities,
}

/// 传递给正在运行 Turn 的非重入安全边界信号类型。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AgentTurnSignalKind {
    /// 持久 mailbox 中出现了新消息。
    MailboxAvailable,
    /// 当前 Turn 收到了用户 steer。
    UserSteer,
}

/// 执行端口只能在安全消息边界处理的唤醒信号。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentTurnSignal {
    /// 需要在安全边界检查消息的 Agent。
    pub agent_id: AgentId,
    /// 接收信号的当前 Turn。
    pub turn_id: TurnId,
    /// 本次信号的消息类型。
    pub kind: AgentTurnSignalKind,
    /// 产生本次合并信号时 Agent 已提交的活动版本。
    pub activity_version: u64,
}

/// 执行端口在释放协调器容量前必须确认的整棵树静止请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuiesceAgentTree {
    /// 需要停止接收新 Turn 并终止全部运行任务的根 Agent 标识。
    pub root_agent_id: AgentId,
    /// 需要静止的根 Session 标识。
    pub root_session_id: SessionId,
    /// 执行端必须确认均已终止的全部 Agent 标识。
    pub agent_ids: Vec<AgentId>,
}

/// 关闭根 Session 时交给执行端口的全树清理请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CloseAgentTree {
    /// 需要关闭的根 Agent 标识。
    pub root_agent_id: AgentId,
    /// 需要关闭的根 Session 标识。
    pub root_session_id: SessionId,
    /// 需要停止后台进程的全部 Agent 标识。
    pub agent_ids: Vec<AgentId>,
    /// 需要由系统层按受管登记解析并消费的 Worktree lease。
    pub worktree_leases: Vec<WorktreeLease>,
}

/// 根树从开放到清理完成之间的持久生命周期阶段。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RecoveredRootLifecycle {
    /// 根树仍可创建和调度 Turn。
    Open,
    /// 关闭命令已提交，正在等待执行端确认全部运行任务静止。
    Closing,
    /// 执行端已确认静止并释放容量，正在等待系统层清理 Worktree。
    CleanupPending,
}

/// mailbox 消息的业务类型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MailboxMessageKind {
    /// Agent 之间主动发送的普通文本。
    AgentMessage,
    /// 子 Agent Turn 收敛后自动发给直接父 Agent 的报告。
    ChildTurnFinished {
        /// 子 Agent Turn 的最终状态。
        outcome: AgentTurnOutcome,
    },
}

/// 按目标 Agent 单调序号排列的持久 mailbox 消息。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MailboxMessage {
    /// 全局唯一的消息标识。
    pub message_id: MailboxMessageId,
    /// 目标 mailbox 内从一开始单调递增的序号。
    pub sequence: u64,
    /// 发送消息的 Agent。
    pub source_agent_id: AgentId,
    /// 发送方在根树内的稳定路径，供模型直接回复而不暴露内部 AgentId。
    pub source_agent_path: AgentPath,
    /// 发送方 Turn 的有效 Plan 守卫，防止延迟消息在更宽松的目标 Turn 中执行。
    pub source_plan_guard: PlanGuard,
    /// 接收消息的 Agent。
    pub target_agent_id: AgentId,
    /// 是否允许在目标空闲时触发新 Turn。
    pub delivery: MailboxDelivery,
    /// 消息的业务类型。
    pub kind: MailboxMessageKind,
    /// 需要在下一次模型采样中注入的完整文本。
    pub content: String,
    /// 产生该消息的来源 Turn。
    pub related_turn_id: Option<TurnId>,
    /// 触发该消息的直接来源 Turn，供延迟 Followup 保留原始因果。
    pub parent_turn_id: Option<TurnId>,
    /// 触发该消息的根 Turn，供跨主 Turn 的延迟 Followup 保留原始因果。
    pub root_turn_id: Option<TurnId>,
}

/// 当前 Turn 收到的用户 steer 内容。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UserSteer {
    /// 在该 Agent 内单调递增的 steer 序号。
    pub sequence: u64,
    /// steer 所属的活跃 Turn。
    pub turn_id: TurnId,
    /// 用户追加的完整文本。
    pub content: String,
}

/// WaitAgent 在不消费正文时返回的 mailbox 活动摘要。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MailboxActivitySummary {
    /// 当前尚未消费的 mailbox 消息数。
    pub pending_count: usize,
    /// 当前最新 mailbox 消息的单调序号。
    pub latest_sequence: u64,
}

/// WaitAgent 在不消费正文时返回的用户 steer 摘要。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserSteerSummary {
    /// 当前 Turn 尚未消费的 steer 数。
    pub pending_count: usize,
    /// 当前 Turn 最新 steer 的单调序号。
    pub latest_sequence: u64,
}

/// WaitAgent 只报告唤醒原因，不直接返回 mailbox 正文。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WaitAgentOutcome {
    /// 等待时已有或新增 mailbox 活动。
    MailboxActivity(MailboxActivitySummary),
    /// 当前 Turn 收到用户 steer。
    UserSteer(UserSteerSummary),
    /// 等待达到调用方指定的硬超时。
    TimedOut,
    /// 等待期间当前 Turn 已终止或根树已关闭。
    TurnEnded,
}

/// Collaboration 实时投影和事件日志共用的领域事件类型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CollaborationEventKind {
    /// 根或子 Agent 身份已持久化。
    AgentSpawned {
        /// 创建后不可静默改变的 Agent 定义。
        definition: Box<AgentDefinition>,
        /// 创建事件应投影的初始调度状态。
        initial_status: CollaborationAgentStatus,
        /// 根 Agent 的每树 Turn 上限；子 Agent 固定为 `None`。
        per_root_turn_limit: Option<usize>,
    },
    /// Agent 调度或最近 Turn 状态已变化。
    AgentStatusChanged {
        /// 状态变化前的值。
        previous: CollaborationAgentStatus,
        /// 状态变化后的值。
        current: CollaborationAgentStatus,
    },
    /// mailbox 消息已持久化入队。
    AgentMessageQueued {
        /// 按目标 mailbox 序列排序的完整消息。
        message: MailboxMessage,
    },
    /// mailbox 前缀已持久交给某个 Turn，等待 Transcript 提交后确认。
    AgentMessagesClaimed {
        /// 按 FIFO 顺序 claim 的消息标识。
        message_ids: Vec<MailboxMessageId>,
        /// 本次 claim 的最大 mailbox 序号。
        through_sequence: u64,
    },
    /// mailbox 前缀已确认进入可恢复 Transcript 并被原子消费。
    AgentMessagesConsumed {
        /// 按 FIFO 顺序消费的消息标识。
        message_ids: Vec<MailboxMessageId>,
        /// 本次消费的最大 mailbox 序号。
        through_sequence: u64,
    },
    /// 尚未消费的旧完成通知已被同一子 Agent 的较新终态替代。
    AgentCompletionNotificationSuperseded {
        /// 被替代且不再投影到 mailbox 的旧消息标识。
        message_id: MailboxMessageId,
    },
    /// Turn 已入队，但尚未取得容量。
    AgentTurnQueued {
        /// 入队 Turn 的触发原因。
        cause: AgentTurnCause,
        /// 根用户 Turn 或初始子 Agent Turn 的完整输入。
        prompt: Option<String>,
    },
    /// Turn 已同时取得全局与根级槽位并开始。
    AgentTurnStarted {
        /// 启动 Turn 的触发原因。
        cause: AgentTurnCause,
    },
    /// 执行端口已幂等接收 Turn，持久 StartTurn outbox 可以确认完成。
    AgentTurnDispatchAcknowledged,
    /// Turn 已正常完成。
    AgentTurnCompleted {
        /// 可选的最终文本。
        final_message: Option<String>,
    },
    /// Turn 已被取消或 StopAgent 中断。
    AgentTurnInterrupted,
    /// Turn 已失败。
    AgentTurnFailed {
        /// 已归一化的失败原因。
        message: String,
    },
    /// 当前 Turn 收到了用户 steer。
    AgentUserSteered {
        /// 已持久化的 steer 内容。
        steer: UserSteer,
    },
    /// 当前 Turn 已持久 claim 一组用户 steer，等待 Transcript 提交后确认。
    AgentUserSteersClaimed {
        /// 已 claim 的 steer 序号。
        sequences: Vec<u64>,
    },
    /// 当前 Turn 已确认一组用户 steer 进入可恢复 Transcript。
    AgentUserSteersConsumed {
        /// 已消费的 steer 序号。
        sequences: Vec<u64>,
    },
    /// 根 Session 已进入关闭阶段，后续不再接受或调度 Turn。
    AgentTreeClosing,
    /// 执行端已确认全树静止，协调器此时才释放预约容量。
    AgentTreeQuiesced,
    /// 系统层已幂等停止整棵树并完成全部托管 Worktree 清理。
    AgentTreeCleanupCompleted,
    /// 协作工具的幂等身份、输入摘要和首次结果已与业务事件原子提交。
    CollaborationInvocationCommitted {
        /// 可由事件重放恢复的最小幂等提交凭据。
        receipt: Box<CollaborationInvocationReceipt>,
    },
}

/// 包含 ACP 投影所需关联标识的 Collaboration 领域事件。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CollaborationEvent {
    /// 事件所属 Agent 的独立 Session。
    pub session_id: SessionId,
    /// 事件直接关联的 Turn；纯身份事件可为 `None`。
    pub turn_id: Option<TurnId>,
    /// 触发该事件的来源 Agent。
    pub source_agent_id: AgentId,
    /// 该事件正在描述的 Agent。
    pub agent_id: AgentId,
    /// 该 Agent 的直接父 Agent。
    pub parent_agent_id: Option<AgentId>,
    /// 该 Agent 在所属根树中的稳定路径。
    pub agent_path: AgentPath,
    /// 创建该子 Turn 的直接父 Turn。
    pub parent_turn_id: Option<TurnId>,
    /// 跨 Agent 关联的根 Turn。
    pub root_turn_id: Option<TurnId>,
    /// 在协调器内从一开始单调递增的事件序号。
    pub sequence: u64,
    /// 事件的领域负载。
    pub kind: CollaborationEventKind,
}

/// 存储或执行端口返回的可展示错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollaborationPortError {
    /// 已归一化且不包含秘密的错误文本。
    message: String,
}

impl CollaborationPortError {
    /// 从可展示文本创建端口错误。
    pub fn new(message: impl Into<String>) -> Self {
        const SUFFIX: &str = "\n[端口错误已截断]";
        let message = message.into();
        Self {
            message: bounded_utf8_with_suffix(&message, MAX_PORT_ERROR_BYTES, SUFFIX),
        }
    }

    /// 返回已归一化的错误文本。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CollaborationPortError {
    /// 将端口错误文本写入格式化器。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CollaborationPortError {}

/// 一批 Collaboration 事件的稳定内容标识。
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CollaborationEventBatchId(String);

impl CollaborationEventBatchId {
    /// 返回可用于 Store 幂等键的稳定十六进制标识。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CollaborationEventBatchId {
    /// 将批次标识原样写入格式化器。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for CollaborationEventBatchId {
    /// 将稳定批次摘要序列化为十六进制字符串。
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CollaborationEventBatchId {
    /// 反序列化时拒绝非标准 SHA-256 十六进制批次标识。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            Ok(Self(value))
        } else {
            Err(serde::de::Error::custom(
                "协作事件批次标识必须是 64 位小写十六进制",
            ))
        }
    }
}

/// 交给 Store 原子追加且可安全重放的稳定事件批次。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CollaborationEventBatch {
    /// 由期望水位与规范化事件内容共同计算的幂等标识。
    pub batch_id: CollaborationEventBatchId,
    /// 追加前 Store 必须处于的上一事件序号。
    pub expected_sequence: u64,
    /// 按连续事件序号排列的不可分割事件集合。
    pub events: Vec<CollaborationEvent>,
}

/// Store 对稳定事件批次的提交边界判断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CollaborationAppendResult {
    /// 当前调用首次完整追加了该批次。
    Appended,
    /// 同一批次先前已经完整提交，本次没有重复写入。
    AlreadyCommitted {
        /// Store 当前已经提交的最后事件序号。
        current_sequence: u64,
    },
    /// Store 可以证明该批次不存在，并返回当前仍可继续对账的事件水位。
    Absent {
        /// Store 当前已经提交的最后事件序号。
        current_sequence: u64,
    },
    /// Store 当前水位已经偏离批次期望，协调器必须冻结并冷恢复。
    Conflict {
        /// Store 实际已经提交的最后事件序号。
        actual_sequence: u64,
    },
    /// Store 无法判断该批次是否已经完整提交。
    Indeterminate {
        /// 不包含秘密且可向上展示的不确定原因。
        error: CollaborationPortError,
    },
}

/// 执行端口对 StartTurn 副作用边界的明确判断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentTurnStartResult {
    /// 当前调用首次接受并创建了 Turn 执行任务。
    Accepted,
    /// 执行端此前已经按 TurnId 接受该任务，本次没有重复创建。
    AlreadyAccepted,
    /// 执行端可能已经接受，协调器只能保留 outbox 后重试同一 TurnId。
    RetryableUnknown {
        /// 不包含秘密且可向上展示的不确定原因。
        error: CollaborationPortError,
    },
    /// 执行端保证没有产生副作用，协调器可以安全补偿为失败终态。
    PermanentRejectedBeforeSideEffect {
        /// 不包含秘密且可向上展示的永久拒绝原因。
        error: CollaborationPortError,
    },
}

/// 执行端口对全树静止副作用边界的明确判断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentTreeQuiesceResult {
    /// 当前调用首次确认该根树的全部任务已经停止。
    Quiesced,
    /// 执行端此前已经完成同一根树的静止，本次没有重复副作用。
    AlreadyQuiesced,
    /// 执行端可能已经静止根树，协调器必须保留同一请求后重试对账。
    RetryableUnknown {
        /// 不包含秘密且可向上展示的不确定原因。
        error: CollaborationPortError,
    },
    /// 执行端保证没有完成静止，协调器必须保留容量和关闭 outbox。
    PermanentRejectedBeforeQuiesce {
        /// 不包含秘密且可向上展示的永久拒绝原因。
        error: CollaborationPortError,
    },
}

/// 一次领域转换必须原子保存的事件批次与完整协调器 checkpoint。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CollaborationTransitionCommit {
    /// 本次转换产生且可按稳定标识安全重放的事件批次。
    pub batch: CollaborationEventBatch,
    /// 应与批次末事件序号完全一致的完整冷恢复 checkpoint。
    pub checkpoint: RecoveredCoordinator,
}

impl CollaborationTransitionCommit {
    /// 校验事件连续性、稳定批次标识和 checkpoint 水位完全一致。
    pub fn validate(&self) -> Result<(), CollaborationError> {
        if self.batch.events.is_empty() {
            return Err(CollaborationError::InvalidRecovery {
                message: "协作提交不能包含空事件批次".to_owned(),
            });
        }
        let mut committed_sequence = self.batch.expected_sequence;
        for event in &self.batch.events {
            committed_sequence = committed_sequence
                .checked_add(1)
                .ok_or(CollaborationError::SequenceExhausted)?;
            if event.sequence != committed_sequence {
                return Err(CollaborationError::InvalidRecovery {
                    message: "协作事件批次序号不连续".to_owned(),
                });
            }
        }
        let expected = collaboration_event_batch(self.batch.expected_sequence, &self.batch.events);
        if expected.batch_id != self.batch.batch_id {
            return Err(CollaborationError::InvalidRecovery {
                message: "协作事件批次标识与规范内容不一致".to_owned(),
            });
        }
        if self.checkpoint.last_event_sequence != committed_sequence {
            return Err(CollaborationError::InvalidRecovery {
                message: "协作 checkpoint 水位与事件批次末序号不一致".to_owned(),
            });
        }
        Ok(())
    }
}

/// 为事件日志与冷恢复提供原子持久化的存储端口。
pub trait CollaborationStore: Send + Sync {
    /// 返回 Store 当前已经提交的最后事件序号。
    fn current_sequence(&self) -> Result<u64, CollaborationPortError>;

    /// 返回最近一次已确认提交的完整协调器 checkpoint；全新 Store 返回空。
    fn load_coordinator_checkpoint(
        &self,
    ) -> Result<Option<RecoveredCoordinator>, CollaborationPortError>;

    /// 按稳定批次标识和期望水位，将事件批次与对应完整 checkpoint 原子提交。
    fn commit_transition(
        &self,
        commit: &CollaborationTransitionCommit,
    ) -> CollaborationAppendResult;

    /// 提交由协调器冷恢复内部生成的收敛批次；该批次可将未知执行中的 Turn 收敛为 Interrupted。
    ///
    /// 普通业务批次仍通过 [`Self::commit_transition`]，避免外部输入伪造恢复专用的
    /// Running 到 Interrupted 过渡。默认实现保持无需额外恢复约束的 Store 的普通提交语义。
    fn commit_recovery_transition(
        &self,
        commit: &CollaborationTransitionCommit,
    ) -> CollaborationAppendResult {
        self.commit_transition(commit)
    }

    /// 根据目标 Agent 标识加载独立于全局水位的局部驱逐 checkpoint。
    fn load_agent_checkpoint(
        &self,
        agent_id: &AgentId,
    ) -> Result<Option<RecoveredAgentCheckpoint>, CollaborationPortError>;

    /// 在驱逐 Agent 前原子保存一个带局部修订号的单 Agent checkpoint。
    fn save_agent_checkpoint(
        &self,
        checkpoint: &RecoveredAgentCheckpoint,
    ) -> Result<(), CollaborationPortError>;
}

/// 不阻塞协调器、真正启动和唤醒 Agent Loop 的执行端口。
pub trait AgentExecutionPort: Send + Sync {
    /// 幂等接收已预约容量的 Turn，并必须按 TurnId 去重后立即返回。
    fn start_turn(&self, launch: AgentTurnLaunch) -> AgentTurnStartResult;

    /// 通知正在运行的 Turn 于下一安全边界检查消息。
    fn signal_turn(&self, signal: AgentTurnSignal) -> Result<(), CollaborationPortError>;

    /// 幂等停止根树的全部执行任务；返回确认前协调器不得释放任何 Turn 容量。
    fn quiesce_tree(&self, request: QuiesceAgentTree) -> AgentTreeQuiesceResult;

    /// 在执行端已确认静止后，按系统登记的 lease 所有权幂等清理临时 Worktree。
    ///
    /// 实现必须只解析和消费受管 lease，绝不能把用户提供的路径当作删除授权。
    fn close_tree(&self, request: CloseAgentTree) -> Result<(), CollaborationPortError>;
}

/// 为确定性测试和生产 UUID 实现隔离标识生成的端口。
pub trait CollaborationIdGenerator: Send + Sync {
    /// 生成一个新 Agent 标识。
    fn next_agent_id(&self) -> AgentId;
    /// 生成一个新独立 Session 标识。
    fn next_session_id(&self) -> SessionId;
    /// 生成一个新 mailbox 消息标识。
    fn next_message_id(&self) -> MailboxMessageId;
}

/// 使用 UUID v7 生成按时间可排序标识的默认实现。
#[derive(Clone, Copy, Debug, Default)]
pub struct UuidCollaborationIdGenerator;

impl CollaborationIdGenerator for UuidCollaborationIdGenerator {
    /// 生成带 `agent-` 前缀的 UUID v7 Agent 标识。
    fn next_agent_id(&self) -> AgentId {
        AgentId::new(format!("agent-{}", Uuid::now_v7())).expect("UUID v7 Agent 标识始终非空")
    }

    /// 生成带 `session-` 前缀的 UUID v7 Session 标识。
    fn next_session_id(&self) -> SessionId {
        SessionId::new(format!("session-{}", Uuid::now_v7())).expect("UUID v7 Session 标识始终非空")
    }

    /// 生成带 `message-` 前缀的 UUID v7 mailbox 消息标识。
    fn next_message_id(&self) -> MailboxMessageId {
        MailboxMessageId(format!("message-{}", Uuid::now_v7()))
    }
}

/// 持久化快照中一封 mailbox 消息及其 Followup 触发身份与当前归属。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredMailboxMessage {
    /// 需要恢复的完整 mailbox 消息。
    pub message: MailboxMessage,
    /// TriggerTurn 首次为空闲目标创建的 Turn；创建后永不随重试或恢复改写。
    pub initial_triggered_turn_id: Option<TurnId>,
    /// 该 TriggerTurn 消息当前归属的待执行或活跃 Turn；普通消息固定为 `None`。
    pub claimed_turn_id: Option<TurnId>,
}

/// 用于重试与冷恢复的最近 Turn 快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredTurn {
    /// 最近 Turn 标识。
    pub turn_id: TurnId,
    /// 最近 Turn 的触发原因。
    pub cause: AgentTurnCause,
    /// 最近 Turn 的可选初始输入。
    pub prompt: Option<String>,
    /// 最近 Turn 的直接父 Turn。
    pub parent_turn_id: Option<TurnId>,
    /// 最近 Turn 所属的根 Turn。
    pub root_turn_id: TurnId,
    /// 最近 Turn 已持久化的终态。
    pub outcome: AgentTurnOutcome,
}

/// 可从 Session Store 恢复的非驻留 Agent 快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredAgent {
    /// 不可静默改变的 Agent 定义。
    pub definition: AgentDefinition,
    /// 当前未决 Turn 或最近 Turn 的可恢复状态。
    pub status: CollaborationAgentStatus,
    /// 尚未 exactly-once 消费的 FIFO mailbox。
    pub mailbox: Vec<RecoveredMailboxMessage>,
    /// 下一封 mailbox 消息应使用的单调序号。
    pub next_mailbox_sequence: u64,
    /// 已持久 claim 的 mailbox 批次所属 Turn；没有未确认批次时为 `None`。
    pub mailbox_claim_turn_id: Option<TurnId>,
    /// 已持久 claim 的 mailbox 批次最大序号；必须与所属 Turn 同时存在或同时为空。
    pub mailbox_claim_through_sequence: Option<u64>,
    /// 下一条用户 steer 应使用的单调序号。
    pub next_steer_sequence: u64,
    /// 已持久 claim 的用户 steer 批次所属 Turn；没有未确认批次时为 `None`。
    pub steer_claim_turn_id: Option<TurnId>,
    /// 已持久 claim 的用户 steer 批次最大序号；必须与所属 Turn 同时存在或同时为空。
    pub steer_claim_through_sequence: Option<u64>,
    /// 用于后续重试的最近 Turn 快照。
    pub last_turn: Option<RecoveredTurn>,
    /// live checkpoint 中未决 Turn 的来源 Agent；空闲快照固定为 `None`。
    pub current_source_agent_id: Option<AgentId>,
    /// live checkpoint 中未决 Turn 的触发原因；空闲快照固定为 `None`。
    pub current_turn_cause: Option<AgentTurnCause>,
    /// live checkpoint 中未决 Turn 的可选初始输入。
    pub current_turn_prompt: Option<String>,
    /// live checkpoint 中未决 Turn 的直接父 Turn。
    pub current_parent_turn_id: Option<TurnId>,
    /// live checkpoint 中未决 Turn 所属的根 Turn。
    pub current_root_turn_id: Option<TurnId>,
    /// live checkpoint 中未决 Turn 冻结的计划只读守卫。
    pub current_plan_guard: Option<PlanGuard>,
    /// 尚未交给未决 Turn 上下文的用户 steer。
    pub pending_steers: Vec<UserSteer>,
    /// StartTurn durable outbox 是否仍等待执行端口确认。
    pub start_pending: bool,
}

/// 一个驱逐 Agent 的局部 checkpoint，不依赖全局事件水位或其他根树清单。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredAgentCheckpoint {
    /// Agent 所属根身份，防止跨树替换局部快照。
    pub root_agent_id: AgentId,
    /// 该根树内单调递增的局部 checkpoint 修订号。
    pub revision: u64,
    /// 驱逐时保存的完整单 Agent 状态。
    pub agent: RecoveredAgent,
}

/// 一棵根 Agent 树的自包含 checkpoint；全局水位和身份命名空间由协调器快照保存。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredAgentTree {
    /// 根 Agent 标识。
    pub root_agent_id: AgentId,
    /// 根 Session 所有者标识。
    pub root_session_id: SessionId,
    /// 该根树允许同时运行的子 Agent Turn 上限；根 Turn 不计入。
    pub per_root_turn_limit: usize,
    /// 根树开放、静止中或待清理的持久生命周期阶段。
    pub lifecycle: RecoveredRootLifecycle,
    /// live checkpoint 为 `true`；静止导出快照为 `false`。
    pub live: bool,
    /// 该根树下一次分配 TurnId 时使用的持久单调序号。
    pub next_turn_sequence: u64,
    /// 该根树下一次保存驱逐 Agent 时使用的局部修订号。
    pub next_checkpoint_revision: u64,
    /// 根树创建过的全部不可变 Agent 定义；包含已从驻留内存驱逐的子 Agent。
    pub known_agents: Vec<AgentDefinition>,
    /// 快照中的根 Agent 和单层子 Agent。
    pub agents: Vec<RecoveredAgent>,
}

/// 跨 Runner 重放协作工具时使用的可信调用身份。
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CollaborationInvocationKey {
    /// 发起协作调用的根 Agent 或单层子 Agent。
    pub source_agent_id: AgentId,
    /// 首次执行协作调用的来源 Turn。
    pub source_turn_id: TurnId,
    /// Runner 从真实模型响应冻结的工具调用标识。
    pub tool_call_id: ToolCallId,
}

/// 幂等记录保存的完整规范业务输入。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CollaborationInvocationInput {
    /// 创建单层子 Agent 的完整请求。
    SpawnAgent(Box<SpawnAgentRequest>),
    /// 向同一根树 Agent 投递一封消息。
    SendMessage {
        /// 接收消息的目标 Agent。
        target_agent_id: AgentId,
        /// 未丢失也未摘要的完整消息正文。
        content: String,
        /// 只入队或在空闲时触发 Turn 的投递语义。
        delivery: MailboxDelivery,
    },
    /// 请求停止同一根树内目标子 Agent 的当前 Turn。
    StopAgent {
        /// 需要停止的目标子 Agent。
        target_agent_id: AgentId,
    },
    /// 以外部稳定操作标识向一个正在运行的 Agent 注入用户 steer。
    SteerAgent {
        /// 接收 steer 的目标 Agent。
        target_agent_id: AgentId,
        /// 未丢失也未摘要的完整用户正文。
        content: String,
    },
    /// 为失败或中断的同树 Agent 创建一个新 Turn。
    RetryAgent {
        /// 需要重试的目标 Agent。
        target_agent_id: AgentId,
    },
    /// 恢复失败或中断的同树单层子 Agent。
    ResumeAgent {
        /// 需要恢复的目标 Agent。
        target_agent_id: AgentId,
    },
}

/// 协作幂等记录中不含用户正文的稳定操作类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CollaborationInvocationKind {
    /// 创建单层子 Agent。
    SpawnAgent,
    /// 以 QueueOnly 或 TriggerTurn 语义投递 Agent 消息。
    SendMessage,
    /// 停止目标子 Agent 的当前 Turn。
    StopAgent,
    /// 向正在运行的 Agent 注入用户 steer。
    SteerAgent,
    /// 重试失败或中断的 Agent。
    RetryAgent,
    /// 恢复失败或中断的单层子 Agent。
    ResumeAgent,
}

/// 幂等记录保存的首次成功结果。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CollaborationInvocationOutput {
    /// 首次创建的子 Agent 身份和初始 Turn。
    SpawnedAgent(SpawnedAgent),
    /// 首次入队的消息身份及可选触发 Turn。
    Message {
        /// 首次生成且不会因重放改变的 mailbox 消息标识。
        message_id: MailboxMessageId,
        /// TriggerTurn 首次为空闲目标创建的 Turn；QueueOnly 固定为 `None`。
        triggered_turn_id: Option<TurnId>,
    },
    /// 首次停止请求确定的目标 Agent 和目标 Turn。
    StoppedAgent {
        /// 首次停止请求作用的目标子 Agent。
        target_agent_id: AgentId,
        /// 首次停止请求作用且后续必须原样返回的 Turn。
        stopped_turn_id: TurnId,
    },
    /// 首次持久化且后续重放必须原样返回的用户 steer。
    UserSteer(UserSteer),
    /// 首次重试创建且后续重放必须原样返回的新 Turn。
    RetriedAgent {
        /// 首次重试的目标 Agent。
        target_agent_id: AgentId,
        /// 首次重试分配的新 Turn。
        retry_turn_id: TurnId,
    },
    /// 首次恢复创建且后续重放必须原样返回的新 Turn。
    ResumedAgent {
        /// 首次恢复的目标 Agent。
        target_agent_id: AgentId,
        /// 首次恢复分配的新 Turn。
        resume_turn_id: TurnId,
    },
}

/// 与协作业务事件同批持久化的最小幂等提交凭据。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CollaborationInvocationReceipt {
    /// 跨 Runner 重放时使用的可信调用身份。
    pub key: CollaborationInvocationKey,
    /// 首次提交的稳定协作操作类型。
    pub kind: CollaborationInvocationKind,
    /// 对完整规范业务输入计算的版本化 SHA-256 摘要。
    pub input_digest: [u8; 32],
    /// 首次提交且后续必须原样返回的成功结果。
    pub output: CollaborationInvocationOutput,
}

/// 协调器 checkpoint 中一条可排序的协作工具幂等记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredCollaborationInvocation {
    /// 跨 Runner 重放时使用的可信调用身份。
    pub key: CollaborationInvocationKey,
    /// 首次提交的稳定协作操作类型。
    pub kind: CollaborationInvocationKind,
    /// 对首次完整规范业务输入计算的版本化 SHA-256 摘要。
    pub input_digest: [u8; 32],
    /// 首次提交且后续原样返回的成功结果。
    pub output: CollaborationInvocationOutput,
}

/// 协调器 checkpoint 中一个外部根 Turn 标识的不可变幂等绑定。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredRootTurnBinding {
    /// 由 Session 命令层提供且不能由协调器改写的根 Turn 标识。
    pub turn_id: TurnId,
    /// 首次绑定该 Turn 的固定根 Agent。
    pub root_agent_id: AgentId,
    /// 对完整根用户输入计算的 SHA-256 摘要，不在幂等账本重复保存正文。
    pub prompt_digest: [u8; 32],
    /// 首次启动时冻结且后续重试必须一致的 Plan 守卫。
    pub plan_guard: PlanGuard,
}

/// 即使根树为空也能恢复全局水位和单调身份分配器的协调器快照。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredCoordinator {
    /// 快照提交时协调器最近一个全局事件序号。
    pub last_event_sequence: u64,
    /// 根身份的持久命名空间；同一协调器生命周期内不可改变。
    pub root_identity_namespace: AgentId,
    /// 下一棵根树应使用的持久单调序号。
    pub next_root_sequence: u64,
    /// 按规范键排序的全部未移除根树及其关闭元数据。
    pub roots: Vec<RecoveredAgentTree>,
    /// 按来源 Agent、Turn 和 ToolCall 排序的全部协作工具幂等记录。
    pub invocations: Vec<RecoveredCollaborationInvocation>,
    /// 按 Turn 标识排序的全部外部根 Turn 幂等绑定。
    pub root_turn_bindings: Vec<RecoveredRootTurnBinding>,
}

/// 运行中子 Agent 全局并发上限的经校验配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollaborationLimits {
    /// 所有根树合计允许的运行中子 Agent Turn 数；根 Turn 不计入。
    pub global_turn_limit: usize,
}

impl CollaborationLimits {
    /// 创建不允许零槽位的全局容量配置。
    pub fn new(global_turn_limit: usize) -> Result<Self, CollaborationError> {
        if global_turn_limit == 0 {
            return Err(CollaborationError::InvalidTurnLimit);
        }
        Ok(Self { global_turn_limit })
    }
}

/// 可在多个 Coordinator 间共享的进程级子 Agent Turn 容量。
///
/// 等待者只在 Turn 入队或容量变化时被驱动，不创建轮询任务。
pub struct CollaborationGlobalTurnLimiter {
    state: Mutex<GlobalTurnLimiterState>,
}

/// 全局 limiter 的小型驻留状态。
struct GlobalTurnLimiterState {
    limit: usize,
    in_use: usize,
    dispatching: bool,
    coordinators: HashMap<u64, Weak<CollaborationCoordinatorInner>>,
    waiters: VecDeque<u64>,
    waiting: HashSet<u64>,
}

/// `dispatching` 标志的 RAII 复位守卫：Drop 在 panic 展开时也会复位，
/// 防止一次深层 panic 永久饿死全局子 Agent 派发。
struct GlobalDispatchingGuard {
    limiter: Arc<CollaborationGlobalTurnLimiter>,
}

impl Drop for GlobalDispatchingGuard {
    fn drop(&mut self) {
        match self.limiter.state.lock() {
            Ok(mut state) => state.dispatching = false,
            Err(poisoned) => {
                poisoned.into_inner().dispatching = false;
            }
        }
    }
}

impl CollaborationGlobalTurnLimiter {
    /// 创建一个不允许零槽位的共享子 Agent 容量限制器。
    pub fn new(global_turn_limit: usize) -> Result<Self, CollaborationError> {
        if global_turn_limit == 0 {
            return Err(CollaborationError::InvalidTurnLimit);
        }
        Ok(Self {
            state: Mutex::new(GlobalTurnLimiterState {
                limit: global_turn_limit,
                in_use: 0,
                dispatching: false,
                coordinators: HashMap::new(),
                waiters: VecDeque::new(),
                waiting: HashSet::new(),
            }),
        })
    }

    /// 返回当前已占用子 Turn 数与全局上限。
    pub fn capacity(&self) -> Result<(usize, usize), CollaborationError> {
        let state = self
            .state
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        Ok((state.in_use, state.limit))
    }
}

/// 一个子 Turn 持有的 RAII 全局槽位。
struct GlobalTurnPermit {
    limiter: Weak<CollaborationGlobalTurnLimiter>,
}

impl fmt::Debug for GlobalTurnPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GlobalTurnPermit")
    }
}

impl Drop for GlobalTurnPermit {
    fn drop(&mut self) {
        let Some(limiter) = self.limiter.upgrade() else {
            return;
        };
        if let Ok(mut state) = limiter.state.lock() {
            debug_assert!(state.in_use > 0, "全局子 Agent permit 计数不得下溢");
            if state.in_use > 0 {
                state.in_use -= 1;
            }
        }
    }
}

/// 注册一棵新根 Agent 树的请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootAgentRequest {
    /// 已由上层 Session 管理器分配的根 Session 标识。
    pub session_id: SessionId,
    /// 根 Agent 的独立运行配置与最低 Plan 约束。
    pub profile: AgentProfile,
    /// 该根树同时运行子 Agent Turn 的上限；根 Turn 不计入。
    pub per_root_turn_limit: usize,
}

/// 根 Agent 创建单层子 Agent 的请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnAgentRequest {
    /// 用于稳定 `/root/...` 路径的小写任务名。
    pub task_name: String,
    /// 子 Agent 第一个 Turn 的完整任务文本。
    pub initial_task: String,
    /// 子 Agent 生命周期内稳定且会向同树其他 Agent 展示的职责摘要。
    pub assignment: String,
    /// 子 Agent 的父上下文继承方式。
    pub context_inheritance: ContextInheritance,
    /// 按继承范围在 spawn 时冻结并规范编码的 Provider 中立父消息。
    pub context_snapshot: Vec<String>,
    /// 显式选择扩展 Agent 时在提交前冻结的模板；缺省通用子 Agent 为空。
    pub agent_template: Option<AgentTemplateSnapshot>,
    /// 子 Agent 请求的运行配置，Plan 只能被父 Turn 进一步收紧。
    pub profile: AgentProfile,
}

/// 返回给调用方的稳定 Agent 身份摘要。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentHandle {
    /// Agent 的唯一标识。
    pub agent_id: AgentId,
    /// Agent 的独立 Session 标识。
    pub session_id: SessionId,
    /// Agent 在根树内的稳定路径。
    pub path: AgentPath,
}

/// `list_agents` 返回的同根树 Agent 身份与当前生命周期摘要。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollaborationAgentSummary {
    /// 不包含模型、目录或工具快照的稳定 Agent 身份。
    pub agent: AgentHandle,
    /// 直接父 Agent；根 Agent 固定为 `None`。
    pub parent_agent_id: Option<AgentId>,
    /// 子 Agent 生命周期内稳定的职责；根 Agent 固定为 `None`。
    pub assignment: Option<String>,
    /// 查询时的当前 Turn 或最近 Turn 状态。
    pub status: CollaborationAgentStatus,
    /// 当前未决 Turn 的有界任务摘要；空闲或没有初始正文时为空。
    pub current_turn_summary: Option<String>,
    /// 当前未决 Turn 所属根 Turn；空闲 Agent 固定为空。
    pub current_root_turn_id: Option<TurnId>,
}

/// SpawnAgent 立即返回的身份与初始 Turn 标识。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SpawnedAgent {
    /// 已持久化的子 Agent 身份。
    pub agent: AgentHandle,
    /// 已入队或启动的初始 Turn 标识。
    pub initial_turn_id: TurnId,
}

/// 当前全局与各根树的槽位投影。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollaborationCapacity {
    /// 全局正在使用的子 Agent Turn 槽位数。
    pub global_in_use: usize,
    /// 全局子 Agent Turn 槽位上限。
    pub global_limit: usize,
    /// 按根 Agent 排列的当前子 Turn 使用槽位与上限。
    pub roots: Vec<(AgentId, usize, usize)>,
}

/// 一次已提交限额更新观察到的单项派发错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollaborationLimitDispatchError {
    /// 错误归属的进程内 Coordinator；`None` 表示全局 limiter 自身失败。
    coordinator_id: Option<u64>,
    /// 不改变限额提交事实的派发错误。
    error: CollaborationError,
}

impl CollaborationLimitDispatchError {
    /// 返回错误归属的进程内 Coordinator 标识。
    pub fn coordinator_id(&self) -> Option<u64> {
        self.coordinator_id
    }

    /// 返回原始派发错误。
    pub fn error(&self) -> &CollaborationError {
        &self.error
    }
}

impl fmt::Display for CollaborationLimitDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.coordinator_id {
            Some(coordinator_id) => {
                write!(formatter, "Coordinator {coordinator_id}: {}", self.error)
            }
            None => write!(formatter, "全局 limiter: {}", self.error),
        }
    }
}

/// 新增派发诊断统一经过凭据脱敏后才进入 tracing 边界。
pub(crate) fn redacted_dispatch_error(error: &CollaborationError) -> String {
    keencode_model::redact_error_secrets_bounded(&error.to_string(), MAX_PORT_ERROR_BYTES)
}

/// 多 Coordinator 限额已经原子提交后的调度结果。
///
/// `dispatch_errors` 只描述新限额发布后唤醒既有等待 Turn 时发生的错误；它们不得
/// 被解释为配置提交失败，也不能再通过回滚撤销已经启动的 Turn。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollaborationLimitUpdateReport {
    dispatch_errors: Vec<CollaborationLimitDispatchError>,
    dispatch_in_progress: bool,
}

impl CollaborationLimitUpdateReport {
    /// 返回配置提交后各 Coordinator 的调度错误。
    pub fn dispatch_errors(&self) -> &[CollaborationLimitDispatchError] {
        &self.dispatch_errors
    }

    /// 是否已有另一轮全局派发正在消费等待队列。
    ///
    /// `true` 不表示配置失败；后续派发错误会在产生位置携带 Coordinator 标识记录。
    pub fn dispatch_in_progress(&self) -> bool {
        self.dispatch_in_progress
    }
}

/// 重复或过期终态回调的处理结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnCompletionDisposition {
    /// 该终态首次被原子提交。
    Committed,
    /// 该 Turn 已终止或根树已关闭，回调被幂等忽略。
    IgnoredStale,
}

/// 对指定 Agent 精确 Turn 发出取消请求后的幂等结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnCancellationDisposition {
    /// 本次调用首次提交取消请求。
    Requested,
    /// 相同 Turn 先前已经进入取消阶段，本次没有重复发信号。
    AlreadyRequested,
    /// 指定 Turn 属于该 Agent，但查询时已经处于终态。
    NotRunning,
}

/// Collaboration 领域校验、端口或恢复失败。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CollaborationError {
    /// 全局或根级 Turn 上限不能为零。
    InvalidTurnLimit,
    /// 路径或子 Agent 名称不符合规则。
    InvalidAgentPath(AgentPathError),
    /// mailbox 消息标识为空。
    InvalidMessageId,
    /// 需要发送或 steer 的文本为空。
    EmptyMessage,
    /// 子 Agent 职责为空、包含边界空白或控制字符，或超过专用上限。
    InvalidAssignment,
    /// 用户或执行端口提供的文本超过确定性内存边界。
    TextTooLarge {
        /// 超限字段的稳定名称。
        field: &'static str,
        /// 允许的最大 UTF-8 字节数。
        maximum_bytes: usize,
    },
    /// Agent、mailbox、steer 或工具快照超过固定数量边界。
    ResourceLimitExceeded {
        /// 超限资源的稳定名称。
        resource: &'static str,
        /// 允许的最大数量或总字节数。
        maximum: usize,
    },
    /// Agent 模型、目录或工具快照配置不完整或不安全。
    InvalidAgentProfile {
        /// 不包含原始路径或模型内容的失败说明。
        message: &'static str,
    },
    /// 标识生成端口返回了仍在使用的重复标识。
    IdentifierCollision {
        /// 冲突标识所属的稳定类别。
        kind: &'static str,
    },
    /// 上下文继承策略与冻结快照不一致或参数越界。
    InvalidContextInheritance,
    /// 指定 Agent 不存在且无法冷恢复。
    AgentNotFound {
        /// 未找到的 Agent 标识。
        agent_id: AgentId,
    },
    /// 根 Session 或 Agent 身份与已注册值冲突。
    DuplicateAgent {
        /// 冲突的 Agent 标识。
        agent_id: AgentId,
    },
    /// 同一根树已使用相同稳定 AgentPath。
    DuplicateAgentPath {
        /// 冲突的稳定路径。
        path: AgentPath,
    },
    /// 同一可信调用身份使用了不同的协作操作或业务输入。
    IdempotencyConflict {
        /// 冲突调用的来源 Agent。
        source_agent_id: AgentId,
        /// 冲突调用的来源 Turn。
        source_turn_id: TurnId,
        /// 冲突调用的真实工具调用标识。
        tool_call_id: ToolCallId,
    },
    /// 子 Agent 尝试再创建一层 Agent。
    RecursiveSpawnForbidden {
        /// 被拒绝的来源 Agent。
        source_agent_id: AgentId,
    },
    /// 只有正在运行的来源 Agent 才能调用协作操作。
    SourceAgentNotRunning {
        /// 未处于 Running 状态的来源 Agent。
        source_agent_id: AgentId,
    },
    /// 源和目标 Agent 不属于同一根树。
    CrossTreeOperation,
    /// 目标 Agent 已永久停止。
    TargetStopped {
        /// 已停止的目标 Agent。
        agent_id: AgentId,
    },
    /// 目标 Agent 当前不是可以执行该操作的空闲状态。
    TargetNotIdle {
        /// 非空闲的目标 Agent。
        agent_id: AgentId,
    },
    /// 目标 Agent 没有可以中断的活跃 Turn。
    TargetNotRunning {
        /// 没有活跃 Turn 的目标 Agent。
        agent_id: AgentId,
    },
    /// StopAgent 不允许以根 Agent 为目标。
    CannotStopRoot,
    /// StopAgent 不允许中断调用者自身。
    CannotStopSelf,
    /// FollowupTask 不允许为根 Agent 创建内部 Turn；向根报告应使用 SendMessage。
    CannotFollowupRoot,
    /// 只读来源不能把指令投递给正在非只读执行的子 Agent。
    ReadOnlyMessageToWritableChild,
    /// 只有失败或中断的 Turn 才能重试。
    RetryNotAllowed {
        /// 当前不允许重试的 Agent。
        agent_id: AgentId,
    },
    /// 调用方指定的 Turn 不是该 Agent 的当前 Turn。
    TurnMismatch {
        /// 发生 Turn 不匹配的 Agent。
        agent_id: AgentId,
        /// 调用方提供的 Turn 标识。
        turn_id: TurnId,
    },
    /// 执行器尝试结束仍有未消费用户 Steer 的 Turn。
    PendingUserSteers {
        /// 尚有 Steer 的 Agent。
        agent_id: AgentId,
        /// 尚有 Steer 的活跃 Turn。
        turn_id: TurnId,
    },
    /// Runtime 使用了与当前持久 claim 不一致的 Turn 或最大序号。
    InputClaimMismatch {
        /// claim 所属 Agent。
        agent_id: AgentId,
        /// 调用方提供的 Turn。
        turn_id: TurnId,
        /// mailbox 或用户 steer 的稳定类别。
        input_kind: &'static str,
    },
    /// 执行器尝试结束仍有未确认 Transcript 输入批次的 Turn。
    PendingInputClaim {
        /// 尚有 claim 的 Agent。
        agent_id: AgentId,
        /// 尚有 claim 的活跃 Turn。
        turn_id: TurnId,
        /// mailbox 或用户 steer 的稳定类别。
        input_kind: &'static str,
    },
    /// mailbox 消费批次不能为零。
    InvalidMailboxBatch,
    /// 根 Agent 树已关闭，不再接受新工作。
    TreeClosed {
        /// 已关闭的根 Agent。
        root_agent_id: AgentId,
    },
    /// 冷恢复快照的所有者、父链或 Agent 定义校验失败。
    InvalidRecovery {
        /// 不包含秘密的校验失败原因。
        message: String,
    },
    /// 事件或 mailbox 单调序号已耗尽。
    SequenceExhausted,
    /// 共享领域状态锁已中毒。
    StatePoisoned,
    /// Store 无法确认最后一批事件，协调器必须从持久状态重新构建。
    StoreRecoveryRequired {
        /// 不包含秘密且可向上展示的冻结原因。
        message: String,
    },
    /// 原子事件追加或冷恢复端口失败。
    Store {
        /// 已归一化的存储端口错误。
        message: String,
    },
    /// 领域命令已经提交，但执行端确认或清理仍需通过 outbox 收敛。
    CommittedExecutionPending {
        /// 已归一化且明确禁止调用方重发原命令正文的待收敛说明。
        message: String,
    },
}

impl fmt::Display for CollaborationError {
    /// 输出面向本地日志和界面的归一化错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTurnLimit => formatter.write_str("Turn 并发上限必须大于零"),
            Self::InvalidAgentPath(error) => write!(formatter, "{error}"),
            Self::InvalidMessageId => formatter.write_str("mailbox 消息标识不能为空"),
            Self::EmptyMessage => formatter.write_str("协作消息不能为空"),
            Self::InvalidAssignment => formatter
                .write_str("子 Agent 职责必须非空、无首尾空白或控制字符，且不超过 512 UTF-8 字节"),
            Self::TextTooLarge {
                field,
                maximum_bytes,
            } => write!(formatter, "{field} 超过最大 UTF-8 字节数 {maximum_bytes}"),
            Self::ResourceLimitExceeded { resource, maximum } => {
                write!(formatter, "{resource} 超过最大限制 {maximum}")
            }
            Self::InvalidAgentProfile { message } => {
                write!(formatter, "Agent 运行配置无效：{message}")
            }
            Self::IdentifierCollision { kind } => write!(formatter, "{kind} 标识发生冲突"),
            Self::InvalidContextInheritance => {
                formatter.write_str("上下文继承策略无效或与冻结快照不一致")
            }
            Self::AgentNotFound { agent_id } => write!(formatter, "Agent {agent_id} 不存在"),
            Self::DuplicateAgent { agent_id } => write!(formatter, "Agent {agent_id} 已存在"),
            Self::DuplicateAgentPath { path } => write!(formatter, "Agent 路径 {path} 已存在"),
            Self::IdempotencyConflict {
                source_agent_id,
                source_turn_id,
                tool_call_id,
            } => write!(
                formatter,
                "协作调用幂等冲突：Agent {source_agent_id} 的 Turn {source_turn_id} 已使用 ToolCall {tool_call_id} 提交不同输入"
            ),
            Self::RecursiveSpawnForbidden { source_agent_id } => {
                write!(formatter, "子 Agent {source_agent_id} 不允许递归创建 Agent")
            }
            Self::SourceAgentNotRunning { source_agent_id } => {
                write!(formatter, "来源 Agent {source_agent_id} 当前未运行")
            }
            Self::CrossTreeOperation => formatter.write_str("不允许跨根 Agent 树协作"),
            Self::TargetStopped { agent_id } => write!(formatter, "目标 Agent {agent_id} 已停止"),
            Self::TargetNotIdle { agent_id } => write!(formatter, "目标 Agent {agent_id} 不空闲"),
            Self::TargetNotRunning { agent_id } => {
                write!(formatter, "目标 Agent {agent_id} 未运行")
            }
            Self::CannotStopRoot => formatter.write_str("StopAgent 不能中断根 Agent"),
            Self::CannotStopSelf => formatter.write_str("StopAgent 不能中断调用者自身"),
            Self::CannotFollowupRoot => {
                formatter.write_str("FollowupTask 不能为根 Agent 创建内部 Turn")
            }
            Self::ReadOnlyMessageToWritableChild => {
                formatter.write_str("只读来源不能向正在非只读执行的子 Agent 投递消息")
            }
            Self::RetryNotAllowed { agent_id } => {
                write!(formatter, "Agent {agent_id} 当前不能重试")
            }
            Self::TurnMismatch { agent_id, turn_id } => {
                write!(
                    formatter,
                    "Turn {turn_id} 不是 Agent {agent_id} 的当前 Turn"
                )
            }
            Self::PendingUserSteers { agent_id, turn_id } => {
                write!(
                    formatter,
                    "Agent {agent_id} 的 Turn {turn_id} 仍有未消费用户 Steer"
                )
            }
            Self::InputClaimMismatch {
                agent_id,
                turn_id,
                input_kind,
            } => write!(
                formatter,
                "Agent {agent_id} 的 Turn {turn_id} 与当前 {input_kind} 输入 claim 不一致"
            ),
            Self::PendingInputClaim {
                agent_id,
                turn_id,
                input_kind,
            } => write!(
                formatter,
                "Agent {agent_id} 的 Turn {turn_id} 仍有未确认的 {input_kind} Transcript 输入 claim"
            ),
            Self::InvalidMailboxBatch => formatter.write_str("mailbox 消费批次必须大于零"),
            Self::TreeClosed { root_agent_id } => {
                write!(formatter, "Agent 树 {root_agent_id} 已关闭")
            }
            Self::InvalidRecovery { message } => write!(formatter, "Agent 冷恢复失败: {message}"),
            Self::SequenceExhausted => formatter.write_str("Collaboration 单调序号已耗尽"),
            Self::StatePoisoned => formatter.write_str("Collaboration 状态锁已中毒"),
            Self::StoreRecoveryRequired { message } => {
                write!(formatter, "Collaboration 存储状态待恢复: {message}")
            }
            Self::Store { message } => write!(formatter, "Collaboration 存储失败: {message}"),
            Self::CommittedExecutionPending { message } => {
                write!(formatter, "Collaboration 命令已提交，执行待收敛: {message}")
            }
        }
    }
}

impl Error for CollaborationError {}

impl From<AgentPathError> for CollaborationError {
    /// 将 AgentPath 校验错误嵌入 Collaboration 错误。
    fn from(error: AgentPathError) -> Self {
        Self::InvalidAgentPath(error)
    }
}

/// mailbox 内部条目同时记录 TriggerTurn 的首次触发身份与当前 Turn 归属。
#[derive(Clone, Debug)]
struct MailboxEntry {
    /// 对外可见并持久化的完整消息。
    message: MailboxMessage,
    /// 首次入队时为空闲目标创建的 Turn；后续只读。
    initial_triggered_turn_id: Option<TurnId>,
    /// 当前为该 TriggerTurn 消息创建或复用的待执行 Turn。
    claimed_turn_id: Option<TurnId>,
}

/// 用于重试和冷恢复的最近 Turn 内部记录。
#[derive(Clone, Debug)]
struct TurnRecord {
    /// Turn 标识。
    turn_id: TurnId,
    /// Turn 触发原因。
    cause: AgentTurnCause,
    /// 可选的初始任务文本。
    prompt: Option<String>,
    /// 直接父 Turn。
    parent_turn_id: Option<TurnId>,
    /// 根 Turn。
    root_turn_id: TurnId,
    /// 已持久化的终态。
    outcome: AgentTurnOutcome,
}

/// live checkpoint 恢复时需要持久追加的确定性中断记录。
#[derive(Clone, Debug)]
struct RecoveredTurnResolution {
    /// 需要收敛的 Agent 定义。
    definition: AgentDefinition,
    /// 恢复前的未决状态。
    previous_status: CollaborationAgentStatus,
    /// 根据权威 Runtime 终态或保守中断策略形成的最近 Turn。
    turn: TurnRecord,
    /// 原未决 Turn 的来源 Agent。
    source_agent_id: AgentId,
}

/// Runtime 已读取但尚未确认写入可恢复 Transcript 的输入批次。
#[derive(Clone, Debug, Eq, PartialEq)]
struct InputBatchClaim {
    /// 负责把该批输入提交到 Transcript 的 Turn。
    turn_id: TurnId,
    /// 本批次覆盖的最大 mailbox 或 steer 单调序号。
    through_sequence: u64,
}

/// 驻留内存的 Agent 领域状态。
#[derive(Clone, Debug)]
struct AgentEntry {
    /// 不可静默改变的 Agent 定义。
    definition: AgentDefinition,
    /// 当前调度或最近 Turn 状态。
    status: CollaborationAgentStatus,
    /// 尚未 exactly-once 消费的 FIFO mailbox。
    mailbox: VecDeque<MailboxEntry>,
    /// 当前 mailbox 正文的 UTF-8 总字节数。
    mailbox_bytes: usize,
    /// 当前 mailbox 中子 Agent 完成通知的数量。
    completion_count: usize,
    /// 当前 mailbox 中子 Agent 完成通知的正文字节数。
    completion_bytes: usize,
    /// 下一封 mailbox 消息的单调序号。
    next_mailbox_sequence: u64,
    /// 已交给 Runtime 但尚未确认进入 Transcript 的 mailbox 前缀。
    mailbox_claim: Option<InputBatchClaim>,
    /// 尚未交给当前 Turn 上下文的用户 steer。
    steers: VecDeque<UserSteer>,
    /// 当前未消费用户 steer 正文的 UTF-8 总字节数。
    steer_bytes: usize,
    /// 下一条用户 steer 的单调序号。
    next_steer_sequence: u64,
    /// 已交给 Runtime 但尚未确认进入 Transcript 的 steer 批次。
    steer_claim: Option<InputBatchClaim>,
    /// 用于重试的最近 Turn 记录。
    last_turn: Option<TurnRecord>,
    /// mailbox、steer 或 Turn 终止的单调活动版本。
    activity_version: u64,
    /// 向任意数量 WaitAgent 等待者广播最新活动版本。
    activity_sender: watch::Sender<u64>,
}

/// 驱逐 Agent 的可信局部 checkpoint 引用。
#[derive(Clone, Debug)]
struct EvictedAgentCheckpointRef {
    /// 根树内单调递增的 checkpoint 修订号。
    revision: u64,
    /// 对单 Agent 恢复内容计算的规范 SHA-256 摘要。
    digest: [u8; 32],
    /// 局部 checkpoint 中尚未消费的 steer 数量。
    steer_count: usize,
    /// 局部 checkpoint 中尚未消费的 steer 正文字节数。
    steer_bytes: usize,
    /// 局部 checkpoint 中 mailbox 首次触发 Turn 标识的总字节数。
    initial_triggered_turn_bytes: usize,
    /// 除 mailbox 与 steer 外，该 Agent 动态保留文本的字节数。
    dynamic_text_bytes: usize,
}

/// 一棵根 Agent 树的容量与已占用路径。
#[derive(Clone, Debug)]
struct RootEntry {
    /// 根 Agent 标识。
    root_agent_id: AgentId,
    /// 根 Session 所有者标识。
    root_session_id: SessionId,
    /// 根树 Turn 并发上限。
    turn_limit: usize,
    /// 根树当前已原子预约的 Turn 槽位数。
    in_use: usize,
    /// 根树当前开放、静止中或待清理的持久生命周期阶段。
    lifecycle: RecoveredRootLifecycle,
    /// 当前进程是否暂时禁止新领域副作用；不写入 checkpoint，冷启动后重新开放。
    suspended: bool,
    /// 整棵树尚未消费的 mailbox 消息数量。
    mailbox_count: usize,
    /// 整棵树尚未消费的 mailbox 正文字节数。
    mailbox_bytes: usize,
    /// 整棵树尚未消费的子 Agent 完成通知数量。
    completion_count: usize,
    /// 整棵树尚未消费的子 Agent 完成通知正文字节数。
    completion_bytes: usize,
    /// 已从驻留内存驱逐的 Agent 及其局部修订号与不可伪造状态摘要。
    evicted_agent_checkpoints: HashMap<AgentId, EvictedAgentCheckpointRef>,
    /// 该根树下一次分配 TurnId 时使用的持久单调序号。
    next_turn_sequence: u64,
    /// 下一次驱逐 Agent 时分配的根内局部 checkpoint 修订号。
    next_checkpoint_revision: u64,
    /// 已创建过的 Agent 标识与不可变定义映射，驱逐冷状态后也不允许重用。
    known_agents: HashMap<AgentId, AgentDefinition>,
}

/// 已持久化入队、尚未预约容量的 Turn。
#[derive(Clone, Debug)]
struct QueuedTurn {
    /// 将要执行该 Turn 的 Agent。
    agent_id: AgentId,
    /// 所属根 Agent。
    root_agent_id: AgentId,
    /// 待执行 Turn 标识。
    turn_id: TurnId,
    /// 触发该 Turn 的来源 Agent。
    source_agent_id: AgentId,
    /// 直接父 Turn。
    parent_turn_id: Option<TurnId>,
    /// 根 Turn。
    root_turn_id: TurnId,
    /// 入队原因。
    cause: AgentTurnCause,
    /// 可选的初始任务文本。
    prompt: Option<String>,
    /// 本 Turn 不可被子 Agent 放宽的计划只读守卫。
    plan_guard: PlanGuard,
}

/// 已进入执行阶段的活跃 Turn。
#[derive(Clone, Debug)]
struct ActiveTurn {
    /// 正在执行 Turn 的 Agent。
    agent_id: AgentId,
    /// 触发该 Turn 的来源 Agent。
    source_agent_id: AgentId,
    /// 所属根 Agent。
    root_agent_id: AgentId,
    /// 活跃 Turn 标识。
    turn_id: TurnId,
    /// 直接父 Turn。
    parent_turn_id: Option<TurnId>,
    /// 根 Turn。
    root_turn_id: TurnId,
    /// Turn 触发原因。
    cause: AgentTurnCause,
    /// 可选的初始任务文本。
    prompt: Option<String>,
    /// 本 Turn 不可被子 Agent 放宽的计划只读守卫。
    plan_guard: PlanGuard,
    /// 只影响本 Turn 的独立取消令牌。
    cancellation: TurnCancellation,
    /// 当前 Turn 是否由所属根 Turn 的用户取消级联覆盖。
    ///
    /// 该标记只服务当前进程内迟到终态的收敛：级联取消必须保留动态输入，且不能
    /// 因 TriggerTurn mailbox 自动续跑。冷恢复会把所有未决 Turn 直接收敛为中断，
    /// 因而不需要把这个瞬态执行标记另行写入 checkpoint。
    cancelled_by_root_turn: bool,
    /// 子 Agent Turn 持有的进程级全局槽位；根 Turn 固定为 `None`。
    global_permit: Option<Arc<GlobalTurnPermit>>,
}

/// 驻留协调器中一次协作工具调用的首次输入和成功结果。
#[derive(Clone, Debug)]
struct CollaborationInvocationRecord {
    /// 首次提交的稳定协作操作类型。
    kind: CollaborationInvocationKind,
    /// 对首次完整规范业务输入计算的版本化 SHA-256 摘要。
    input_digest: [u8; 32],
    /// 首次提交且后续原样返回的成功结果。
    output: CollaborationInvocationOutput,
}

/// 驻留协调器中一个外部根 Turn 的不可变幂等绑定。
#[derive(Clone, Debug, Eq, PartialEq)]
struct RootTurnBinding {
    /// 首次绑定该 Turn 的固定根 Agent。
    root_agent_id: AgentId,
    /// 对完整根用户输入计算的 SHA-256 摘要。
    prompt_digest: [u8; 32],
    /// 首次启动时冻结的 Plan 守卫。
    plan_guard: PlanGuard,
}

/// SignalTurn durable outbox 的独立合并键，避免不同信号类型互相覆盖。
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AgentTurnSignalKey {
    /// 接收信号的 Agent 标识。
    agent_id: AgentId,
    /// 接收信号的当前 Turn 标识。
    turn_id: TurnId,
    /// 需要在安全边界处理的独立信号类型。
    kind: AgentTurnSignalKind,
}

impl AgentTurnSignalKey {
    /// 从可执行信号生成稳定 outbox 键。
    fn from_signal(signal: &AgentTurnSignal) -> Self {
        Self {
            agent_id: signal.agent_id.clone(),
            turn_id: signal.turn_id.clone(),
            kind: signal.kind,
        }
    }
}

/// 协调器所有可持久化状态和少量驻留唤醒句柄。
#[derive(Clone, Debug)]
struct CoordinatorState {
    /// 最近已提交的全局事件序号。
    last_event_sequence: u64,
    /// 根身份的持久命名空间。
    root_identity_namespace: AgentId,
    /// 下一棵根树应使用的持久单调序号。
    next_root_sequence: u64,
    /// 本 Coordinator 当前已预约的子 Agent Turn 槽位数。
    global_in_use: usize,
    /// 按根 Agent 标识索引的根树状态。
    roots: HashMap<AgentId, RootEntry>,
    /// 按 Agent 标识索引的驻留 Agent 状态。
    agents: HashMap<AgentId, AgentEntry>,
    /// 全局入队顺序中尚未预约容量的 Turn。
    pending_turns: VecDeque<QueuedTurn>,
    /// 按 Turn 标识索引的活跃 Turn。
    active_turns: HashMap<TurnId, ActiveTurn>,
    /// 按可信 Agent、Turn 与 ToolCall 身份索引的协作工具幂等记录。
    collaboration_invocations: HashMap<CollaborationInvocationKey, CollaborationInvocationRecord>,
    /// 按外部根 Turn 标识索引且直到对应根树关闭才释放的幂等绑定。
    root_turn_bindings: HashMap<TurnId, RootTurnBinding>,
    /// 尚未由执行端口确认的 durable StartTurn outbox。
    start_outbox: HashMap<TurnId, AgentTurnLaunch>,
    /// 尚未由执行端口确认、按 Agent、Turn 与信号类型独立合并的 SignalTurn outbox。
    signal_outbox: HashMap<AgentTurnSignalKey, AgentTurnSignal>,
    /// 尚未由执行端口确认的 durable QuiesceTree outbox。
    quiesce_outbox: HashMap<AgentId, QuiesceAgentTree>,
    /// 尚未由系统层确认的 durable CloseTree outbox。
    close_outbox: HashMap<AgentId, CloseAgentTree>,
    /// Store 连续两次无法确认同一批次时冻结后续领域操作的原因。
    store_recovery_required: Option<String>,
    /// 进程内单调转换计数；不持久化，只用于提交泵判定批次前提是否仍成立。
    transition_count: u64,
}

/// 每棵根树在执行端口边界上的串行化栅栏状态。
#[derive(Debug, Default)]
struct RootExecutionFence {
    /// 根树进入关闭阶段后阻止任何更晚的 StartTurn 或 SignalTurn 副作用。
    closing: bool,
}

/// 领域事件提交后才能执行的非权威动作。
#[derive(Clone, Debug)]
enum PostCommitAction {
    /// 将已预约槽位的 Turn 交给执行端口。
    StartTurn(Box<AgentTurnLaunch>),
    /// 在下一安全边界唤醒当前 Turn。
    SignalTurn(AgentTurnSignal),
    /// 取消一个独立 Turn。
    CancelTurn(TurnCancellation),
    /// 向 WaitAgent 等待者广播新活动版本。
    NotifyWaiters {
        /// 可向多个等待者广播的 watch 发送端。
        sender: watch::Sender<u64>,
        /// 已提交的最新活动版本。
        version: u64,
    },
    /// 要求执行端停止整棵树，并等待明确静止确认后再释放容量。
    QuiesceTree(QuiesceAgentTree),
    /// 交给系统层停止全树进程和清理 Worktree。
    CloseTree(CloseAgentTree),
}

/// 一次候选状态转换的返回值、事件和提交后动作。
struct Transition<T> {
    /// 领域操作的返回值。
    output: T,
    /// 必须先原子追加的权威事件。
    events: Vec<CollaborationEvent>,
    /// 状态提交后才能执行的非权威动作。
    actions: Vec<PostCommitAction>,
}

/// 一个领域事件共享的来源、直接 Turn 与根 Turn 关联。
#[derive(Clone, Debug)]
struct EventLink {
    /// 触发事件的来源 Agent。
    source_agent_id: AgentId,
    /// 事件直接关联的 Turn。
    turn_id: Option<TurnId>,
    /// 直接父 Turn。
    parent_turn_id: Option<TurnId>,
    /// 跨 Agent 关联的根 Turn。
    root_turn_id: Option<TurnId>,
}

/// 尚未分配目标 mailbox 序号的消息入队草稿。
#[derive(Clone, Debug)]
struct MailboxDraft {
    /// 发送消息的 Agent。
    source_agent_id: AgentId,
    /// 发送方在同一根树内的稳定路径。
    source_agent_path: AgentPath,
    /// 发送方当前 Turn 的有效 Plan 守卫。
    source_plan_guard: PlanGuard,
    /// 接收消息的 Agent。
    target_agent_id: AgentId,
    /// 全局唯一消息标识。
    message_id: MailboxMessageId,
    /// 空闲目标的唤醒语义。
    delivery: MailboxDelivery,
    /// 消息业务类型。
    kind: MailboxMessageKind,
    /// 完整消息文本。
    content: String,
    /// 产生消息的来源 Turn。
    related_turn_id: Option<TurnId>,
    /// 直接父 Turn。
    parent_turn_id: Option<TurnId>,
    /// 根 Turn。
    root_turn_id: Option<TurnId>,
    /// TriggerTurn 首次为空闲目标创建的 Turn。
    initial_triggered_turn_id: Option<TurnId>,
    /// TriggerTurn 当前归属的待执行或活跃 Turn。
    claimed_turn_id: Option<TurnId>,
}

/// 尚未分配目标 mailbox 序号的子 Agent 完成通知草稿。
struct CompletionDraft<'a> {
    /// 已收敛子 Agent 的不可变定义。
    source_definition: &'a AgentDefinition,
    /// 直接父 Agent 的目标 mailbox。
    target_agent_id: &'a AgentId,
    /// 需要转换为有界通知的完整终态。
    outcome: &'a AgentTurnOutcome,
    /// 已收敛且永不复用的子 Turn 标识。
    related_turn_id: &'a TurnId,
    /// 子 Turn 的直接父 Turn。
    parent_turn_id: Option<TurnId>,
    /// 跨父子 Agent 关联的根 Turn。
    root_turn_id: TurnId,
}

/// 线程安全的 Collaboration v2 协调器。
///
/// 所有领域转换先在候选快照上完成，事件原子追加成功后才替换驻留状态，
/// 因此容量预约、mailbox 消费与终态释放不会产生部分提交。
#[derive(Clone)]
pub struct CollaborationCoordinator {
    inner: Arc<CollaborationCoordinatorInner>,
}

/// Coordinator 的共享内核，使全局 limiter 只需保留弱引用即可事件驱动唤醒。
struct CollaborationCoordinatorInner {
    /// 进程内区分 Coordinator 的非持久标识。
    coordinator_id: u64,
    /// 可与其他 Coordinator 共享的全局子 Turn limiter。
    global_turn_limiter: Arc<CollaborationGlobalTurnLimiter>,
    /// 事件追加与冷恢复端口。
    store: Arc<dyn CollaborationStore>,
    /// Agent Loop 启动、唤醒与全树清理端口。
    execution: Arc<dyn AgentExecutionPort>,
    /// 可替换的标识生成端口。
    ids: Arc<dyn CollaborationIdGenerator>,
    /// 使全局与根级槽位预约成为一次原子操作的共享状态。
    state: Mutex<CoordinatorState>,
    /// 按根 Agent 标识保存 StartTurn 与 CloseTree 共用的执行线性化栅栏。
    execution_fences: Mutex<HashMap<AgentId, Arc<Mutex<RootExecutionFence>>>>,
    /// 串行根身份分配，使持久单调 counter 与对应执行栅栏在注册期间保持一致。
    root_registration: Mutex<()>,
    /// 事件批次落盘的 FIFO 提交泵；首个批次到达时懒启动。
    commit_dispatch: Mutex<CommitDispatch>,
}

/// 状态锁外串行执行磁盘提交的协调器提交泵。
#[derive(Default)]
struct CommitDispatch {
    sender: Option<mpsc::SyncSender<CommitJob>>,
    worker: Option<thread::JoinHandle<()>>,
    /// 当前队尾批次独占的回滚快照槽；新批次入队时掏空前一个，使任一时刻
    /// 只有队尾批次保有回滚能力，非队尾批次失败一律保守冻结。
    last_rollback: Option<Arc<Mutex<Option<CoordinatorState>>>>,
}

/// 一次待落盘的批次提交任务及其应答通道。
struct CommitJob {
    /// 是否为冷恢复收敛批次。
    recovery: bool,
    /// 事件批次与完整 checkpoint 的原子提交载荷。
    commit: CollaborationTransitionCommit,
    /// 本批次应用后的转换计数；提交前校验内存态前提仍然成立。
    applied_count: u64,
    /// 队尾批次的回滚快照；被后继掏空后本批次失败只能冻结。
    rollback: Arc<Mutex<Option<CoordinatorState>>>,
    /// 调用方等待落盘确认的应答端。
    reply: mpsc::Sender<CommitReply>,
}

/// 提交泵对单个批次的最终处置结果。
enum CommitReply {
    /// 批次已经确认落盘。
    Committed,
    /// 批次确认未提交且内存态已整体回滚，调用方可安全重试整个操作。
    RolledBack(CollaborationError),
    /// 批次结果未知或前提失效，协调器已冻结。
    Frozen(CollaborationError),
}

impl CommitDispatch {
    /// 确保提交泵线程存在并返回入队端；线程只在首个批次时创建一次。
    fn ensure_worker(
        &mut self,
        inner: &Arc<CollaborationCoordinatorInner>,
    ) -> Result<mpsc::SyncSender<CommitJob>, CollaborationError> {
        if self.sender.is_none() {
            let (sender, receiver) = mpsc::sync_channel(64);
            let worker = thread::Builder::new()
                .name(format!("collab-commit-{}", inner.coordinator_id))
                .spawn({
                    let worker_inner = Arc::downgrade(inner);
                    let store = Arc::clone(&inner.store);
                    move || commit_worker(worker_inner, store, receiver)
                })
                .map_err(|_| CollaborationError::Store {
                    message: "协作提交泵线程启动失败".to_owned(),
                })?;
            self.sender = Some(sender.clone());
            self.worker = Some(worker);
        }
        Ok(self.sender.clone().expect("提交泵入队端在确保后必定存在"))
    }
}

/// 提交泵主循环：按 FIFO 串行落盘。
///
/// - 明确未提交（Absent 且水位未动）且本批次仍持有回滚快照时，把内存态
///   整体回滚并应答 `RolledBack`，调用方可以安全重试同一操作。
/// - 其余任何失败都冻结协调器；后续排队批次的前提计数不再匹配，同样
///   以可重试错误应答并跳过落盘。
fn commit_worker(
    inner: Weak<CollaborationCoordinatorInner>,
    store: Arc<dyn CollaborationStore>,
    receiver: mpsc::Receiver<CommitJob>,
) {
    while let Ok(job) = receiver.recv() {
        // 提交前确认内存态前提：协调器存活、未冻结且转换计数仍停在本批次应用点。
        let prerequisite = Weak::upgrade(&inner).and_then(|inner| {
            let state = inner.state.lock().ok()?;
            if state.store_recovery_required.is_some() {
                None
            } else {
                Some(state.transition_count)
            }
        });
        let result = match prerequisite {
            // 后继批次可以在本批次等待落盘期间入队并应用，因此只要内存计数
            // 不低于本批次应用点，前提就仍然成立；只有前序回滚才会把计数
            // 拉回应用点之前。
            Some(count) if count >= job.applied_count => {
                append_events_to_store(store.as_ref(), &job.commit, job.recovery)
            }
            Some(_) => Err(CommitFailure::Prerequisite(
                "前序批次已回滚，本转换前提失效，需要重试".to_owned(),
            )),
            None => Err(CommitFailure::Prerequisite(
                "协调器已冻结，本批次跳过落盘".to_owned(),
            )),
        };
        let reply = match result {
            Ok(()) => {
                release_rollback(&job.rollback);
                CommitReply::Committed
            }
            Err(CommitFailure::Retryable { error }) => {
                let snapshot = job
                    .rollback
                    .lock()
                    .ok()
                    .and_then(|mut slot| slot.take())
                    .and_then(|snapshot| {
                        // 只有内存态仍停在本批次应用点时回滚才是安全的。
                        let inner = Weak::upgrade(&inner)?;
                        let mut state = inner.state.lock().ok()?;
                        (state.transition_count == job.applied_count)
                            .then(|| std::mem::replace(&mut *state, snapshot))
                    });
                match snapshot {
                    Some(_discarded) => CommitReply::RolledBack(error),
                    None => {
                        let message = error.to_string();
                        CommitReply::Frozen(freeze(&inner, message))
                    }
                }
            }
            Err(CommitFailure::Prerequisite(message)) => {
                release_rollback(&job.rollback);
                CommitReply::RolledBack(CollaborationError::Store { message })
            }
            Err(CommitFailure::Fatal {
                error,
                recovery_message,
            }) => {
                release_rollback(&job.rollback);
                CommitReply::Frozen(freeze(&inner, format!("{error}；{recovery_message}")))
            }
        };
        let _ = job.reply.send(reply);
    }
}

/// 批次结局已定后丢弃回滚快照。
///
/// 快照持有应用前旧状态里 `GlobalTurnPermit` 的最后引用；不及时释放会把
/// 全局 limiter 的容量归还推迟到协调器销毁，饿死后续子 Turn 调度。
fn release_rollback(rollback: &Arc<Mutex<Option<CoordinatorState>>>) {
    if let Ok(mut slot) = rollback.lock() {
        *slot = None;
    }
}

/// 写入冻结标志并返回对应的协调器错误。
fn freeze(inner: &Weak<CollaborationCoordinatorInner>, message: String) -> CollaborationError {
    if let Some(inner) = inner.upgrade()
        && let Ok(mut state) = inner.state.lock()
        && state.store_recovery_required.is_none()
    {
        state.store_recovery_required = Some(message.clone());
    }
    CollaborationError::StoreRecoveryRequired { message }
}

static NEXT_COORDINATOR_ID: AtomicU64 = AtomicU64::new(1);

impl Drop for CollaborationCoordinatorInner {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            state.active_turns.clear();
        }
        if let Ok(mut limiter) = self.global_turn_limiter.state.lock() {
            limiter.coordinators.remove(&self.coordinator_id);
            limiter.waiting.remove(&self.coordinator_id);
            limiter
                .waiters
                .retain(|candidate| *candidate != self.coordinator_id);
        }
        let _ = self.global_turn_limiter.drive();
        // 关闭提交泵并等在途批次落盘完毕，保证协调器销毁后磁盘水位可信。
        if let Ok(dispatch) = self.commit_dispatch.get_mut() {
            dispatch.sender = None;
            if let Some(worker) = dispatch.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

impl CollaborationGlobalTurnLimiter {
    /// 在全局 FIFO 中最多保留一个 Coordinator 唤醒项。
    fn enqueue(&self, coordinator_id: u64) -> Result<(), CollaborationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        if state.coordinators.contains_key(&coordinator_id) && state.waiting.insert(coordinator_id)
        {
            state.waiters.push_back(coordinator_id);
        }
        Ok(())
    }

    /// 为冷恢复中正在收敛的子 Turn 恢复已占用槽位。
    fn force_acquire(self: &Arc<Self>) -> Result<Arc<GlobalTurnPermit>, CollaborationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        state.in_use = state
            .in_use
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        Ok(Arc::new(GlobalTurnPermit {
            limiter: Arc::downgrade(self),
        }))
    }

    /// 一次性发布共享全局上限和多个 Coordinator 的指定根上限。
    ///
    /// 返回 `Err` 时尚未修改任何限额；返回 `Ok` 时全部限额已经在同一个临界区
    /// 完成发布。发布后的等待 Turn 调度不属于可回滚配置事务，其错误单独进入报告。
    pub fn update_limits_atomically(
        self: &Arc<Self>,
        roots: &[(&CollaborationCoordinator, &AgentId)],
        global_turn_limit: usize,
        per_root_turn_limit: usize,
    ) -> Result<CollaborationLimitUpdateReport, CollaborationError> {
        if global_turn_limit == 0 || per_root_turn_limit == 0 {
            return Err(CollaborationError::InvalidTurnLimit);
        }

        // 同一 Coordinator 只获取一次状态锁；按进程内单调标识排序，避免多个
        // 原子更新调用以不同次序获取 Coordinator 锁。
        let mut grouped = HashMap::<u64, (Arc<CollaborationCoordinatorInner>, Vec<AgentId>)>::new();
        for (coordinator, root_agent_id) in roots {
            if !Arc::ptr_eq(&coordinator.inner.global_turn_limiter, self) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "原子限额更新包含其他全局 limiter 的 Coordinator".to_owned(),
                });
            }
            let entry = grouped
                .entry(coordinator.inner.coordinator_id)
                .or_insert_with(|| (Arc::clone(&coordinator.inner), Vec::new()));
            if entry.1.contains(root_agent_id) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "原子限额更新包含重复根 Agent".to_owned(),
                });
            }
            entry.1.push((*root_agent_id).clone());
        }
        let mut grouped = grouped.into_values().collect::<Vec<_>>();
        grouped.sort_by_key(|(inner, _roots)| inner.coordinator_id);
        for (_inner, roots) in &mut grouped {
            roots.sort();
        }

        let mut coordinator_states = Vec::with_capacity(grouped.len());
        for (inner, roots) in &grouped {
            let state = inner
                .state
                .lock()
                .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
            if let Some(message) = &state.store_recovery_required {
                return Err(CollaborationError::StoreRecoveryRequired {
                    message: message.clone(),
                });
            }
            if let Some(root_agent_id) = roots
                .iter()
                .find(|root_agent_id| !state.roots.contains_key(*root_agent_id))
            {
                return Err(CollaborationError::AgentNotFound {
                    agent_id: root_agent_id.clone(),
                });
            }
            coordinator_states.push(state);
        }
        // 既有路径只会在持有 Coordinator 状态锁时短暂读取全局状态；全局驱动
        // 在进入 Coordinator 前会释放全局锁，因此这里以全局锁收尾不会反向死锁。
        let mut global_state = self
            .state
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        for (state, (_inner, roots)) in coordinator_states.iter_mut().zip(&grouped) {
            for root_agent_id in roots {
                state
                    .roots
                    .get_mut(root_agent_id)
                    .expect("根 Agent 已在同一状态锁下完成预检")
                    .turn_limit = per_root_turn_limit;
            }
        }
        global_state.limit = global_turn_limit;
        drop(global_state);
        drop(coordinator_states);

        // 所有受影响 Coordinator 必须先完成入队，再由一次全局驱动统一收集结果。
        // 否则第一个 Coordinator 发起的 drive 可能同时消费其他 Coordinator 的等待项，
        // 而逐个 request_global_dispatch 只会返回调用方自己的错误，导致其余错误丢失。
        let mut dispatch_errors = Vec::new();
        for (inner, _roots) in &grouped {
            let coordinator = CollaborationCoordinator {
                inner: Arc::clone(inner),
            };
            match coordinator.has_schedulable_child_turn() {
                Ok(true) => {
                    if let Err(error) = self.enqueue(inner.coordinator_id) {
                        let failure = CollaborationLimitDispatchError {
                            coordinator_id: Some(inner.coordinator_id),
                            error,
                        };
                        tracing::warn!(
                            coordinator_id = inner.coordinator_id,
                            error = %redacted_dispatch_error(&failure.error),
                            "限额提交后无法登记 Coordinator 派发"
                        );
                        dispatch_errors.push(failure);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    let failure = CollaborationLimitDispatchError {
                        coordinator_id: Some(inner.coordinator_id),
                        error,
                    };
                    tracing::warn!(
                        coordinator_id = inner.coordinator_id,
                        error = %redacted_dispatch_error(&failure.error),
                        "限额提交后无法检查 Coordinator 待派发 Turn"
                    );
                    dispatch_errors.push(failure);
                }
            }
        }
        let dispatch_in_progress = match self.drive() {
            Ok(mut report) => {
                dispatch_errors.append(&mut report.errors);
                report.dispatch_in_progress
            }
            Err(error) => {
                tracing::warn!(
                    error = %redacted_dispatch_error(&error),
                    "限额提交后的全局派发失败"
                );
                dispatch_errors.push(CollaborationLimitDispatchError {
                    coordinator_id: None,
                    error,
                });
                false
            }
        };
        Ok(CollaborationLimitUpdateReport {
            dispatch_errors,
            dispatch_in_progress,
        })
    }

    /// 动态调整全局子 Turn 上限；降低时不取消已运行 Turn。
    pub fn update_limit(
        self: &Arc<Self>,
        limit: usize,
    ) -> Result<CollaborationLimitUpdateReport, CollaborationError> {
        self.update_limits_atomically(&[], limit, 1)
    }

    /// 以 Coordinator 为轮转单位消费全局 FIFO，每次只向一个等待者发放一个槽位。
    fn drive(self: &Arc<Self>) -> Result<GlobalDispatchReport, CollaborationError> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
            if state.dispatching {
                return Ok(GlobalDispatchReport {
                    dispatch_in_progress: true,
                    ..GlobalDispatchReport::default()
                });
            }
            state.dispatching = true;
        }
        // drive_inner 链路一旦 panic 展开，dispatching 若不复位会让全局子
        // Agent 调度永久饿死（Turn 停留 WaitingCapacity）；RAII 守卫保证
        // 展开/提前返回路径同样复位。手工复位路径保留，语义不变。
        let _dispatching_guard = GlobalDispatchingGuard {
            limiter: Arc::clone(self),
        };

        let mut combined = GlobalDispatchReport::default();
        loop {
            let pass = self.drive_inner();
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(poisoned) => {
                    poisoned.into_inner().dispatching = false;
                    tracing::warn!("全局派发结束时 limiter 状态锁已中毒");
                    return Err(CollaborationError::StatePoisoned);
                }
            };
            let (mut report, deferred_waiters) = match pass {
                Ok(pass) => pass,
                Err(error) => {
                    state.dispatching = false;
                    tracing::warn!(
                        error = %redacted_dispatch_error(&error),
                        "全局派发轮次失败"
                    );
                    return Err(error);
                }
            };
            combined.errors.append(&mut report.errors);
            let should_continue = state.in_use < state.limit && !state.waiters.is_empty();
            for coordinator_id in deferred_waiters {
                if state.coordinators.contains_key(&coordinator_id)
                    && state.waiting.insert(coordinator_id)
                {
                    state.waiters.push_back(coordinator_id);
                }
            }
            if should_continue {
                drop(state);
                continue;
            }
            state.dispatching = false;
            return Ok(combined);
        }
    }

    /// 执行一次非重入派发；失败的 Coordinator 留在队列中等待后续事件重试。
    fn drive_inner(
        self: &Arc<Self>,
    ) -> Result<(GlobalDispatchReport, Vec<u64>), CollaborationError> {
        let mut report = GlobalDispatchReport::default();
        let mut deferred_waiters = Vec::new();
        loop {
            let next = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
                if state.in_use >= state.limit {
                    None
                } else {
                    let mut next = None;
                    while let Some(coordinator_id) = state.waiters.pop_front() {
                        state.waiting.remove(&coordinator_id);
                        if let Some(coordinator) = state
                            .coordinators
                            .get(&coordinator_id)
                            .and_then(Weak::upgrade)
                        {
                            state.in_use = state
                                .in_use
                                .checked_add(1)
                                .ok_or(CollaborationError::SequenceExhausted)?;
                            next = Some((coordinator_id, coordinator));
                            break;
                        }
                        state.coordinators.remove(&coordinator_id);
                    }
                    next
                }
            };
            let Some((coordinator_id, inner)) = next else {
                break;
            };
            let permit = Arc::new(GlobalTurnPermit {
                limiter: Arc::downgrade(self),
            });
            let coordinator = CollaborationCoordinator { inner };
            let outcome = coordinator.start_one_reserved_child(permit);
            if let Some(error) = outcome.error {
                let failure = CollaborationLimitDispatchError {
                    coordinator_id: Some(coordinator_id),
                    error,
                };
                tracing::warn!(
                    coordinator_id,
                    error = %redacted_dispatch_error(&failure.error),
                    "Coordinator 子 Turn 派发未收敛"
                );
                report.errors.push(failure);
            }
            match coordinator.has_schedulable_child_turn() {
                Ok(true) if outcome.started => {
                    if let Err(error) = self.enqueue(coordinator_id) {
                        let failure = CollaborationLimitDispatchError {
                            coordinator_id: Some(coordinator_id),
                            error,
                        };
                        tracing::warn!(
                            coordinator_id,
                            error = %redacted_dispatch_error(&failure.error),
                            "Coordinator 后续等待 Turn 重新入队失败"
                        );
                        report.errors.push(failure);
                    }
                }
                Ok(true) => deferred_waiters.push(coordinator_id),
                Ok(false) => {}
                Err(error) => {
                    let failure = CollaborationLimitDispatchError {
                        coordinator_id: Some(coordinator_id),
                        error,
                    };
                    tracing::warn!(
                        coordinator_id,
                        error = %redacted_dispatch_error(&failure.error),
                        "Coordinator 派发后状态检查失败"
                    );
                    report.errors.push(failure);
                }
            }
        }
        Ok((report, deferred_waiters))
    }
}

/// 一次全局驱动中按 Coordinator 归属收集的非 limiter 错误。
#[derive(Default)]
struct GlobalDispatchReport {
    /// 某个 Coordinator 的持久化或执行错误不得污染其他 Session 的调用结果。
    errors: Vec<CollaborationLimitDispatchError>,
    /// `true` 表示当前调用遇到已有 driver，未同步等待其结果。
    dispatch_in_progress: bool,
}

impl GlobalDispatchReport {
    /// 取出指定 Coordinator 在本轮驱动中的首个错误。
    fn error_for(&self, coordinator_id: u64) -> Option<CollaborationError> {
        self.errors
            .iter()
            .find(|failure| failure.coordinator_id == Some(coordinator_id))
            .map(|failure| failure.error.clone())
    }
}

/// 全局 limiter 驱动一个候选子 Turn 后的结果。
struct ReservedChildStartOutcome {
    /// 是否已经持久提交过该子 Turn 的运行态。
    started: bool,
    /// 可选的持久或执行错误。
    error: Option<CollaborationError>,
}

/// 提交泵内单批次落盘失败分类。
enum CommitFailure {
    /// Store 明确未提交且水位未动：队尾批次回滚内存态后调用方可安全重试。
    Retryable { error: CollaborationError },
    /// 水位偏离、冲突或结果未知：必须冻结协调器等待重建。
    Fatal {
        error: CollaborationError,
        recovery_message: String,
    },
    /// 前序批次已回滚或协调器已冻结，本批次跳过落盘直接按可重试错误应答。
    Prerequisite(String),
}

/// 按稳定批次标识把事件与 checkpoint 原子提交到 Store，并对不确定结果原样重放。
///
/// 本函数只在提交泵线程内串行执行；`Absent` 且水位未动的明确未提交结果归为
/// 可回滚失败，其余任何失败都要求冻结。
fn append_events_to_store(
    store: &dyn CollaborationStore,
    commit: &CollaborationTransitionCommit,
    recovery: bool,
) -> Result<(), CommitFailure> {
    let batch_id = commit.batch.batch_id.clone();
    let expected_sequence = commit.batch.expected_sequence;
    let committed_sequence = commit
        .batch
        .events
        .last()
        .map_or(expected_sequence, |event| event.sequence);
    enum AttemptOutcome {
        Committed,
        Failed(CommitFailure),
        Indeterminate(CollaborationPortError),
    }
    let attempt = |store: &dyn CollaborationStore,
                   first_indeterminate: Option<&CollaborationPortError>|
     -> AttemptOutcome {
        let prefix =
            first_indeterminate.map(|error| format!("首次结果不确定（{}），", error.message()));
        let result = if recovery {
            store.commit_recovery_transition(commit)
        } else {
            store.commit_transition(commit)
        };
        let fatal = |error: CollaborationError| -> CommitFailure {
            let mut recovery_message = error.to_string();
            if let Some(prefix) = &prefix {
                recovery_message = format!("{prefix}{recovery_message}");
            }
            CommitFailure::Fatal {
                error,
                recovery_message,
            }
        };
        match result {
            CollaborationAppendResult::Appended => AttemptOutcome::Committed,
            CollaborationAppendResult::AlreadyCommitted { current_sequence }
                if current_sequence == committed_sequence =>
            {
                AttemptOutcome::Committed
            }
            CollaborationAppendResult::AlreadyCommitted { current_sequence } => {
                AttemptOutcome::Failed(fatal(CollaborationError::StoreRecoveryRequired {
                    message: format!(
                        "事件批次 {batch_id} 已提交，但 Store 当前水位 {current_sequence} 与批次末序号 {committed_sequence} 不一致"
                    ),
                }))
            }
            CollaborationAppendResult::Absent { current_sequence }
                if current_sequence == expected_sequence =>
            {
                AttemptOutcome::Failed(CommitFailure::Retryable {
                    error: CollaborationError::Store {
                        message: format!(
                            "事件批次 {batch_id} 未提交，Store 水位仍为 {current_sequence}"
                        ),
                    },
                })
            }
            CollaborationAppendResult::Absent { current_sequence } => {
                AttemptOutcome::Failed(fatal(CollaborationError::StoreRecoveryRequired {
                    message: format!(
                        "事件批次 {batch_id} 未提交但 Store 水位 {current_sequence} 已偏离期望 {expected_sequence}"
                    ),
                }))
            }
            CollaborationAppendResult::Conflict { actual_sequence } => {
                AttemptOutcome::Failed(fatal(CollaborationError::StoreRecoveryRequired {
                    message: format!(
                        "事件批次 {batch_id} 与 Store 实际水位 {actual_sequence} 冲突，期望水位为 {expected_sequence}"
                    ),
                }))
            }
            CollaborationAppendResult::Indeterminate { error } => {
                AttemptOutcome::Indeterminate(error)
            }
        }
    };
    let first_error = match attempt(store, None) {
        AttemptOutcome::Committed => return Ok(()),
        AttemptOutcome::Failed(failure) => return Err(failure),
        AttemptOutcome::Indeterminate(error) => error,
    };
    match attempt(store, Some(&first_error)) {
        AttemptOutcome::Committed => Ok(()),
        AttemptOutcome::Failed(failure) => Err(failure),
        AttemptOutcome::Indeterminate(error) => {
            let message = format!(
                "事件批次 {batch_id} 连续两次无法确认提交状态（{}；{}）",
                first_error.message(),
                error.message()
            );
            Err(CommitFailure::Fatal {
                error: CollaborationError::StoreRecoveryRequired {
                    message: message.clone(),
                },
                recovery_message: message,
            })
        }
    }
}

impl CollaborationCoordinator {
    /// 返回用于同一进程内关联共享 limiter 诊断的稳定 Coordinator 标识。
    pub fn coordinator_id(&self) -> u64 {
        self.inner.coordinator_id
    }

    /// 从已校验容量和端口创建一个空协调器。
    pub fn new(
        limits: CollaborationLimits,
        store: Arc<dyn CollaborationStore>,
        execution: Arc<dyn AgentExecutionPort>,
        ids: Arc<dyn CollaborationIdGenerator>,
    ) -> Self {
        let global_turn_limiter = Arc::new(
            CollaborationGlobalTurnLimiter::new(limits.global_turn_limit)
                .expect("已校验 CollaborationLimits 必须有效"),
        );
        Self::new_with_global_turn_limiter(global_turn_limiter, store, execution, ids)
    }

    /// 使用进程级共享 limiter 创建 Coordinator，用于跨 Session 统一限制子 Agent。
    pub fn new_with_global_turn_limiter(
        global_turn_limiter: Arc<CollaborationGlobalTurnLimiter>,
        store: Arc<dyn CollaborationStore>,
        execution: Arc<dyn AgentExecutionPort>,
        ids: Arc<dyn CollaborationIdGenerator>,
    ) -> Self {
        let root_identity_namespace = ids.next_agent_id();
        let coordinator_id = NEXT_COORDINATOR_ID.fetch_add(1, Ordering::Relaxed);
        let inner = Arc::new(CollaborationCoordinatorInner {
            coordinator_id,
            global_turn_limiter: Arc::clone(&global_turn_limiter),
            store,
            execution,
            ids,
            state: Mutex::new(CoordinatorState {
                last_event_sequence: 0,
                root_identity_namespace,
                next_root_sequence: 1,
                global_in_use: 0,
                roots: HashMap::new(),
                agents: HashMap::new(),
                pending_turns: VecDeque::new(),
                active_turns: HashMap::new(),
                collaboration_invocations: HashMap::new(),
                root_turn_bindings: HashMap::new(),
                start_outbox: HashMap::new(),
                signal_outbox: HashMap::new(),
                quiesce_outbox: HashMap::new(),
                close_outbox: HashMap::new(),
                store_recovery_required: None,
                transition_count: 0,
            }),
            execution_fences: Mutex::new(HashMap::new()),
            root_registration: Mutex::new(()),
            commit_dispatch: Mutex::new(CommitDispatch::default()),
        });
        if let Ok(mut limiter) = global_turn_limiter.state.lock() {
            limiter
                .coordinators
                .insert(coordinator_id, Arc::downgrade(&inner));
        }
        Self { inner }
    }

    /// 在空协调器中原子恢复全局水位、根身份命名空间和全部未移除根树。
    pub fn restore_coordinator(
        &self,
        recovered: RecoveredCoordinator,
    ) -> Result<Vec<AgentHandle>, CollaborationError> {
        self.restore_coordinator_with_authoritative_outcomes(recovered, &HashMap::new())
    }

    /// 恢复协调器；Open/Running Turn 接受 Runtime 权威终态，已提交取消和 Closing 树不允许被其覆盖。
    pub fn restore_coordinator_with_authoritative_outcomes(
        &self,
        mut recovered: RecoveredCoordinator,
        authoritative_outcomes: &HashMap<TurnId, AgentTurnOutcome>,
    ) -> Result<Vec<AgentHandle>, CollaborationError> {
        if recovered.roots.len() > MAX_ROOT_TREES
            || recovered.invocations.len() > MAX_COLLABORATION_INVOCATIONS_PER_COORDINATOR
            || recovered.root_turn_bindings.len() > MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR
            || recovered.next_root_sequence == 0
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复根树或协作调用记录超过限制，或根身份 counter 无效".to_owned(),
            });
        }
        normalize_recovered_root_order(&mut recovered.roots);
        normalize_recovered_invocation_order(&mut recovered.invocations);
        recovered
            .root_turn_bindings
            .sort_by(|left, right| left.turn_id.cmp(&right.turn_id));
        let expected_sequence = recovered.last_event_sequence;
        let root_identity_namespace = recovered.root_identity_namespace.clone();
        let next_root_sequence = recovered.next_root_sequence;
        let recovered_trees = recovered.roots;
        let recovered_invocations = recovered.invocations;
        let recovered_root_turn_bindings = recovered.root_turn_bindings;
        let mut handles = Vec::with_capacity(recovered_trees.len());
        let declared_root_ids = recovered_trees
            .iter()
            .map(|tree| tree.root_agent_id.clone())
            .collect::<HashSet<_>>();
        if declared_root_ids.len() != recovered_trees.len() {
            return Err(CollaborationError::InvalidRecovery {
                message: "多根恢复包含重复根 Agent".to_owned(),
            });
        }
        let mut external_root_turn_roots = HashMap::new();
        for binding in &recovered_root_turn_bindings {
            if !declared_root_ids.contains(&binding.root_agent_id)
                || declared_root_ids.iter().any(|root_agent_id| {
                    turn_sequence_for_root(root_agent_id, &binding.turn_id).is_some()
                })
                || external_root_turn_roots
                    .insert(binding.turn_id.clone(), binding.root_agent_id.clone())
                    .is_some()
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "外部根 Turn 幂等绑定重复、占用内部命名空间或指向未知根树".to_owned(),
                });
            }
        }
        let mut root_ids = HashSet::new();
        let mut root_session_ids = HashSet::new();
        let mut agent_ids = HashSet::new();
        let mut session_ids = HashSet::new();
        let mut worktree_leases = HashSet::new();
        let mut mailbox_message_ids = HashSet::new();
        for recovered in &recovered_trees {
            if !root_ids.insert(recovered.root_agent_id.clone())
                || !root_session_ids.insert(recovered.root_session_id.clone())
                || !root_agent_id_belongs_to_namespace(
                    &root_identity_namespace,
                    next_root_sequence,
                    &recovered.root_agent_id,
                ) && recovered.root_agent_id.as_str() != "root"
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "多根恢复包含重复、越界或不受支持的固定根身份".to_owned(),
                });
            }
            let root_definition = validate_restorable_tree(recovered, &external_root_turn_roots)?;
            for definition in &recovered.known_agents {
                if !agent_ids.insert(definition.agent_id.clone())
                    || !session_ids.insert(definition.session_id.clone())
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "多根恢复包含重复 Agent 或 Session 标识".to_owned(),
                    });
                }
                if let Some(worktree_lease) = &definition.profile.worktree_lease {
                    if !worktree_leases.insert(worktree_lease.clone()) {
                        return Err(CollaborationError::InvalidRecovery {
                            message: "多根恢复重复绑定同一 Worktree lease".to_owned(),
                        });
                    }
                }
            }
            for agent in &recovered.agents {
                for mailbox in &agent.mailbox {
                    if !mailbox_message_ids.insert(mailbox.message.message_id.clone()) {
                        return Err(CollaborationError::InvalidRecovery {
                            message: "多根恢复包含重复 mailbox 消息标识".to_owned(),
                        });
                    }
                }
            }
            handles.push(AgentHandle {
                agent_id: root_definition.agent_id.clone(),
                session_id: root_definition.session_id.clone(),
                path: root_definition.path,
            });
        }
        let mut state = self.lock_state()?;
        if state.last_event_sequence != 0
            || state.global_in_use != 0
            || !state.roots.is_empty()
            || !state.agents.is_empty()
            || !state.pending_turns.is_empty()
            || !state.active_turns.is_empty()
            || !state.collaboration_invocations.is_empty()
            || !state.root_turn_bindings.is_empty()
            || !state.start_outbox.is_empty()
            || !state.signal_outbox.is_empty()
            || !state.quiesce_outbox.is_empty()
            || !state.close_outbox.is_empty()
            || state.store_recovery_required.is_some()
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "完整根树只能原子恢复到空协调器".to_owned(),
            });
        }
        self.verify_store_sequence(&mut state, expected_sequence, "协调器恢复")?;

        let mut candidate = state.clone();
        candidate.last_event_sequence = expected_sequence;
        candidate.root_identity_namespace = root_identity_namespace;
        candidate.next_root_sequence = next_root_sequence;
        for outcome in authoritative_outcomes.values() {
            validate_turn_outcome(outcome)?;
        }
        let mut resolved_turns = Vec::new();
        let mut consumed_authoritative_turns = HashSet::new();
        for recovered in &recovered_trees {
            let mailbox_count = recovered
                .agents
                .iter()
                .map(|agent| agent.mailbox.len())
                .sum();
            let mailbox_bytes = recovered
                .agents
                .iter()
                .flat_map(|agent| &agent.mailbox)
                .map(|entry| entry.message.content.len())
                .sum();
            let completion_count = recovered
                .agents
                .iter()
                .flat_map(|agent| &agent.mailbox)
                .filter(|entry| {
                    matches!(
                        &entry.message.kind,
                        MailboxMessageKind::ChildTurnFinished { .. }
                    )
                })
                .count();
            let completion_bytes = recovered
                .agents
                .iter()
                .flat_map(|agent| &agent.mailbox)
                .filter(|entry| {
                    matches!(
                        &entry.message.kind,
                        MailboxMessageKind::ChildTurnFinished { .. }
                    )
                })
                .map(|entry| entry.message.content.len())
                .sum();
            let known_agents = recovered
                .known_agents
                .iter()
                .map(|definition| (definition.agent_id.clone(), definition.clone()))
                .collect::<HashMap<_, _>>();
            candidate.roots.insert(
                recovered.root_agent_id.clone(),
                RootEntry {
                    root_agent_id: recovered.root_agent_id.clone(),
                    root_session_id: recovered.root_session_id.clone(),
                    turn_limit: recovered.per_root_turn_limit,
                    in_use: 0,
                    lifecycle: recovered.lifecycle,
                    suspended: false,
                    mailbox_count,
                    mailbox_bytes,
                    completion_count,
                    completion_bytes,
                    evicted_agent_checkpoints: HashMap::new(),
                    next_turn_sequence: recovered.next_turn_sequence,
                    next_checkpoint_revision: recovered.next_checkpoint_revision,
                    known_agents,
                },
            );
            for agent in &recovered.agents {
                let current_turn_id = recovered_current_turn_id(&agent.status).cloned();
                if let Some(turn_id) = current_turn_id {
                    let source_agent_id = agent
                        .current_source_agent_id
                        .clone()
                        .expect("live checkpoint 来源 Agent 已校验");
                    let cause = agent
                        .current_turn_cause
                        .clone()
                        .expect("live checkpoint 当前 Turn 原因已校验");
                    let root_turn_id = agent
                        .current_root_turn_id
                        .clone()
                        .expect("live checkpoint 根 Turn 已校验");
                    let plan_guard = agent
                        .current_plan_guard
                        .expect("live checkpoint Plan 守卫已校验");
                    if recovered.lifecycle == RecoveredRootLifecycle::Open {
                        let authoritative_outcome = authoritative_outcomes.get(&turn_id);
                        if authoritative_outcome.is_some() {
                            consumed_authoritative_turns.insert(turn_id.clone());
                        }
                        let outcome =
                            if matches!(&agent.status, CollaborationAgentStatus::Cancelling { .. })
                            {
                                AgentTurnOutcome::Interrupted
                            } else {
                                authoritative_outcome
                                    .cloned()
                                    .unwrap_or(AgentTurnOutcome::Interrupted)
                            };
                        resolved_turns.push(RecoveredTurnResolution {
                            definition: agent.definition.clone(),
                            previous_status: agent.status.clone(),
                            turn: TurnRecord {
                                turn_id,
                                cause,
                                prompt: agent.current_turn_prompt.clone(),
                                parent_turn_id: agent.current_parent_turn_id.clone(),
                                root_turn_id,
                                outcome,
                            },
                            source_agent_id,
                        });
                    } else if recovered.lifecycle == RecoveredRootLifecycle::Closing {
                        let cancellation = TurnCancellation::new();
                        cancellation.cancel();
                        let is_child = agent.definition.depth == AgentDepth::CHILD;
                        let global_permit = if is_child {
                            Some(self.inner.global_turn_limiter.force_acquire()?)
                        } else {
                            None
                        };
                        candidate.active_turns.insert(
                            turn_id.clone(),
                            ActiveTurn {
                                agent_id: agent.definition.agent_id.clone(),
                                source_agent_id,
                                root_agent_id: recovered.root_agent_id.clone(),
                                turn_id,
                                parent_turn_id: agent.current_parent_turn_id.clone(),
                                root_turn_id,
                                cause,
                                prompt: agent.current_turn_prompt.clone(),
                                plan_guard,
                                cancellation,
                                cancelled_by_root_turn: false,
                                global_permit,
                            },
                        );
                        if is_child {
                            candidate.global_in_use = candidate
                                .global_in_use
                                .checked_add(1)
                                .ok_or(CollaborationError::SequenceExhausted)?;
                            let root = candidate
                                .roots
                                .get_mut(&recovered.root_agent_id)
                                .expect("恢复 Closing 根树已创建");
                            root.in_use = root
                                .in_use
                                .checked_add(1)
                                .ok_or(CollaborationError::SequenceExhausted)?;
                        }
                    }
                }
                candidate.agents.insert(
                    agent.definition.agent_id.clone(),
                    agent_entry_from_recovered(agent),
                );
            }
            match recovered.lifecycle {
                RecoveredRootLifecycle::Open => {}
                RecoveredRootLifecycle::Closing => {
                    let quiesce = quiesce_request_from_definitions(
                        &recovered.root_agent_id,
                        &recovered.root_session_id,
                        &recovered.known_agents,
                    );
                    candidate
                        .quiesce_outbox
                        .insert(recovered.root_agent_id.clone(), quiesce);
                }
                RecoveredRootLifecycle::CleanupPending => {
                    let close = close_request_from_definitions(
                        &recovered.root_agent_id,
                        &recovered.root_session_id,
                        &recovered.known_agents,
                    );
                    candidate
                        .close_outbox
                        .insert(recovered.root_agent_id.clone(), close);
                }
            }
        }

        candidate.root_turn_bindings = recovered_root_turn_bindings
            .into_iter()
            .map(|binding| {
                (
                    binding.turn_id,
                    RootTurnBinding {
                        root_agent_id: binding.root_agent_id,
                        prompt_digest: binding.prompt_digest,
                        plan_guard: binding.plan_guard,
                    },
                )
            })
            .collect();
        restore_collaboration_invocations(&mut candidate, &recovered_invocations)?;

        if consumed_authoritative_turns.len() != authoritative_outcomes.len() {
            return Err(CollaborationError::InvalidRecovery {
                message: "权威 Runtime 终态包含不属于当前未决 checkpoint 的 Turn".to_owned(),
            });
        }

        if candidate
            .roots
            .values()
            .any(|root| root.in_use > root.turn_limit)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复中的 Closing 根树活跃子 Turn 超过根级容量".to_owned(),
            });
        }

        resolved_turns.sort_by(|left, right| {
            (
                &left.definition.root_agent_id,
                &left.definition.path,
                &left.turn.turn_id,
            )
                .cmp(&(
                    &right.definition.root_agent_id,
                    &right.definition.path,
                    &right.turn.turn_id,
                ))
        });
        let mut events = Vec::new();
        let mut actions = Vec::new();
        for resolution in resolved_turns {
            let agent_id = resolution.definition.agent_id.clone();
            let turn_id = resolution.turn.turn_id.clone();
            let agent = candidate
                .agents
                .get_mut(&agent_id)
                .expect("恢复中断 Agent 已驻留");
            if agent.status != resolution.previous_status {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复中断 Agent 的当前状态在候选构建期间发生变化".to_owned(),
                });
            }
            for mailbox in &mut agent.mailbox {
                if mailbox.claimed_turn_id.as_ref() == Some(&turn_id) {
                    mailbox.claimed_turn_id = None;
                }
            }
            let link = EventLink {
                source_agent_id: resolution.source_agent_id,
                turn_id: Some(turn_id.clone()),
                parent_turn_id: resolution.turn.parent_turn_id.clone(),
                root_turn_id: Some(resolution.turn.root_turn_id.clone()),
            };
            push_event(
                &mut candidate,
                &mut events,
                &resolution.definition,
                link.clone(),
                terminal_event_kind(&resolution.turn.outcome),
            )?;
            set_status(
                &mut candidate,
                &agent_id,
                outcome_status(&turn_id, &resolution.turn.outcome),
                link,
                &mut events,
            )?;
            candidate
                .agents
                .get_mut(&agent_id)
                .expect("恢复中断 Agent 已驻留")
                .last_turn = Some(resolution.turn.clone());
            mark_activity(&mut candidate, &agent_id, &mut actions)?;
            if let Some(parent_agent_id) = &resolution.definition.parent_agent_id {
                queue_completion_message(
                    &mut candidate,
                    CompletionDraft {
                        source_definition: &resolution.definition,
                        target_agent_id: parent_agent_id,
                        outcome: &resolution.turn.outcome,
                        related_turn_id: &turn_id,
                        parent_turn_id: resolution.turn.parent_turn_id,
                        root_turn_id: resolution.turn.root_turn_id,
                    },
                    &mut events,
                    &mut actions,
                )?;
            }
        }
        validate_coordinator_quotas(&candidate)?;
        let applied_count = state.transition_count.saturating_add(1);
        let ticket = if events.is_empty() {
            *state = candidate;
            state.transition_count = applied_count;
            None
        } else {
            let commit = self.build_commit(&candidate, expected_sequence, &events)?;
            let rollback = Arc::new(Mutex::new(None));
            let reply =
                self.enqueue_commit_job(commit, true, Arc::clone(&rollback), applied_count)?;
            let previous = std::mem::replace(&mut *state, candidate);
            state.transition_count = applied_count;
            *rollback.lock().expect("回滚槽锁不应中毒") = Some(previous);
            Some(reply)
        };
        drop(state);
        if let Some(reply) = ticket {
            self.await_commit(reply)?;
        }
        let result = self.execute_actions(actions);
        let dispatch = self.request_global_dispatch();
        match (result, dispatch) {
            (Ok(()), Ok(())) => Ok(handles),
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        }
    }

    /// 注册一棵空闲根 Agent 树，但不自动创建用户 Turn。
    pub fn register_root(
        &self,
        request: RootAgentRequest,
    ) -> Result<AgentHandle, CollaborationError> {
        self.register_root_inner(None, request)
    }

    /// 使用应用层唯一固定身份注册根 Agent；该身份不会推进协调器内部根命名序号。
    pub fn register_root_with_id(
        &self,
        root_agent_id: AgentId,
        request: RootAgentRequest,
    ) -> Result<AgentHandle, CollaborationError> {
        if root_agent_id.as_str() != "root" {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "应用层固定根 Agent 标识必须为 root",
            });
        }
        self.register_root_inner(Some(root_agent_id), request)
    }

    /// 在线性化注册门内完成自动或应用固定根身份的唯一注册转换。
    fn register_root_inner(
        &self,
        requested_root_agent_id: Option<AgentId>,
        request: RootAgentRequest,
    ) -> Result<AgentHandle, CollaborationError> {
        if request.per_root_turn_limit == 0 {
            return Err(CollaborationError::InvalidTurnLimit);
        }
        validate_agent_profile(&request.profile)?;
        let _registration = self
            .inner
            .root_registration
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        let (root_agent_id, advance_root_sequence) = {
            if let Some(root_agent_id) = requested_root_agent_id {
                (root_agent_id, false)
            } else {
                let state = self.lock_state()?;
                (prospective_root_agent_id(&state)?, true)
            }
        };
        let fence = self.execution_fence(&root_agent_id)?;
        let fence_state = fence
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        if fence_state.closing {
            return Err(CollaborationError::DuplicateAgent {
                agent_id: root_agent_id,
            });
        }
        let definition = AgentDefinition {
            agent_id: root_agent_id.clone(),
            session_id: request.session_id.clone(),
            root_agent_id: root_agent_id.clone(),
            root_session_id: request.session_id,
            parent_agent_id: None,
            path: AgentPath::root(),
            assignment: None,
            depth: AgentDepth::ROOT,
            context_inheritance: ContextInheritance::None,
            context_snapshot: Vec::new(),
            agent_template: None,
            profile: request.profile,
        };
        let handle = AgentHandle {
            agent_id: root_agent_id.clone(),
            session_id: definition.session_id.clone(),
            path: definition.path.clone(),
        };
        self.apply_transition(|state| {
            if advance_root_sequence {
                let allocated_root_agent_id = allocate_root_agent_id(state)?;
                if allocated_root_agent_id != root_agent_id {
                    return Err(CollaborationError::IdentifierCollision { kind: "Root Agent" });
                }
            }
            ensure_worktree_lease_available(state, &definition.profile)?;
            if state.roots.len() >= MAX_ROOT_TREES {
                return Err(CollaborationError::ResourceLimitExceeded {
                    resource: "协调器根 Agent 树数量",
                    maximum: MAX_ROOT_TREES,
                });
            }
            if state
                .roots
                .values()
                .any(|root| root.known_agents.contains_key(&root_agent_id))
                || state.agents.contains_key(&root_agent_id)
                || state
                    .start_outbox
                    .values()
                    .any(|launch| launch.agent.root_agent_id == root_agent_id)
                || state.quiesce_outbox.contains_key(&root_agent_id)
                || state.close_outbox.contains_key(&root_agent_id)
            {
                return Err(CollaborationError::DuplicateAgent {
                    agent_id: root_agent_id.clone(),
                });
            }
            if state.roots.values().any(|root| {
                root.known_agents
                    .values()
                    .any(|known| known.session_id == definition.root_session_id)
            }) {
                return Err(CollaborationError::IdentifierCollision {
                    kind: "Agent Session",
                });
            }
            let (sender, _receiver) = watch::channel(0);
            let mut known_agents = HashMap::new();
            known_agents.insert(root_agent_id.clone(), definition.clone());
            state.roots.insert(
                root_agent_id.clone(),
                RootEntry {
                    root_agent_id: root_agent_id.clone(),
                    root_session_id: definition.root_session_id.clone(),
                    turn_limit: request.per_root_turn_limit,
                    in_use: 0,
                    lifecycle: RecoveredRootLifecycle::Open,
                    suspended: false,
                    mailbox_count: 0,
                    mailbox_bytes: 0,
                    completion_count: 0,
                    completion_bytes: 0,
                    evicted_agent_checkpoints: HashMap::new(),
                    next_turn_sequence: 1,
                    next_checkpoint_revision: 1,
                    known_agents,
                },
            );
            state.agents.insert(
                root_agent_id.clone(),
                AgentEntry {
                    definition: definition.clone(),
                    status: CollaborationAgentStatus::Idle,
                    mailbox: VecDeque::new(),
                    mailbox_bytes: 0,
                    completion_count: 0,
                    completion_bytes: 0,
                    next_mailbox_sequence: 1,
                    mailbox_claim: None,
                    steers: VecDeque::new(),
                    steer_bytes: 0,
                    next_steer_sequence: 1,
                    steer_claim: None,
                    last_turn: None,
                    activity_version: 0,
                    activity_sender: sender,
                },
            );
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: root_agent_id.clone(),
                    turn_id: None,
                    parent_turn_id: None,
                    root_turn_id: None,
                },
                CollaborationEventKind::AgentSpawned {
                    definition: Box::new(definition.clone()),
                    initial_status: CollaborationAgentStatus::Idle,
                    per_root_turn_limit: Some(request.per_root_turn_limit),
                },
            )?;
            Ok(Transition {
                output: handle.clone(),
                events,
                actions: Vec::new(),
            })
        })
    }

    /// 为空闲根 Agent 创建一个新用户 Turn，并冻结本 Turn 的实际 Plan 守卫。
    pub fn begin_root_turn(
        &self,
        root_agent_id: &AgentId,
        prompt: impl Into<String>,
        plan_guard: PlanGuard,
    ) -> Result<TurnId, CollaborationError> {
        self.begin_root_turn_inner(root_agent_id, None, prompt.into(), plan_guard, false)
    }

    /// 使用 Session 命令层提供的权威 Turn 标识启动根任务，并持久保证相同绑定幂等。
    pub fn begin_root_turn_with_id(
        &self,
        root_agent_id: &AgentId,
        turn_id: TurnId,
        prompt: impl Into<String>,
        plan_guard: PlanGuard,
    ) -> Result<TurnId, CollaborationError> {
        self.begin_root_turn_inner(
            root_agent_id,
            Some(turn_id),
            prompt.into(),
            plan_guard,
            false,
        )
    }

    /// 在 Runtime 已确认 Journal 尚未形成 TurnStarted 时，安全重试同一外部根 Turn。
    ///
    /// 只有冷恢复后仍保留相同中断 Turn 的幂等绑定才会被清除并重新入队；
    /// 已经进入 Journal 的 Turn 仍由普通幂等路径处理，禁止再次采样。
    pub fn retry_unstarted_root_turn_with_id(
        &self,
        root_agent_id: &AgentId,
        turn_id: TurnId,
        prompt: impl Into<String>,
        plan_guard: PlanGuard,
    ) -> Result<TurnId, CollaborationError> {
        self.begin_root_turn_inner(
            root_agent_id,
            Some(turn_id),
            prompt.into(),
            plan_guard,
            true,
        )
    }

    /// 仅在根 Agent 没有未决 Turn 时替换下一轮使用的模型、Plan、目录和工具快照。
    pub fn update_root_profile(
        &self,
        root_agent_id: &AgentId,
        profile: AgentProfile,
    ) -> Result<(), CollaborationError> {
        validate_agent_profile(&profile)?;
        let root_agent_id = root_agent_id.clone();
        self.apply_transition(|state| {
            ensure_tree_open(state, &root_agent_id)?;
            let root_agent = resident_agent(state, &root_agent_id)?;
            if root_agent.definition.depth != AgentDepth::ROOT {
                return Err(CollaborationError::CrossTreeOperation);
            }
            if !root_agent.status.is_idle() {
                return Err(CollaborationError::TargetNotIdle {
                    agent_id: root_agent_id.clone(),
                });
            }
            if let Some(worktree_lease) = profile.worktree_lease.as_ref()
                && state.roots.values().any(|root| {
                    root.known_agents.values().any(|definition| {
                        definition.agent_id != root_agent_id
                            && definition.profile.worktree_lease.as_ref() == Some(worktree_lease)
                    })
                })
            {
                return Err(CollaborationError::InvalidAgentProfile {
                    message: "Worktree lease 已绑定到其他 Agent",
                });
            }
            state
                .agents
                .get_mut(&root_agent_id)
                .expect("根 Agent 在上方已校验")
                .definition
                .profile = profile.clone();
            state
                .roots
                .get_mut(&root_agent_id)
                .expect("根树在上方已校验")
                .known_agents
                .get_mut(&root_agent_id)
                .expect("根树必须保存根 Agent 定义")
                .profile = profile;
            Ok(Transition {
                output: (),
                events: Vec::new(),
                actions: Vec::new(),
            })
        })
    }

    /// 统一执行内部单调 Turn 与外部权威根 Turn 的入队转换。
    fn begin_root_turn_inner(
        &self,
        root_agent_id: &AgentId,
        requested_turn_id: Option<TurnId>,
        prompt: String,
        plan_guard: PlanGuard,
        allow_unstarted_retry: bool,
    ) -> Result<TurnId, CollaborationError> {
        validate_required_text(&prompt, "根 Turn 输入")?;
        let root_agent_id = root_agent_id.clone();
        self.apply_transition(|state| {
            let (agent_depth, agent_plan_guard, agent_status, previous_turn_id) = {
                let agent = resident_agent(state, &root_agent_id)?;
                (
                    agent.definition.depth,
                    agent.definition.profile.plan_guard,
                    agent.status.clone(),
                    agent
                        .last_turn
                        .as_ref()
                        .map(|last_turn| last_turn.turn_id.clone()),
                )
            };
            if agent_depth != AgentDepth::ROOT {
                return Err(CollaborationError::CrossTreeOperation);
            }
            ensure_tree_open(state, &root_agent_id)?;
            let effective_plan_guard =
                if matches!(agent_plan_guard.state(), PlanGuardState::ReadOnly)
                    || matches!(plan_guard.state(), PlanGuardState::ReadOnly)
                {
                    PlanGuard::read_only()
                } else {
                    PlanGuard::inactive()
                };
            let prompt_digest = root_turn_prompt_digest(&prompt);
            if let Some(turn_id) = &requested_turn_id {
                let binding_matches = state
                    .root_turn_bindings
                    .get(turn_id)
                    .map(|binding| {
                        binding.root_agent_id == root_agent_id
                            && binding.prompt_digest == prompt_digest
                            && binding.plan_guard == effective_plan_guard
                    })
                    .unwrap_or(false);
                if binding_matches {
                    if !allow_unstarted_retry {
                        return Ok(Transition {
                            output: turn_id.clone(),
                            events: Vec::new(),
                            actions: Vec::new(),
                        });
                    }
                    let retryable = matches!(
                        &agent_status,
                        CollaborationAgentStatus::Interrupted {
                            turn_id: bound_turn_id,
                        }
                            if bound_turn_id == turn_id
                    ) && previous_turn_id.as_ref() == Some(turn_id)
                        && matches!(
                            state
                                .agents
                                .get(&root_agent_id)
                                .and_then(|agent| agent.last_turn.as_ref())
                                .map(|last_turn| &last_turn.outcome),
                            Some(AgentTurnOutcome::Interrupted)
                        )
                        && !state
                            .pending_turns
                            .iter()
                            .any(|queued| queued.turn_id == *turn_id)
                        && !state.active_turns.contains_key(turn_id);
                    if !retryable {
                        return Err(CollaborationError::IdentifierCollision { kind: "Root Turn" });
                    }
                    state.root_turn_bindings.remove(turn_id);
                } else if state.root_turn_bindings.contains_key(turn_id) {
                    return Err(CollaborationError::IdentifierCollision { kind: "Root Turn" });
                }
            }
            if !agent_status.is_idle() {
                return Err(CollaborationError::TargetNotIdle {
                    agent_id: root_agent_id.clone(),
                });
            }
            let turn_id =
                if let Some(turn_id) = requested_turn_id.clone() {
                    if state.root_turn_bindings.len() >= MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR {
                        return Err(CollaborationError::ResourceLimitExceeded {
                            resource: "协调器外部根 Turn 幂等绑定数量",
                            maximum: MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR,
                        });
                    }
                    if state.roots.keys().any(|root_agent_id| {
                        turn_sequence_for_root(root_agent_id, &turn_id).is_some()
                    }) {
                        return Err(CollaborationError::IdentifierCollision { kind: "Root Turn" });
                    }
                    state.root_turn_bindings.insert(
                        turn_id.clone(),
                        RootTurnBinding {
                            root_agent_id: root_agent_id.clone(),
                            prompt_digest,
                            plan_guard: effective_plan_guard,
                        },
                    );
                    turn_id
                } else {
                    allocate_turn_id(state, &root_agent_id)?
                };
            if let Some(previous_turn_id) = previous_turn_id {
                let agent = state
                    .agents
                    .get_mut(&root_agent_id)
                    .expect("根 Agent 在上方已校验");
                rebind_pending_inputs(agent, &previous_turn_id, &turn_id);
            }
            let queued = QueuedTurn {
                agent_id: root_agent_id.clone(),
                root_agent_id: root_agent_id.clone(),
                turn_id: turn_id.clone(),
                source_agent_id: root_agent_id.clone(),
                parent_turn_id: None,
                root_turn_id: turn_id.clone(),
                cause: AgentTurnCause::RootUser,
                prompt: Some(prompt.clone()),
                plan_guard: effective_plan_guard,
            };
            let mut events = Vec::new();
            let mut actions = Vec::new();
            queue_turn(state, queued, &mut events)?;
            schedule_root_turns(state, &mut events, &mut actions)?;
            Ok(Transition {
                output: turn_id.clone(),
                events,
                actions,
            })
        })
    }

    /// 从正在运行的根 Agent 创建单层子 Agent，并立即返回身份。
    pub fn spawn_agent(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: &ToolCallId,
        request: SpawnAgentRequest,
    ) -> Result<SpawnedAgent, CollaborationError> {
        validate_required_text(&request.initial_task, "子 Agent 初始任务")?;
        validate_agent_assignment(&request.assignment)?;
        validate_context_inheritance(&request.context_inheritance)?;
        validate_context_snapshot(&request.context_inheritance, &request.context_snapshot)?;
        if let Some(template) = &request.agent_template {
            validate_agent_template_snapshot(template)?;
        }
        validate_agent_profile(&request.profile)?;
        let path = AgentPath::root().child(request.task_name.clone())?;
        let source_agent_id = source_agent_id.clone();
        let source_turn_id = source_turn_id.clone();
        let invocation_key = CollaborationInvocationKey {
            source_agent_id: source_agent_id.clone(),
            source_turn_id: source_turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
        };
        let invocation_input = CollaborationInvocationInput::SpawnAgent(Box::new(request.clone()));
        self.apply_transition(|state| {
            if let Some(output) =
                replay_collaboration_invocation(state, &invocation_key, &invocation_input)?
            {
                let CollaborationInvocationOutput::SpawnedAgent(spawned) = output else {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "SpawnAgent 幂等记录保存了不匹配的结果类型".to_owned(),
                    });
                };
                return Ok(Transition {
                    output: spawned,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let source_turn = active_source_turn(state, &source_agent_id, &source_turn_id)?;
            let source = resident_agent(state, &source_agent_id)?;
            if !source.definition.depth.can_spawn_child() {
                return Err(CollaborationError::RecursiveSpawnForbidden {
                    source_agent_id: source_agent_id.clone(),
                });
            }
            ensure_tree_open(state, &source.definition.root_agent_id)?;
            let root_agent_id = source.definition.root_agent_id.clone();
            let root_session_id = source.definition.root_session_id.clone();
            let effective_profile = constrain_child_profile(
                &source.definition.profile,
                source_turn.plan_guard,
                &request.profile,
            );
            ensure_worktree_lease_available(state, &effective_profile)?;
            let root = state.roots.get(&root_agent_id).ok_or_else(|| {
                CollaborationError::AgentNotFound {
                    agent_id: root_agent_id.clone(),
                }
            })?;
            if root.known_agents.values().any(|known| known.path == path) {
                return Err(CollaborationError::DuplicateAgentPath { path: path.clone() });
            }
            if root.known_agents.len() >= MAX_AGENTS_PER_ROOT {
                return Err(CollaborationError::ResourceLimitExceeded {
                    resource: "单棵根树 Agent 数量",
                    maximum: MAX_AGENTS_PER_ROOT,
                });
            }
            let child_agent_id = self.inner.ids.next_agent_id();
            let child_session_id = self.inner.ids.next_session_id();
            if state
                .roots
                .values()
                .any(|known_root| known_root.known_agents.contains_key(&child_agent_id))
                || state.agents.contains_key(&child_agent_id)
            {
                return Err(CollaborationError::DuplicateAgent {
                    agent_id: child_agent_id.clone(),
                });
            }
            if state.roots.values().any(|known_root| {
                known_root
                    .known_agents
                    .values()
                    .any(|known| known.session_id == child_session_id)
            }) {
                return Err(CollaborationError::IdentifierCollision {
                    kind: "Agent Session",
                });
            }
            let definition = AgentDefinition {
                agent_id: child_agent_id.clone(),
                session_id: child_session_id.clone(),
                root_agent_id: root_agent_id.clone(),
                root_session_id,
                parent_agent_id: Some(source_agent_id.clone()),
                path: path.clone(),
                assignment: Some(request.assignment.clone()),
                depth: AgentDepth::CHILD,
                context_inheritance: request.context_inheritance.clone(),
                context_snapshot: request.context_snapshot.clone(),
                agent_template: request.agent_template.clone(),
                profile: effective_profile,
            };
            let invocation_link = EventLink {
                source_agent_id: source_agent_id.clone(),
                turn_id: Some(source_turn.turn_id.clone()),
                parent_turn_id: Some(source_turn.turn_id.clone()),
                root_turn_id: Some(source_turn.root_turn_id.clone()),
            };
            let (sender, _receiver) = watch::channel(0);
            state.agents.insert(
                child_agent_id.clone(),
                AgentEntry {
                    definition: definition.clone(),
                    status: CollaborationAgentStatus::PendingInit,
                    mailbox: VecDeque::new(),
                    mailbox_bytes: 0,
                    completion_count: 0,
                    completion_bytes: 0,
                    next_mailbox_sequence: 1,
                    mailbox_claim: None,
                    steers: VecDeque::new(),
                    steer_bytes: 0,
                    next_steer_sequence: 1,
                    steer_claim: None,
                    last_turn: None,
                    activity_version: 0,
                    activity_sender: sender,
                },
            );
            let root = state
                .roots
                .get_mut(&root_agent_id)
                .expect("根 Agent 在上方已校验");
            root.known_agents
                .insert(child_agent_id.clone(), definition.clone());

            let initial_turn_id = allocate_turn_id(state, &root_agent_id)?;

            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                invocation_link.clone(),
                CollaborationEventKind::AgentSpawned {
                    definition: Box::new(definition.clone()),
                    initial_status: CollaborationAgentStatus::PendingInit,
                    per_root_turn_limit: None,
                },
            )?;
            let queued = QueuedTurn {
                agent_id: child_agent_id.clone(),
                root_agent_id,
                turn_id: initial_turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                parent_turn_id: Some(source_turn.turn_id),
                root_turn_id: source_turn.root_turn_id,
                cause: AgentTurnCause::InitialTask,
                prompt: Some(request.initial_task.clone()),
                plan_guard: definition.profile.plan_guard,
            };
            let mut actions = Vec::new();
            queue_turn(state, queued, &mut events)?;
            schedule_root_turns(state, &mut events, &mut actions)?;
            let output = SpawnedAgent {
                agent: AgentHandle {
                    agent_id: child_agent_id.clone(),
                    session_id: child_session_id.clone(),
                    path: path.clone(),
                },
                initial_turn_id: initial_turn_id.clone(),
            };
            let receipt = record_collaboration_invocation(
                state,
                invocation_key.clone(),
                invocation_input.clone(),
                CollaborationInvocationOutput::SpawnedAgent(output.clone()),
            )?;
            push_event(
                state,
                &mut events,
                &definition,
                invocation_link,
                CollaborationEventKind::CollaborationInvocationCommitted {
                    receipt: Box::new(receipt),
                },
            )?;
            Ok(Transition {
                output,
                events,
                actions,
            })
        })
    }

    /// 只将消息加入目标 mailbox，不唤醒空闲 Agent。
    pub fn send_message(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: &ToolCallId,
        target_agent_id: &AgentId,
        content: impl Into<String>,
    ) -> Result<MailboxMessageId, CollaborationError> {
        self.message_agent(
            source_agent_id,
            source_turn_id,
            tool_call_id,
            target_agent_id,
            content.into(),
            MailboxDelivery::QueueOnly,
        )
        .map(|(message_id, _turn_id)| message_id)
    }

    /// 将消息加入 mailbox，并仅在目标空闲时触发一个新 Turn。
    pub fn followup_agent(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: &ToolCallId,
        target_agent_id: &AgentId,
        content: impl Into<String>,
    ) -> Result<(MailboxMessageId, Option<TurnId>), CollaborationError> {
        self.message_agent(
            source_agent_id,
            source_turn_id,
            tool_call_id,
            target_agent_id,
            content.into(),
            MailboxDelivery::TriggerTurn,
        )
    }

    /// 实现 QueueOnly 和 TriggerTurn 共享的持久 mailbox 入队逻辑。
    fn message_agent(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: &ToolCallId,
        target_agent_id: &AgentId,
        content: String,
        delivery: MailboxDelivery,
    ) -> Result<(MailboxMessageId, Option<TurnId>), CollaborationError> {
        validate_required_text(&content, "Agent mailbox 消息")?;
        let source_agent_id = source_agent_id.clone();
        let source_turn_id = source_turn_id.clone();
        let target_agent_id = target_agent_id.clone();
        let invocation_key = CollaborationInvocationKey {
            source_agent_id: source_agent_id.clone(),
            source_turn_id: source_turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
        };
        let invocation_input = CollaborationInvocationInput::SendMessage {
            target_agent_id: target_agent_id.clone(),
            content: content.clone(),
            delivery,
        };
        self.apply_transition(|state| {
            if let Some(output) =
                replay_collaboration_invocation(state, &invocation_key, &invocation_input)?
            {
                let CollaborationInvocationOutput::Message {
                    message_id,
                    triggered_turn_id,
                } = output
                else {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "SendMessage 幂等记录保存了不匹配的结果类型".to_owned(),
                    });
                };
                return Ok(Transition {
                    output: (message_id, triggered_turn_id),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let source_turn = active_source_turn(state, &source_agent_id, &source_turn_id)?;
            ensure_agent_loaded(
                state,
                self.inner.store.as_ref(),
                &source_agent_id,
                &target_agent_id,
            )?;
            let source = resident_agent(state, &source_agent_id)?;
            let target = resident_agent(state, &target_agent_id)?;
            if source.definition.root_agent_id != target.definition.root_agent_id {
                return Err(CollaborationError::CrossTreeOperation);
            }
            if delivery == MailboxDelivery::TriggerTurn
                && target.definition.depth == AgentDepth::ROOT
            {
                return Err(CollaborationError::CannotFollowupRoot);
            }
            if !target.status.can_receive_messages() {
                return Err(CollaborationError::TargetStopped {
                    agent_id: target_agent_id.clone(),
                });
            }
            ensure_tree_open(state, &target.definition.root_agent_id)?;
            let target_was_idle = target.status.is_idle();
            let target_waiting_turn = match &target.status {
                CollaborationAgentStatus::WaitingCapacity { turn_id } => Some(turn_id.clone()),
                _ => None,
            };
            let target_active_turn = target.status.active_turn_id().cloned();
            let target_root_agent_id = target.definition.root_agent_id.clone();
            let target_definition = target.definition.clone();
            let source_path = source.definition.path.clone();
            let source_plan_guard =
                strictest_plan_guard(source.definition.profile.plan_guard, source_turn.plan_guard);
            let target_current_plan_guard = match &target.status {
                CollaborationAgentStatus::WaitingCapacity { turn_id } => state
                    .pending_turns
                    .iter()
                    .find(|turn| turn.agent_id == target_agent_id && &turn.turn_id == turn_id)
                    .map(|turn| turn.plan_guard),
                CollaborationAgentStatus::Running { turn_id }
                | CollaborationAgentStatus::Cancelling { turn_id } => state
                    .active_turns
                    .get(turn_id)
                    .filter(|turn| turn.agent_id == target_agent_id)
                    .map(|turn| turn.plan_guard),
                _ => None,
            };
            if target_definition.depth == AgentDepth::CHILD
                && matches!(source_plan_guard.state(), PlanGuardState::ReadOnly)
                && target_current_plan_guard
                    .is_some_and(|guard| matches!(guard.state(), PlanGuardState::Inactive))
            {
                return Err(CollaborationError::ReadOnlyMessageToWritableChild);
            }
            let target_plan_guard = target
                .mailbox
                .iter()
                .filter(|entry| matches!(entry.message.kind, MailboxMessageKind::AgentMessage))
                .fold(
                    strictest_plan_guard(target_definition.profile.plan_guard, source_plan_guard),
                    |guard, entry| strictest_plan_guard(guard, entry.message.source_plan_guard),
                );
            let invocation_link = EventLink {
                source_agent_id: source_agent_id.clone(),
                turn_id: Some(source_turn.turn_id.clone()),
                parent_turn_id: Some(source_turn.turn_id.clone()),
                root_turn_id: Some(source_turn.root_turn_id.clone()),
            };
            let candidate_turn_id = if delivery.wakes_idle_agent() && target_was_idle {
                Some(allocate_turn_id(state, &target_root_agent_id)?)
            } else {
                None
            };
            let claimed_turn_id = if delivery.wakes_idle_agent() {
                if target_was_idle {
                    candidate_turn_id.clone()
                } else {
                    target_waiting_turn
                }
            } else {
                None
            };
            let message_id = self.inner.ids.next_message_id();
            if state.collaboration_invocations.values().any(|record| {
                matches!(
                    &record.output,
                    CollaborationInvocationOutput::Message {
                        message_id: known,
                        ..
                    } if known == &message_id
                )
            }) {
                return Err(CollaborationError::IdentifierCollision {
                    kind: "mailbox 消息",
                });
            }
            let mut events = Vec::new();
            let mut actions = Vec::new();
            let message = queue_mailbox_message(
                state,
                MailboxDraft {
                    source_agent_id: source_agent_id.clone(),
                    source_agent_path: source_path,
                    source_plan_guard,
                    target_agent_id: target_agent_id.clone(),
                    message_id: message_id.clone(),
                    delivery,
                    kind: MailboxMessageKind::AgentMessage,
                    content: content.clone(),
                    related_turn_id: Some(source_turn.turn_id.clone()),
                    parent_turn_id: Some(source_turn.turn_id.clone()),
                    root_turn_id: Some(source_turn.root_turn_id.clone()),
                    initial_triggered_turn_id: candidate_turn_id.clone(),
                    claimed_turn_id,
                },
                &mut events,
                &mut actions,
            )?;
            if let Some(active_turn_id) = target_active_turn {
                queue_turn_signal(
                    state,
                    &target_agent_id,
                    &active_turn_id,
                    AgentTurnSignalKind::MailboxAvailable,
                    &mut actions,
                )?;
            }
            let triggered_turn_id = if delivery.wakes_idle_agent() && target_was_idle {
                let turn_id = candidate_turn_id
                    .clone()
                    .expect("TriggerTurn 在进入转换前已生成 Turn 标识");
                let queued = QueuedTurn {
                    agent_id: target_agent_id.clone(),
                    root_agent_id: target_root_agent_id,
                    turn_id: turn_id.clone(),
                    source_agent_id: source_agent_id.clone(),
                    parent_turn_id: Some(source_turn.turn_id.clone()),
                    root_turn_id: source_turn.root_turn_id.clone(),
                    cause: AgentTurnCause::Followup {
                        message_id: message.message_id.clone(),
                    },
                    prompt: None,
                    plan_guard: target_plan_guard,
                };
                queue_turn(state, queued, &mut events)?;
                Some(turn_id)
            } else {
                None
            };
            schedule_root_turns(state, &mut events, &mut actions)?;
            let receipt = record_collaboration_invocation(
                state,
                invocation_key.clone(),
                invocation_input.clone(),
                CollaborationInvocationOutput::Message {
                    message_id: message_id.clone(),
                    triggered_turn_id: triggered_turn_id.clone(),
                },
            )?;
            push_event(
                state,
                &mut events,
                &target_definition,
                invocation_link,
                CollaborationEventKind::CollaborationInvocationCommitted {
                    receipt: Box::new(receipt),
                },
            )?;
            Ok(Transition {
                output: (message_id.clone(), triggered_turn_id),
                events,
                actions,
            })
        })
    }

    /// 等待任意 mailbox 活动、当前 Turn 用户 steer 或硬超时。
    pub async fn wait_agent(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        timeout: Duration,
    ) -> Result<WaitAgentOutcome, CollaborationError> {
        let mut receiver = {
            let state = self.lock_state()?;
            let agent = resident_agent(&state, agent_id)?;
            if agent.status.active_turn_id() != Some(turn_id) {
                return Err(CollaborationError::TurnMismatch {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                });
            }
            agent.activity_sender.subscribe()
        };
        let deadline = Instant::now().checked_add(timeout).ok_or(
            CollaborationError::ResourceLimitExceeded {
                resource: "WaitAgent 超时时间",
                maximum: usize::MAX,
            },
        )?;
        loop {
            let observed_version = *receiver.borrow_and_update();
            {
                let state = self.lock_state()?;
                let Some(agent) = state.agents.get(agent_id) else {
                    return Ok(WaitAgentOutcome::TurnEnded);
                };
                if agent.status.active_turn_id() != Some(turn_id) {
                    return Ok(WaitAgentOutcome::TurnEnded);
                }
                let mailbox_claimed_through = agent
                    .mailbox_claim
                    .as_ref()
                    .map_or(0, |claim| claim.through_sequence);
                let mut mailbox_count = 0usize;
                let mut latest_mailbox = 0u64;
                for entry in agent
                    .mailbox
                    .iter()
                    .filter(|entry| entry.message.sequence > mailbox_claimed_through)
                {
                    mailbox_count = mailbox_count.saturating_add(1);
                    latest_mailbox = latest_mailbox.max(entry.message.sequence);
                }
                if mailbox_count > 0 {
                    return Ok(WaitAgentOutcome::MailboxActivity(MailboxActivitySummary {
                        pending_count: mailbox_count,
                        latest_sequence: latest_mailbox,
                    }));
                }
                let steer_claimed_through = agent
                    .steer_claim
                    .as_ref()
                    .filter(|claim| &claim.turn_id == turn_id)
                    .map_or(0, |claim| claim.through_sequence);
                let mut steer_count = 0usize;
                let mut latest_steer = 0u64;
                for steer in agent.steers.iter().filter(|steer| {
                    &steer.turn_id == turn_id && steer.sequence > steer_claimed_through
                }) {
                    steer_count = steer_count.saturating_add(1);
                    latest_steer = latest_steer.max(steer.sequence);
                }
                if steer_count > 0 {
                    return Ok(WaitAgentOutcome::UserSteer(UserSteerSummary {
                        pending_count: steer_count,
                        latest_sequence: latest_steer,
                    }));
                }
                if agent.activity_version != observed_version {
                    continue;
                }
            }
            match timeout_at(deadline, receiver.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_closed)) => return Ok(WaitAgentOutcome::TurnEnded),
                Err(_elapsed) => return Ok(WaitAgentOutcome::TimedOut),
            }
        }
    }

    /// 取消指定 Agent 的当前 Turn；根 Agent 的用户 Turn 会级联取消同一根 Turn 树。
    pub fn cancel_current_turn(&self, agent_id: &AgentId) -> Result<TurnId, CollaborationError> {
        let agent_id = agent_id.clone();
        self.apply_transition(|state| {
            let status = resident_agent(state, &agent_id)?.status.clone();
            let Some(turn_id) = recovered_current_turn_id(&status).cloned() else {
                return Err(CollaborationError::TargetNotRunning {
                    agent_id: agent_id.clone(),
                });
            };
            let transition = cancel_turn_transition(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: turn_id,
                events: transition.events,
                actions: transition.actions,
            })
        })
    }

    /// 按 Agent 与 Turn 双重身份精确取消；根 Turn 同时覆盖其全部未终止子 Turn。
    pub fn cancel_turn(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
    ) -> Result<TurnCancellationDisposition, CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| cancel_turn_transition(state, &agent_id, &turn_id))
    }

    /// StopAgent 仅中断目标子 Agent 的当前 Turn，保留身份和 mailbox。
    pub fn stop_agent(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: &ToolCallId,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        let source_agent_id = source_agent_id.clone();
        let source_turn_id = source_turn_id.clone();
        let target_agent_id = target_agent_id.clone();
        let invocation_key = CollaborationInvocationKey {
            source_agent_id: source_agent_id.clone(),
            source_turn_id: source_turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
        };
        let invocation_input = CollaborationInvocationInput::StopAgent {
            target_agent_id: target_agent_id.clone(),
        };
        self.apply_transition(|state| {
            if let Some(output) =
                replay_collaboration_invocation(state, &invocation_key, &invocation_input)?
            {
                let CollaborationInvocationOutput::StoppedAgent {
                    target_agent_id: recorded_target_agent_id,
                    stopped_turn_id,
                } = output
                else {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "StopAgent 幂等记录保存了不匹配的结果类型".to_owned(),
                    });
                };
                if recorded_target_agent_id != target_agent_id {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "StopAgent 幂等记录保存了不匹配的目标 Agent".to_owned(),
                    });
                }
                return Ok(Transition {
                    output: stopped_turn_id,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            if source_agent_id == target_agent_id {
                return Err(CollaborationError::CannotStopSelf);
            }
            let source_turn = active_source_turn(state, &source_agent_id, &source_turn_id)?;
            ensure_agent_loaded(
                state,
                self.inner.store.as_ref(),
                &source_agent_id,
                &target_agent_id,
            )?;
            let source = resident_agent(state, &source_agent_id)?;
            let target = resident_agent(state, &target_agent_id)?;
            if source.definition.root_agent_id != target.definition.root_agent_id {
                return Err(CollaborationError::CrossTreeOperation);
            }
            if target.definition.depth == AgentDepth::ROOT {
                return Err(CollaborationError::CannotStopRoot);
            }
            let target_definition = target.definition.clone();
            let target_status = target.status.clone();
            let invocation_link = EventLink {
                source_agent_id: source_agent_id.clone(),
                turn_id: Some(source_turn.turn_id.clone()),
                parent_turn_id: Some(source_turn.turn_id.clone()),
                root_turn_id: Some(source_turn.root_turn_id.clone()),
            };
            let mut events = Vec::new();
            let mut actions = Vec::new();
            let stopped_turn_id = if let CollaborationAgentStatus::WaitingCapacity { turn_id } =
                target_status
            {
                interrupt_waiting_turn(
                    state,
                    &target_agent_id,
                    &turn_id,
                    &source_agent_id,
                    WaitingTurnInterruptionMode::Standalone,
                    &mut events,
                    &mut actions,
                )?;
                turn_id
            } else {
                let Some(turn_id) = target_status.active_turn_id().cloned() else {
                    return Err(CollaborationError::TargetNotRunning {
                        agent_id: target_agent_id.clone(),
                    });
                };
                if !matches!(target_status, CollaborationAgentStatus::Cancelling { .. }) {
                    let active = state.active_turns.get(&turn_id).cloned().ok_or_else(|| {
                        CollaborationError::TurnMismatch {
                            agent_id: target_agent_id.clone(),
                            turn_id: turn_id.clone(),
                        }
                    })?;
                    set_status(
                        state,
                        &target_agent_id,
                        CollaborationAgentStatus::Cancelling {
                            turn_id: turn_id.clone(),
                        },
                        EventLink {
                            source_agent_id: source_agent_id.clone(),
                            turn_id: Some(turn_id.clone()),
                            parent_turn_id: Some(source_turn.turn_id.clone()),
                            root_turn_id: Some(source_turn.root_turn_id.clone()),
                        },
                        &mut events,
                    )?;
                    mark_activity(state, &target_agent_id, &mut actions)?;
                    actions.push(PostCommitAction::CancelTurn(active.cancellation));
                }
                turn_id
            };
            let receipt = record_collaboration_invocation(
                state,
                invocation_key.clone(),
                invocation_input.clone(),
                CollaborationInvocationOutput::StoppedAgent {
                    target_agent_id: target_agent_id.clone(),
                    stopped_turn_id: stopped_turn_id.clone(),
                },
            )?;
            push_event(
                state,
                &mut events,
                &target_definition,
                invocation_link,
                CollaborationEventKind::CollaborationInvocationCommitted {
                    receipt: Box::new(receipt),
                },
            )?;
            Ok(Transition {
                output: stopped_turn_id,
                events,
                actions,
            })
        })
    }

    /// 为失败或中断的目标 Agent 创建一个新 Turn，不重用旧 Turn ID。
    pub fn retry_agent(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        self.retry_agent_inner(source_agent_id, source_turn_id, None, target_agent_id)
    }

    /// 由可信运行中 Turn 恢复同一根树内失败或中断的单层子 Agent。
    ///
    /// 恢复与重试都不会复用旧 Turn；恢复会以目标最近 Turn 的父链和根 Turn
    /// 作为新 Turn 的因果锚点，并把尚未确认的动态输入一并重绑定到新 Turn。
    pub fn resume_agent(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        self.resume_agent_inner(
            source_agent_id,
            Some(source_turn_id),
            None,
            target_agent_id,
            false,
        )
    }

    /// 使用可信 ToolCall 身份恢复目标子 Agent，并跨 Runner 重放首次新 Turn。
    pub fn resume_agent_with_operation(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: &ToolCallId,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        self.resume_agent_inner(
            source_agent_id,
            Some(source_turn_id),
            Some(tool_call_id),
            target_agent_id,
            false,
        )
    }

    /// 由根 Agent 授权恢复同一根 Session 内的单层子 Agent。
    ///
    /// 该入口不要求根 Agent 存在活跃 Turn。它只允许运行时已经授权的根身份
    /// 调用，并使用目标旧 Turn 作为因果锚点，避免 ACP 客户端伪造活跃根 Turn。
    pub fn resume_agent_for_root(
        &self,
        root_agent_id: &AgentId,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        self.resume_agent_inner(root_agent_id, None, None, target_agent_id, true)
    }

    /// 以根 Session 的稳定操作身份恢复子 Agent，并幂等重放首次新 Turn。
    pub fn resume_agent_for_root_with_operation(
        &self,
        root_agent_id: &AgentId,
        operation_id: &ToolCallId,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        self.resume_agent_inner(
            root_agent_id,
            None,
            Some(operation_id),
            target_agent_id,
            true,
        )
    }

    /// 查询根 Session 已提交的恢复收据，不创建新 Turn。
    ///
    /// 即使幂等记录仍在内存中，关闭中的根树也不能借重放继续对外成功；目标身份
    /// 同样必须仍属于该根树。相同 operationId 指向另一目标时保持首次调用冲突语义。
    pub fn replay_root_resume_receipt(
        &self,
        root_agent_id: &AgentId,
        operation_id: &ToolCallId,
        target_agent_id: &AgentId,
    ) -> Result<Option<TurnId>, CollaborationError> {
        let state = self.lock_state()?;
        ensure_tree_open(&state, root_agent_id)?;
        let root_agent = resident_agent(&state, root_agent_id)?;
        if root_agent.definition.depth != AgentDepth::ROOT
            || root_agent.definition.root_agent_id != *root_agent_id
        {
            return Err(CollaborationError::CrossTreeOperation);
        }
        let target_definition = state
            .roots
            .values()
            .find_map(|root| root.known_agents.get(target_agent_id))
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: target_agent_id.clone(),
            })?;
        if target_definition.root_agent_id != root_agent.definition.root_agent_id {
            return Err(CollaborationError::CrossTreeOperation);
        }
        let input = CollaborationInvocationInput::ResumeAgent {
            target_agent_id: target_agent_id.clone(),
        };
        let output = replay_root_resume_invocation(
            &state,
            root_agent_id,
            operation_id,
            target_agent_id,
            &input,
        )?;
        match output {
            Some(CollaborationInvocationOutput::ResumedAgent {
                target_agent_id: recorded_target_agent_id,
                resume_turn_id,
            }) if recorded_target_agent_id == *target_agent_id => Ok(Some(resume_turn_id)),
            Some(_) => Err(CollaborationError::InvalidRecovery {
                message: "ResumeAgent 幂等记录保存了不匹配的结果类型或目标 Agent".to_owned(),
            }),
            None => Ok(None),
        }
    }

    /// 使用可信 ToolCall 身份重试目标 Agent，并跨 Runner 重放返回首次创建的 Turn。
    pub fn retry_agent_with_operation(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: &ToolCallId,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        self.retry_agent_inner(
            source_agent_id,
            source_turn_id,
            Some(tool_call_id),
            target_agent_id,
        )
    }

    /// 统一执行内部测试入口与生产幂等入口的重试转换。
    fn retry_agent_inner(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
        tool_call_id: Option<&ToolCallId>,
        target_agent_id: &AgentId,
    ) -> Result<TurnId, CollaborationError> {
        let source_agent_id = source_agent_id.clone();
        let source_turn_id = source_turn_id.clone();
        let target_agent_id = target_agent_id.clone();
        let invocation_key = tool_call_id.map(|tool_call_id| CollaborationInvocationKey {
            source_agent_id: source_agent_id.clone(),
            source_turn_id: source_turn_id.clone(),
            tool_call_id: tool_call_id.clone(),
        });
        let invocation_input = CollaborationInvocationInput::RetryAgent {
            target_agent_id: target_agent_id.clone(),
        };
        self.apply_transition(|state| {
            if let Some(invocation_key) = invocation_key.as_ref()
                && let Some(output) =
                    replay_collaboration_invocation(state, invocation_key, &invocation_input)?
            {
                let CollaborationInvocationOutput::RetriedAgent { retry_turn_id, .. } = output
                else {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "RetryAgent 幂等记录保存了不匹配的结果类型".to_owned(),
                    });
                };
                return Ok(Transition {
                    output: retry_turn_id,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let source_turn = active_source_turn(state, &source_agent_id, &source_turn_id)?;
            ensure_agent_loaded(
                state,
                self.inner.store.as_ref(),
                &source_agent_id,
                &target_agent_id,
            )?;
            let source = resident_agent(state, &source_agent_id)?;
            let target = resident_agent(state, &target_agent_id)?;
            if source.definition.root_agent_id != target.definition.root_agent_id {
                return Err(CollaborationError::CrossTreeOperation);
            }
            if !matches!(
                target.status,
                CollaborationAgentStatus::Interrupted { .. }
                    | CollaborationAgentStatus::Failed { .. }
            ) {
                return Err(CollaborationError::RetryNotAllowed {
                    agent_id: target_agent_id.clone(),
                });
            }
            let last_turn =
                target
                    .last_turn
                    .clone()
                    .ok_or_else(|| CollaborationError::RetryNotAllowed {
                        agent_id: target_agent_id.clone(),
                    })?;
            let previous_turn_id = last_turn.turn_id.clone();
            let target_root_agent_id = target.definition.root_agent_id.clone();
            let target_plan_guard = effective_child_plan_guard(
                source.definition.profile.plan_guard,
                source_turn.plan_guard,
                target.definition.profile.plan_guard,
            );
            let target_definition = target.definition.clone();
            let invocation_link = EventLink {
                source_agent_id: source_agent_id.clone(),
                turn_id: Some(source_turn.turn_id.clone()),
                parent_turn_id: source_turn.parent_turn_id.clone(),
                root_turn_id: Some(source_turn.root_turn_id.clone()),
            };
            let new_turn_id = allocate_turn_id(state, &target_root_agent_id)?;
            let queued = QueuedTurn {
                agent_id: target_agent_id.clone(),
                root_agent_id: target_root_agent_id,
                turn_id: new_turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                parent_turn_id: Some(source_turn.turn_id.clone()),
                root_turn_id: source_turn.root_turn_id.clone(),
                cause: AgentTurnCause::Retry {
                    previous_turn_id: previous_turn_id.clone(),
                },
                prompt: last_turn.prompt,
                plan_guard: target_plan_guard,
            };
            let target = state
                .agents
                .get_mut(&target_agent_id)
                .expect("重试目标在上方已校验");
            rebind_pending_inputs(target, &previous_turn_id, &new_turn_id);
            let mut events = Vec::new();
            let mut actions = Vec::new();
            queue_turn(state, queued, &mut events)?;
            schedule_root_turns(state, &mut events, &mut actions)?;
            if let Some(invocation_key) = invocation_key.as_ref() {
                let receipt = record_collaboration_invocation(
                    state,
                    invocation_key.clone(),
                    invocation_input.clone(),
                    CollaborationInvocationOutput::RetriedAgent {
                        target_agent_id: target_agent_id.clone(),
                        retry_turn_id: new_turn_id.clone(),
                    },
                )?;
                push_event(
                    state,
                    &mut events,
                    &target_definition,
                    invocation_link,
                    CollaborationEventKind::CollaborationInvocationCommitted {
                        receipt: Box::new(receipt),
                    },
                )?;
            }
            Ok(Transition {
                output: new_turn_id.clone(),
                events,
                actions,
            })
        })
    }

    /// 统一执行可信来源和根 Session 授权的恢复转换。
    fn resume_agent_inner(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: Option<&TurnId>,
        tool_call_id: Option<&ToolCallId>,
        target_agent_id: &AgentId,
        root_authorized: bool,
    ) -> Result<TurnId, CollaborationError> {
        let source_agent_id = source_agent_id.clone();
        let source_turn_id = source_turn_id.cloned();
        let target_agent_id = target_agent_id.clone();
        let invocation_input = CollaborationInvocationInput::ResumeAgent {
            target_agent_id: target_agent_id.clone(),
        };
        self.apply_transition(|state| {
            if root_authorized {
                ensure_tree_open(state, &source_agent_id)?;
            }
            if !root_authorized
                && let (Some(source_turn_id), Some(tool_call_id)) =
                    (source_turn_id.as_ref(), tool_call_id)
            {
                let invocation_key = CollaborationInvocationKey {
                    source_agent_id: source_agent_id.clone(),
                    source_turn_id: source_turn_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                };
                if let Some(output) =
                    replay_collaboration_invocation(state, &invocation_key, &invocation_input)?
                {
                    let CollaborationInvocationOutput::ResumedAgent { resume_turn_id, .. } = output
                    else {
                        return Err(CollaborationError::InvalidRecovery {
                            message: "ResumeAgent 幂等记录保存了不匹配的结果类型".to_owned(),
                        });
                    };
                    return Ok(Transition {
                        output: resume_turn_id,
                        events: Vec::new(),
                        actions: Vec::new(),
                    });
                }
            }

            let source_turn = if root_authorized {
                None
            } else {
                Some(active_source_turn(
                    state,
                    &source_agent_id,
                    source_turn_id.as_ref().expect("非根恢复必须提供来源 Turn"),
                )?)
            };
            ensure_agent_loaded(
                state,
                self.inner.store.as_ref(),
                &source_agent_id,
                &target_agent_id,
            )?;
            let source = resident_agent(state, &source_agent_id)?;
            let target = resident_agent(state, &target_agent_id)?;
            if (root_authorized && source.definition.depth != AgentDepth::ROOT)
                || source.definition.root_agent_id != target.definition.root_agent_id
            {
                return Err(CollaborationError::CrossTreeOperation);
            }
            let last_turn =
                target
                    .last_turn
                    .clone()
                    .ok_or_else(|| CollaborationError::RetryNotAllowed {
                        agent_id: target_agent_id.clone(),
                    })?;
            let previous_turn_id = last_turn.turn_id.clone();
            let target_root_agent_id = target.definition.root_agent_id.clone();
            let target_plan_guard = if let Some(source_turn) = source_turn.as_ref() {
                effective_child_plan_guard(
                    source.definition.profile.plan_guard,
                    source_turn.plan_guard,
                    target.definition.profile.plan_guard,
                )
            } else {
                target.definition.profile.plan_guard
            };
            let target_definition = target.definition.clone();
            let parent_turn_id = last_turn.parent_turn_id.clone();
            let root_turn_id = last_turn.root_turn_id.clone();
            let invocation_key = tool_call_id.map(|tool_call_id| CollaborationInvocationKey {
                source_agent_id: source_agent_id.clone(),
                // 根授权恢复以目标旧 Turn 作为稳定因果锚点；普通模型恢复
                // 仍以真实来源 Turn 作为 ToolCall 幂等身份的一部分。
                source_turn_id: source_turn_id
                    .clone()
                    .unwrap_or_else(|| previous_turn_id.clone()),
                tool_call_id: tool_call_id.clone(),
            });
            if let Some(invocation_key) = invocation_key.as_ref()
                && let Some(output) =
                    replay_collaboration_invocation(state, invocation_key, &invocation_input)?
            {
                let CollaborationInvocationOutput::ResumedAgent { resume_turn_id, .. } = output
                else {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "ResumeAgent 幂等记录保存了不匹配的结果类型".to_owned(),
                    });
                };
                return Ok(Transition {
                    output: resume_turn_id,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            if root_authorized
                && let Some(tool_call_id) = tool_call_id
                && let Some(output) = replay_root_resume_invocation(
                    state,
                    &source_agent_id,
                    tool_call_id,
                    &target_agent_id,
                    &invocation_input,
                )?
            {
                let CollaborationInvocationOutput::ResumedAgent { resume_turn_id, .. } = output
                else {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "ResumeAgent 幂等记录保存了不匹配的结果类型".to_owned(),
                    });
                };
                return Ok(Transition {
                    output: resume_turn_id,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            if target.definition.depth != AgentDepth::CHILD
                || !matches!(
                    target.status,
                    CollaborationAgentStatus::Interrupted { .. }
                        | CollaborationAgentStatus::Failed { .. }
                )
            {
                return Err(CollaborationError::RetryNotAllowed {
                    agent_id: target_agent_id.clone(),
                });
            }
            let new_turn_id = allocate_turn_id(state, &target_root_agent_id)?;
            let queued = QueuedTurn {
                agent_id: target_agent_id.clone(),
                root_agent_id: target_root_agent_id,
                turn_id: new_turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                parent_turn_id,
                root_turn_id,
                cause: AgentTurnCause::Retry {
                    previous_turn_id: previous_turn_id.clone(),
                },
                prompt: last_turn.prompt,
                plan_guard: target_plan_guard,
            };
            let target = state
                .agents
                .get_mut(&target_agent_id)
                .expect("恢复目标在上方已校验");
            rebind_pending_inputs(target, &previous_turn_id, &new_turn_id);
            let invocation_link = EventLink {
                source_agent_id: source_agent_id.clone(),
                turn_id: Some(previous_turn_id.clone()),
                parent_turn_id: queued.parent_turn_id.clone(),
                root_turn_id: Some(queued.root_turn_id.clone()),
            };
            let mut events = Vec::new();
            let mut actions = Vec::new();
            queue_turn(state, queued, &mut events)?;
            schedule_root_turns(state, &mut events, &mut actions)?;
            if let Some(invocation_key) = invocation_key {
                let receipt = record_collaboration_invocation(
                    state,
                    invocation_key,
                    invocation_input.clone(),
                    CollaborationInvocationOutput::ResumedAgent {
                        target_agent_id: target_agent_id.clone(),
                        resume_turn_id: new_turn_id.clone(),
                    },
                )?;
                push_event(
                    state,
                    &mut events,
                    &target_definition,
                    invocation_link,
                    CollaborationEventKind::CollaborationInvocationCommitted {
                        receipt: Box::new(receipt),
                    },
                )?;
            }
            Ok(Transition {
                output: new_turn_id,
                events,
                actions,
            })
        })
    }

    /// 由执行端口回传 Turn 终态，原子释放双层槽位并调度后续队列。
    pub fn complete_turn(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        outcome: AgentTurnOutcome,
    ) -> Result<TurnCompletionDisposition, CollaborationError> {
        validate_turn_outcome(&outcome)?;
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            complete_turn_transition(
                state,
                &agent_id,
                &turn_id,
                outcome.clone(),
                TurnCompletionMode::Normal,
            )
        })
    }

    /// 在动态输入正文已提交但 claim 确认失败时收敛当前 Turn，并保留未确认 claim。
    ///
    /// 该路径不会伪造消费回执，也不会把未确认输入自动转移到新的 Followup Turn；
    /// 冷恢复必须依据 Runtime Journal 的权威回执重新完成确认。
    pub fn complete_turn_with_pending_dynamic_input(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        outcome: AgentTurnOutcome,
    ) -> Result<TurnCompletionDisposition, CollaborationError> {
        validate_turn_outcome(&outcome)?;
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            complete_turn_transition(
                state,
                &agent_id,
                &turn_id,
                outcome.clone(),
                TurnCompletionMode::PendingDynamicInput,
            )
        })
    }

    /// 暂停根 Session 的当前进程执行；保留 Agent 身份、mailbox、steer、回执和幂等水位。
    ///
    /// 该路径只用于应用退出或进程重启，不改变持久化根树的 Open 生命周期，也不发出
    /// QuiesceTree/CloseTree，因此下一次冷启动仍可从同一棵树继续工作。显式关闭 Session
    /// 必须继续使用 [`Self::close_root_session`]，以保持 Worktree 清理语义。
    pub fn suspend_root_session(&self, root_agent_id: &AgentId) -> Result<(), CollaborationError> {
        let root_agent_id = root_agent_id.clone();
        let fence = self.execution_fence(&root_agent_id)?;
        let mut fence_state = fence
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        // 先封锁执行栅栏，再构造候选状态，避免已经排队的 StartTurn/SignalTurn 越过暂停点。
        fence_state.closing = true;
        let result = self.commit_transition(|state| {
            materialize_evicted_agents_for_root(state, self.inner.store.as_ref(), &root_agent_id)?;
            let root = state.roots.get(&root_agent_id).cloned().ok_or_else(|| {
                CollaborationError::AgentNotFound {
                    agent_id: root_agent_id.clone(),
                }
            })?;
            if root.lifecycle != RecoveredRootLifecycle::Open {
                return Err(CollaborationError::TreeClosed {
                    root_agent_id: root_agent_id.clone(),
                });
            }
            if root.suspended {
                return Ok(Transition {
                    output: (),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }

            // 这是驻留状态而非持久字段；checkpoint 仍导出 Open，冷启动会自然解除暂停。
            state
                .roots
                .get_mut(&root_agent_id)
                .expect("暂停根树在上方已校验")
                .suspended = true;

            let mut active_turns = state
                .active_turns
                .values()
                .filter(|active| active.root_agent_id == root_agent_id)
                .cloned()
                .collect::<Vec<_>>();
            active_turns.sort_by(|left, right| {
                suspend_agent_order(state, &left.agent_id, &right.agent_id)
                    .then_with(|| left.turn_id.cmp(&right.turn_id))
            });
            let mut pending_turns = state
                .pending_turns
                .iter()
                .filter(|queued| queued.root_agent_id == root_agent_id)
                .cloned()
                .collect::<Vec<_>>();
            pending_turns.sort_by(|left, right| {
                suspend_agent_order(state, &left.agent_id, &right.agent_id)
                    .then_with(|| left.turn_id.cmp(&right.turn_id))
            });

            let mut events = Vec::new();
            let mut actions = Vec::new();
            for active in active_turns {
                // 未确认的 mailbox 输入必须继续留在 claim 中；TriggerTurn 归属则释放，
                // 使冷恢复后的重试不会把旧 Turn 误当成仍在执行。
                unclaim_mailbox_turn(state, &active.turn_id);
                actions.push(PostCommitAction::CancelTurn(active.cancellation.clone()));
                let transition = complete_turn_transition(
                    state,
                    &active.agent_id,
                    &active.turn_id,
                    AgentTurnOutcome::Interrupted,
                    TurnCompletionMode::Suspend,
                )?;
                events.extend(transition.events);
                actions.extend(transition.actions);
            }
            for queued in pending_turns {
                unclaim_mailbox_turn(state, &queued.turn_id);
                interrupt_waiting_turn_for_suspend(
                    state,
                    &queued,
                    &root_agent_id,
                    &mut events,
                    &mut actions,
                )?;
            }
            Ok(Transition {
                output: (),
                events,
                actions,
            })
        });
        if result.is_err() {
            // Store 不确定或 checkpoint 失败时也必须保持当前实例 fail-closed；即使
            // 持久化水位尚未确认，后续领域命令仍不能继续产生新的副作用。
            if let Ok(mut state) = self.inner.state.lock() {
                if let Some(root) = state.roots.get_mut(&root_agent_id) {
                    root.suspended = true;
                }
            }
        }
        let (_output, actions) = result?;
        drop(fence_state);
        let result = self.execute_actions(actions);
        let dispatch = self.request_global_dispatch();
        match (result, dispatch) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        }
    }

    /// 关闭根 Session；先停止调度并等待执行端静止确认，再释放容量和清理 Worktree。
    pub fn close_root_session(&self, root_agent_id: &AgentId) -> Result<(), CollaborationError> {
        let root_agent_id = root_agent_id.clone();
        let fence = self.execution_fence(&root_agent_id)?;
        let (output, actions) = {
            // 关闭提交与 StartTurn/SignalTurn 的执行副作用必须共享同一根级栅栏；
            // 否则关闭可以先提交 Closing，迟到的后置动作仍会在其后产生副作用。
            let mut fence_state = fence
                .lock()
                .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
            let result = self.commit_transition(|state| {
                materialize_evicted_agents_for_root(
                    state,
                    self.inner.store.as_ref(),
                    &root_agent_id,
                )?;
                let root = state.roots.get(&root_agent_id).cloned().ok_or_else(|| {
                    CollaborationError::AgentNotFound {
                        agent_id: root_agent_id.clone(),
                    }
                })?;
                if root.suspended {
                    return Err(CollaborationError::TreeClosed {
                        root_agent_id: root_agent_id.clone(),
                    });
                }
                if root.lifecycle != RecoveredRootLifecycle::Open {
                    let action = match root.lifecycle {
                        RecoveredRootLifecycle::Open => None,
                        RecoveredRootLifecycle::Closing => state
                            .quiesce_outbox
                            .get(&root_agent_id)
                            .cloned()
                            .map(PostCommitAction::QuiesceTree),
                        RecoveredRootLifecycle::CleanupPending => state
                            .close_outbox
                            .get(&root_agent_id)
                            .cloned()
                            .map(PostCommitAction::CloseTree),
                    };
                    return Ok(Transition {
                        output: (),
                        events: Vec::new(),
                        actions: action.into_iter().collect(),
                    });
                }
                let mut events = Vec::new();
                let mut actions = Vec::new();
                let active_turns = state
                    .active_turns
                    .values()
                    .filter(|active| active.root_agent_id == root_agent_id)
                    .cloned()
                    .collect::<Vec<_>>();
                for active in &active_turns {
                    state.start_outbox.remove(&active.turn_id);
                    remove_turn_signals(state, &active.agent_id, &active.turn_id);
                    actions.push(PostCommitAction::CancelTurn(active.cancellation.clone()));
                }
                state
                    .pending_turns
                    .retain(|turn| turn.root_agent_id != root_agent_id);
                let root_entry = state
                    .roots
                    .get_mut(&root_agent_id)
                    .expect("根 Agent 在上方已校验");
                root_entry.lifecycle = RecoveredRootLifecycle::Closing;

                let resident_agent_ids = state
                    .agents
                    .values()
                    .filter(|agent| agent.definition.root_agent_id == root_agent_id)
                    .map(|agent| agent.definition.agent_id.clone())
                    .collect::<Vec<_>>();
                for agent_id in &resident_agent_ids {
                    let previous = resident_agent(state, agent_id)?.status.clone();
                    let previous_turn_id = recovered_current_turn_id(&previous).cloned();
                    let agent = state.agents.get_mut(agent_id).expect("Agent 在上方已校验");
                    agent.mailbox.clear();
                    agent.mailbox_bytes = 0;
                    agent.completion_count = 0;
                    agent.completion_bytes = 0;
                    agent.mailbox_claim = None;
                    agent.steers.clear();
                    agent.steer_bytes = 0;
                    agent.steer_claim = None;
                    let next_status = if previous.active_turn_id().is_some() {
                        CollaborationAgentStatus::Cancelling {
                            turn_id: previous
                                .active_turn_id()
                                .expect("活跃状态在上方已判断")
                                .clone(),
                        }
                    } else {
                        CollaborationAgentStatus::Stopped
                    };
                    if previous != next_status {
                        set_status(
                            state,
                            agent_id,
                            next_status,
                            EventLink {
                                source_agent_id: root_agent_id.clone(),
                                turn_id: previous_turn_id,
                                parent_turn_id: None,
                                root_turn_id: None,
                            },
                            &mut events,
                        )?;
                        mark_activity(state, agent_id, &mut actions)?;
                    }
                }
                let root_entry = state
                    .roots
                    .get_mut(&root_agent_id)
                    .expect("关闭根树在上方已校验");
                root_entry.mailbox_count = 0;
                root_entry.mailbox_bytes = 0;
                root_entry.completion_count = 0;
                root_entry.completion_bytes = 0;
                let root_definition = resident_agent(state, &root_agent_id)?.definition.clone();
                push_event(
                    state,
                    &mut events,
                    &root_definition,
                    EventLink {
                        source_agent_id: root_agent_id.clone(),
                        turn_id: None,
                        parent_turn_id: None,
                        root_turn_id: None,
                    },
                    CollaborationEventKind::AgentTreeClosing,
                )?;
                let definitions = root.known_agents.values().cloned().collect::<Vec<_>>();
                let quiesce = quiesce_request_from_definitions(
                    &root_agent_id,
                    &root.root_session_id,
                    &definitions,
                );
                state
                    .quiesce_outbox
                    .insert(root_agent_id.clone(), quiesce.clone());
                actions.push(PostCommitAction::QuiesceTree(quiesce));
                Ok(Transition {
                    output: (),
                    events,
                    actions,
                })
            });
            if result.is_ok() {
                // 在释放栅栏前设置关闭标记，覆盖提交完成到 QuiesceTree 后置动作开始之间
                // 的窗口；后续 StartTurn/SignalTurn 即使已排队也只能被丢弃。
                fence_state.closing = true;
            }
            result?
        };
        let result = self.execute_actions(actions);
        let dispatch = self.request_global_dispatch();
        match (result, dispatch) {
            (Ok(()), Ok(())) => Ok(output),
            (Err(error), _) | (Ok(()), Err(error)) => Err(error),
        }
    }

    /// 按稳定 AgentPath 解析当前驻留 Agent 身份。
    pub fn resolve_path(
        &self,
        root_agent_id: &AgentId,
        path: &AgentPath,
    ) -> Result<Option<AgentHandle>, CollaborationError> {
        let state = self.lock_state()?;
        let root =
            state
                .roots
                .get(root_agent_id)
                .ok_or_else(|| CollaborationError::AgentNotFound {
                    agent_id: root_agent_id.clone(),
                })?;
        Ok(root
            .known_agents
            .values()
            .find(|definition| &definition.path == path)
            .map(|definition| AgentHandle {
                agent_id: definition.agent_id.clone(),
                session_id: definition.session_id.clone(),
                path: definition.path.clone(),
            }))
    }

    /// 以可信来源 Agent 的根树为边界解析模型提供的稳定绝对路径。
    ///
    /// 此处不要求来源 Turn 仍活跃，因为领域命令需要先把路径还原为内部身份，才能由
    /// 幂等记录重放已经提交的结果；新副作用仍由各领域入口校验当前来源 Turn。
    pub fn resolve_path_for_source(
        &self,
        source_agent_id: &AgentId,
        path: &AgentPath,
    ) -> Result<Option<AgentHandle>, CollaborationError> {
        let state = self.lock_state()?;
        let root_agent_id = state
            .roots
            .values()
            .find(|root| root.known_agents.contains_key(source_agent_id))
            .map(|root| root.root_agent_id.clone())
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: source_agent_id.clone(),
            })?;
        ensure_tree_open(&state, &root_agent_id)?;
        let root =
            state
                .roots
                .get(&root_agent_id)
                .ok_or_else(|| CollaborationError::AgentNotFound {
                    agent_id: root_agent_id.clone(),
                })?;
        Ok(root
            .known_agents
            .values()
            .find(|definition| &definition.path == path)
            .map(|definition| AgentHandle {
                agent_id: definition.agent_id.clone(),
                session_id: definition.session_id.clone(),
                path: definition.path.clone(),
            }))
    }

    /// 返回指定驻留 Agent 的当前状态快照。
    pub fn agent_status(
        &self,
        agent_id: &AgentId,
    ) -> Result<CollaborationAgentStatus, CollaborationError> {
        let state = self.lock_state()?;
        Ok(resident_agent(&state, agent_id)?.status.clone())
    }

    /// 返回可信来源 Turn 所属根树的全部已知 Agent，并按稳定路径排序。
    pub fn list_agents(
        &self,
        source_agent_id: &AgentId,
        source_turn_id: &TurnId,
    ) -> Result<Vec<CollaborationAgentSummary>, CollaborationError> {
        let mut state = self.lock_state()?;
        let root_agent_id =
            active_source_turn(&state, source_agent_id, source_turn_id)?.root_agent_id;
        ensure_tree_open(&state, &root_agent_id)?;
        materialize_evicted_agents_for_root(&mut state, self.inner.store.as_ref(), &root_agent_id)?;
        let mut agents = state
            .agents
            .values()
            .filter(|entry| entry.definition.root_agent_id == root_agent_id)
            .map(|entry| collaboration_agent_summary(&state, entry))
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.agent.path.cmp(&right.agent.path));
        Ok(agents)
    }

    /// 按根 Agent 标识返回仍开放的根树全部已知 Agent，并按稳定路径排序。
    pub fn list_agents_for_root(
        &self,
        root_agent_id: &AgentId,
    ) -> Result<Vec<CollaborationAgentSummary>, CollaborationError> {
        let mut state = self.lock_state()?;
        ensure_tree_open(&state, root_agent_id)?;
        materialize_evicted_agents_for_root(&mut state, self.inner.store.as_ref(), root_agent_id)?;
        let mut agents = state
            .agents
            .values()
            .filter(|entry| entry.definition.root_agent_id == *root_agent_id)
            .map(|entry| collaboration_agent_summary(&state, entry))
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.agent.path.cmp(&right.agent.path));
        Ok(agents)
    }

    /// 当前 Coordinator 是否存在同时满足根级容量与开放状态的子 Turn。
    fn has_schedulable_child_turn(&self) -> Result<bool, CollaborationError> {
        let state = self.lock_state()?;
        Ok(state.pending_turns.iter().any(|queued| {
            state
                .agents
                .get(&queued.agent_id)
                .is_some_and(|agent| agent.definition.depth == AgentDepth::CHILD)
                && state.roots.get(&queued.root_agent_id).is_some_and(|root| {
                    root.lifecycle == RecoveredRootLifecycle::Open
                        && !root.suspended
                        && root.in_use < root.turn_limit
                })
        }))
    }

    /// 将当前 Coordinator 登记到全局公平队列并事件驱动调度。
    fn request_global_dispatch(&self) -> Result<(), CollaborationError> {
        let schedulable = self.has_schedulable_child_turn()?;
        if schedulable {
            self.inner
                .global_turn_limiter
                .enqueue(self.inner.coordinator_id)?;
        }
        let report = self.inner.global_turn_limiter.drive()?;
        match report.error_for(self.inner.coordinator_id) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// 消费 limiter 已预约的一个槽位，启动本 Coordinator 最早可运行子 Turn。
    fn start_one_reserved_child(&self, permit: Arc<GlobalTurnPermit>) -> ReservedChildStartOutcome {
        let committed = self.commit_transition(|state| {
            let position = state.pending_turns.iter().position(|queued| {
                state
                    .agents
                    .get(&queued.agent_id)
                    .is_some_and(|agent| agent.definition.depth == AgentDepth::CHILD)
                    && state.roots.get(&queued.root_agent_id).is_some_and(|root| {
                        root.lifecycle == RecoveredRootLifecycle::Open
                            && !root.suspended
                            && root.in_use < root.turn_limit
                    })
            });
            let Some(position) = position else {
                return Ok(Transition {
                    output: None,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            };
            let mut events = Vec::new();
            let mut actions = Vec::new();
            let turn_id = schedule_turn_at_position(
                state,
                position,
                Some(Arc::clone(&permit)),
                &mut events,
                &mut actions,
            )?;
            Ok(Transition {
                output: Some(turn_id),
                events,
                actions,
            })
        });
        let (turn_id, actions) = match committed {
            Ok((turn_id, actions)) => (turn_id, actions),
            Err(error) => {
                return ReservedChildStartOutcome {
                    started: false,
                    error: Some(error),
                };
            }
        };
        let Some(_turn_id) = turn_id else {
            return ReservedChildStartOutcome {
                started: false,
                error: None,
            };
        };
        let action_error = self.execute_actions(actions).err();
        ReservedChildStartOutcome {
            started: true,
            error: action_error,
        }
    }

    /// 返回当前全局与每棵根树的容量使用快照。
    pub fn capacity(&self) -> Result<CollaborationCapacity, CollaborationError> {
        let state = self.lock_state()?;
        let mut roots = state
            .roots
            .values()
            .map(|root| (root.root_agent_id.clone(), root.in_use, root.turn_limit))
            .collect::<Vec<_>>();
        roots.sort_by(|left, right| left.0.cmp(&right.0));
        let (global_in_use, global_limit) = self.inner.global_turn_limiter.capacity()?;
        Ok(CollaborationCapacity {
            global_in_use,
            global_limit,
            roots,
        })
    }

    /// 更新共享全局子 Agent Turn 上限；提升后按全局公平队列立即调度。
    pub fn update_global_turn_limit(
        &self,
        global_turn_limit: usize,
    ) -> Result<CollaborationCapacity, CollaborationError> {
        let report = self
            .inner
            .global_turn_limiter
            .update_limit(global_turn_limit)?;
        if !report.dispatch_errors().is_empty() {
            return Err(CollaborationError::CommittedExecutionPending {
                message: report
                    .dispatch_errors()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("；"),
            });
        }
        self.capacity()
    }

    /// 更新指定根树的子 Agent Turn 上限；降低时不取消已运行 Turn。
    pub fn update_root_turn_limit(
        &self,
        root_agent_id: &AgentId,
        per_root_turn_limit: usize,
    ) -> Result<CollaborationCapacity, CollaborationError> {
        if per_root_turn_limit == 0 {
            return Err(CollaborationError::InvalidTurnLimit);
        }
        let root_agent_id = root_agent_id.clone();
        self.apply_transition(|state| {
            let root = state.roots.get_mut(&root_agent_id).ok_or_else(|| {
                CollaborationError::AgentNotFound {
                    agent_id: root_agent_id.clone(),
                }
            })?;
            root.turn_limit = per_root_turn_limit;
            Ok(Transition {
                output: (),
                events: Vec::new(),
                actions: Vec::new(),
            })
        })?;
        self.capacity()
    }

    /// 原子提交候选状态和事件，再执行非权威后置动作。
    fn apply_transition<T>(
        &self,
        planner: impl FnOnce(&mut CoordinatorState) -> Result<Transition<T>, CollaborationError>,
    ) -> Result<T, CollaborationError> {
        let committed = self.commit_transition(planner);
        let result = match committed {
            Ok((output, actions)) => self.execute_actions(actions).map(|()| output),
            Err(error) => Err(error),
        };
        let dispatch = self.request_global_dispatch();
        match (result, dispatch) {
            (Ok(output), Ok(())) => Ok(output),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    /// 在状态锁内构建"事件批次 + 完整 checkpoint"的原子提交载荷。
    fn build_commit(
        &self,
        candidate: &CoordinatorState,
        expected_sequence: u64,
        events: &[CollaborationEvent],
    ) -> Result<CollaborationTransitionCommit, CollaborationError> {
        let batch = collaboration_event_batch(expected_sequence, events);
        let committed_sequence = batch
            .events
            .last()
            .map_or(expected_sequence, |event| event.sequence);
        let checkpoint = checkpoint_coordinator_from_state(candidate, self.inner.store.as_ref())?;
        if checkpoint.last_event_sequence != committed_sequence {
            return Err(CollaborationError::InvalidRecovery {
                message: "候选 checkpoint 水位与事件批次末序号不一致".to_owned(),
            });
        }
        Ok(CollaborationTransitionCommit { batch, checkpoint })
    }

    /// 在状态锁内把批次排入提交泵 FIFO；返回落盘确认的等待票据。
    ///
    /// 入队必须发生在状态锁内，保证落盘顺序与应用顺序一致。调用方随后在
    /// 锁外 `await_commit` 等待落盘确认，因此 `Ok` 仍表示批次已确认落盘。
    fn enqueue_commit_job(
        &self,
        commit: CollaborationTransitionCommit,
        recovery: bool,
        rollback: Arc<Mutex<Option<CoordinatorState>>>,
        applied_count: u64,
    ) -> Result<mpsc::Receiver<CommitReply>, CollaborationError> {
        let (reply_sender, reply) = mpsc::channel();
        let mut dispatch = self
            .inner
            .commit_dispatch
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        // 新批次入队即掏空前一个队尾的回滚快照：从现在起它失败只能冻结。
        if let Some(previous) = dispatch.last_rollback.take() {
            if let Ok(mut slot) = previous.lock() {
                *slot = None;
            }
        }
        dispatch.last_rollback = Some(Arc::clone(&rollback));
        dispatch
            .ensure_worker(&self.inner)?
            .send(CommitJob {
                recovery,
                commit,
                applied_count,
                rollback,
                reply: reply_sender,
            })
            .map_err(|_| CollaborationError::Store {
                message: "协作提交泵已关闭".to_owned(),
            })?;
        Ok(reply)
    }

    /// 在状态锁外等待批次落盘确认。
    fn await_commit(&self, reply: mpsc::Receiver<CommitReply>) -> Result<(), CollaborationError> {
        match reply.recv() {
            Ok(CommitReply::Committed) => Ok(()),
            Ok(CommitReply::RolledBack(error)) | Ok(CommitReply::Frozen(error)) => Err(error),
            Err(_) => {
                let message = "协作提交泵结果未知".to_owned();
                if let Ok(mut state) = self.inner.state.lock()
                    && state.store_recovery_required.is_none()
                {
                    state.store_recovery_required = Some(message.clone());
                }
                Err(CollaborationError::StoreRecoveryRequired { message })
            }
        }
    }

    /// 比较协调器快照与 Store 当前水位，并在偏离时永久冻结当前实例。
    fn verify_store_sequence(
        &self,
        state: &mut CoordinatorState,
        expected_sequence: u64,
        operation: &'static str,
    ) -> Result<(), CollaborationError> {
        let current_sequence =
            self.inner
                .store
                .current_sequence()
                .map_err(|error| CollaborationError::Store {
                    message: format!("{operation}无法读取 Store 当前水位: {}", error.message()),
                })?;
        if current_sequence == expected_sequence {
            return Ok(());
        }
        let message = format!(
            "{operation}所用事件水位 {expected_sequence} 与 Store 当前水位 {current_sequence} 不一致"
        );
        state.store_recovery_required = Some(message.clone());
        Err(CollaborationError::StoreRecoveryRequired { message })
    }

    /// 只提交候选状态与事件，把 durable outbox 动作交给调用方迭代执行。
    ///
    /// 候选状态在锁内乐观应用、批次按 FIFO 入队后立即释放状态锁，磁盘落盘
    /// 由提交泵线程串行完成；本方法在返回前等待落盘确认，因此对调用方而言
    /// `Ok` 仍然表示批次已确认落盘。队尾批次的明确未提交失败会把内存态整体
    /// 回滚并返回可重试错误；其余失败冻结协调器。
    fn commit_transition<T>(
        &self,
        planner: impl FnOnce(&mut CoordinatorState) -> Result<Transition<T>, CollaborationError>,
    ) -> Result<(T, Vec<PostCommitAction>), CollaborationError> {
        let (transition, ticket) = {
            let mut state = self.lock_state()?;
            let expected_sequence = state.last_event_sequence;
            let mut candidate = state.clone();
            let transition = planner(&mut candidate)?;
            validate_coordinator_quotas(&candidate)?;
            let applied_count = state.transition_count.saturating_add(1);
            let ticket = if transition.events.is_empty() {
                *state = candidate;
                state.transition_count = applied_count;
                None
            } else {
                let commit =
                    self.build_commit(&candidate, expected_sequence, &transition.events)?;
                let rollback = Arc::new(Mutex::new(None));
                let reply =
                    self.enqueue_commit_job(commit, false, Arc::clone(&rollback), applied_count)?;
                let previous = std::mem::replace(&mut *state, candidate);
                state.transition_count = applied_count;
                *rollback.lock().expect("回滚槽锁不应中毒") = Some(previous);
                Some(reply)
            };
            (transition, ticket)
        };
        if let Some(reply) = ticket {
            self.await_commit(reply)?;
        }
        Ok((transition.output, transition.actions))
    }

    /// 在根级执行栅栏内迭代后置动作，禁止 StartTurn 与 CloseTree 次序反转。
    fn execute_actions(&self, actions: Vec<PostCommitAction>) -> Result<(), CollaborationError> {
        let mut failures = Vec::new();
        let mut pending = VecDeque::from(actions);
        while let Some(action) = pending.pop_front() {
            match action {
                PostCommitAction::StartTurn(launch) => {
                    let agent_id = launch.agent.agent_id.clone();
                    let root_agent_id = launch.agent.root_agent_id.clone();
                    let turn_id = launch.turn_id.clone();
                    // 准备阶段失败只记录并跳过本动作，不丢弃队列中排在其后的
                    // 其他动作（如 NotifyWaiters / CancelTurn）。
                    let fence = match self.execution_fence(&root_agent_id) {
                        Ok(fence) => fence,
                        Err(error) => {
                            failures.push(format!("Agent Turn 执行栅栏获取失败: {error}"));
                            continue;
                        }
                    };
                    let fence_state = match fence.lock() {
                        Ok(fence_state) => fence_state,
                        Err(_) => {
                            failures.push("Agent Turn 执行栅栏锁已中毒".to_owned());
                            continue;
                        }
                    };
                    if fence_state.closing {
                        continue;
                    }
                    let still_pending = {
                        let state = match self.lock_state() {
                            Ok(state) => state,
                            Err(error) => {
                                failures.push(format!("Agent Turn 状态锁获取失败: {error}"));
                                continue;
                            }
                        };
                        state.start_outbox.contains_key(&turn_id)
                            && state.roots.get(&root_agent_id).is_some_and(|root| {
                                root.lifecycle == RecoveredRootLifecycle::Open && !root.suspended
                            })
                    };
                    if !still_pending {
                        continue;
                    }
                    match self.inner.execution.start_turn(*launch) {
                        AgentTurnStartResult::Accepted | AgentTurnStartResult::AlreadyAccepted => {
                            if let Err(error) = self.commit_transition(|state| {
                                let Some(active) = state.active_turns.get(&turn_id).cloned() else {
                                    return Ok(Transition {
                                        output: (),
                                        events: Vec::new(),
                                        actions: Vec::new(),
                                    });
                                };
                                if state.start_outbox.remove(&turn_id).is_none() {
                                    return Ok(Transition {
                                        output: (),
                                        events: Vec::new(),
                                        actions: Vec::new(),
                                    });
                                }
                                let definition =
                                    resident_agent(state, &active.agent_id)?.definition.clone();
                                let mut events = Vec::new();
                                push_event(
                                    state,
                                    &mut events,
                                    &definition,
                                    EventLink {
                                        source_agent_id: active.source_agent_id,
                                        turn_id: Some(turn_id.clone()),
                                        parent_turn_id: active.parent_turn_id,
                                        root_turn_id: Some(active.root_turn_id),
                                    },
                                    CollaborationEventKind::AgentTurnDispatchAcknowledged,
                                )?;
                                Ok(Transition {
                                    output: (),
                                    events,
                                    actions: Vec::new(),
                                })
                            }) {
                                failures.push(format!("Agent Turn 派发确认失败: {error}"));
                            }
                        }
                        AgentTurnStartResult::RetryableUnknown { error } => {
                            failures.push(format!(
                                "Agent Turn 派发结果不确定，已保留可重试 outbox: {}",
                                error.message()
                            ));
                        }
                        AgentTurnStartResult::PermanentRejectedBeforeSideEffect { error } => {
                            let message = format!("Agent Turn 派发被永久拒绝: {}", error.message());
                            match self.commit_transition(|state| {
                                complete_turn_transition(
                                    state,
                                    &agent_id,
                                    &turn_id,
                                    AgentTurnOutcome::Failed {
                                        message: message.clone(),
                                    },
                                    TurnCompletionMode::DispatchFailed,
                                )
                            }) {
                                Ok((_disposition, actions)) => {
                                    pending.extend(actions);
                                    failures.push(message);
                                }
                                Err(compensation_error) => failures.push(format!(
                                    "{message}；失败 Turn 收敛失败: {compensation_error}"
                                )),
                            }
                        }
                    }
                }
                PostCommitAction::SignalTurn(signal) => {
                    let turn_id = signal.turn_id.clone();
                    let Some(root_agent_id) = ({
                        let state = match self.lock_state() {
                            Ok(state) => state,
                            Err(error) => {
                                failures.push(format!("Agent Turn 信号状态锁获取失败: {error}"));
                                continue;
                            }
                        };
                        state
                            .active_turns
                            .get(&turn_id)
                            .map(|active| active.root_agent_id.clone())
                    }) else {
                        continue;
                    };
                    let fence = match self.execution_fence(&root_agent_id) {
                        Ok(fence) => fence,
                        Err(error) => {
                            failures.push(format!("Agent Turn 信号执行栅栏获取失败: {error}"));
                            continue;
                        }
                    };
                    let fence_state = match fence.lock() {
                        Ok(fence_state) => fence_state,
                        Err(_) => {
                            failures.push("Agent Turn 信号执行栅栏锁已中毒".to_owned());
                            continue;
                        }
                    };
                    if fence_state.closing {
                        continue;
                    }
                    let signal_key = AgentTurnSignalKey::from_signal(&signal);
                    let pending_signal = {
                        let state = match self.lock_state() {
                            Ok(state) => state,
                            Err(error) => {
                                failures.push(format!("Agent Turn 信号状态读取失败: {error}"));
                                continue;
                            }
                        };
                        state.signal_outbox.get(&signal_key).cloned()
                    };
                    let Some(pending_signal) = pending_signal else {
                        continue;
                    };
                    if self
                        .inner
                        .execution
                        .signal_turn(pending_signal.clone())
                        .is_ok()
                    {
                        let mut state = self.lock_state()?;
                        if state.signal_outbox.get(&signal_key).is_some_and(|current| {
                            current.activity_version <= pending_signal.activity_version
                        }) {
                            state.signal_outbox.remove(&signal_key);
                        }
                    }
                }
                PostCommitAction::CancelTurn(cancellation) => cancellation.cancel(),
                PostCommitAction::NotifyWaiters { sender, version } => {
                    sender.send_replace(version);
                }
                PostCommitAction::QuiesceTree(request) => {
                    let root_agent_id = request.root_agent_id.clone();
                    let fence = match self.execution_fence(&root_agent_id) {
                        Ok(fence) => fence,
                        Err(error) => {
                            failures.push(format!("Agent 树静止栅栏获取失败: {error}"));
                            continue;
                        }
                    };
                    let mut fence_state = match fence.lock() {
                        Ok(fence_state) => fence_state,
                        Err(_) => {
                            failures.push("Agent 树静止栅栏锁已中毒".to_owned());
                            continue;
                        }
                    };
                    fence_state.closing = true;
                    let still_pending = match self.lock_state() {
                        Ok(state) => state.quiesce_outbox.contains_key(&root_agent_id),
                        Err(error) => {
                            failures.push(format!("Agent 树静止状态读取失败: {error}"));
                            continue;
                        }
                    };
                    if !still_pending {
                        continue;
                    }
                    match self.inner.execution.quiesce_tree(request) {
                        AgentTreeQuiesceResult::Quiesced
                        | AgentTreeQuiesceResult::AlreadyQuiesced => {
                            match self.commit_transition(|state| {
                                if state.quiesce_outbox.remove(&root_agent_id).is_none() {
                                    return Ok(Transition {
                                        output: (),
                                        events: Vec::new(),
                                        actions: Vec::new(),
                                    });
                                }
                                let root =
                                    state.roots.get(&root_agent_id).cloned().ok_or_else(|| {
                                        CollaborationError::AgentNotFound {
                                            agent_id: root_agent_id.clone(),
                                        }
                                    })?;
                                if root.lifecycle != RecoveredRootLifecycle::Closing {
                                    return Err(CollaborationError::InvalidRecovery {
                                        message: "静止确认对应的根树不在 Closing 阶段".to_owned(),
                                    });
                                }
                                state.global_in_use = state
                                    .global_in_use
                                    .checked_sub(root.in_use)
                                    .ok_or_else(|| CollaborationError::InvalidRecovery {
                                        message: "全树静止时 Coordinator 子 Agent 槽位计数下溢"
                                            .to_owned(),
                                    })?;
                                state.active_turns.retain(|_turn_id, active| {
                                    active.root_agent_id != root_agent_id
                                });
                                state.start_outbox.retain(|_turn_id, launch| {
                                    launch.agent.root_agent_id != root_agent_id
                                });
                                state.signal_outbox.retain(|_key, signal| {
                                    !root.known_agents.contains_key(&signal.agent_id)
                                });
                                let root_entry = state
                                    .roots
                                    .get_mut(&root_agent_id)
                                    .expect("静止根树在上方已校验");
                                root_entry.in_use = 0;
                                root_entry.lifecycle = RecoveredRootLifecycle::CleanupPending;

                                let mut events = Vec::new();
                                let mut actions = Vec::new();
                                let resident_agent_ids = state
                                    .agents
                                    .values()
                                    .filter(|agent| agent.definition.root_agent_id == root_agent_id)
                                    .map(|agent| agent.definition.agent_id.clone())
                                    .collect::<Vec<_>>();
                                for agent_id in &resident_agent_ids {
                                    let previous = resident_agent(state, agent_id)?.status.clone();
                                    if previous != CollaborationAgentStatus::Stopped {
                                        set_status(
                                            state,
                                            agent_id,
                                            CollaborationAgentStatus::Stopped,
                                            EventLink {
                                                source_agent_id: root_agent_id.clone(),
                                                turn_id: previous.active_turn_id().cloned(),
                                                parent_turn_id: None,
                                                root_turn_id: None,
                                            },
                                            &mut events,
                                        )?;
                                        mark_activity(state, agent_id, &mut actions)?;
                                    }
                                }
                                let root_definition =
                                    resident_agent(state, &root_agent_id)?.definition.clone();
                                push_event(
                                    state,
                                    &mut events,
                                    &root_definition,
                                    EventLink {
                                        source_agent_id: root_agent_id.clone(),
                                        turn_id: None,
                                        parent_turn_id: None,
                                        root_turn_id: None,
                                    },
                                    CollaborationEventKind::AgentTreeQuiesced,
                                )?;
                                let definitions =
                                    root.known_agents.values().cloned().collect::<Vec<_>>();
                                let close = close_request_from_definitions(
                                    &root_agent_id,
                                    &root.root_session_id,
                                    &definitions,
                                );
                                state
                                    .close_outbox
                                    .insert(root_agent_id.clone(), close.clone());
                                actions.push(PostCommitAction::CloseTree(close));
                                schedule_root_turns(state, &mut events, &mut actions)?;
                                Ok(Transition {
                                    output: (),
                                    events,
                                    actions,
                                })
                            }) {
                                Ok((_output, actions)) => pending.extend(actions),
                                Err(error) => {
                                    failures.push(format!("Agent 树静止确认失败: {error}"));
                                }
                            }
                        }
                        AgentTreeQuiesceResult::RetryableUnknown { error } => {
                            failures.push(format!(
                                "Agent 树静止结果不确定，已保留可重试 outbox: {}",
                                error.message()
                            ))
                        }
                        AgentTreeQuiesceResult::PermanentRejectedBeforeQuiesce { error } => {
                            failures.push(format!(
                                "Agent 树静止被永久拒绝，容量仍被保留: {}",
                                error.message()
                            ));
                        }
                    }
                }
                PostCommitAction::CloseTree(request) => {
                    let root_agent_id = request.root_agent_id.clone();
                    let fence = match self.execution_fence(&root_agent_id) {
                        Ok(fence) => fence,
                        Err(error) => {
                            failures.push(format!("Agent 树清理栅栏获取失败: {error}"));
                            continue;
                        }
                    };
                    let mut fence_state = match fence.lock() {
                        Ok(fence_state) => fence_state,
                        Err(_) => {
                            failures.push("Agent 树清理栅栏锁已中毒".to_owned());
                            continue;
                        }
                    };
                    fence_state.closing = true;
                    let still_pending = match self.lock_state() {
                        Ok(state) => state.close_outbox.contains_key(&root_agent_id),
                        Err(error) => {
                            failures.push(format!("Agent 树清理状态读取失败: {error}"));
                            continue;
                        }
                    };
                    if !still_pending {
                        continue;
                    }
                    if let Err(error) = self.inner.execution.close_tree(request) {
                        failures.push(format!("Agent 树清理失败: {}", error.message()));
                    } else {
                        match self.commit_transition(|state| {
                            if state.close_outbox.remove(&root_agent_id).is_none() {
                                return Ok(Transition {
                                    output: (),
                                    events: Vec::new(),
                                    actions: Vec::new(),
                                });
                            }
                            let _root = state.roots.get(&root_agent_id).ok_or_else(|| {
                                CollaborationError::AgentNotFound {
                                    agent_id: root_agent_id.clone(),
                                }
                            })?;
                            let definition =
                                resident_agent(state, &root_agent_id)?.definition.clone();
                            let mut events = Vec::new();
                            push_event(
                                state,
                                &mut events,
                                &definition,
                                EventLink {
                                    source_agent_id: root_agent_id.clone(),
                                    turn_id: None,
                                    parent_turn_id: None,
                                    root_turn_id: None,
                                },
                                CollaborationEventKind::AgentTreeCleanupCompleted,
                            )?;
                            unload_closed_root(state, &root_agent_id)?;
                            Ok(Transition {
                                output: (),
                                events,
                                actions: Vec::new(),
                            })
                        }) {
                            Ok((_output, _actions)) => {
                                drop(fence_state);
                                self.inner
                                    .execution_fences
                                    .lock()
                                    .map_err(|_poisoned| CollaborationError::StatePoisoned)?
                                    .remove(&root_agent_id);
                            }
                            Err(error) => {
                                failures.push(format!("Agent 树清理确认失败: {error}"));
                            }
                        }
                    }
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(CollaborationError::CommittedExecutionPending {
                message: failures.join("；"),
            })
        }
    }

    /// 返回指定根树的共享执行栅栏，并为首次出现的根身份创建开放状态。
    fn execution_fence(
        &self,
        root_agent_id: &AgentId,
    ) -> Result<Arc<Mutex<RootExecutionFence>>, CollaborationError> {
        let mut fences = self
            .inner
            .execution_fences
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        Ok(fences
            .entry(root_agent_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(RootExecutionFence::default())))
            .clone())
    }

    /// 获取协调器状态锁，并将中毒转换为领域错误。
    fn lock_state(&self) -> Result<MutexGuard<'_, CoordinatorState>, CollaborationError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
        if let Some(message) = &state.store_recovery_required {
            return Err(CollaborationError::StoreRecoveryRequired {
                message: message.clone(),
            });
        }
        Ok(state)
    }
}

/// 从上一事件水位和完整有序事件内容计算可跨重试复用的稳定批次。
pub(crate) fn collaboration_event_batch(
    expected_sequence: u64,
    events: &[CollaborationEvent],
) -> CollaborationEventBatch {
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.event-batch.v3");
    encoder.u64(expected_sequence);
    encoder.u64(events.len() as u64);
    for event in events {
        encode_collaboration_event(&mut encoder, event);
    }
    let batch_id = CollaborationEventBatchId(encoder.finish_hex());
    CollaborationEventBatch {
        batch_id,
        expected_sequence,
        events: events.to_vec(),
    }
}

/// 从协调器权威队列或活跃账本生成一个 Agent 的当前生命周期摘要。
fn collaboration_agent_summary(
    state: &CoordinatorState,
    entry: &AgentEntry,
) -> CollaborationAgentSummary {
    let current_turn = match &entry.status {
        CollaborationAgentStatus::WaitingCapacity { turn_id } => state
            .pending_turns
            .iter()
            .find(|turn| turn.turn_id == *turn_id && turn.agent_id == entry.definition.agent_id)
            .map(|turn| (turn.prompt.as_deref(), &turn.root_turn_id)),
        CollaborationAgentStatus::Running { turn_id }
        | CollaborationAgentStatus::Cancelling { turn_id } => state
            .active_turns
            .get(turn_id)
            .filter(|turn| turn.agent_id == entry.definition.agent_id)
            .map(|turn| (turn.prompt.as_deref(), &turn.root_turn_id)),
        _ => None,
    };
    let (current_turn_summary, current_root_turn_id) = current_turn
        .map(|(prompt, root_turn_id)| {
            let summary = prompt
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| bounded_utf8_with_suffix(value, MAX_CURRENT_TURN_SUMMARY_BYTES, "…"));
            (summary, Some(root_turn_id.clone()))
        })
        .unwrap_or((None, None));
    CollaborationAgentSummary {
        agent: AgentHandle {
            agent_id: entry.definition.agent_id.clone(),
            session_id: entry.definition.session_id.clone(),
            path: entry.definition.path.clone(),
        },
        parent_agent_id: entry.definition.parent_agent_id.clone(),
        assignment: entry.definition.assignment.clone(),
        status: entry.status.clone(),
        current_turn_summary,
        current_root_turn_id,
    }
}

/// 返回允许执行协作工具的 Running 来源 Turn。
fn active_source_turn(
    state: &CoordinatorState,
    source_agent_id: &AgentId,
    source_turn_id: &TurnId,
) -> Result<ActiveTurn, CollaborationError> {
    let source = resident_agent(state, source_agent_id)?;
    let CollaborationAgentStatus::Running { turn_id } = &source.status else {
        return Err(CollaborationError::SourceAgentNotRunning {
            source_agent_id: source_agent_id.clone(),
        });
    };
    if turn_id != source_turn_id {
        return Err(CollaborationError::TurnMismatch {
            agent_id: source_agent_id.clone(),
            turn_id: source_turn_id.clone(),
        });
    }
    state
        .active_turns
        .get(source_turn_id)
        .cloned()
        .filter(|active| &active.agent_id == source_agent_id)
        .ok_or_else(|| CollaborationError::TurnMismatch {
            agent_id: source_agent_id.clone(),
            turn_id: source_turn_id.clone(),
        })
}

/// 校验某 Turn 正是指定 Agent 的当前活跃 Turn。
fn active_turn_for_agent(
    state: &CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
) -> Result<ActiveTurn, CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    if agent.status.active_turn_id() != Some(turn_id) {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    let active = state.active_turns.get(turn_id).cloned().ok_or_else(|| {
        CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        }
    })?;
    if &active.agent_id != agent_id {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    Ok(active)
}
