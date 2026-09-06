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
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{Instant, timeout_at};
use uuid::Uuid;

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

/// 校验可以稳定嵌入 AgentPath 的子 Agent 名称。
fn validate_child_name(name: &str) -> Result<(), AgentPathError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(AgentPathError::InvalidChildName {
            name: name.to_owned(),
        })
    }
}

/// 校验必须存在且受字节边界保护的协作文本。
fn validate_required_text(value: &str, field: &'static str) -> Result<(), CollaborationError> {
    if value.trim().is_empty() {
        return Err(CollaborationError::EmptyMessage);
    }
    validate_optional_text(value, field)
}

/// 校验允许为空但不能突破内存边界的协作文本。
fn validate_optional_text(value: &str, field: &'static str) -> Result<(), CollaborationError> {
    if value.len() > MAX_COLLABORATION_TEXT_BYTES {
        return Err(CollaborationError::TextTooLarge {
            field,
            maximum_bytes: MAX_COLLABORATION_TEXT_BYTES,
        });
    }
    Ok(())
}

/// 在 UTF-8 字符边界内截断文本，并确保固定后缀也计入最终字节上限。
fn bounded_utf8_with_suffix(value: &str, maximum: usize, suffix: &str) -> String {
    if value.len() <= maximum {
        return value.to_owned();
    }
    if maximum == 0 {
        return String::new();
    }
    let suffix = if suffix.len() <= maximum {
        suffix
    } else {
        let mut suffix_boundary = maximum;
        while suffix_boundary > 0 && !suffix.is_char_boundary(suffix_boundary) {
            suffix_boundary -= 1;
        }
        &suffix[..suffix_boundary]
    };
    let payload_maximum = maximum.saturating_sub(suffix.len());
    let mut payload_boundary = payload_maximum.min(value.len());
    while payload_boundary > 0 && !value.is_char_boundary(payload_boundary) {
        payload_boundary -= 1;
    }
    let mut bounded = String::with_capacity(payload_boundary.saturating_add(suffix.len()));
    bounded.push_str(&value[..payload_boundary]);
    bounded.push_str(suffix);
    bounded
}

/// 校验上下文继承数量，避免恢复或工具输入请求无界历史。
fn validate_context_inheritance(
    inheritance: &ContextInheritance,
) -> Result<(), CollaborationError> {
    if let ContextInheritance::RecentTurns { count } = inheritance {
        if *count == 0 || *count > MAX_RECENT_TURNS {
            return Err(CollaborationError::InvalidContextInheritance);
        }
    }
    Ok(())
}

/// 校验 spawn 时已经冻结的规范 Provider 中立消息，禁止延迟到容量调度时读取父 Transcript。
fn validate_context_snapshot(
    inheritance: &ContextInheritance,
    snapshot: &[String],
) -> Result<(), CollaborationError> {
    if matches!(inheritance, ContextInheritance::None) && !snapshot.is_empty() {
        return Err(CollaborationError::InvalidContextInheritance);
    }
    if snapshot.len() > MAX_CONTEXT_SNAPSHOT_MESSAGES {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "Agent 冻结上下文消息数量",
            maximum: MAX_CONTEXT_SNAPSHOT_MESSAGES,
        });
    }
    let mut total_bytes = 0_usize;
    for encoded in snapshot {
        total_bytes = total_bytes.checked_add(encoded.len()).ok_or(
            CollaborationError::ResourceLimitExceeded {
                resource: "Agent 冻结上下文字节数",
                maximum: MAX_CONTEXT_SNAPSHOT_BYTES,
            },
        )?;
        if encoded.is_empty() || total_bytes > MAX_CONTEXT_SNAPSHOT_BYTES {
            return Err(CollaborationError::ResourceLimitExceeded {
                resource: "Agent 冻结上下文字节数",
                maximum: MAX_CONTEXT_SNAPSHOT_BYTES,
            });
        }
        let message = serde_json::from_str::<Message>(encoded).map_err(|_| {
            CollaborationError::InvalidAgentProfile {
                message: "冻结上下文不是 Provider 中立消息",
            }
        })?;
        message
            .validate()
            .map_err(|_| CollaborationError::InvalidAgentProfile {
                message: "冻结上下文消息无效",
            })?;
        let canonical = serde_json::to_string(&message).map_err(|_| {
            CollaborationError::InvalidAgentProfile {
                message: "冻结上下文消息无法规范编码",
            }
        })?;
        if canonical != *encoded {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "冻结上下文消息不是规范 JSON",
            });
        }
    }
    Ok(())
}

/// 校验 spawn 前冻结的扩展 Agent 模板，不信任冷恢复文件中的路径与资源上限。
fn validate_agent_template_snapshot(
    template: &AgentTemplateSnapshot,
) -> Result<(), CollaborationError> {
    if template.name.trim().is_empty()
        || template.name.trim() != template.name
        || template.name.len() > MAX_PROFILE_FIELD_BYTES
    {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "Agent 模板名称为空、包含首尾空白或过长",
        });
    }
    validate_required_text(&template.system_prompt, "Agent 模板系统提示")?;
    if template
        .max_turns
        .is_some_and(|turns| turns == 0 || turns > MAX_AGENT_TEMPLATE_TURNS)
    {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "Agent 模板轮次上限无效",
        });
    }
    if template.allowed_write_dirs.len() > MAX_AGENT_TEMPLATE_WRITE_DIRS {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "Agent 模板额外写目录数量",
            maximum: MAX_AGENT_TEMPLATE_WRITE_DIRS,
        });
    }
    let mut directories = HashSet::new();
    for directory in &template.allowed_write_dirs {
        let Some(text) = directory.to_str() else {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "Agent 模板额外写目录必须是 UTF-8 相对路径",
            });
        };
        if text.is_empty()
            || text.len() > MAX_PROFILE_FIELD_BYTES
            || text.contains(':')
            || directory.is_absolute()
            || directory
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || !directories.insert(directory)
        {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "Agent 模板额外写目录为空、越界、过长或重复",
            });
        }
    }
    Ok(())
}

/// 校验 Agent 的模型、Plan、目录和冻结工具快照。
fn validate_agent_profile(profile: &AgentProfile) -> Result<(), CollaborationError> {
    if profile.model.trim().is_empty() || profile.model.len() > MAX_PROFILE_FIELD_BYTES {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "模型标识为空或过长",
        });
    }
    if profile
        .reasoning_effort
        .as_ref()
        .is_some_and(|effort| effort.trim().is_empty() || effort.len() > MAX_PROFILE_FIELD_BYTES)
    {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "推理强度为空或过长",
        });
    }
    if !profile.cwd.is_absolute() {
        return Err(CollaborationError::InvalidAgentProfile {
            message: "工作目录必须是绝对路径",
        });
    }
    if profile.tool_snapshot.len() > MAX_TOOL_SNAPSHOT_ENTRIES {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "Agent 工具快照数量",
            maximum: MAX_TOOL_SNAPSHOT_ENTRIES,
        });
    }
    let mut names = HashSet::new();
    for name in &profile.tool_snapshot {
        if name.trim().is_empty()
            || name.len() > MAX_PROFILE_FIELD_BYTES
            || !names.insert(name.as_str())
        {
            return Err(CollaborationError::InvalidAgentProfile {
                message: "工具快照包含空值、超长名称或重复名称",
            });
        }
    }
    Ok(())
}

/// 将子 Agent 请求收紧到父 Agent 已生效的计划只读边界。
fn constrain_child_profile(
    parent: &AgentProfile,
    parent_turn_plan_guard: PlanGuard,
    requested: &AgentProfile,
) -> AgentProfile {
    let mut effective = requested.clone();
    effective.plan_guard = effective_child_plan_guard(
        parent.plan_guard,
        parent_turn_plan_guard,
        requested.plan_guard,
    );
    effective
}

/// 合并父 Agent 配置、来源 Turn 和目标请求中的最严格 Plan 守卫。
fn effective_child_plan_guard(
    parent_plan_guard: PlanGuard,
    parent_turn_plan_guard: PlanGuard,
    requested_plan_guard: PlanGuard,
) -> PlanGuard {
    if matches!(parent_plan_guard.state(), PlanGuardState::ReadOnly)
        || matches!(parent_turn_plan_guard.state(), PlanGuardState::ReadOnly)
        || matches!(requested_plan_guard.state(), PlanGuardState::ReadOnly)
    {
        PlanGuard::read_only()
    } else {
        PlanGuard::inactive()
    }
}

/// 校验执行器终态文本，避免失败补偿或模型输出绕过正文边界。
fn validate_turn_outcome(outcome: &AgentTurnOutcome) -> Result<(), CollaborationError> {
    match outcome {
        AgentTurnOutcome::Completed {
            final_message: Some(message),
        } => validate_optional_text(message, "Agent 最终文本"),
        AgentTurnOutcome::Failed { message } => validate_required_text(message, "Agent 失败说明"),
        AgentTurnOutcome::Completed {
            final_message: None,
        }
        | AgentTurnOutcome::Interrupted => Ok(()),
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

/// 持久化快照中一封 mailbox 消息及其 Followup 触发归属。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecoveredMailboxMessage {
    /// 需要恢复的完整 mailbox 消息。
    pub message: MailboxMessage,
    /// 该 TriggerTurn 消息已经归属的待执行或活跃 Turn；普通消息固定为 `None`。
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
    /// 该根树允许同时运行的 Turn 上限。
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

/// 全局并发上限的经校验配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollaborationLimits {
    /// 所有根树合计允许的活跃 Turn 数。
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

/// 注册一棵新根 Agent 树的请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootAgentRequest {
    /// 已由上层 Session 管理器分配的根 Session 标识。
    pub session_id: SessionId,
    /// 根 Agent 的独立运行配置与最低 Plan 约束。
    pub profile: AgentProfile,
    /// 该根树同时运行 Turn 的上限。
    pub per_root_turn_limit: usize,
}

/// 根 Agent 创建单层子 Agent 的请求。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpawnAgentRequest {
    /// 用于稳定 `/root/...` 路径的小写任务名。
    pub task_name: String,
    /// 子 Agent 第一个 Turn 的完整任务文本。
    pub initial_task: String,
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
    /// 全局正在使用的 Turn 槽位数。
    pub global_in_use: usize,
    /// 全局 Turn 槽位上限。
    pub global_limit: usize,
    /// 按根 Agent 排列的当前使用槽位与上限。
    pub roots: Vec<(AgentId, usize, usize)>,
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

/// mailbox 内部条目同时记录 TriggerTurn 是否已归属到某个 Turn。
#[derive(Clone, Debug)]
struct MailboxEntry {
    /// 对外可见并持久化的完整消息。
    message: MailboxMessage,
    /// 已为该 TriggerTurn 消息创建或复用的待执行 Turn。
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

/// 已同时预约全局与根级槽位的活跃 Turn。
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
    /// 全局 Turn 槽位上限。
    global_limit: usize,
    /// 全局当前已预约 Turn 槽位数。
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
    /// TriggerTurn 已归属的待执行或活跃 Turn。
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
pub struct CollaborationCoordinator {
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
}

impl CollaborationCoordinator {
    /// 从已校验容量和端口创建一个空协调器。
    pub fn new(
        limits: CollaborationLimits,
        store: Arc<dyn CollaborationStore>,
        execution: Arc<dyn AgentExecutionPort>,
        ids: Arc<dyn CollaborationIdGenerator>,
    ) -> Self {
        let root_identity_namespace = ids.next_agent_id();
        Self {
            store,
            execution,
            ids,
            state: Mutex::new(CoordinatorState {
                last_event_sequence: 0,
                root_identity_namespace,
                next_root_sequence: 1,
                global_limit: limits.global_turn_limit,
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
            }),
            execution_fences: Mutex::new(HashMap::new()),
            root_registration: Mutex::new(()),
        }
    }

    /// 在空协调器中原子恢复全局水位、根身份命名空间和全部未移除根树。
    pub fn restore_coordinator(
        &self,
        recovered: RecoveredCoordinator,
    ) -> Result<Vec<AgentHandle>, CollaborationError> {
        self.restore_coordinator_with_authoritative_outcomes(recovered, &HashMap::new())
    }

    /// 恢复协调器，并让同一 Turn 已写入 Runtime Journal 的终态优先于旧 checkpoint 未决状态。
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
                    if let Some(outcome) = authoritative_outcomes.get(&turn_id) {
                        consumed_authoritative_turns.insert(turn_id.clone());
                        resolved_turns.push(RecoveredTurnResolution {
                            definition: agent.definition.clone(),
                            previous_status: agent.status.clone(),
                            turn: TurnRecord {
                                turn_id,
                                cause,
                                prompt: agent.current_turn_prompt.clone(),
                                parent_turn_id: agent.current_parent_turn_id.clone(),
                                root_turn_id,
                                outcome: outcome.clone(),
                            },
                            source_agent_id,
                        });
                    } else if recovered.lifecycle == RecoveredRootLifecycle::Open {
                        resolved_turns.push(RecoveredTurnResolution {
                            definition: agent.definition.clone(),
                            previous_status: agent.status.clone(),
                            turn: TurnRecord {
                                turn_id,
                                cause,
                                prompt: agent.current_turn_prompt.clone(),
                                parent_turn_id: agent.current_parent_turn_id.clone(),
                                root_turn_id,
                                outcome: AgentTurnOutcome::Interrupted,
                            },
                            source_agent_id,
                        });
                    } else if recovered.lifecycle == RecoveredRootLifecycle::Closing {
                        let cancellation = TurnCancellation::new();
                        cancellation.cancel();
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
                            },
                        );
                        candidate.global_in_use = candidate
                            .global_in_use
                            .checked_add(1)
                            .ok_or(CollaborationError::SequenceExhausted)?;
                        candidate
                            .roots
                            .get_mut(&recovered.root_agent_id)
                            .expect("恢复 Closing 根树已创建")
                            .in_use += 1;
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

        if candidate.global_in_use > candidate.global_limit
            || candidate
                .roots
                .values()
                .any(|root| root.in_use > root.turn_limit)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复中的 Closing 根树活跃 Turn 超过全局或根级容量".to_owned(),
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
        if !events.is_empty() {
            self.append_events(&mut state, &candidate, expected_sequence, &events, true)?;
        }
        *state = candidate;
        drop(state);
        self.execute_actions(actions)?;
        Ok(handles)
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
            schedule_available(state, &mut events, &mut actions)?;
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
            let child_agent_id = self.ids.next_agent_id();
            let child_session_id = self.ids.next_session_id();
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
            schedule_available(state, &mut events, &mut actions)?;
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
                self.store.as_ref(),
                &source_agent_id,
                &target_agent_id,
            )?;
            let source = resident_agent(state, &source_agent_id)?;
            let target = resident_agent(state, &target_agent_id)?;
            if source.definition.root_agent_id != target.definition.root_agent_id {
                return Err(CollaborationError::CrossTreeOperation);
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
            let target_plan_guard = effective_child_plan_guard(
                source.definition.profile.plan_guard,
                source_turn.plan_guard,
                target_definition.profile.plan_guard,
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
            let message_id = self.ids.next_message_id();
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
                    target_agent_id: target_agent_id.clone(),
                    message_id: message_id.clone(),
                    delivery,
                    kind: MailboxMessageKind::AgentMessage,
                    content: content.clone(),
                    related_turn_id: Some(source_turn.turn_id.clone()),
                    parent_turn_id: Some(source_turn.turn_id.clone()),
                    root_turn_id: Some(source_turn.root_turn_id.clone()),
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
            schedule_available(state, &mut events, &mut actions)?;
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

    /// 为当前 Turn 持久 claim mailbox 最早前缀；重复调用在确认前返回同一批正文。
    pub fn consume_mailbox(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        maximum: usize,
    ) -> Result<Vec<MailboxMessage>, CollaborationError> {
        if maximum == 0 {
            return Err(CollaborationError::InvalidMailboxBatch);
        }
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let active = active_turn_for_agent(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            if let Some(claim) = &agent.mailbox_claim {
                if claim.turn_id != turn_id {
                    return Err(CollaborationError::InputClaimMismatch {
                        agent_id: agent_id.clone(),
                        turn_id: turn_id.clone(),
                        input_kind: "mailbox",
                    });
                }
                let messages = agent
                    .mailbox
                    .iter()
                    .take_while(|entry| entry.message.sequence <= claim.through_sequence)
                    .map(|entry| entry.message.clone())
                    .collect::<Vec<_>>();
                if messages.last().map(|message| message.sequence) != Some(claim.through_sequence) {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "mailbox claim 未对应仍保留的完整 FIFO 前缀".to_owned(),
                    });
                }
                return Ok(Transition {
                    output: messages,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            if agent.mailbox.is_empty() {
                return Ok(Transition {
                    output: Vec::new(),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let count = maximum.min(agent.mailbox.len());
            let messages = agent
                .mailbox
                .iter()
                .take(count)
                .map(|entry| entry.message.clone())
                .collect::<Vec<_>>();
            let through_sequence = messages
                .last()
                .expect("非空 mailbox 前缀始终有最后一条")
                .sequence;
            let message_ids = messages
                .iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>();
            let definition = agent.definition.clone();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent.mailbox_claim = Some(InputBatchClaim {
                turn_id: turn_id.clone(),
                through_sequence,
            });
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id: active.parent_turn_id,
                    root_turn_id: Some(active.root_turn_id),
                },
                CollaborationEventKind::AgentMessagesClaimed {
                    message_ids,
                    through_sequence,
                },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: messages,
                events,
                actions: Vec::new(),
            })
        })
    }

    /// 在 Runtime 已原子提交 Transcript 后确认并删除此前 claim 的 mailbox 前缀。
    pub fn acknowledge_mailbox(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        through_sequence: u64,
    ) -> Result<(), CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let agent = resident_agent(state, &agent_id)?;
            let Some(claim) = agent.mailbox_claim.clone() else {
                return Ok(Transition {
                    output: (),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            };
            if claim.turn_id != turn_id || claim.through_sequence != through_sequence {
                return Err(CollaborationError::InputClaimMismatch {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                    input_kind: "mailbox",
                });
            }
            let (definition, parent_turn_id, root_turn_id) =
                input_claim_event_context(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            let messages = agent
                .mailbox
                .iter()
                .take_while(|entry| entry.message.sequence <= through_sequence)
                .map(|entry| entry.message.clone())
                .collect::<Vec<_>>();
            if messages.last().map(|message| message.sequence) != Some(through_sequence) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "确认 mailbox claim 时找不到完整 FIFO 前缀".to_owned(),
                });
            }
            let count = messages.len();
            let consumed_bytes = messages
                .iter()
                .map(|message| message.content.len())
                .sum::<usize>();
            let consumed_completion_count = messages
                .iter()
                .filter(|message| {
                    matches!(message.kind, MailboxMessageKind::ChildTurnFinished { .. })
                })
                .count();
            let consumed_completion_bytes = messages
                .iter()
                .filter(|message| {
                    matches!(message.kind, MailboxMessageKind::ChildTurnFinished { .. })
                })
                .map(|message| message.content.len())
                .sum::<usize>();
            let message_ids = messages
                .iter()
                .map(|message| message.message_id.clone())
                .collect::<Vec<_>>();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent.mailbox.drain(..count);
            agent.mailbox_claim = None;
            agent.mailbox_bytes =
                agent
                    .mailbox_bytes
                    .checked_sub(consumed_bytes)
                    .ok_or_else(|| CollaborationError::InvalidRecovery {
                        message: "确认 mailbox 时正文总字节数下溢".to_owned(),
                    })?;
            agent.completion_count = agent
                .completion_count
                .checked_sub(consumed_completion_count)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时完成通知计数下溢".to_owned(),
                })?;
            agent.completion_bytes = agent
                .completion_bytes
                .checked_sub(consumed_completion_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时完成通知字节下溢".to_owned(),
                })?;
            let root = state
                .roots
                .get_mut(&definition.root_agent_id)
                .expect("确认 mailbox 的根树应存在");
            root.mailbox_count = root.mailbox_count.checked_sub(count).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时根树计数下溢".to_owned(),
                }
            })?;
            root.mailbox_bytes =
                root.mailbox_bytes
                    .checked_sub(consumed_bytes)
                    .ok_or_else(|| CollaborationError::InvalidRecovery {
                        message: "确认 mailbox 时根树字节下溢".to_owned(),
                    })?;
            root.completion_count = root
                .completion_count
                .checked_sub(consumed_completion_count)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时根树完成计数下溢".to_owned(),
                })?;
            root.completion_bytes = root
                .completion_bytes
                .checked_sub(consumed_completion_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认 mailbox 时根树完成字节下溢".to_owned(),
                })?;
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id,
                    root_turn_id: Some(root_turn_id),
                },
                CollaborationEventKind::AgentMessagesConsumed {
                    message_ids,
                    through_sequence,
                },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: (),
                events,
                actions: Vec::new(),
            })
        })
    }

    /// 将用户 steer 持久化到指定活跃 Turn，并唤醒 WaitAgent 与执行器安全边界。
    pub fn steer_agent(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        content: impl Into<String>,
    ) -> Result<UserSteer, CollaborationError> {
        let content = content.into();
        validate_required_text(&content, "用户 Steer")?;
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let mut events = Vec::new();
            let mut actions = Vec::new();
            let steer = queue_user_steer(
                state,
                &agent_id,
                &turn_id,
                content.clone(),
                &mut events,
                &mut actions,
            )?;
            Ok(Transition {
                output: steer.clone(),
                events,
                actions,
            })
        })
    }

    /// 以外部稳定操作标识向当前运行 Turn 注入用户 steer，并跨进程持久去重。
    pub fn steer_active_agent_with_operation(
        &self,
        agent_id: &AgentId,
        operation_id: &ToolCallId,
        content: impl Into<String>,
    ) -> Result<UserSteer, CollaborationError> {
        let content = content.into();
        validate_required_text(&content, "用户 Steer")?;
        let agent_id = agent_id.clone();
        let operation_id = operation_id.clone();
        self.apply_transition(|state| {
            let invocation_input = CollaborationInvocationInput::SteerAgent {
                target_agent_id: agent_id.clone(),
                content: content.clone(),
            };
            let active_turn_id = match &resident_agent(state, &agent_id)?.status {
                CollaborationAgentStatus::Running { turn_id } => Some(turn_id.clone()),
                _ => None,
            };
            if let Some((key, record)) = state.collaboration_invocations.iter().find(|(key, _)| {
                key.source_agent_id == agent_id
                    && key.tool_call_id == operation_id
                    && (active_turn_id.as_ref() == Some(&key.source_turn_id)
                        || active_turn_id.is_none())
            }) {
                if record.kind == CollaborationInvocationKind::SteerAgent
                    && record.input_digest
                        == collaboration_invocation_input_digest(&invocation_input)
                    && let CollaborationInvocationOutput::UserSteer(steer) = &record.output
                {
                    return Ok(Transition {
                        output: steer.clone(),
                        events: Vec::new(),
                        actions: Vec::new(),
                    });
                }
                return Err(CollaborationError::IdempotencyConflict {
                    source_agent_id: key.source_agent_id.clone(),
                    source_turn_id: key.source_turn_id.clone(),
                    tool_call_id: key.tool_call_id.clone(),
                });
            }
            let Some(turn_id) = active_turn_id else {
                return Err(CollaborationError::TargetNotRunning {
                    agent_id: agent_id.clone(),
                });
            };
            let active = active_turn_for_agent(state, &agent_id, &turn_id)?;
            let definition = resident_agent(state, &agent_id)?.definition.clone();
            let invocation_key = CollaborationInvocationKey {
                source_agent_id: agent_id.clone(),
                source_turn_id: turn_id.clone(),
                tool_call_id: operation_id.clone(),
            };
            let mut events = Vec::new();
            let mut actions = Vec::new();
            let steer = queue_user_steer(
                state,
                &agent_id,
                &turn_id,
                content.clone(),
                &mut events,
                &mut actions,
            )?;
            let receipt = record_collaboration_invocation(
                state,
                invocation_key,
                invocation_input,
                CollaborationInvocationOutput::UserSteer(steer.clone()),
            )?;
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id),
                    parent_turn_id: active.parent_turn_id,
                    root_turn_id: Some(active.root_turn_id),
                },
                CollaborationEventKind::CollaborationInvocationCommitted {
                    receipt: Box::new(receipt),
                },
            )?;
            Ok(Transition {
                output: steer,
                events,
                actions,
            })
        })
    }

    /// 为当前 Turn 持久 claim 全部现有用户 steer；确认前重复调用返回同一批正文。
    pub fn consume_user_steers(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
    ) -> Result<Vec<UserSteer>, CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let active = active_turn_for_agent(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            if let Some(claim) = &agent.steer_claim {
                if claim.turn_id != turn_id {
                    return Err(CollaborationError::InputClaimMismatch {
                        agent_id: agent_id.clone(),
                        turn_id: turn_id.clone(),
                        input_kind: "用户 steer",
                    });
                }
                let steers = agent
                    .steers
                    .iter()
                    .filter(|steer| {
                        steer.turn_id == turn_id && steer.sequence <= claim.through_sequence
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if steers.last().map(|steer| steer.sequence) != Some(claim.through_sequence) {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "用户 steer claim 未对应仍保留的完整批次".to_owned(),
                    });
                }
                return Ok(Transition {
                    output: steers,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let steers = agent
                .steers
                .iter()
                .filter(|steer| steer.turn_id == turn_id)
                .cloned()
                .collect::<Vec<_>>();
            if steers.is_empty() {
                return Ok(Transition {
                    output: Vec::new(),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let sequences = steers
                .iter()
                .map(|steer| steer.sequence)
                .collect::<Vec<_>>();
            let through_sequence = *sequences.last().expect("非空用户 steer 批次始终有最大序号");
            let definition = agent.definition.clone();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent.steer_claim = Some(InputBatchClaim {
                turn_id: turn_id.clone(),
                through_sequence,
            });
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id: active.parent_turn_id,
                    root_turn_id: Some(active.root_turn_id),
                },
                CollaborationEventKind::AgentUserSteersClaimed { sequences },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: steers,
                events,
                actions: Vec::new(),
            })
        })
    }

    /// 在 Runtime 已原子提交 Transcript 后确认并删除此前 claim 的用户 steer。
    pub fn acknowledge_user_steers(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
        through_sequence: u64,
    ) -> Result<(), CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let agent = resident_agent(state, &agent_id)?;
            let Some(claim) = agent.steer_claim.clone() else {
                return Ok(Transition {
                    output: (),
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            };
            if claim.turn_id != turn_id || claim.through_sequence != through_sequence {
                return Err(CollaborationError::InputClaimMismatch {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                    input_kind: "用户 steer",
                });
            }
            let (definition, parent_turn_id, root_turn_id) =
                input_claim_event_context(state, &agent_id, &turn_id)?;
            let agent = resident_agent(state, &agent_id)?;
            let steers = agent
                .steers
                .iter()
                .filter(|steer| steer.turn_id == turn_id && steer.sequence <= through_sequence)
                .cloned()
                .collect::<Vec<_>>();
            if steers.last().map(|steer| steer.sequence) != Some(through_sequence) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "确认用户 steer claim 时找不到完整批次".to_owned(),
                });
            }
            let sequences = steers
                .iter()
                .map(|steer| steer.sequence)
                .collect::<Vec<_>>();
            let consumed_bytes = steers
                .iter()
                .map(|steer| steer.content.len())
                .sum::<usize>();
            let agent = state.agents.get_mut(&agent_id).expect("Agent 在上方已校验");
            agent
                .steers
                .retain(|steer| steer.turn_id != turn_id || steer.sequence > through_sequence);
            agent.steer_claim = None;
            agent.steer_bytes = agent
                .steer_bytes
                .checked_sub(consumed_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "确认用户 steer 时正文总字节数下溢".to_owned(),
                })?;
            let mut events = Vec::new();
            push_event(
                state,
                &mut events,
                &definition,
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id,
                    root_turn_id: Some(root_turn_id),
                },
                CollaborationEventKind::AgentUserSteersConsumed { sequences },
            )?;
            invalidate_quiet_turn_signal(state, &agent_id, &turn_id)?;
            Ok(Transition {
                output: (),
                events,
                actions: Vec::new(),
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

    /// 取消指定 Agent 的当前 Turn，不影响同一根树中的其他 Agent。
    pub fn cancel_current_turn(&self, agent_id: &AgentId) -> Result<TurnId, CollaborationError> {
        let agent_id = agent_id.clone();
        self.apply_transition(|state| {
            let status = resident_agent(state, &agent_id)?.status.clone();
            if let CollaborationAgentStatus::WaitingCapacity { turn_id } = status {
                let mut events = Vec::new();
                let mut actions = Vec::new();
                interrupt_waiting_turn(
                    state,
                    &agent_id,
                    &turn_id,
                    &agent_id,
                    &mut events,
                    &mut actions,
                )?;
                return Ok(Transition {
                    output: turn_id,
                    events,
                    actions,
                });
            }
            let Some(turn_id) = status.active_turn_id().cloned() else {
                return Err(CollaborationError::TargetNotRunning {
                    agent_id: agent_id.clone(),
                });
            };
            if matches!(status, CollaborationAgentStatus::Cancelling { .. }) {
                return Ok(Transition {
                    output: turn_id,
                    events: Vec::new(),
                    actions: Vec::new(),
                });
            }
            let active = state.active_turns.get(&turn_id).cloned().ok_or_else(|| {
                CollaborationError::TurnMismatch {
                    agent_id: agent_id.clone(),
                    turn_id: turn_id.clone(),
                }
            })?;
            let mut events = Vec::new();
            let mut actions = Vec::new();
            set_status(
                state,
                &agent_id,
                CollaborationAgentStatus::Cancelling {
                    turn_id: turn_id.clone(),
                },
                EventLink {
                    source_agent_id: agent_id.clone(),
                    turn_id: Some(turn_id.clone()),
                    parent_turn_id: active.parent_turn_id.clone(),
                    root_turn_id: Some(active.root_turn_id.clone()),
                },
                &mut events,
            )?;
            mark_activity(state, &agent_id, &mut actions)?;
            actions.push(PostCommitAction::CancelTurn(active.cancellation));
            Ok(Transition {
                output: turn_id,
                events,
                actions,
            })
        })
    }

    /// 按 Agent 与 Turn 双重身份精确取消，不把过期 UI 请求作用到更晚的新 Turn。
    pub fn cancel_turn(
        &self,
        agent_id: &AgentId,
        turn_id: &TurnId,
    ) -> Result<TurnCancellationDisposition, CollaborationError> {
        let agent_id = agent_id.clone();
        let turn_id = turn_id.clone();
        self.apply_transition(|state| {
            let status = resident_agent(state, &agent_id)?.status.clone();
            match status {
                CollaborationAgentStatus::WaitingCapacity {
                    turn_id: current_turn_id,
                } => {
                    if current_turn_id != turn_id {
                        return Err(CollaborationError::TurnMismatch {
                            agent_id: agent_id.clone(),
                            turn_id: turn_id.clone(),
                        });
                    }
                    let mut events = Vec::new();
                    let mut actions = Vec::new();
                    interrupt_waiting_turn(
                        state,
                        &agent_id,
                        &turn_id,
                        &agent_id,
                        &mut events,
                        &mut actions,
                    )?;
                    Ok(Transition {
                        output: TurnCancellationDisposition::Requested,
                        events,
                        actions,
                    })
                }
                CollaborationAgentStatus::Running {
                    turn_id: current_turn_id,
                } => {
                    if current_turn_id != turn_id {
                        return Err(CollaborationError::TurnMismatch {
                            agent_id: agent_id.clone(),
                            turn_id: turn_id.clone(),
                        });
                    }
                    let active = state.active_turns.get(&turn_id).cloned().ok_or_else(|| {
                        CollaborationError::TurnMismatch {
                            agent_id: agent_id.clone(),
                            turn_id: turn_id.clone(),
                        }
                    })?;
                    let mut events = Vec::new();
                    let mut actions = Vec::new();
                    set_status(
                        state,
                        &agent_id,
                        CollaborationAgentStatus::Cancelling {
                            turn_id: turn_id.clone(),
                        },
                        EventLink {
                            source_agent_id: agent_id.clone(),
                            turn_id: Some(turn_id.clone()),
                            parent_turn_id: active.parent_turn_id.clone(),
                            root_turn_id: Some(active.root_turn_id.clone()),
                        },
                        &mut events,
                    )?;
                    mark_activity(state, &agent_id, &mut actions)?;
                    actions.push(PostCommitAction::CancelTurn(active.cancellation));
                    Ok(Transition {
                        output: TurnCancellationDisposition::Requested,
                        events,
                        actions,
                    })
                }
                CollaborationAgentStatus::Cancelling {
                    turn_id: current_turn_id,
                } => {
                    if current_turn_id != turn_id {
                        return Err(CollaborationError::TurnMismatch {
                            agent_id: agent_id.clone(),
                            turn_id: turn_id.clone(),
                        });
                    }
                    Ok(Transition {
                        output: TurnCancellationDisposition::AlreadyRequested,
                        events: Vec::new(),
                        actions: Vec::new(),
                    })
                }
                _ => {
                    let agent = resident_agent(state, &agent_id)?;
                    if agent
                        .last_turn
                        .as_ref()
                        .is_some_and(|last_turn| last_turn.turn_id == turn_id)
                    {
                        return Ok(Transition {
                            output: TurnCancellationDisposition::NotRunning,
                            events: Vec::new(),
                            actions: Vec::new(),
                        });
                    }
                    Err(CollaborationError::TurnMismatch {
                        agent_id: agent_id.clone(),
                        turn_id: turn_id.clone(),
                    })
                }
            }
        })
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
                self.store.as_ref(),
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
                self.store.as_ref(),
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
            schedule_available(state, &mut events, &mut actions)?;
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
            materialize_evicted_agents_for_root(state, self.store.as_ref(), &root_agent_id)?;
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
            if let Ok(mut state) = self.state.lock() {
                if let Some(root) = state.roots.get_mut(&root_agent_id) {
                    root.suspended = true;
                }
            }
        }
        let (_output, actions) = result?;
        drop(fence_state);
        self.execute_actions(actions)
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
                materialize_evicted_agents_for_root(state, self.store.as_ref(), &root_agent_id)?;
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
        self.execute_actions(actions)?;
        Ok(output)
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
        materialize_evicted_agents_for_root(&mut state, self.store.as_ref(), &root_agent_id)?;
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
        materialize_evicted_agents_for_root(&mut state, self.store.as_ref(), root_agent_id)?;
        let mut agents = state
            .agents
            .values()
            .filter(|entry| entry.definition.root_agent_id == *root_agent_id)
            .map(|entry| collaboration_agent_summary(&state, entry))
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.agent.path.cmp(&right.agent.path));
        Ok(agents)
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
        Ok(CollaborationCapacity {
            global_in_use: state.global_in_use,
            global_limit: state.global_limit,
            roots,
        })
    }

    /// 原子更新全局活跃 Turn 上限；提升后按既有公平队列立即调度，降低时保留已运行 Turn。
    pub fn update_global_turn_limit(
        &self,
        global_turn_limit: usize,
    ) -> Result<CollaborationCapacity, CollaborationError> {
        if global_turn_limit == 0 {
            return Err(CollaborationError::InvalidTurnLimit);
        }
        self.apply_transition(|state| {
            state.global_limit = global_turn_limit;
            let mut events = Vec::new();
            let mut actions = Vec::new();
            schedule_available(state, &mut events, &mut actions)?;
            let mut roots = state
                .roots
                .values()
                .map(|root| (root.root_agent_id.clone(), root.in_use, root.turn_limit))
                .collect::<Vec<_>>();
            roots.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(Transition {
                output: CollaborationCapacity {
                    global_in_use: state.global_in_use,
                    global_limit: state.global_limit,
                    roots,
                },
                events,
                actions,
            })
        })
    }

    /// 原子更新全局与指定根树的 Turn 上限；降低时不取消已预约或运行的 Turn。
    pub fn update_turn_limits(
        &self,
        root_agent_id: &AgentId,
        global_turn_limit: usize,
        per_root_turn_limit: usize,
    ) -> Result<CollaborationCapacity, CollaborationError> {
        if global_turn_limit == 0 || per_root_turn_limit == 0 {
            return Err(CollaborationError::InvalidTurnLimit);
        }
        let root_agent_id = root_agent_id.clone();
        self.apply_transition(|state| {
            ensure_tree_open(state, &root_agent_id)?;
            state.global_limit = global_turn_limit;
            state
                .roots
                .get_mut(&root_agent_id)
                .expect("根树在上方已校验")
                .turn_limit = per_root_turn_limit;
            let mut events = Vec::new();
            let mut actions = Vec::new();
            schedule_available(state, &mut events, &mut actions)?;
            let mut roots = state
                .roots
                .values()
                .map(|root| (root.root_agent_id.clone(), root.in_use, root.turn_limit))
                .collect::<Vec<_>>();
            roots.sort_by(|left, right| left.0.cmp(&right.0));
            Ok(Transition {
                output: CollaborationCapacity {
                    global_in_use: state.global_in_use,
                    global_limit: state.global_limit,
                    roots,
                },
                events,
                actions,
            })
        })
    }

    /// 返回尚未消费的 mailbox 消息快照，不改变 exactly-once 状态。
    pub fn mailbox(&self, agent_id: &AgentId) -> Result<Vec<MailboxMessage>, CollaborationError> {
        let state = self.lock_state()?;
        let agent = resident_agent(&state, agent_id)?;
        Ok(agent
            .mailbox
            .iter()
            .map(|entry| entry.message.clone())
            .collect())
    }

    /// 生成可交给 Session Store 定期快照的根树冷恢复数据。
    pub fn checkpoint_root(
        &self,
        root_agent_id: &AgentId,
    ) -> Result<RecoveredAgentTree, CollaborationError> {
        let state = self.lock_state()?;
        checkpoint_root_with_store(&state, root_agent_id, self.store.as_ref())
    }

    /// 生成不含未决 Turn 或 durable outbox 的静止导出快照。
    pub fn checkpoint_quiescent_root(
        &self,
        root_agent_id: &AgentId,
    ) -> Result<RecoveredAgentTree, CollaborationError> {
        let state = self.lock_state()?;
        let mut checkpoint =
            checkpoint_root_with_store(&state, root_agent_id, self.store.as_ref())?;
        let has_current_turn = checkpoint.agents.iter().any(|agent| {
            matches!(
                agent.status,
                CollaborationAgentStatus::WaitingCapacity { .. }
                    | CollaborationAgentStatus::Running { .. }
                    | CollaborationAgentStatus::Cancelling { .. }
            )
        });
        if has_current_turn
            || state.close_outbox.contains_key(root_agent_id)
            || checkpoint.agents.iter().any(|agent| {
                recovered_current_turn_id(&agent.status).is_some_and(|turn_id| {
                    state
                        .signal_outbox
                        .keys()
                        .any(|key| &key.turn_id == turn_id)
                })
            })
            || checkpoint.agents.iter().any(|agent| agent.start_pending)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "静止 checkpoint 不能包含未决 Turn 或 durable outbox".to_owned(),
            });
        }
        checkpoint.live = false;
        Ok(checkpoint)
    }

    /// 在同一状态锁下生成包含空根集、全局水位和根身份 counter 的原子快照。
    pub fn checkpoint_coordinator(&self) -> Result<RecoveredCoordinator, CollaborationError> {
        let state = self.lock_state()?;
        checkpoint_coordinator_from_state(&state, self.store.as_ref())
    }

    /// 返回测试可见的执行 fence 数量，用于证明已移除根不会形成无界墓碑。
    #[cfg(test)]
    pub(crate) fn execution_fence_count(&self) -> usize {
        self.execution_fences
            .lock()
            .expect("测试执行 fence 锁不应中毒")
            .len()
    }

    /// 显式重试所有未确认的 StartTurn、SignalTurn、QuiesceTree 与 CloseTree 命令。
    pub fn reconcile_outbox(&self) -> Result<usize, CollaborationError> {
        let (mut starts, mut signals, mut quiesces, mut closes) = {
            let mut state = self.lock_state()?;
            let expected_sequence = state.last_event_sequence;
            self.verify_store_sequence(&mut state, expected_sequence, "durable outbox 对账")?;
            let starts = state.start_outbox.values().cloned().collect::<Vec<_>>();
            let signals = state.signal_outbox.values().cloned().collect::<Vec<_>>();
            let quiesces = state.quiesce_outbox.values().cloned().collect::<Vec<_>>();
            let closes = state.close_outbox.values().cloned().collect::<Vec<_>>();
            (starts, signals, quiesces, closes)
        };
        starts.sort_by(|left, right| left.turn_id.cmp(&right.turn_id));
        signals.sort_by(|left, right| {
            (&left.agent_id, &left.turn_id, left.kind).cmp(&(
                &right.agent_id,
                &right.turn_id,
                right.kind,
            ))
        });
        quiesces.sort_by(|left, right| left.root_agent_id.cmp(&right.root_agent_id));
        closes.sort_by(|left, right| left.root_agent_id.cmp(&right.root_agent_id));
        let count = starts
            .len()
            .saturating_add(signals.len())
            .saturating_add(quiesces.len())
            .saturating_add(closes.len());
        let mut actions = starts
            .into_iter()
            .map(|launch| PostCommitAction::StartTurn(Box::new(launch)))
            .collect::<Vec<_>>();
        actions.extend(signals.into_iter().map(PostCommitAction::SignalTurn));
        actions.extend(quiesces.into_iter().map(PostCommitAction::QuiesceTree));
        actions.extend(closes.into_iter().map(PostCommitAction::CloseTree));
        self.execute_actions(actions)?;
        Ok(count)
    }

    /// 将一个非活跃子 Agent 从内存驱逐，保留根树身份与路径占用。
    pub fn evict_idle_agent(&self, agent_id: &AgentId) -> Result<(), CollaborationError> {
        let mut state = self.lock_state()?;
        let agent = resident_agent(&state, agent_id)?;
        if agent.definition.depth == AgentDepth::ROOT
            || !agent.status.is_idle()
            || agent.mailbox_claim.is_some()
            || agent.steer_claim.is_some()
        {
            return Err(CollaborationError::TargetNotIdle {
                agent_id: agent_id.clone(),
            });
        }
        let root_agent_id = agent.definition.root_agent_id.clone();
        let recovered = recovered_agent_from_entry(&state, agent)?;
        let revision = state
            .roots
            .get(&root_agent_id)
            .expect("驱逐目标根树应存在")
            .next_checkpoint_revision;
        let checkpoint = RecoveredAgentCheckpoint {
            root_agent_id: root_agent_id.clone(),
            revision,
            agent: recovered,
        };
        self.store
            .save_agent_checkpoint(&checkpoint)
            .map_err(|error| CollaborationError::Store {
                message: error.message().to_owned(),
            })?;
        let digest = recovered_agent_checkpoint_digest(&checkpoint);
        let steer_count = checkpoint.agent.pending_steers.len();
        let steer_bytes = checkpoint
            .agent
            .pending_steers
            .iter()
            .map(|steer| steer.content.len())
            .sum();
        let dynamic_text_bytes = recovered_agent_dynamic_text_bytes(&checkpoint.agent);
        let mut candidate = state.clone();
        let root = candidate
            .roots
            .get_mut(&root_agent_id)
            .expect("驱逐目标根树在上方已校验");
        root.next_checkpoint_revision = revision
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        root.evicted_agent_checkpoints.insert(
            agent_id.clone(),
            EvictedAgentCheckpointRef {
                revision,
                digest,
                steer_count,
                steer_bytes,
                dynamic_text_bytes,
            },
        );
        candidate.agents.remove(agent_id);
        validate_coordinator_quotas(&candidate)?;
        *state = candidate;
        Ok(())
    }

    /// 原子提交候选状态和事件，再执行非权威后置动作。
    fn apply_transition<T>(
        &self,
        planner: impl FnOnce(&mut CoordinatorState) -> Result<Transition<T>, CollaborationError>,
    ) -> Result<T, CollaborationError> {
        let (output, actions) = self.commit_transition(planner)?;
        self.execute_actions(actions)?;
        Ok(output)
    }

    /// 使用稳定批次标识原子提交事件与完整 checkpoint，并对不确定结果原样重放。
    fn append_events(
        &self,
        state: &mut CoordinatorState,
        candidate: &CoordinatorState,
        expected_sequence: u64,
        events: &[CollaborationEvent],
        allow_recovery_interruptions: bool,
    ) -> Result<(), CollaborationError> {
        let batch = collaboration_event_batch(expected_sequence, events);
        let committed_sequence = batch
            .events
            .last()
            .map_or(expected_sequence, |event| event.sequence);
        let batch_id = batch.batch_id.clone();
        let checkpoint = checkpoint_coordinator_from_state(candidate, self.store.as_ref())?;
        if checkpoint.last_event_sequence != committed_sequence {
            return Err(CollaborationError::InvalidRecovery {
                message: "候选 checkpoint 水位与事件批次末序号不一致".to_owned(),
            });
        }
        let commit = CollaborationTransitionCommit { batch, checkpoint };
        let first_error = match if allow_recovery_interruptions {
            self.store.commit_recovery_transition(&commit)
        } else {
            self.store.commit_transition(&commit)
        } {
            CollaborationAppendResult::Appended => return Ok(()),
            CollaborationAppendResult::AlreadyCommitted { current_sequence }
                if current_sequence == committed_sequence =>
            {
                return Ok(());
            }
            CollaborationAppendResult::AlreadyCommitted { current_sequence } => {
                let message = format!(
                    "事件批次 {} 已提交，但 Store 当前水位 {} 与批次末序号 {} 不一致",
                    batch_id, current_sequence, committed_sequence
                );
                state.store_recovery_required = Some(message.clone());
                return Err(CollaborationError::StoreRecoveryRequired { message });
            }
            CollaborationAppendResult::Absent { current_sequence }
                if current_sequence == expected_sequence =>
            {
                return Err(CollaborationError::Store {
                    message: format!(
                        "事件批次 {} 未提交，Store 水位仍为 {}",
                        batch_id, current_sequence
                    ),
                });
            }
            CollaborationAppendResult::Absent { current_sequence } => {
                let message = format!(
                    "事件批次 {} 未提交但 Store 水位 {} 已偏离期望 {}",
                    batch_id, current_sequence, expected_sequence
                );
                state.store_recovery_required = Some(message.clone());
                return Err(CollaborationError::StoreRecoveryRequired { message });
            }
            CollaborationAppendResult::Conflict { actual_sequence } => {
                let message = format!(
                    "事件批次 {} 与 Store 实际水位 {} 冲突，期望水位为 {}",
                    batch_id, actual_sequence, expected_sequence
                );
                state.store_recovery_required = Some(message.clone());
                return Err(CollaborationError::StoreRecoveryRequired { message });
            }
            CollaborationAppendResult::Indeterminate { error } => error,
        };
        match if allow_recovery_interruptions {
            self.store.commit_recovery_transition(&commit)
        } else {
            self.store.commit_transition(&commit)
        } {
            CollaborationAppendResult::Appended => Ok(()),
            CollaborationAppendResult::AlreadyCommitted { current_sequence }
                if current_sequence == committed_sequence =>
            {
                Ok(())
            }
            CollaborationAppendResult::AlreadyCommitted { current_sequence } => {
                let message = format!(
                    "事件批次 {} 首次结果不确定（{}），对账确认已提交，但 Store 当前水位 {} 与批次末序号 {} 不一致",
                    batch_id,
                    first_error.message(),
                    current_sequence,
                    committed_sequence
                );
                state.store_recovery_required = Some(message.clone());
                Err(CollaborationError::StoreRecoveryRequired { message })
            }
            CollaborationAppendResult::Absent { current_sequence }
                if current_sequence == expected_sequence =>
            {
                Err(CollaborationError::Store {
                    message: format!(
                        "事件批次 {} 首次结果不确定（{}），对账后确认未提交且水位仍为 {}",
                        batch_id,
                        first_error.message(),
                        current_sequence
                    ),
                })
            }
            CollaborationAppendResult::Absent { current_sequence } => {
                let message = format!(
                    "事件批次 {} 首次结果不确定（{}），对账时 Store 水位 {} 已偏离期望 {}",
                    batch_id,
                    first_error.message(),
                    current_sequence,
                    expected_sequence
                );
                state.store_recovery_required = Some(message.clone());
                Err(CollaborationError::StoreRecoveryRequired { message })
            }
            CollaborationAppendResult::Conflict { actual_sequence } => {
                let message = format!(
                    "事件批次 {} 首次结果不确定（{}），对账时与 Store 水位 {} 冲突",
                    batch_id,
                    first_error.message(),
                    actual_sequence
                );
                state.store_recovery_required = Some(message.clone());
                Err(CollaborationError::StoreRecoveryRequired { message })
            }
            CollaborationAppendResult::Indeterminate { error } => {
                let message = format!(
                    "事件批次 {} 连续两次无法确认提交状态（{}；{}）",
                    batch_id,
                    first_error.message(),
                    error.message()
                );
                state.store_recovery_required = Some(message.clone());
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
            self.store
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
    fn commit_transition<T>(
        &self,
        planner: impl FnOnce(&mut CoordinatorState) -> Result<Transition<T>, CollaborationError>,
    ) -> Result<(T, Vec<PostCommitAction>), CollaborationError> {
        {
            let mut state = self.lock_state()?;
            let expected_sequence = state.last_event_sequence;
            let mut candidate = state.clone();
            let transition = planner(&mut candidate)?;
            validate_coordinator_quotas(&candidate)?;
            if !transition.events.is_empty() {
                self.append_events(
                    &mut state,
                    &candidate,
                    expected_sequence,
                    &transition.events,
                    false,
                )?;
            }
            *state = candidate;
            Ok((transition.output, transition.actions))
        }
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
                    let fence = self.execution_fence(&root_agent_id)?;
                    let fence_state = fence
                        .lock()
                        .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
                    if fence_state.closing {
                        continue;
                    }
                    let still_pending = {
                        let state = self.lock_state()?;
                        state.start_outbox.contains_key(&turn_id)
                            && state.roots.get(&root_agent_id).is_some_and(|root| {
                                root.lifecycle == RecoveredRootLifecycle::Open && !root.suspended
                            })
                    };
                    if !still_pending {
                        continue;
                    }
                    match self.execution.start_turn(*launch) {
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
                        let state = self.lock_state()?;
                        state
                            .active_turns
                            .get(&turn_id)
                            .map(|active| active.root_agent_id.clone())
                    }) else {
                        continue;
                    };
                    let fence = self.execution_fence(&root_agent_id)?;
                    let fence_state = fence
                        .lock()
                        .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
                    if fence_state.closing {
                        continue;
                    }
                    let signal_key = AgentTurnSignalKey::from_signal(&signal);
                    let pending_signal = {
                        let state = self.lock_state()?;
                        state.signal_outbox.get(&signal_key).cloned()
                    };
                    let Some(pending_signal) = pending_signal else {
                        continue;
                    };
                    if self.execution.signal_turn(pending_signal.clone()).is_ok() {
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
                    let fence = self.execution_fence(&root_agent_id)?;
                    let mut fence_state = fence
                        .lock()
                        .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
                    fence_state.closing = true;
                    let still_pending = self
                        .lock_state()?
                        .quiesce_outbox
                        .contains_key(&root_agent_id);
                    if !still_pending {
                        continue;
                    }
                    match self.execution.quiesce_tree(request) {
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
                                        message: "全树静止时全局槽位计数下溢".to_owned(),
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
                                schedule_available(state, &mut events, &mut actions)?;
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
                    let fence = self.execution_fence(&root_agent_id)?;
                    let mut fence_state = fence
                        .lock()
                        .map_err(|_poisoned| CollaborationError::StatePoisoned)?;
                    fence_state.closing = true;
                    let still_pending =
                        self.lock_state()?.close_outbox.contains_key(&root_agent_id);
                    if !still_pending {
                        continue;
                    }
                    if let Err(error) = self.execution.close_tree(request) {
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
                                self.execution_fences
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
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.event-batch.v2");
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

/// 对稳定领域值进行版本化、定长标签和长度前缀编码的 SHA-256 累加器。
struct CanonicalDigest {
    /// 当前规范编码的增量摘要状态。
    digest: Sha256,
}

impl CanonicalDigest {
    /// 以不可变格式版本前缀开始一次规范编码。
    fn new(version: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update((version.len() as u64).to_be_bytes());
        digest.update(version);
        Self { digest }
    }

    /// 编码一个枚举或结构字段标签。
    fn tag(&mut self, value: u8) {
        self.digest.update([value]);
    }

    /// 编码一个布尔值。
    fn bool(&mut self, value: bool) {
        self.tag(u8::from(value));
    }

    /// 以大端固定宽度编码无符号整数。
    fn u64(&mut self, value: u64) {
        self.digest.update(value.to_be_bytes());
    }

    /// 以 UTF-8 字节长度和原始字节编码文本。
    fn text(&mut self, value: &str) {
        self.u64(value.len() as u64);
        self.digest.update(value.as_bytes());
    }

    /// 完成摘要并返回固定小写十六进制文本。
    fn finish_hex(self) -> String {
        let bytes = self.finish_bytes();
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("写入 String 不会失败");
        }
        encoded
    }

    /// 完成摘要并返回固定 32 字节结果。
    fn finish_bytes(self) -> [u8; 32] {
        self.digest.finalize().into()
    }
}

/// 规范编码一个可选文本。
fn encode_optional_text(encoder: &mut CanonicalDigest, value: Option<&str>) {
    encoder.bool(value.is_some());
    if let Some(value) = value {
        encoder.text(value);
    }
}

/// 规范编码一个可选 Turn 标识。
fn encode_optional_turn(encoder: &mut CanonicalDigest, value: Option<&TurnId>) {
    encoder.bool(value.is_some());
    if let Some(value) = value {
        encoder.text(value.as_str());
    }
}

/// 规范编码一个可选 Agent 标识。
fn encode_optional_agent(encoder: &mut CanonicalDigest, value: Option<&AgentId>) {
    encoder.bool(value.is_some());
    if let Some(value) = value {
        encoder.text(value.as_str());
    }
}

/// 规范编码平台路径；持久层必须按同一文本恢复后再计算批次标识。
fn encode_path(encoder: &mut CanonicalDigest, value: &Path) {
    encoder.text(&value.as_os_str().to_string_lossy());
}

/// 规范编码上下文继承策略。
fn encode_context_inheritance(encoder: &mut CanonicalDigest, inheritance: &ContextInheritance) {
    match inheritance {
        ContextInheritance::None => encoder.tag(0),
        ContextInheritance::All => encoder.tag(1),
        ContextInheritance::RecentTurns { count } => {
            encoder.tag(2);
            encoder.u64(u64::from(*count));
        }
    }
}

/// 规范编码 Agent 创建时冻结的运行配置。
fn encode_agent_profile(encoder: &mut CanonicalDigest, profile: &AgentProfile) {
    encoder.text(&profile.model);
    encode_optional_text(encoder, profile.reasoning_effort.as_deref());
    encoder.tag(match profile.plan_guard.state() {
        PlanGuardState::Inactive => 0,
        PlanGuardState::ReadOnly => 1,
    });
    encode_path(encoder, &profile.cwd);
    encoder.bool(profile.worktree_lease.is_some());
    if let Some(worktree_lease) = &profile.worktree_lease {
        encoder.text(worktree_lease.as_str());
    }
    encoder.u64(profile.tool_snapshot.len() as u64);
    for tool_name in &profile.tool_snapshot {
        encoder.text(tool_name);
    }
}

/// 规范编码 spawn 前冻结的扩展 Agent 模板。
fn encode_agent_template_snapshot(
    encoder: &mut CanonicalDigest,
    template: Option<&AgentTemplateSnapshot>,
) {
    encoder.bool(template.is_some());
    if let Some(template) = template {
        encoder.text(&template.name);
        encoder.text(&template.system_prompt);
        encoder.bool(template.max_turns.is_some());
        if let Some(max_turns) = template.max_turns {
            encoder.u64(u64::from(max_turns));
        }
        encoder.u64(template.allowed_write_dirs.len() as u64);
        for directory in &template.allowed_write_dirs {
            encode_path(encoder, directory);
        }
    }
}

/// 规范编码 Agent 创建时冻结的完整定义。
fn encode_agent_definition(encoder: &mut CanonicalDigest, definition: &AgentDefinition) {
    encoder.text(definition.agent_id.as_str());
    encoder.text(definition.session_id.as_str());
    encoder.text(definition.root_agent_id.as_str());
    encoder.text(definition.root_session_id.as_str());
    encode_optional_agent(encoder, definition.parent_agent_id.as_ref());
    encoder.text(definition.path.as_str());
    encoder.tag(definition.depth.value());
    encode_context_inheritance(encoder, &definition.context_inheritance);
    encoder.u64(definition.context_snapshot.len() as u64);
    for message in &definition.context_snapshot {
        encoder.text(message);
    }
    encode_agent_template_snapshot(encoder, definition.agent_template.as_ref());
    encode_agent_profile(encoder, &definition.profile);
}

/// 规范编码协作工具的可信幂等键。
fn encode_collaboration_invocation_key(
    encoder: &mut CanonicalDigest,
    key: &CollaborationInvocationKey,
) {
    encoder.text(key.source_agent_id.as_str());
    encoder.text(key.source_turn_id.as_str());
    encoder.text(key.tool_call_id.as_str());
}

/// 规范编码协作工具的完整强类型业务输入。
fn encode_collaboration_invocation_input(
    encoder: &mut CanonicalDigest,
    input: &CollaborationInvocationInput,
) {
    match input {
        CollaborationInvocationInput::SpawnAgent(request) => {
            encoder.tag(0);
            encoder.text(&request.task_name);
            encoder.text(&request.initial_task);
            encode_context_inheritance(encoder, &request.context_inheritance);
            encoder.u64(request.context_snapshot.len() as u64);
            for message in &request.context_snapshot {
                encoder.text(message);
            }
            encode_agent_template_snapshot(encoder, request.agent_template.as_ref());
            encode_agent_profile(encoder, &request.profile);
        }
        CollaborationInvocationInput::SendMessage {
            target_agent_id,
            content,
            delivery,
        } => {
            encoder.tag(1);
            encoder.text(target_agent_id.as_str());
            encoder.text(content);
            encoder.tag(match delivery {
                MailboxDelivery::QueueOnly => 0,
                MailboxDelivery::TriggerTurn => 1,
            });
        }
        CollaborationInvocationInput::StopAgent { target_agent_id } => {
            encoder.tag(2);
            encoder.text(target_agent_id.as_str());
        }
        CollaborationInvocationInput::SteerAgent {
            target_agent_id,
            content,
        } => {
            encoder.tag(3);
            encoder.text(target_agent_id.as_str());
            encoder.text(content);
        }
        CollaborationInvocationInput::RetryAgent { target_agent_id } => {
            encoder.tag(4);
            encoder.text(target_agent_id.as_str());
        }
    }
}

/// 对完整规范协作工具输入计算版本化 SHA-256 摘要。
fn collaboration_invocation_input_digest(input: &CollaborationInvocationInput) -> [u8; 32] {
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.invocation-input.v2");
    encode_collaboration_invocation_input(&mut encoder, input);
    encoder.finish_bytes()
}

/// 对外部根 Turn 的完整用户输入计算版本化摘要，供跨恢复幂等绑定使用。
pub fn root_turn_prompt_digest(prompt: &str) -> [u8; 32] {
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.root-turn-prompt.v1");
    encoder.text(prompt);
    encoder.finish_bytes()
}

/// 规范编码协作工具首次提交的成功结果。
fn encode_collaboration_invocation_output(
    encoder: &mut CanonicalDigest,
    output: &CollaborationInvocationOutput,
) {
    match output {
        CollaborationInvocationOutput::SpawnedAgent(spawned) => {
            encoder.tag(0);
            encoder.text(spawned.agent.agent_id.as_str());
            encoder.text(spawned.agent.session_id.as_str());
            encoder.text(spawned.agent.path.as_str());
            encoder.text(spawned.initial_turn_id.as_str());
        }
        CollaborationInvocationOutput::Message {
            message_id,
            triggered_turn_id,
        } => {
            encoder.tag(1);
            encoder.text(message_id.as_str());
            encode_optional_turn(encoder, triggered_turn_id.as_ref());
        }
        CollaborationInvocationOutput::StoppedAgent {
            target_agent_id,
            stopped_turn_id,
        } => {
            encoder.tag(2);
            encoder.text(target_agent_id.as_str());
            encoder.text(stopped_turn_id.as_str());
        }
        CollaborationInvocationOutput::UserSteer(steer) => {
            encoder.tag(3);
            encoder.u64(steer.sequence);
            encoder.text(steer.turn_id.as_str());
            encoder.text(&steer.content);
        }
        CollaborationInvocationOutput::RetriedAgent {
            target_agent_id,
            retry_turn_id,
        } => {
            encoder.tag(4);
            encoder.text(target_agent_id.as_str());
            encoder.text(retry_turn_id.as_str());
        }
    }
}

/// 规范编码与业务事件同批提交的协作幂等凭据。
fn encode_collaboration_invocation_receipt(
    encoder: &mut CanonicalDigest,
    receipt: &CollaborationInvocationReceipt,
) {
    encode_collaboration_invocation_key(encoder, &receipt.key);
    encoder.tag(match receipt.kind {
        CollaborationInvocationKind::SpawnAgent => 0,
        CollaborationInvocationKind::SendMessage => 1,
        CollaborationInvocationKind::StopAgent => 2,
        CollaborationInvocationKind::SteerAgent => 3,
        CollaborationInvocationKind::RetryAgent => 4,
    });
    encoder.u64(receipt.input_digest.len() as u64);
    for byte in receipt.input_digest {
        encoder.tag(byte);
    }
    encode_collaboration_invocation_output(encoder, &receipt.output);
}

/// 规范编码 Agent 调度状态。
fn encode_agent_status(encoder: &mut CanonicalDigest, status: &CollaborationAgentStatus) {
    match status {
        CollaborationAgentStatus::PendingInit => encoder.tag(0),
        CollaborationAgentStatus::Idle => encoder.tag(1),
        CollaborationAgentStatus::WaitingCapacity { turn_id } => {
            encoder.tag(2);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Running { turn_id } => {
            encoder.tag(3);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Cancelling { turn_id } => {
            encoder.tag(4);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Completed {
            turn_id,
            final_message,
        } => {
            encoder.tag(5);
            encoder.text(turn_id.as_str());
            encode_optional_text(encoder, final_message.as_deref());
        }
        CollaborationAgentStatus::Interrupted { turn_id } => {
            encoder.tag(6);
            encoder.text(turn_id.as_str());
        }
        CollaborationAgentStatus::Failed { turn_id, message } => {
            encoder.tag(7);
            encoder.text(turn_id.as_str());
            encoder.text(message);
        }
        CollaborationAgentStatus::Stopped => encoder.tag(8),
    }
}

/// 规范编码 Turn 的触发原因。
fn encode_turn_cause(encoder: &mut CanonicalDigest, cause: &AgentTurnCause) {
    match cause {
        AgentTurnCause::RootUser => encoder.tag(0),
        AgentTurnCause::InitialTask => encoder.tag(1),
        AgentTurnCause::Followup { message_id } => {
            encoder.tag(2);
            encoder.text(message_id.as_str());
        }
        AgentTurnCause::Retry { previous_turn_id } => {
            encoder.tag(3);
            encoder.text(previous_turn_id.as_str());
        }
    }
}

/// 规范编码 Turn 终态。
fn encode_turn_outcome(encoder: &mut CanonicalDigest, outcome: &AgentTurnOutcome) {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => {
            encoder.tag(0);
            encode_optional_text(encoder, final_message.as_deref());
        }
        AgentTurnOutcome::Interrupted => encoder.tag(1),
        AgentTurnOutcome::Failed { message } => {
            encoder.tag(2);
            encoder.text(message);
        }
    }
}

/// 规范编码 mailbox 消息和完整因果字段。
fn encode_mailbox_message(encoder: &mut CanonicalDigest, message: &MailboxMessage) {
    encoder.text(message.message_id.as_str());
    encoder.u64(message.sequence);
    encoder.text(message.source_agent_id.as_str());
    encoder.text(message.target_agent_id.as_str());
    encoder.tag(match message.delivery {
        MailboxDelivery::QueueOnly => 0,
        MailboxDelivery::TriggerTurn => 1,
    });
    match &message.kind {
        MailboxMessageKind::AgentMessage => encoder.tag(0),
        MailboxMessageKind::ChildTurnFinished { outcome } => {
            encoder.tag(1);
            encode_turn_outcome(encoder, outcome);
        }
    }
    encoder.text(&message.content);
    encode_optional_turn(encoder, message.related_turn_id.as_ref());
    encode_optional_turn(encoder, message.parent_turn_id.as_ref());
    encode_optional_turn(encoder, message.root_turn_id.as_ref());
}

/// 规范编码一个 Collaboration 领域事件，新增字段必须在此显式纳入批次身份。
fn encode_collaboration_event(encoder: &mut CanonicalDigest, event: &CollaborationEvent) {
    encoder.text(event.session_id.as_str());
    encode_optional_turn(encoder, event.turn_id.as_ref());
    encoder.text(event.source_agent_id.as_str());
    encoder.text(event.agent_id.as_str());
    encode_optional_agent(encoder, event.parent_agent_id.as_ref());
    encoder.text(event.agent_path.as_str());
    encode_optional_turn(encoder, event.parent_turn_id.as_ref());
    encode_optional_turn(encoder, event.root_turn_id.as_ref());
    encoder.u64(event.sequence);
    match &event.kind {
        CollaborationEventKind::AgentSpawned {
            definition,
            initial_status,
            per_root_turn_limit,
        } => {
            encoder.tag(0);
            encode_agent_definition(encoder, definition);
            encode_agent_status(encoder, initial_status);
            encoder.bool(per_root_turn_limit.is_some());
            if let Some(limit) = per_root_turn_limit {
                encoder.u64(*limit as u64);
            }
        }
        CollaborationEventKind::AgentStatusChanged { previous, current } => {
            encoder.tag(1);
            encode_agent_status(encoder, previous);
            encode_agent_status(encoder, current);
        }
        CollaborationEventKind::AgentMessageQueued { message } => {
            encoder.tag(2);
            encode_mailbox_message(encoder, message);
        }
        CollaborationEventKind::AgentMessagesConsumed {
            message_ids,
            through_sequence,
        } => {
            encoder.tag(3);
            encoder.u64(message_ids.len() as u64);
            for message_id in message_ids {
                encoder.text(message_id.as_str());
            }
            encoder.u64(*through_sequence);
        }
        CollaborationEventKind::AgentCompletionNotificationSuperseded { message_id } => {
            encoder.tag(4);
            encoder.text(message_id.as_str());
        }
        CollaborationEventKind::AgentTurnQueued { cause, prompt } => {
            encoder.tag(5);
            encode_turn_cause(encoder, cause);
            encode_optional_text(encoder, prompt.as_deref());
        }
        CollaborationEventKind::AgentTurnStarted { cause } => {
            encoder.tag(6);
            encode_turn_cause(encoder, cause);
        }
        CollaborationEventKind::AgentTurnDispatchAcknowledged => encoder.tag(7),
        CollaborationEventKind::AgentTurnCompleted { final_message } => {
            encoder.tag(8);
            encode_optional_text(encoder, final_message.as_deref());
        }
        CollaborationEventKind::AgentTurnInterrupted => encoder.tag(9),
        CollaborationEventKind::AgentTurnFailed { message } => {
            encoder.tag(10);
            encoder.text(message);
        }
        CollaborationEventKind::AgentUserSteered { steer } => {
            encoder.tag(11);
            encoder.u64(steer.sequence);
            encoder.text(steer.turn_id.as_str());
            encoder.text(&steer.content);
        }
        CollaborationEventKind::AgentUserSteersConsumed { sequences } => {
            encoder.tag(12);
            encoder.u64(sequences.len() as u64);
            for sequence in sequences {
                encoder.u64(*sequence);
            }
        }
        CollaborationEventKind::AgentTreeClosing => encoder.tag(13),
        CollaborationEventKind::AgentTreeQuiesced => encoder.tag(14),
        CollaborationEventKind::AgentTreeCleanupCompleted => encoder.tag(15),
        CollaborationEventKind::CollaborationInvocationCommitted { receipt } => {
            encoder.tag(16);
            encode_collaboration_invocation_receipt(encoder, receipt);
        }
        CollaborationEventKind::AgentMessagesClaimed {
            message_ids,
            through_sequence,
        } => {
            encoder.tag(17);
            encoder.u64(message_ids.len() as u64);
            for message_id in message_ids {
                encoder.text(message_id.as_str());
            }
            encoder.u64(*through_sequence);
        }
        CollaborationEventKind::AgentUserSteersClaimed { sequences } => {
            encoder.tag(18);
            encoder.u64(sequences.len() as u64);
            for sequence in sequences {
                encoder.u64(*sequence);
            }
        }
    }
}

/// 规范编码可恢复 Turn 快照。
fn encode_recovered_turn(encoder: &mut CanonicalDigest, turn: &RecoveredTurn) {
    encoder.text(turn.turn_id.as_str());
    encode_turn_cause(encoder, &turn.cause);
    encode_optional_text(encoder, turn.prompt.as_deref());
    encode_optional_turn(encoder, turn.parent_turn_id.as_ref());
    encoder.text(turn.root_turn_id.as_str());
    encode_turn_outcome(encoder, &turn.outcome);
}

/// 规范编码一个完整单 Agent 恢复状态。
fn encode_recovered_agent(encoder: &mut CanonicalDigest, agent: &RecoveredAgent) {
    encode_agent_definition(encoder, &agent.definition);
    encode_agent_status(encoder, &agent.status);
    encoder.u64(agent.mailbox.len() as u64);
    for mailbox in &agent.mailbox {
        encode_mailbox_message(encoder, &mailbox.message);
        encode_optional_turn(encoder, mailbox.claimed_turn_id.as_ref());
    }
    encoder.u64(agent.next_mailbox_sequence);
    encode_optional_turn(encoder, agent.mailbox_claim_turn_id.as_ref());
    encoder.bool(agent.mailbox_claim_through_sequence.is_some());
    if let Some(sequence) = agent.mailbox_claim_through_sequence {
        encoder.u64(sequence);
    }
    encoder.u64(agent.next_steer_sequence);
    encode_optional_turn(encoder, agent.steer_claim_turn_id.as_ref());
    encoder.bool(agent.steer_claim_through_sequence.is_some());
    if let Some(sequence) = agent.steer_claim_through_sequence {
        encoder.u64(sequence);
    }
    encoder.bool(agent.last_turn.is_some());
    if let Some(turn) = &agent.last_turn {
        encode_recovered_turn(encoder, turn);
    }
    encode_optional_agent(encoder, agent.current_source_agent_id.as_ref());
    encoder.bool(agent.current_turn_cause.is_some());
    if let Some(cause) = &agent.current_turn_cause {
        encode_turn_cause(encoder, cause);
    }
    encode_optional_text(encoder, agent.current_turn_prompt.as_deref());
    encode_optional_turn(encoder, agent.current_parent_turn_id.as_ref());
    encode_optional_turn(encoder, agent.current_root_turn_id.as_ref());
    encoder.bool(agent.current_plan_guard.is_some());
    if let Some(plan_guard) = agent.current_plan_guard {
        encoder.tag(match plan_guard.state() {
            PlanGuardState::Inactive => 0,
            PlanGuardState::ReadOnly => 1,
        });
    }
    encoder.u64(agent.pending_steers.len() as u64);
    for steer in &agent.pending_steers {
        encoder.u64(steer.sequence);
        encoder.text(steer.turn_id.as_str());
        encoder.text(&steer.content);
    }
    encoder.bool(agent.start_pending);
}

/// 对局部驱逐 checkpoint 计算版本化规范摘要。
fn recovered_agent_checkpoint_digest(checkpoint: &RecoveredAgentCheckpoint) -> [u8; 32] {
    let mut encoder = CanonicalDigest::new(b"keencode.collaboration.agent-checkpoint.v3");
    encoder.text(checkpoint.root_agent_id.as_str());
    encoder.u64(checkpoint.revision);
    encode_recovered_agent(&mut encoder, &checkpoint.agent);
    encoder.finish_bytes()
}

/// 返回 Turn 终态中由模型或执行器提供的动态文本字节数。
fn turn_outcome_text_bytes(outcome: &AgentTurnOutcome) -> usize {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => {
            final_message.as_ref().map_or(0, String::len)
        }
        AgentTurnOutcome::Interrupted => 0,
        AgentTurnOutcome::Failed { message } => message.len(),
    }
}

/// 返回最近 Turn 记录中除 mailbox 与 steer 外的动态文本字节数。
fn recovered_turn_text_bytes(turn: &RecoveredTurn) -> usize {
    turn.prompt
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(turn_outcome_text_bytes(&turn.outcome))
}

/// 返回单 Agent 恢复状态中除 mailbox 与 steer 外的动态文本字节数。
fn recovered_agent_dynamic_text_bytes(agent: &RecoveredAgent) -> usize {
    agent
        .last_turn
        .as_ref()
        .map_or(0, recovered_turn_text_bytes)
        .saturating_add(agent.current_turn_prompt.as_ref().map_or(0, String::len))
}

/// 返回不可变 Agent 配置中由用户提供的文本字节数。
fn agent_definition_text_bytes(definition: &AgentDefinition) -> usize {
    let mut total = definition.profile.model.len();
    total = total.saturating_add(
        definition
            .profile
            .reasoning_effort
            .as_ref()
            .map_or(0, String::len),
    );
    total = total.saturating_add(
        definition
            .profile
            .worktree_lease
            .as_ref()
            .map_or(0, |lease| lease.as_str().len()),
    );
    total = total.saturating_add(
        definition
            .profile
            .tool_snapshot
            .iter()
            .map(String::len)
            .sum::<usize>(),
    );
    total = total.saturating_add(
        definition
            .context_snapshot
            .iter()
            .map(String::len)
            .sum::<usize>(),
    );
    if let Some(template) = &definition.agent_template {
        total = total
            .saturating_add(template.name.len())
            .saturating_add(template.system_prompt.len())
            .saturating_add(
                template
                    .allowed_write_dirs
                    .iter()
                    .map(|directory| directory.as_os_str().to_string_lossy().len())
                    .sum::<usize>(),
            );
    }
    total
}

/// 返回一条协作工具幂等记录保留的键、摘要与结果字节数。
fn collaboration_invocation_text_bytes(
    key: &CollaborationInvocationKey,
    record: &CollaborationInvocationRecord,
) -> usize {
    let total = key
        .source_agent_id
        .as_str()
        .len()
        .saturating_add(key.source_turn_id.as_str().len())
        .saturating_add(key.tool_call_id.as_str().len())
        .saturating_add(record.input_digest.len());
    total.saturating_add(match &record.output {
        CollaborationInvocationOutput::SpawnedAgent(spawned) => spawned
            .agent
            .agent_id
            .as_str()
            .len()
            .saturating_add(spawned.agent.session_id.as_str().len())
            .saturating_add(spawned.agent.path.as_str().len())
            .saturating_add(spawned.initial_turn_id.as_str().len()),
        CollaborationInvocationOutput::Message {
            message_id,
            triggered_turn_id,
        } => message_id.as_str().len().saturating_add(
            triggered_turn_id
                .as_ref()
                .map_or(0, |turn_id| turn_id.as_str().len()),
        ),
        CollaborationInvocationOutput::StoppedAgent {
            target_agent_id,
            stopped_turn_id,
        } => target_agent_id
            .as_str()
            .len()
            .saturating_add(stopped_turn_id.as_str().len()),
        CollaborationInvocationOutput::UserSteer(steer) => steer
            .turn_id
            .as_str()
            .len()
            .saturating_add(steer.content.len()),
        CollaborationInvocationOutput::RetriedAgent {
            target_agent_id,
            retry_turn_id,
        } => target_agent_id
            .as_str()
            .len()
            .saturating_add(retry_turn_id.as_str().len()),
    })
}

/// 校验协调器级 Agent、mailbox、steer、幂等记录和总文本硬配额。
fn validate_coordinator_quotas(state: &CoordinatorState) -> Result<(), CollaborationError> {
    if state.collaboration_invocations.len() > MAX_COLLABORATION_INVOCATIONS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器协作工具幂等记录数量",
            maximum: MAX_COLLABORATION_INVOCATIONS_PER_COORDINATOR,
        });
    }
    if state.root_turn_bindings.len() > MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器外部根 Turn 幂等绑定数量",
            maximum: MAX_ROOT_TURN_BINDINGS_PER_COORDINATOR,
        });
    }
    let agent_count = state
        .roots
        .values()
        .map(|root| root.known_agents.len())
        .sum::<usize>();
    if agent_count > MAX_AGENTS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器 Agent 身份数量",
            maximum: MAX_AGENTS_PER_COORDINATOR,
        });
    }
    let mailbox_count = state
        .roots
        .values()
        .map(|root| root.mailbox_count)
        .sum::<usize>();
    if mailbox_count > MAX_MAILBOX_MESSAGES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费 mailbox 消息数量",
            maximum: MAX_MAILBOX_MESSAGES_PER_COORDINATOR,
        });
    }
    let mailbox_bytes = state
        .roots
        .values()
        .map(|root| root.mailbox_bytes)
        .sum::<usize>();
    if mailbox_bytes > MAX_MAILBOX_BYTES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费 mailbox 正文字节数",
            maximum: MAX_MAILBOX_BYTES_PER_COORDINATOR,
        });
    }
    let resident_steer_count = state
        .agents
        .values()
        .map(|agent| agent.steers.len())
        .sum::<usize>();
    let evicted_steer_count = state
        .roots
        .values()
        .flat_map(|root| root.evicted_agent_checkpoints.values())
        .map(|checkpoint| checkpoint.steer_count)
        .sum::<usize>();
    let steer_count = resident_steer_count.saturating_add(evicted_steer_count);
    if steer_count > MAX_PENDING_STEERS_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费用户 steer 数量",
            maximum: MAX_PENDING_STEERS_PER_COORDINATOR,
        });
    }
    let resident_steer_bytes = state
        .agents
        .values()
        .map(|agent| agent.steer_bytes)
        .sum::<usize>();
    let evicted_steer_bytes = state
        .roots
        .values()
        .flat_map(|root| root.evicted_agent_checkpoints.values())
        .map(|checkpoint| checkpoint.steer_bytes)
        .sum::<usize>();
    let steer_bytes = resident_steer_bytes.saturating_add(evicted_steer_bytes);
    if steer_bytes > MAX_PENDING_STEER_BYTES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器未消费用户 steer 正文字节数",
            maximum: MAX_PENDING_STEER_BYTES_PER_COORDINATOR,
        });
    }
    let definition_bytes = state
        .roots
        .values()
        .flat_map(|root| root.known_agents.values())
        .map(agent_definition_text_bytes)
        .sum::<usize>();
    let resident_dynamic_bytes = state
        .agents
        .values()
        .map(|agent| {
            agent.last_turn.as_ref().map_or(0, |turn| {
                turn.prompt
                    .as_ref()
                    .map_or(0, String::len)
                    .saturating_add(turn_outcome_text_bytes(&turn.outcome))
            })
        })
        .sum::<usize>();
    let evicted_dynamic_bytes = state
        .roots
        .values()
        .flat_map(|root| root.evicted_agent_checkpoints.values())
        .map(|checkpoint| checkpoint.dynamic_text_bytes)
        .sum::<usize>();
    let pending_prompt_bytes = state
        .pending_turns
        .iter()
        .map(|turn| turn.prompt.as_ref().map_or(0, String::len))
        .sum::<usize>();
    let active_prompt_bytes = state
        .active_turns
        .values()
        .map(|turn| turn.prompt.as_ref().map_or(0, String::len))
        .sum::<usize>();
    let invocation_bytes =
        state
            .collaboration_invocations
            .iter()
            .fold(0usize, |total, (key, record)| {
                total.saturating_add(collaboration_invocation_text_bytes(key, record))
            });
    let root_turn_binding_bytes =
        state
            .root_turn_bindings
            .iter()
            .fold(0_usize, |total, (turn_id, binding)| {
                total
                    .saturating_add(turn_id.as_str().len())
                    .saturating_add(binding.root_agent_id.as_str().len())
                    .saturating_add(binding.prompt_digest.len())
            });
    let retained_text_bytes = mailbox_bytes
        .saturating_add(steer_bytes)
        .saturating_add(definition_bytes)
        .saturating_add(resident_dynamic_bytes)
        .saturating_add(evicted_dynamic_bytes)
        .saturating_add(pending_prompt_bytes)
        .saturating_add(active_prompt_bytes)
        .saturating_add(invocation_bytes)
        .saturating_add(root_turn_binding_bytes);
    if retained_text_bytes > MAX_RETAINED_TEXT_BYTES_PER_COORDINATOR {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "协调器保留文本总字节数",
            maximum: MAX_RETAINED_TEXT_BYTES_PER_COORDINATOR,
        });
    }
    Ok(())
}

/// 从同一个协调器状态生成一棵根树的完整冷恢复快照。
fn checkpoint_root_from_state(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<RecoveredAgentTree, CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    let mut agents = state
        .agents
        .values()
        .filter(|agent| &agent.definition.root_agent_id == root_agent_id)
        .map(|agent| recovered_agent_from_entry(state, agent))
        .collect::<Result<Vec<_>, _>>()?;
    agents.sort_by(|left, right| left.definition.path.cmp(&right.definition.path));
    let mut known_agents = root.known_agents.values().cloned().collect::<Vec<_>>();
    known_agents.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(RecoveredAgentTree {
        root_agent_id: root.root_agent_id.clone(),
        root_session_id: root.root_session_id.clone(),
        per_root_turn_limit: root.turn_limit,
        lifecycle: root.lifecycle,
        live: true,
        next_turn_sequence: root.next_turn_sequence,
        next_checkpoint_revision: root.next_checkpoint_revision,
        known_agents,
        agents,
    })
}

/// 从同一个候选状态生成事件批次必须原子携带的完整协调器 checkpoint。
fn checkpoint_coordinator_from_state(
    state: &CoordinatorState,
    store: &dyn CollaborationStore,
) -> Result<RecoveredCoordinator, CollaborationError> {
    let mut root_ids = state.roots.keys().cloned().collect::<Vec<_>>();
    root_ids.sort();
    let mut roots = root_ids
        .iter()
        .map(|root_agent_id| checkpoint_root_with_store(state, root_agent_id, store))
        .collect::<Result<Vec<_>, _>>()?;
    normalize_recovered_root_order(&mut roots);
    let mut invocations = state
        .collaboration_invocations
        .iter()
        .map(|(key, record)| RecoveredCollaborationInvocation {
            key: key.clone(),
            kind: record.kind,
            input_digest: record.input_digest,
            output: record.output.clone(),
        })
        .collect::<Vec<_>>();
    normalize_recovered_invocation_order(&mut invocations);
    let mut root_turn_bindings = state
        .root_turn_bindings
        .iter()
        .map(|(turn_id, binding)| RecoveredRootTurnBinding {
            turn_id: turn_id.clone(),
            root_agent_id: binding.root_agent_id.clone(),
            prompt_digest: binding.prompt_digest,
            plan_guard: binding.plan_guard,
        })
        .collect::<Vec<_>>();
    root_turn_bindings.sort_by(|left, right| left.turn_id.cmp(&right.turn_id));
    Ok(RecoveredCoordinator {
        last_event_sequence: state.last_event_sequence,
        root_identity_namespace: state.root_identity_namespace.clone(),
        next_root_sequence: state.next_root_sequence,
        roots,
        invocations,
        root_turn_bindings,
    })
}

/// 以驱逐前持久摘要为锚点合并非驻留 Agent，生成真正自包含的 checkpoint。
fn checkpoint_root_with_store(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
    store: &dyn CollaborationStore,
) -> Result<RecoveredAgentTree, CollaborationError> {
    let mut checkpoint = checkpoint_root_from_state(state, root_agent_id)?;
    let root = state
        .roots
        .get(root_agent_id)
        .expect("checkpoint 根树在上方已校验");
    let resident_ids = checkpoint
        .agents
        .iter()
        .map(|agent| agent.definition.agent_id.clone())
        .collect::<HashSet<_>>();
    for definition in root.known_agents.values() {
        if resident_ids.contains(&definition.agent_id) {
            continue;
        }
        let expected = root
            .evicted_agent_checkpoints
            .get(&definition.agent_id)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "非驻留 Agent 缺少局部 checkpoint 引用".to_owned(),
            })?;
        let recovered_checkpoint = store
            .load_agent_checkpoint(&definition.agent_id)
            .map_err(|error| CollaborationError::Store {
                message: error.message().to_owned(),
            })?
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: definition.agent_id.clone(),
            })?;
        if recovered_checkpoint.root_agent_id != *root_agent_id
            || recovered_checkpoint.revision != expected.revision
            || recovered_agent_checkpoint_digest(&recovered_checkpoint) != expected.digest
            || recovered_checkpoint.agent.definition != *definition
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "非驻留 Agent 局部 checkpoint 的根身份、修订号或摘要不一致".to_owned(),
            });
        }
        checkpoint.agents.push(recovered_checkpoint.agent);
    }
    checkpoint
        .agents
        .sort_by(|left, right| left.definition.path.cmp(&right.definition.path));
    if checkpoint.agents.len() != root.known_agents.len() {
        return Err(CollaborationError::InvalidRecovery {
            message: "自包含 checkpoint 未覆盖全部已知 Agent".to_owned(),
        });
    }
    Ok(checkpoint)
}

/// 从不可变 Agent 定义生成确定性排序的幂等全树静止命令。
fn quiesce_request_from_definitions(
    root_agent_id: &AgentId,
    root_session_id: &SessionId,
    definitions: &[AgentDefinition],
) -> QuiesceAgentTree {
    let mut definitions = definitions.to_vec();
    definitions.sort_by(|left, right| left.path.cmp(&right.path));
    QuiesceAgentTree {
        root_agent_id: root_agent_id.clone(),
        root_session_id: root_session_id.clone(),
        agent_ids: definitions
            .iter()
            .map(|definition| definition.agent_id.clone())
            .collect(),
    }
}

/// 从不可变 Agent 定义生成确定性排序的幂等全树清理命令。
fn close_request_from_definitions(
    root_agent_id: &AgentId,
    root_session_id: &SessionId,
    definitions: &[AgentDefinition],
) -> CloseAgentTree {
    let mut definitions = definitions.to_vec();
    definitions.sort_by(|left, right| left.path.cmp(&right.path));
    let agent_ids = definitions
        .iter()
        .map(|definition| definition.agent_id.clone())
        .collect();
    let mut worktree_leases = definitions
        .iter()
        .filter_map(|definition| definition.profile.worktree_lease.clone())
        .collect::<Vec<_>>();
    worktree_leases.sort();
    CloseAgentTree {
        root_agent_id: root_agent_id.clone(),
        root_session_id: root_session_id.clone(),
        agent_ids,
        worktree_leases,
    }
}

/// 在清理确认事件进入同一提交批次后卸载已关闭根树的全部驻留历史。
fn unload_closed_root(
    state: &mut CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    if root.lifecycle != RecoveredRootLifecycle::CleanupPending || root.in_use != 0 {
        return Err(CollaborationError::InvalidRecovery {
            message: "只有已关闭且无槽位占用的根树可以卸载".to_owned(),
        });
    }
    if state
        .pending_turns
        .iter()
        .any(|turn| &turn.root_agent_id == root_agent_id)
        || state
            .active_turns
            .values()
            .any(|turn| &turn.root_agent_id == root_agent_id)
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "卸载已关闭根树时仍存在未决 Turn".to_owned(),
        });
    }
    let agent_ids = root.known_agents.keys().cloned().collect::<HashSet<_>>();
    state
        .collaboration_invocations
        .retain(|key, _record| !agent_ids.contains(&key.source_agent_id));
    state
        .root_turn_bindings
        .retain(|_turn_id, binding| &binding.root_agent_id != root_agent_id);
    state
        .agents
        .retain(|agent_id, _agent| !agent_ids.contains(agent_id));
    state
        .start_outbox
        .retain(|_turn_id, launch| &launch.agent.root_agent_id != root_agent_id);
    state
        .signal_outbox
        .retain(|_turn_id, signal| !agent_ids.contains(&signal.agent_id));
    state.close_outbox.remove(root_agent_id);
    state.roots.remove(root_agent_id);
    Ok(())
}

/// 在关闭根树前按驱逐摘要物化全部非驻留 Agent，使关闭 checkpoint 自包含。
fn materialize_evicted_agents_for_root(
    state: &mut CoordinatorState,
    store: &dyn CollaborationStore,
    root_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    let mut evicted = root
        .evicted_agent_checkpoints
        .iter()
        .map(|(agent_id, checkpoint_ref)| {
            let definition = root
                .known_agents
                .get(agent_id)
                .expect("驱逐摘要必须对应已知 Agent")
                .clone();
            (
                definition.path.clone(),
                agent_id.clone(),
                checkpoint_ref.clone(),
                definition,
            )
        })
        .collect::<Vec<_>>();
    evicted.sort_by(|left, right| left.0.cmp(&right.0));
    for (_path, agent_id, expected, expected_definition) in evicted {
        let recovered_checkpoint = store
            .load_agent_checkpoint(&agent_id)
            .map_err(|error| CollaborationError::Store {
                message: error.message().to_owned(),
            })?
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: agent_id.clone(),
            })?;
        if recovered_checkpoint.root_agent_id != *root_agent_id
            || recovered_checkpoint.revision != expected.revision
            || recovered_agent_checkpoint_digest(&recovered_checkpoint) != expected.digest
            || recovered_checkpoint.agent.definition != expected_definition
            || !recovered_checkpoint.agent.status.is_idle()
            || !recovered_terminal_state_matches(&recovered_checkpoint.agent)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "关闭根树时驱逐 Agent 与可信摘要或终态不一致".to_owned(),
            });
        }
        state.agents.insert(
            agent_id.clone(),
            agent_entry_from_recovered(&recovered_checkpoint.agent),
        );
        state
            .roots
            .get_mut(root_agent_id)
            .expect("物化驱逐 Agent 的根树在上方已校验")
            .evicted_agent_checkpoints
            .remove(&agent_id);
    }
    let root = state
        .roots
        .get(root_agent_id)
        .expect("物化驱逐 Agent 的根树在上方已校验");
    let resident_count = state
        .agents
        .values()
        .filter(|agent| &agent.definition.root_agent_id == root_agent_id)
        .count();
    if resident_count != root.known_agents.len() || !root.evicted_agent_checkpoints.is_empty() {
        return Err(CollaborationError::InvalidRecovery {
            message: "关闭根树前没有物化全部已知 Agent".to_owned(),
        });
    }
    Ok(())
}

/// 对同一可信调用身份执行成功重放，输入改变时返回稳定冲突。
fn replay_collaboration_invocation(
    state: &CoordinatorState,
    key: &CollaborationInvocationKey,
    input: &CollaborationInvocationInput,
) -> Result<Option<CollaborationInvocationOutput>, CollaborationError> {
    let Some(record) = state.collaboration_invocations.get(key) else {
        return Ok(None);
    };
    let kind = collaboration_invocation_kind(input);
    let input_digest = collaboration_invocation_input_digest(input);
    if record.kind == kind && record.input_digest == input_digest {
        return Ok(Some(record.output.clone()));
    }
    Err(CollaborationError::IdempotencyConflict {
        source_agent_id: key.source_agent_id.clone(),
        source_turn_id: key.source_turn_id.clone(),
        tool_call_id: key.tool_call_id.clone(),
    })
}

/// 将首次成功结果写入当前候选状态，禁止覆盖任何既有可信调用身份。
fn record_collaboration_invocation(
    state: &mut CoordinatorState,
    key: CollaborationInvocationKey,
    input: CollaborationInvocationInput,
    output: CollaborationInvocationOutput,
) -> Result<CollaborationInvocationReceipt, CollaborationError> {
    let kind = collaboration_invocation_kind(&input);
    if !collaboration_invocation_types_match(kind, &output) {
        return Err(CollaborationError::InvalidRecovery {
            message: "协作工具幂等记录的输入与结果类型不匹配".to_owned(),
        });
    }
    if state.collaboration_invocations.contains_key(&key) {
        return Err(CollaborationError::IdempotencyConflict {
            source_agent_id: key.source_agent_id,
            source_turn_id: key.source_turn_id,
            tool_call_id: key.tool_call_id,
        });
    }
    let input_digest = collaboration_invocation_input_digest(&input);
    let receipt = CollaborationInvocationReceipt {
        key: key.clone(),
        kind,
        input_digest,
        output: output.clone(),
    };
    state.collaboration_invocations.insert(
        key,
        CollaborationInvocationRecord {
            kind,
            input_digest,
            output,
        },
    );
    Ok(receipt)
}

/// 返回完整临时业务输入对应的不含正文操作类型。
fn collaboration_invocation_kind(
    input: &CollaborationInvocationInput,
) -> CollaborationInvocationKind {
    match input {
        CollaborationInvocationInput::SpawnAgent(_) => CollaborationInvocationKind::SpawnAgent,
        CollaborationInvocationInput::SendMessage { .. } => {
            CollaborationInvocationKind::SendMessage
        }
        CollaborationInvocationInput::StopAgent { .. } => CollaborationInvocationKind::StopAgent,
        CollaborationInvocationInput::SteerAgent { .. } => CollaborationInvocationKind::SteerAgent,
        CollaborationInvocationInput::RetryAgent { .. } => CollaborationInvocationKind::RetryAgent,
    }
}

/// 判断持久幂等记录是否保存了与操作类型一致的结果。
fn collaboration_invocation_types_match(
    kind: CollaborationInvocationKind,
    output: &CollaborationInvocationOutput,
) -> bool {
    matches!(
        (kind, output),
        (
            CollaborationInvocationKind::SpawnAgent,
            CollaborationInvocationOutput::SpawnedAgent(_)
        ) | (
            CollaborationInvocationKind::SendMessage,
            CollaborationInvocationOutput::Message { .. }
        ) | (
            CollaborationInvocationKind::StopAgent,
            CollaborationInvocationOutput::StoppedAgent { .. }
        ) | (
            CollaborationInvocationKind::SteerAgent,
            CollaborationInvocationOutput::UserSteer(_)
        ) | (
            CollaborationInvocationKind::RetryAgent,
            CollaborationInvocationOutput::RetriedAgent { .. }
        )
    )
}

/// 在候选状态内追加一条用户 steer、对应权威事件和安全边界唤醒动作。
fn queue_user_steer(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    content: String,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<UserSteer, CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let root_agent_id = agent.definition.root_agent_id.clone();
    ensure_tree_open(state, &root_agent_id)?;
    match &agent.status {
        CollaborationAgentStatus::Running {
            turn_id: active_turn_id,
        } if active_turn_id == turn_id => {}
        CollaborationAgentStatus::Running { .. } => {
            return Err(CollaborationError::TurnMismatch {
                agent_id: agent_id.clone(),
                turn_id: turn_id.clone(),
            });
        }
        _ => {
            return Err(CollaborationError::TargetNotRunning {
                agent_id: agent_id.clone(),
            });
        }
    }
    let active = active_turn_for_agent(state, agent_id, turn_id)?;
    let agent = resident_agent(state, agent_id)?;
    if agent.steers.len() >= MAX_PENDING_STEERS_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "未消费用户 Steer 数量",
            maximum: MAX_PENDING_STEERS_PER_AGENT,
        });
    }
    let next_steer_bytes = agent
        .steer_bytes
        .checked_add(content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    if next_steer_bytes > MAX_PENDING_STEER_BYTES_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "未消费用户 Steer 总字节数",
            maximum: MAX_PENDING_STEER_BYTES_PER_AGENT,
        });
    }
    let sequence = agent.next_steer_sequence;
    let next_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    let definition = agent.definition.clone();
    let steer = UserSteer {
        sequence,
        turn_id: turn_id.clone(),
        content,
    };
    let agent = state.agents.get_mut(agent_id).expect("Agent 在上方已校验");
    agent.next_steer_sequence = next_sequence;
    agent.steer_bytes = next_steer_bytes;
    agent.steers.push_back(steer.clone());
    push_event(
        state,
        events,
        &definition,
        EventLink {
            source_agent_id: agent_id.clone(),
            turn_id: Some(turn_id.clone()),
            parent_turn_id: active.parent_turn_id,
            root_turn_id: Some(active.root_turn_id),
        },
        CollaborationEventKind::AgentUserSteered {
            steer: steer.clone(),
        },
    )?;
    mark_activity(state, agent_id, actions)?;
    queue_turn_signal(
        state,
        agent_id,
        turn_id,
        AgentTurnSignalKind::UserSteer,
        actions,
    )?;
    Ok(steer)
}

/// 返回已驻留 Agent，或生成类型化不存在错误。
fn resident_agent<'a>(
    state: &'a CoordinatorState,
    agent_id: &AgentId,
) -> Result<&'a AgentEntry, CollaborationError> {
    state
        .agents
        .get(agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: agent_id.clone(),
        })
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

/// 为活跃或崩溃后中断的输入 claim 恢复稳定事件因果字段。
fn input_claim_event_context(
    state: &CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
) -> Result<(AgentDefinition, Option<TurnId>, TurnId), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    if let Some(active) = state
        .active_turns
        .get(turn_id)
        .filter(|active| &active.agent_id == agent_id)
    {
        return Ok((
            agent.definition.clone(),
            active.parent_turn_id.clone(),
            active.root_turn_id.clone(),
        ));
    }
    if let Some(last_turn) = agent
        .last_turn
        .as_ref()
        .filter(|last_turn| &last_turn.turn_id == turn_id)
    {
        return Ok((
            agent.definition.clone(),
            last_turn.parent_turn_id.clone(),
            last_turn.root_turn_id.clone(),
        ));
    }
    Err(CollaborationError::InputClaimMismatch {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        input_kind: "可恢复",
    })
}

/// 将崩溃、取消或失败 Turn 遗留的输入及其 claim 原子重绑定到后续 Turn。
fn rebind_pending_inputs(agent: &mut AgentEntry, previous_turn_id: &TurnId, turn_id: &TurnId) {
    for steer in &mut agent.steers {
        if &steer.turn_id == previous_turn_id {
            steer.turn_id = turn_id.clone();
        }
    }
    if agent
        .mailbox_claim
        .as_ref()
        .is_some_and(|claim| &claim.turn_id == previous_turn_id)
    {
        agent
            .mailbox_claim
            .as_mut()
            .expect("mailbox claim 在上方已确认存在")
            .turn_id = turn_id.clone();
    }
    if agent
        .steer_claim
        .as_ref()
        .is_some_and(|claim| &claim.turn_id == previous_turn_id)
    {
        agent
            .steer_claim
            .as_mut()
            .expect("steer claim 在上方已确认存在")
            .turn_id = turn_id.clone();
    }
}

/// 校验根树存在且尚未关闭。
fn ensure_tree_open(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    let root = state
        .roots
        .get(root_agent_id)
        .ok_or_else(|| CollaborationError::AgentNotFound {
            agent_id: root_agent_id.clone(),
        })?;
    if root.lifecycle != RecoveredRootLifecycle::Open || root.suspended {
        return Err(CollaborationError::TreeClosed {
            root_agent_id: root_agent_id.clone(),
        });
    }
    Ok(())
}

/// 当目标 Agent 未驻留时从 Session Store 恢复并校验其所有者和父链。
fn ensure_agent_loaded(
    state: &mut CoordinatorState,
    store: &dyn CollaborationStore,
    source_agent_id: &AgentId,
    target_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    if state.agents.contains_key(target_agent_id) {
        return Ok(());
    }
    let source = resident_agent(state, source_agent_id)?;
    let source_root_agent_id = source.definition.root_agent_id.clone();
    let root = state
        .roots
        .get(&source_root_agent_id)
        .expect("来源 Agent 所属根树已驻留");
    let expected_target_definition =
        root.known_agents
            .get(target_agent_id)
            .cloned()
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: target_agent_id.clone(),
            })?;
    let expected = root
        .evicted_agent_checkpoints
        .get(target_agent_id)
        .cloned()
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "非驻留目标缺少局部 checkpoint 引用".to_owned(),
        })?;
    let recovered_checkpoint = store
        .load_agent_checkpoint(target_agent_id)
        .map_err(|error| CollaborationError::Store {
            message: error.message().to_owned(),
        })?
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "已知 Agent 缺少局部 checkpoint".to_owned(),
        })?;
    let recovered_agent = &recovered_checkpoint.agent;
    if recovered_agent.definition != expected_target_definition
        || recovered_checkpoint.root_agent_id != source_root_agent_id
        || recovered_checkpoint.revision != expected.revision
        || !recovered_agent.status.is_idle()
        || !recovered_terminal_state_matches(recovered_agent)
        || recovered_agent_checkpoint_digest(&recovered_checkpoint) != expected.digest
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "冷加载 Agent 的定义、终态或驱逐摘要不一致".to_owned(),
        });
    }
    let entry = agent_entry_from_recovered(recovered_agent);
    state.agents.insert(target_agent_id.clone(), entry);
    state
        .roots
        .get_mut(&source_root_agent_id)
        .expect("冷加载目标所属根树应存在")
        .evicted_agent_checkpoints
        .remove(target_agent_id);
    Ok(())
}

/// 从完整协调器 checkpoint 校验并恢复全部协作工具幂等记录。
fn restore_collaboration_invocations(
    state: &mut CoordinatorState,
    recovered: &[RecoveredCollaborationInvocation],
) -> Result<(), CollaborationError> {
    let mut records = HashMap::with_capacity(recovered.len());
    let mut spawned_agent_ids = HashSet::new();
    let mut message_ids = HashSet::new();
    for invocation in recovered {
        if invocation.key.source_agent_id.as_str().len() > MAX_PROFILE_FIELD_BYTES
            || invocation.key.source_turn_id.as_str().len() > MAX_PROFILE_FIELD_BYTES
            || !collaboration_invocation_types_match(invocation.kind, &invocation.output)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "协作工具幂等键越界或输入与结果类型不匹配".to_owned(),
            });
        }
        if records.contains_key(&invocation.key) {
            return Err(CollaborationError::InvalidRecovery {
                message: "协调器 checkpoint 包含重复协作工具幂等键".to_owned(),
            });
        }
        validate_recovered_collaboration_invocation(
            state,
            invocation,
            &mut spawned_agent_ids,
            &mut message_ids,
        )?;
        records.insert(
            invocation.key.clone(),
            CollaborationInvocationRecord {
                kind: invocation.kind,
                input_digest: invocation.input_digest,
                output: invocation.output.clone(),
            },
        );
    }
    state.collaboration_invocations = records;
    Ok(())
}

/// 校验一条恢复幂等记录的树归属、操作类型与首次结果引用。
fn validate_recovered_collaboration_invocation(
    state: &CoordinatorState,
    invocation: &RecoveredCollaborationInvocation,
    spawned_agent_ids: &mut HashSet<AgentId>,
    message_ids: &mut HashSet<MailboxMessageId>,
) -> Result<(), CollaborationError> {
    let source_definition = state
        .roots
        .values()
        .find_map(|root| root.known_agents.get(&invocation.key.source_agent_id))
        .cloned()
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "协作工具幂等记录的来源 Agent 不属于任何恢复根树".to_owned(),
        })?;
    let root = state
        .roots
        .get(&source_definition.root_agent_id)
        .expect("幂等记录来源 Agent 的根树应存在");
    if !state_turn_belongs_to_root(
        state,
        &source_definition.root_agent_id,
        &invocation.key.source_turn_id,
    ) {
        return Err(CollaborationError::InvalidRecovery {
            message: "协作工具幂等记录的来源 Turn 跨根或尚未分配".to_owned(),
        });
    }

    match (invocation.kind, &invocation.output) {
        (
            CollaborationInvocationKind::SpawnAgent,
            CollaborationInvocationOutput::SpawnedAgent(spawned),
        ) => {
            let definition = root
                .known_agents
                .get(&spawned.agent.agent_id)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "SpawnAgent 幂等结果引用了未知子 Agent".to_owned(),
                })?;
            let initial_turn_sequence =
                turn_sequence_for_root(&source_definition.root_agent_id, &spawned.initial_turn_id);
            let source_turn_sequence = turn_sequence_for_root(
                &source_definition.root_agent_id,
                &invocation.key.source_turn_id,
            );
            if source_definition.depth != AgentDepth::ROOT
                || !spawned_agent_ids.insert(spawned.agent.agent_id.clone())
                || definition.depth != AgentDepth::CHILD
                || definition.parent_agent_id.as_ref() != Some(&invocation.key.source_agent_id)
                || definition.session_id != spawned.agent.session_id
                || definition.path != spawned.agent.path
                || !turn_id_belongs_to_root(
                    &source_definition.root_agent_id,
                    root.next_turn_sequence,
                    &spawned.initial_turn_id,
                )
                || initial_turn_sequence.is_none()
                || source_turn_sequence.is_some_and(|source| {
                    initial_turn_sequence.is_some_and(|initial| initial <= source)
                })
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SpawnAgent 幂等结果与来源、定义或初始 Turn 不一致".to_owned(),
                });
            }
        }
        (
            CollaborationInvocationKind::SendMessage,
            CollaborationInvocationOutput::Message {
                message_id,
                triggered_turn_id,
            },
        ) => {
            if !message_ids.insert(message_id.clone())
                || triggered_turn_id.as_ref().is_some_and(|turn_id| {
                    !turn_id_belongs_to_root(
                        &source_definition.root_agent_id,
                        root.next_turn_sequence,
                        turn_id,
                    ) || turn_sequence_for_root(&source_definition.root_agent_id, turn_id).is_none()
                        || turn_sequence_for_root(
                            &source_definition.root_agent_id,
                            &invocation.key.source_turn_id,
                        )
                        .is_some_and(|source| {
                            turn_sequence_for_root(&source_definition.root_agent_id, turn_id)
                                .is_some_and(|triggered| triggered <= source)
                        })
                })
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SendMessage 幂等结果的消息或触发 Turn 无效".to_owned(),
                });
            }
            let matching_entries = state
                .agents
                .values()
                .flat_map(|agent| agent.mailbox.iter())
                .filter(|entry| &entry.message.message_id == message_id)
                .collect::<Vec<_>>();
            if matching_entries.len() > 1 {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SendMessage 幂等结果对应多个 mailbox 条目".to_owned(),
                });
            }
            if let Some(entry) = matching_entries.first() {
                let message = &entry.message;
                if message.source_agent_id != invocation.key.source_agent_id
                    || message.kind != MailboxMessageKind::AgentMessage
                    || message.related_turn_id.as_ref() != Some(&invocation.key.source_turn_id)
                    || message.parent_turn_id.as_ref() != Some(&invocation.key.source_turn_id)
                    || message.root_turn_id.as_ref().is_none_or(|turn_id| {
                        !state_turn_belongs_to_root(
                            state,
                            &source_definition.root_agent_id,
                            turn_id,
                        )
                    })
                    || triggered_turn_id
                        .as_ref()
                        .is_some_and(|turn_id| entry.claimed_turn_id.as_ref() != Some(turn_id))
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "SendMessage 幂等结果与未消费 mailbox 条目不一致".to_owned(),
                    });
                }
            }
        }
        (
            CollaborationInvocationKind::StopAgent,
            CollaborationInvocationOutput::StoppedAgent {
                target_agent_id,
                stopped_turn_id,
            },
        ) => {
            let target_definition = root.known_agents.get(target_agent_id).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "StopAgent 幂等结果引用了未知目标 Agent".to_owned(),
                }
            })?;
            if target_agent_id == &invocation.key.source_agent_id
                || target_definition.depth != AgentDepth::CHILD
                || !turn_id_belongs_to_root(
                    &source_definition.root_agent_id,
                    root.next_turn_sequence,
                    stopped_turn_id,
                )
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "StopAgent 幂等结果与来源、目标或停止 Turn 不一致".to_owned(),
                });
            }
        }
        (
            CollaborationInvocationKind::SteerAgent,
            CollaborationInvocationOutput::UserSteer(steer),
        ) => {
            validate_required_text(&steer.content, "恢复用户 steer")?;
            if steer.sequence == 0 || steer.turn_id != invocation.key.source_turn_id {
                return Err(CollaborationError::InvalidRecovery {
                    message: "SteerAgent 幂等结果与来源 Turn 或序号不一致".to_owned(),
                });
            }
        }
        (
            CollaborationInvocationKind::RetryAgent,
            CollaborationInvocationOutput::RetriedAgent {
                target_agent_id,
                retry_turn_id,
            },
        ) => {
            let target_definition = root.known_agents.get(target_agent_id).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "RetryAgent 幂等结果引用了未知目标 Agent".to_owned(),
                }
            })?;
            let retry_sequence =
                turn_sequence_for_root(&source_definition.root_agent_id, retry_turn_id);
            let source_sequence = turn_sequence_for_root(
                &source_definition.root_agent_id,
                &invocation.key.source_turn_id,
            );
            if target_definition.root_agent_id != source_definition.root_agent_id
                || !turn_id_belongs_to_root(
                    &source_definition.root_agent_id,
                    root.next_turn_sequence,
                    retry_turn_id,
                )
                || retry_sequence.is_none()
                || source_sequence
                    .is_some_and(|source| retry_sequence.is_some_and(|retry| retry <= source))
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "RetryAgent 幂等结果与来源、目标或新 Turn 不一致".to_owned(),
                });
            }
        }
        _ => {
            return Err(CollaborationError::InvalidRecovery {
                message: "协作工具幂等记录的输入与结果类型不匹配".to_owned(),
            });
        }
    }
    Ok(())
}

/// 校验可在进程冷启动时整体恢复的自包含根树。
fn validate_restorable_tree(
    tree: &RecoveredAgentTree,
    external_root_turn_roots: &HashMap<TurnId, AgentId>,
) -> Result<AgentDefinition, CollaborationError> {
    if tree.per_root_turn_limit == 0 || tree.next_checkpoint_revision == 0 {
        return Err(CollaborationError::InvalidRecovery {
            message: "完整恢复快照的容量或局部 checkpoint counter 无效".to_owned(),
        });
    }
    let root_definition = tree
        .known_agents
        .iter()
        .find(|definition| definition.depth == AgentDepth::ROOT)
        .cloned()
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "完整恢复快照缺少根 Agent".to_owned(),
        })?;
    validate_recovered_tree(
        tree,
        &tree.root_agent_id,
        &tree.root_session_id,
        &root_definition,
        tree.per_root_turn_limit,
        tree.lifecycle,
        external_root_turn_roots,
    )?;
    Ok(root_definition)
}

/// 校验非活跃 Agent 状态与最近 Turn 快照严格对应。
fn recovered_terminal_state_matches(agent: &RecoveredAgent) -> bool {
    match (&agent.status, &agent.last_turn) {
        (CollaborationAgentStatus::Idle, None) => true,
        (
            CollaborationAgentStatus::Completed {
                turn_id,
                final_message,
            },
            Some(last_turn),
        ) => {
            &last_turn.turn_id == turn_id
                && matches!(
                    &last_turn.outcome,
                    AgentTurnOutcome::Completed {
                        final_message: recovered_message
                    } if recovered_message == final_message
                )
        }
        (CollaborationAgentStatus::Interrupted { turn_id }, Some(last_turn)) => {
            &last_turn.turn_id == turn_id
                && matches!(&last_turn.outcome, AgentTurnOutcome::Interrupted)
        }
        (CollaborationAgentStatus::Failed { turn_id, message }, Some(last_turn)) => {
            &last_turn.turn_id == turn_id
                && matches!(
                    &last_turn.outcome,
                    AgentTurnOutcome::Failed {
                        message: recovered_message
                    } if recovered_message == message
                )
        }
        _ => false,
    }
}

/// 返回快照状态中尚未收敛的当前 Turn 标识。
fn recovered_current_turn_id(status: &CollaborationAgentStatus) -> Option<&TurnId> {
    match status {
        CollaborationAgentStatus::WaitingCapacity { turn_id }
        | CollaborationAgentStatus::Running { turn_id }
        | CollaborationAgentStatus::Cancelling { turn_id } => Some(turn_id),
        _ => None,
    }
}

/// 统一表示当前或历史恢复 Turn 的不可变因果字段。
struct RecoveredTurnFields<'a> {
    /// 当前校验的 Turn 标识。
    turn_id: &'a TurnId,
    /// 创建该 Turn 的触发原因。
    cause: &'a AgentTurnCause,
    /// 根任务或初始子任务携带的可选输入。
    prompt: Option<&'a String>,
    /// 直接触发当前 Turn 的父 Turn。
    parent_turn_id: Option<&'a TurnId>,
    /// 当前调用链最初的根 Turn。
    root_turn_id: &'a TurnId,
}

/// 校验一个历史或当前 Turn 的因果字段与根命名空间单调序号一致。
fn validate_recovered_turn_fields(
    definition: &AgentDefinition,
    fields: RecoveredTurnFields<'_>,
    next_turn_sequence: u64,
    external_root_turn_roots: &HashMap<TurnId, AgentId>,
) -> Result<(), CollaborationError> {
    let RecoveredTurnFields {
        turn_id,
        cause,
        prompt,
        parent_turn_id,
        root_turn_id,
    } = fields;
    let belongs = |candidate: &TurnId| {
        turn_id_belongs_to_root(&definition.root_agent_id, next_turn_sequence, candidate)
            || external_root_turn_roots.get(candidate) == Some(&definition.root_agent_id)
    };
    let turn_sequence = turn_sequence_for_root(&definition.root_agent_id, turn_id);
    let parent_sequence =
        parent_turn_id.and_then(|parent| turn_sequence_for_root(&definition.root_agent_id, parent));
    let root_sequence = turn_sequence_for_root(&definition.root_agent_id, root_turn_id);
    if !belongs(turn_id)
        || !belongs(root_turn_id)
        || parent_turn_id.is_some_and(|parent| parent == turn_id || !belongs(parent))
        || parent_sequence
            .zip(turn_sequence)
            .is_some_and(|(parent, current)| parent >= current)
        || root_sequence
            .zip(turn_sequence)
            .is_some_and(|(root, current)| root > current)
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复 Turn 引用了越界、跨根或自循环的 Turn 标识".to_owned(),
        });
    }
    if let Some(prompt) = prompt {
        validate_required_text(prompt, "恢复 Turn 输入").map_err(|error| {
            CollaborationError::InvalidRecovery {
                message: error.to_string(),
            }
        })?;
    }
    let valid_cause = match cause {
        AgentTurnCause::RootUser => {
            definition.depth == AgentDepth::ROOT
                && prompt.is_some()
                && parent_turn_id.is_none()
                && root_turn_id == turn_id
        }
        AgentTurnCause::InitialTask => {
            definition.depth == AgentDepth::CHILD
                && turn_sequence.is_some()
                && prompt.is_some()
                && parent_turn_id.is_some()
                && root_turn_id != turn_id
        }
        AgentTurnCause::Followup { message_id } => {
            turn_sequence.is_some()
                && prompt.is_none()
                && parent_turn_id.is_some()
                && !message_id.as_str().trim().is_empty()
                && message_id.as_str().len() <= MAX_PROFILE_FIELD_BYTES
                && root_turn_id != turn_id
        }
        AgentTurnCause::Retry { previous_turn_id } => {
            parent_turn_id.is_some()
                && previous_turn_id != turn_id
                && belongs(previous_turn_id)
                && turn_sequence_for_root(&definition.root_agent_id, previous_turn_id)
                    .zip(turn_sequence)
                    .is_some_and(|(previous, current)| previous < current)
                && root_turn_id != turn_id
        }
    };
    if !valid_cause {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复 Turn 的原因、输入或父 Turn 关系无效".to_owned(),
        });
    }
    Ok(())
}

/// 校验冷恢复树的所有者、根定义、单层父链、Turn 和消息边界。
fn validate_recovered_tree(
    tree: &RecoveredAgentTree,
    expected_root_agent_id: &AgentId,
    expected_root_session_id: &SessionId,
    expected_root_definition: &AgentDefinition,
    expected_turn_limit: usize,
    expected_lifecycle: RecoveredRootLifecycle,
    external_root_turn_roots: &HashMap<TurnId, AgentId>,
) -> Result<(), CollaborationError> {
    if tree.root_agent_id != *expected_root_agent_id
        || tree.root_session_id != *expected_root_session_id
        || tree.lifecycle != expected_lifecycle
        || tree.per_root_turn_limit != expected_turn_limit
        || !tree.live && tree.lifecycle != RecoveredRootLifecycle::Open
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树所有者、容量、live 或关闭清理状态无效".to_owned(),
        });
    }
    if tree.next_turn_sequence == 0 || tree.next_checkpoint_revision == 0 {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树的 Turn 或局部 checkpoint 单调序号不能为零".to_owned(),
        });
    }
    if tree.known_agents.is_empty()
        || tree.known_agents.len() > MAX_AGENTS_PER_ROOT
        || tree.agents.is_empty()
        || tree.agents.len() != tree.known_agents.len()
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树不是覆盖全部已知 Agent 的自包含 checkpoint".to_owned(),
        });
    }

    let mut known_by_id = HashMap::new();
    let mut known_paths = HashSet::new();
    let mut session_ids = HashSet::new();
    let mut worktree_leases = HashSet::new();
    let mut known_root_count = 0usize;
    for definition in &tree.known_agents {
        validate_agent_profile(&definition.profile).map_err(|error| {
            CollaborationError::InvalidRecovery {
                message: error.to_string(),
            }
        })?;
        validate_context_inheritance(&definition.context_inheritance).map_err(|error| {
            CollaborationError::InvalidRecovery {
                message: error.to_string(),
            }
        })?;
        validate_context_snapshot(
            &definition.context_inheritance,
            &definition.context_snapshot,
        )
        .map_err(|error| CollaborationError::InvalidRecovery {
            message: error.to_string(),
        })?;
        if let Some(template) = &definition.agent_template {
            validate_agent_template_snapshot(template).map_err(|error| {
                CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                }
            })?;
        }
        if definition.root_agent_id != tree.root_agent_id
            || definition.root_session_id != tree.root_session_id
            || definition.depth != definition.path.depth()
            || known_by_id
                .insert(definition.agent_id.clone(), definition)
                .is_some()
            || !known_paths.insert(definition.path.clone())
            || !session_ids.insert(definition.session_id.clone())
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "已知 Agent 的所有者、深度、标识、Session 或路径不一致".to_owned(),
            });
        }
        if let Some(worktree_lease) = &definition.profile.worktree_lease {
            if !worktree_leases.insert(worktree_lease.clone()) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复树重复绑定同一 Worktree lease".to_owned(),
                });
            }
        }
        match definition.depth {
            depth if depth == AgentDepth::ROOT => {
                known_root_count = known_root_count.saturating_add(1);
                if definition.agent_id != tree.root_agent_id
                    || definition.parent_agent_id.is_some()
                    || definition.path != AgentPath::root()
                    || definition.session_id != tree.root_session_id
                    || definition.context_inheritance != ContextInheritance::None
                    || !definition.context_snapshot.is_empty()
                    || definition.agent_template.is_some()
                    || definition != expected_root_definition
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复根 Agent 定义无效".to_owned(),
                    });
                }
            }
            depth if depth == AgentDepth::CHILD => {
                if definition.parent_agent_id.as_ref() != Some(&tree.root_agent_id)
                    || !definition.path.as_str().starts_with("/root/")
                    || matches!(definition.context_inheritance, ContextInheritance::None)
                        && !definition.context_snapshot.is_empty()
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复子 Agent 父链或路径无效".to_owned(),
                    });
                }
            }
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 Agent 超过单层限制".to_owned(),
                });
            }
        }
    }
    if known_root_count != 1 {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树根定义不一致".to_owned(),
        });
    }

    let recovered_by_id = tree
        .agents
        .iter()
        .map(|agent| (agent.definition.agent_id.clone(), agent))
        .collect::<HashMap<_, _>>();
    let mut resident_ids = HashSet::new();
    let mut message_ids = HashSet::new();
    let mut completion_sources = HashSet::new();
    let mut tree_user_count = 0usize;
    let mut tree_user_bytes = 0usize;
    let mut tree_completion_count = 0usize;
    let mut tree_completion_bytes = 0usize;
    for agent in &tree.agents {
        let definition = &agent.definition;
        let turn_belongs = |turn_id: &TurnId| {
            turn_id_belongs_to_root(&tree.root_agent_id, tree.next_turn_sequence, turn_id)
                || external_root_turn_roots.get(turn_id) == Some(&tree.root_agent_id)
        };
        if known_by_id.get(&definition.agent_id).copied() != Some(definition)
            || !resident_ids.insert(definition.agent_id.clone())
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "驻留 Agent 不属于已知定义或发生重复".to_owned(),
            });
        }
        let current_turn_id = recovered_current_turn_id(&agent.status);
        let interrupted_or_failed_turn_id = match &agent.status {
            CollaborationAgentStatus::Interrupted { turn_id }
            | CollaborationAgentStatus::Failed { turn_id, .. } => Some(turn_id),
            _ => None,
        };
        let claim_turn_is_valid = |claim_turn_id: &TurnId| {
            current_turn_id == Some(claim_turn_id)
                || interrupted_or_failed_turn_id == Some(claim_turn_id)
        };
        match (
            agent.mailbox_claim_turn_id.as_ref(),
            agent.mailbox_claim_through_sequence,
        ) {
            (None, None) => {}
            (Some(claim_turn_id), Some(through_sequence)) => {
                let claimed = agent
                    .mailbox
                    .iter()
                    .take_while(|entry| entry.message.sequence <= through_sequence)
                    .collect::<Vec<_>>();
                if !claim_turn_is_valid(claim_turn_id)
                    || claimed.last().map(|entry| entry.message.sequence) != Some(through_sequence)
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复 mailbox claim 未绑定当前或可重试 Turn 的完整 FIFO 前缀"
                            .to_owned(),
                    });
                }
            }
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 mailbox claim 的 Turn 与最大序号必须同时存在".to_owned(),
                });
            }
        }
        match (
            agent.steer_claim_turn_id.as_ref(),
            agent.steer_claim_through_sequence,
        ) {
            (None, None) => {}
            (Some(claim_turn_id), Some(through_sequence)) => {
                let claimed = agent
                    .pending_steers
                    .iter()
                    .filter(|steer| {
                        &steer.turn_id == claim_turn_id && steer.sequence <= through_sequence
                    })
                    .collect::<Vec<_>>();
                if !claim_turn_is_valid(claim_turn_id)
                    || claimed.last().map(|steer| steer.sequence) != Some(through_sequence)
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "恢复用户 steer claim 未绑定当前或可重试 Turn 的完整批次"
                            .to_owned(),
                    });
                }
            }
            _ => {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复用户 steer claim 的 Turn 与最大序号必须同时存在".to_owned(),
                });
            }
        }
        if current_turn_id.is_some() && !tree.live {
            return Err(CollaborationError::InvalidRecovery {
                message: "静止 checkpoint 不能包含未决 Turn".to_owned(),
            });
        }
        match tree.lifecycle {
            RecoveredRootLifecycle::Open => {
                if matches!(
                    agent.status,
                    CollaborationAgentStatus::PendingInit | CollaborationAgentStatus::Stopped
                ) {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "开放恢复树包含不可恢复的 Agent 状态".to_owned(),
                    });
                }
            }
            RecoveredRootLifecycle::Closing => {
                if !matches!(
                    agent.status,
                    CollaborationAgentStatus::Cancelling { .. } | CollaborationAgentStatus::Stopped
                ) || !agent.mailbox.is_empty()
                    || !agent.pending_steers.is_empty()
                    || agent.start_pending
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "Closing 根树只能包含已清空的 Cancelling 或 Stopped Agent"
                            .to_owned(),
                    });
                }
            }
            RecoveredRootLifecycle::CleanupPending => {
                if agent.status != CollaborationAgentStatus::Stopped
                    || !agent.mailbox.is_empty()
                    || !agent.pending_steers.is_empty()
                    || current_turn_id.is_some()
                    || agent.start_pending
                {
                    return Err(CollaborationError::InvalidRecovery {
                        message: "CleanupPending 根树必须只包含已清空的 Stopped Agent".to_owned(),
                    });
                }
            }
        }

        if let Some(turn_id) = current_turn_id {
            let source_agent_id = agent.current_source_agent_id.as_ref().ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 缺少来源 Agent".to_owned(),
                }
            })?;
            let cause = agent.current_turn_cause.as_ref().ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 缺少触发原因".to_owned(),
                }
            })?;
            let root_turn_id = agent.current_root_turn_id.as_ref().ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 缺少根 Turn".to_owned(),
                }
            })?;
            let current_plan_guard =
                agent
                    .current_plan_guard
                    .ok_or_else(|| CollaborationError::InvalidRecovery {
                        message: "live checkpoint 当前 Turn 缺少 Plan 守卫".to_owned(),
                    })?;
            if matches!(
                definition.profile.plan_guard.state(),
                PlanGuardState::ReadOnly
            ) && matches!(current_plan_guard.state(), PlanGuardState::Inactive)
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 放宽了 Agent 的 Plan 守卫".to_owned(),
                });
            }
            if !known_by_id.contains_key(source_agent_id)
                || matches!(cause, AgentTurnCause::RootUser)
                    && source_agent_id != &definition.agent_id
                || matches!(cause, AgentTurnCause::InitialTask)
                    && definition.parent_agent_id.as_ref() != Some(source_agent_id)
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 的来源 Agent 无效".to_owned(),
                });
            }
            validate_recovered_turn_fields(
                definition,
                RecoveredTurnFields {
                    turn_id,
                    cause,
                    prompt: agent.current_turn_prompt.as_ref(),
                    parent_turn_id: agent.current_parent_turn_id.as_ref(),
                    root_turn_id,
                },
                tree.next_turn_sequence,
                external_root_turn_roots,
            )?;
            if agent
                .last_turn
                .as_ref()
                .is_some_and(|last_turn| &last_turn.turn_id == turn_id)
                || matches!(
                    agent.status,
                    CollaborationAgentStatus::WaitingCapacity { .. }
                ) && agent.start_pending
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "live checkpoint 当前 Turn 与最近 Turn、outbox 或 steer 不一致"
                        .to_owned(),
                });
            }
        } else if agent.current_source_agent_id.is_some()
            || agent.current_turn_cause.is_some()
            || agent.current_turn_prompt.is_some()
            || agent.current_parent_turn_id.is_some()
            || agent.current_root_turn_id.is_some()
            || agent.current_plan_guard.is_some()
            || agent.start_pending
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "非活跃 Agent 不能保留当前 Turn 或 StartTurn outbox 元数据".to_owned(),
            });
        }

        if tree.lifecycle == RecoveredRootLifecycle::Open
            && current_turn_id.is_none()
            && !recovered_terminal_state_matches(agent)
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 Agent 状态与最近 Turn 终态不一致".to_owned(),
            });
        }

        if let Some(last_turn) = &agent.last_turn {
            validate_recovered_turn_fields(
                definition,
                RecoveredTurnFields {
                    turn_id: &last_turn.turn_id,
                    cause: &last_turn.cause,
                    prompt: last_turn.prompt.as_ref(),
                    parent_turn_id: last_turn.parent_turn_id.as_ref(),
                    root_turn_id: &last_turn.root_turn_id,
                },
                tree.next_turn_sequence,
                external_root_turn_roots,
            )?;
            validate_turn_outcome(&last_turn.outcome).map_err(|error| {
                CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                }
            })?;
        }

        let steer_turn_id = current_turn_id.or(match &agent.status {
            CollaborationAgentStatus::Interrupted { turn_id }
            | CollaborationAgentStatus::Failed { turn_id, .. } => Some(turn_id),
            _ => None,
        });
        if agent.pending_steers.len() > MAX_PENDING_STEERS_PER_AGENT {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 steer 数量超过限制".to_owned(),
            });
        }
        let mut last_steer_sequence = 0u64;
        let mut steer_bytes = 0usize;
        for steer in &agent.pending_steers {
            validate_required_text(&steer.content, "恢复用户 steer").map_err(|error| {
                CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                }
            })?;
            if steer.sequence <= last_steer_sequence
                || steer_turn_id != Some(&steer.turn_id)
                || !turn_belongs(&steer.turn_id)
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 steer 的序号或 Turn 归属无效".to_owned(),
                });
            }
            steer_bytes = steer_bytes
                .checked_add(steer.content.len())
                .ok_or(CollaborationError::SequenceExhausted)?;
            last_steer_sequence = steer.sequence;
        }
        if steer_bytes > MAX_PENDING_STEER_BYTES_PER_AGENT
            || agent.next_steer_sequence <= last_steer_sequence
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 steer 的累计字节或下一序号无效".to_owned(),
            });
        }

        let mut last_sequence = 0u64;
        let mut agent_user_count = 0usize;
        let mut agent_user_bytes = 0usize;
        let mut agent_completion_count = 0usize;
        let mut agent_completion_bytes = 0usize;
        for mailbox in &agent.mailbox {
            validate_required_text(&mailbox.message.content, "恢复 mailbox 正文").map_err(
                |error| CollaborationError::InvalidRecovery {
                    message: error.to_string(),
                },
            )?;
            if mailbox.message.message_id.as_str().len() > MAX_PROFILE_FIELD_BYTES
                || mailbox.message.target_agent_id != definition.agent_id
                || !known_by_id.contains_key(&mailbox.message.source_agent_id)
                || mailbox.message.related_turn_id.is_none()
                || mailbox
                    .message
                    .related_turn_id
                    .as_ref()
                    .is_some_and(|turn_id| !turn_belongs(turn_id))
                || mailbox.message.parent_turn_id.is_none()
                || mailbox
                    .message
                    .parent_turn_id
                    .as_ref()
                    .is_some_and(|turn_id| !turn_belongs(turn_id))
                || mailbox.message.root_turn_id.is_none()
                || mailbox
                    .message
                    .root_turn_id
                    .as_ref()
                    .is_some_and(|turn_id| !turn_belongs(turn_id))
                || mailbox.message.sequence <= last_sequence
                || !message_ids.insert(mailbox.message.message_id.clone())
            {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 mailbox 的来源、目标、Turn 或 FIFO 序列无效".to_owned(),
                });
            }
            if mailbox.claimed_turn_id.as_ref().is_some_and(|claimed| {
                mailbox.message.delivery != MailboxDelivery::TriggerTurn
                    || !matches!(mailbox.message.kind, MailboxMessageKind::AgentMessage)
                    || current_turn_id != Some(claimed)
                    || !turn_belongs(claimed)
            }) {
                return Err(CollaborationError::InvalidRecovery {
                    message: "恢复 mailbox 保留了无效的 TriggerTurn 归属".to_owned(),
                });
            }
            match &mailbox.message.kind {
                MailboxMessageKind::AgentMessage => {
                    if mailbox.message.related_turn_id != mailbox.message.parent_turn_id {
                        return Err(CollaborationError::InvalidRecovery {
                            message: "Agent mailbox 的来源与直接父 Turn 因果不一致".to_owned(),
                        });
                    }
                    agent_user_count = agent_user_count
                        .checked_add(1)
                        .ok_or(CollaborationError::SequenceExhausted)?;
                    agent_user_bytes = agent_user_bytes
                        .checked_add(mailbox.message.content.len())
                        .ok_or(CollaborationError::SequenceExhausted)?;
                }
                MailboxMessageKind::ChildTurnFinished { outcome } => {
                    validate_turn_outcome(outcome).map_err(|error| {
                        CollaborationError::InvalidRecovery {
                            message: error.to_string(),
                        }
                    })?;
                    let source = known_by_id
                        .get(&mailbox.message.source_agent_id)
                        .copied()
                        .ok_or_else(|| CollaborationError::InvalidRecovery {
                            message: "完成消息来源 Agent 不存在".to_owned(),
                        })?;
                    let source_agent =
                        recovered_by_id
                            .get(&source.agent_id)
                            .copied()
                            .ok_or_else(|| CollaborationError::InvalidRecovery {
                                message: "完成消息来源 Agent 不在自包含 checkpoint 中".to_owned(),
                            })?;
                    let source_last_turn = source_agent.last_turn.as_ref().ok_or_else(|| {
                        CollaborationError::InvalidRecovery {
                            message: "完成消息来源 Agent 缺少最近终态".to_owned(),
                        }
                    })?;
                    let expected_outcome = bounded_completion_outcome(&source_last_turn.outcome);
                    let expected_content = bounded_text(
                        &child_completion_content(&source.path, &expected_outcome),
                        MAX_COMPLETION_NOTIFICATION_BYTES,
                    );
                    if source.depth != AgentDepth::CHILD
                        || mailbox.message.target_agent_id != tree.root_agent_id
                        || mailbox.message.delivery != MailboxDelivery::QueueOnly
                        || mailbox.claimed_turn_id.is_some()
                        || mailbox.message.related_turn_id.as_ref()
                            != Some(&source_last_turn.turn_id)
                        || mailbox.message.parent_turn_id != source_last_turn.parent_turn_id
                        || mailbox.message.root_turn_id.as_ref()
                            != Some(&source_last_turn.root_turn_id)
                        || outcome != &expected_outcome
                        || mailbox.message.content != expected_content
                        || mailbox.message.content.len() > MAX_COMPLETION_NOTIFICATION_BYTES
                        || !completion_sources.insert(source.agent_id.clone())
                    {
                        return Err(CollaborationError::InvalidRecovery {
                            message: "子 Agent 完成消息不是来源 Agent 的最新有界 QueueOnly 终态"
                                .to_owned(),
                        });
                    }
                    agent_completion_count = agent_completion_count
                        .checked_add(1)
                        .ok_or(CollaborationError::SequenceExhausted)?;
                    agent_completion_bytes = agent_completion_bytes
                        .checked_add(mailbox.message.content.len())
                        .ok_or(CollaborationError::SequenceExhausted)?;
                }
            }
            last_sequence = mailbox.message.sequence;
        }
        if agent_user_count > MAX_MAILBOX_MESSAGES_PER_AGENT
            || agent_user_bytes > MAX_MAILBOX_BYTES_PER_AGENT
            || agent_completion_count > MAX_COMPLETION_MESSAGES_PER_TREE
            || agent_completion_bytes > MAX_COMPLETION_BYTES_PER_TREE
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 Agent 的普通消息或完成通知超过独立边界".to_owned(),
            });
        }
        tree_user_count = tree_user_count
            .checked_add(agent_user_count)
            .ok_or(CollaborationError::SequenceExhausted)?;
        tree_user_bytes = tree_user_bytes
            .checked_add(agent_user_bytes)
            .ok_or(CollaborationError::SequenceExhausted)?;
        tree_completion_count = tree_completion_count
            .checked_add(agent_completion_count)
            .ok_or(CollaborationError::SequenceExhausted)?;
        tree_completion_bytes = tree_completion_bytes
            .checked_add(agent_completion_bytes)
            .ok_or(CollaborationError::SequenceExhausted)?;
        if agent.next_mailbox_sequence <= last_sequence {
            return Err(CollaborationError::InvalidRecovery {
                message: "恢复 mailbox 的下一序号无效".to_owned(),
            });
        }
    }
    if resident_ids != known_by_id.keys().cloned().collect::<HashSet<_>>()
        || !resident_ids.contains(&tree.root_agent_id)
        || tree_user_count > MAX_USER_MAILBOX_MESSAGES_PER_TREE
        || tree_user_bytes > MAX_USER_MAILBOX_BYTES_PER_TREE
        || tree_completion_count > MAX_COMPLETION_MESSAGES_PER_TREE
        || tree_completion_bytes > MAX_COMPLETION_BYTES_PER_TREE
    {
        return Err(CollaborationError::InvalidRecovery {
            message: "恢复树缺少 Agent，或普通消息与完成通知突破树级独立边界".to_owned(),
        });
    }
    Ok(())
}

/// 在候选状态中分配下一全局事件序号并追加完整事件。
fn push_event(
    state: &mut CoordinatorState,
    events: &mut Vec<CollaborationEvent>,
    agent: &AgentDefinition,
    link: EventLink,
    kind: CollaborationEventKind,
) -> Result<(), CollaborationError> {
    let sequence = state
        .last_event_sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    state.last_event_sequence = sequence;
    events.push(CollaborationEvent {
        session_id: agent.session_id.clone(),
        turn_id: link.turn_id,
        source_agent_id: link.source_agent_id,
        agent_id: agent.agent_id.clone(),
        parent_agent_id: agent.parent_agent_id.clone(),
        agent_path: agent.path.clone(),
        parent_turn_id: link.parent_turn_id,
        root_turn_id: link.root_turn_id,
        sequence,
        kind,
    });
    Ok(())
}

/// 替换 Agent 状态并追加独立的 agent_status_changed 事件。
fn set_status(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    current: CollaborationAgentStatus,
    link: EventLink,
    events: &mut Vec<CollaborationEvent>,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let previous = agent.status.clone();
    if previous == current {
        return Ok(());
    }
    let definition = agent.definition.clone();
    state
        .agents
        .get_mut(agent_id)
        .expect("Agent 在上方已校验")
        .status = current.clone();
    push_event(
        state,
        events,
        &definition,
        link,
        CollaborationEventKind::AgentStatusChanged { previous, current },
    )
}

/// 在不修改状态时计算下一棵根树的单调命名空间身份。
fn prospective_root_agent_id(state: &CoordinatorState) -> Result<AgentId, CollaborationError> {
    if state.next_root_sequence == 0 {
        return Err(CollaborationError::SequenceExhausted);
    }
    AgentId::new(format!(
        "root/{}/{}",
        state.root_identity_namespace, state.next_root_sequence
    ))
    .map_err(|_error| CollaborationError::IdentifierCollision { kind: "Root Agent" })
}

/// 校验根身份属于持久命名空间且序号已经由 counter 分配。
fn root_agent_id_belongs_to_namespace(
    namespace: &AgentId,
    next_root_sequence: u64,
    root_agent_id: &AgentId,
) -> bool {
    let prefix = format!("root/{namespace}/");
    root_agent_id
        .as_str()
        .strip_prefix(&prefix)
        .and_then(|sequence| sequence.parse::<u64>().ok())
        .is_some_and(|sequence| sequence > 0 && sequence < next_root_sequence)
}

/// 返回恢复 Agent 当前或最近 Turn 的规范排序文本。
fn recovered_agent_sort_turn_id(agent: &RecoveredAgent) -> &str {
    recovered_current_turn_id(&agent.status)
        .or_else(|| agent.last_turn.as_ref().map(|turn| &turn.turn_id))
        .map_or("", TurnId::as_str)
}

/// 按 `(root_agent_id, agent_path, turn_id)` 规范化多根恢复顺序。
fn normalize_recovered_root_order(roots: &mut [RecoveredAgentTree]) {
    for tree in roots.iter_mut() {
        tree.known_agents
            .sort_by(|left, right| left.path.cmp(&right.path));
        tree.agents.sort_by(|left, right| {
            (&left.definition.path, recovered_agent_sort_turn_id(left))
                .cmp(&(&right.definition.path, recovered_agent_sort_turn_id(right)))
        });
    }
    roots.sort_by(|left, right| left.root_agent_id.cmp(&right.root_agent_id));
}

/// 按 `(source_agent_id, source_turn_id, tool_call_id)` 规范化幂等记录顺序。
fn normalize_recovered_invocation_order(invocations: &mut [RecoveredCollaborationInvocation]) {
    invocations.sort_by(|left, right| left.key.cmp(&right.key));
}

/// 原子分配下一棵根树身份并推进持久单调 counter。
fn allocate_root_agent_id(state: &mut CoordinatorState) -> Result<AgentId, CollaborationError> {
    let root_agent_id = prospective_root_agent_id(state)?;
    state.next_root_sequence = state
        .next_root_sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    Ok(root_agent_id)
}

/// 为根树分配跨恢复单调递增且永不复用的 TurnId。
fn allocate_turn_id(
    state: &mut CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<TurnId, CollaborationError> {
    let root =
        state
            .roots
            .get_mut(root_agent_id)
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: root_agent_id.clone(),
            })?;
    let sequence = root.next_turn_sequence;
    root.next_turn_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    TurnId::new(format!("turn/{root_agent_id}/{sequence}"))
        .map_err(|_error| CollaborationError::IdentifierCollision { kind: "Turn" })
}

/// 校验 TurnId 属于指定根 Session 命名空间且序号已经分配。
fn turn_id_belongs_to_root(
    root_agent_id: &AgentId,
    next_turn_sequence: u64,
    turn_id: &TurnId,
) -> bool {
    turn_sequence_for_root(root_agent_id, turn_id)
        .is_some_and(|sequence| sequence > 0 && sequence < next_turn_sequence)
}

/// 判断 Turn 是否属于根树的内部单调命名空间或已持久绑定的外部根 Turn。
fn state_turn_belongs_to_root(
    state: &CoordinatorState,
    root_agent_id: &AgentId,
    turn_id: &TurnId,
) -> bool {
    state.roots.get(root_agent_id).is_some_and(|root| {
        turn_id_belongs_to_root(root_agent_id, root.next_turn_sequence, turn_id)
            || state
                .root_turn_bindings
                .get(turn_id)
                .is_some_and(|binding| &binding.root_agent_id == root_agent_id)
    })
}

/// 从属于指定根身份的 TurnId 解析单调序号。
fn turn_sequence_for_root(root_agent_id: &AgentId, turn_id: &TurnId) -> Option<u64> {
    let prefix = format!("turn/{root_agent_id}/");
    turn_id
        .as_str()
        .strip_prefix(&prefix)
        .and_then(|sequence| sequence.parse::<u64>().ok())
}

/// 将新 Turn 持久化入队，但不预约任何容量。
fn queue_turn(
    state: &mut CoordinatorState,
    queued: QueuedTurn,
    events: &mut Vec<CollaborationEvent>,
) -> Result<(), CollaborationError> {
    ensure_tree_open(state, &queued.root_agent_id)?;
    let agent = resident_agent(state, &queued.agent_id)?;
    if !agent.status.is_idle() && agent.status != CollaborationAgentStatus::PendingInit {
        return Err(CollaborationError::TargetNotIdle {
            agent_id: queued.agent_id.clone(),
        });
    }
    if agent
        .mailbox_claim
        .as_ref()
        .is_some_and(|claim| claim.turn_id != queued.turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: queued.agent_id.clone(),
            turn_id: agent
                .mailbox_claim
                .as_ref()
                .expect("mailbox claim 在上方已确认存在")
                .turn_id
                .clone(),
            input_kind: "mailbox",
        });
    }
    if agent
        .steer_claim
        .as_ref()
        .is_some_and(|claim| claim.turn_id != queued.turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: queued.agent_id.clone(),
            turn_id: agent
                .steer_claim
                .as_ref()
                .expect("steer claim 在上方已确认存在")
                .turn_id
                .clone(),
            input_kind: "用户 steer",
        });
    }
    if !matches!(queued.cause, AgentTurnCause::Retry { .. }) {
        if let Some(steer) = agent
            .steers
            .iter()
            .find(|steer| steer.turn_id != queued.turn_id)
        {
            return Err(CollaborationError::PendingUserSteers {
                agent_id: queued.agent_id.clone(),
                turn_id: steer.turn_id.clone(),
            });
        }
    }
    let definition = agent.definition.clone();
    if !state_turn_belongs_to_root(state, &queued.root_agent_id, &queued.turn_id)
        || state
            .pending_turns
            .iter()
            .any(|turn| turn.turn_id == queued.turn_id)
        || state.active_turns.contains_key(&queued.turn_id)
    {
        return Err(CollaborationError::IdentifierCollision { kind: "Turn" });
    }
    push_event(
        state,
        events,
        &definition,
        EventLink {
            source_agent_id: queued.source_agent_id.clone(),
            turn_id: Some(queued.turn_id.clone()),
            parent_turn_id: queued.parent_turn_id.clone(),
            root_turn_id: Some(queued.root_turn_id.clone()),
        },
        CollaborationEventKind::AgentTurnQueued {
            cause: queued.cause.clone(),
            prompt: queued.prompt.clone(),
        },
    )?;
    set_status(
        state,
        &queued.agent_id,
        CollaborationAgentStatus::WaitingCapacity {
            turn_id: queued.turn_id.clone(),
        },
        EventLink {
            source_agent_id: queued.source_agent_id.clone(),
            turn_id: Some(queued.turn_id.clone()),
            parent_turn_id: queued.parent_turn_id.clone(),
            root_turn_id: Some(queued.root_turn_id.clone()),
        },
        events,
    )?;
    state.pending_turns.push_back(queued);
    Ok(())
}

/// 原子释放一个真实执行 Turn 占用的全局与根树容量。
fn release_turn_capacity(
    state: &mut CoordinatorState,
    root_agent_id: &AgentId,
) -> Result<(), CollaborationError> {
    state.global_in_use =
        state
            .global_in_use
            .checked_sub(1)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "全局槽位计数下溢".to_owned(),
            })?;
    let root =
        state
            .roots
            .get_mut(root_agent_id)
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: root_agent_id.clone(),
            })?;
    root.in_use =
        root.in_use
            .checked_sub(1)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "根树槽位计数下溢".to_owned(),
            })?;
    Ok(())
}

/// 执行器终态回调在输入 claim 上采用的内部收敛策略。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnCompletionMode {
    /// 普通终态，要求当前 Turn 已确认所有输入 claim。
    Normal,
    /// Turn 派发在执行副作用前被永久拒绝，释放并清理派发 claim。
    DispatchFailed,
    /// 动态输入已写入 Transcript 但 ack 未完成，保留输入 claim 供冷恢复。
    PendingDynamicInput,
    /// 应用退出时收敛 Turn；保留所有 steer 和动态输入 claim，不创建后续 Turn。
    Suspend,
}

/// 规划一个执行器终态，供公开回调和 durable StartTurn 补偿共同复用。
fn complete_turn_transition(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    outcome: AgentTurnOutcome,
    mode: TurnCompletionMode,
) -> Result<Transition<TurnCompletionDisposition>, CollaborationError> {
    let Some(active) = state.active_turns.get(turn_id).cloned() else {
        return Ok(Transition {
            output: TurnCompletionDisposition::IgnoredStale,
            events: Vec::new(),
            actions: Vec::new(),
        });
    };
    if &active.agent_id != agent_id {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    if state
        .roots
        .get(&active.root_agent_id)
        .is_some_and(|root| root.lifecycle != RecoveredRootLifecycle::Open)
    {
        return Ok(Transition {
            output: TurnCompletionDisposition::IgnoredStale,
            events: Vec::new(),
            actions: Vec::new(),
        });
    }
    let Some(agent) = state.agents.get(agent_id) else {
        return Ok(Transition {
            output: TurnCompletionDisposition::IgnoredStale,
            events: Vec::new(),
            actions: Vec::new(),
        });
    };
    let was_cancelling = matches!(agent.status, CollaborationAgentStatus::Cancelling { .. });
    let allows_pending_input_claim = !matches!(mode, TurnCompletionMode::Normal);
    let dispatch_failed = matches!(mode, TurnCompletionMode::DispatchFailed);
    if !allows_pending_input_claim
        && !was_cancelling
        && agent
            .mailbox_claim
            .as_ref()
            .is_some_and(|claim| &claim.turn_id == turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
            input_kind: "mailbox",
        });
    }
    if !allows_pending_input_claim
        && !was_cancelling
        && agent
            .steer_claim
            .as_ref()
            .is_some_and(|claim| &claim.turn_id == turn_id)
    {
        return Err(CollaborationError::PendingInputClaim {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
            input_kind: "用户 steer",
        });
    }
    if !allows_pending_input_claim
        && !was_cancelling
        && agent.steers.iter().any(|steer| &steer.turn_id == turn_id)
    {
        return Err(CollaborationError::PendingUserSteers {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    let effective_outcome = if was_cancelling {
        AgentTurnOutcome::Interrupted
    } else {
        outcome
    };
    state.active_turns.remove(turn_id);
    state.start_outbox.remove(turn_id);
    remove_turn_signals(state, agent_id, turn_id);
    release_turn_capacity(state, &active.root_agent_id)?;

    let mut events = Vec::new();
    let mut actions = Vec::new();
    if dispatch_failed {
        actions.push(PostCommitAction::CancelTurn(active.cancellation.clone()));
    }
    let definition = resident_agent(state, agent_id)?.definition.clone();
    let link = EventLink {
        source_agent_id: agent_id.clone(),
        turn_id: Some(turn_id.clone()),
        parent_turn_id: active.parent_turn_id.clone(),
        root_turn_id: Some(active.root_turn_id.clone()),
    };
    push_event(
        state,
        &mut events,
        &definition,
        link.clone(),
        terminal_event_kind(&effective_outcome),
    )?;
    set_status(
        state,
        agent_id,
        outcome_status(turn_id, &effective_outcome),
        link,
        &mut events,
    )?;
    let completed_turn = TurnRecord {
        turn_id: turn_id.clone(),
        cause: active.cause.clone(),
        prompt: active.prompt.clone(),
        parent_turn_id: active.parent_turn_id.clone(),
        root_turn_id: active.root_turn_id.clone(),
        outcome: effective_outcome.clone(),
    };
    let agent = state.agents.get_mut(agent_id).expect("Agent 在上方已校验");
    agent.last_turn = Some(completed_turn.clone());
    if was_cancelling && !matches!(mode, TurnCompletionMode::Suspend) {
        let claimed_through = agent
            .steer_claim
            .as_ref()
            .filter(|claim| &claim.turn_id == turn_id)
            .map(|claim| claim.through_sequence);
        agent.steers.retain(|steer| {
            &steer.turn_id != turn_id
                || claimed_through.is_some_and(|through| steer.sequence <= through)
        });
        agent.steer_bytes = agent.steers.iter().map(|steer| steer.content.len()).sum();
    }
    mark_activity(state, agent_id, &mut actions)?;

    let tree_closed = state
        .roots
        .get(&active.root_agent_id)
        .is_none_or(|root| root.lifecycle != RecoveredRootLifecycle::Open);
    if !tree_closed {
        if let Some(parent_agent_id) = definition.parent_agent_id.clone() {
            queue_completion_message(
                state,
                CompletionDraft {
                    source_definition: &definition,
                    target_agent_id: &parent_agent_id,
                    outcome: &effective_outcome,
                    related_turn_id: turn_id,
                    parent_turn_id: active.parent_turn_id.clone(),
                    root_turn_id: active.root_turn_id.clone(),
                },
                &mut events,
                &mut actions,
            )?;
        }
        if dispatch_failed {
            let agent = state
                .agents
                .get_mut(agent_id)
                .expect("派发失败 Agent 在上方已校验");
            for mailbox in &mut agent.mailbox {
                if mailbox.claimed_turn_id.as_ref() == Some(turn_id) {
                    mailbox.claimed_turn_id = None;
                }
            }
        } else if matches!(mode, TurnCompletionMode::Normal) {
            claim_followup_after_turn(state, agent_id, &completed_turn, &mut events)?;
        }
        schedule_available(state, &mut events, &mut actions)?;
    }
    Ok(Transition {
        output: TurnCompletionDisposition::Committed,
        events,
        actions,
    })
}

/// 将尚未取得容量的 Turn 直接收敛为中断，并保留与运行中取消一致的后续语义。
fn interrupt_waiting_turn(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    source_agent_id: &AgentId,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let position = state
        .pending_turns
        .iter()
        .position(|queued| &queued.agent_id == agent_id && &queued.turn_id == turn_id)
        .ok_or_else(|| CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        })?;
    let queued = state
        .pending_turns
        .remove(position)
        .expect("已查找到的等待 Turn 始终存在");
    let agent = resident_agent(state, agent_id)?;
    if agent.status
        != (CollaborationAgentStatus::WaitingCapacity {
            turn_id: turn_id.clone(),
        })
    {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    let definition = agent.definition.clone();
    let outcome = AgentTurnOutcome::Interrupted;
    let completed_turn = TurnRecord {
        turn_id: queued.turn_id.clone(),
        cause: queued.cause.clone(),
        prompt: queued.prompt.clone(),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: queued.root_turn_id.clone(),
        outcome: outcome.clone(),
    };
    let link = EventLink {
        source_agent_id: source_agent_id.clone(),
        turn_id: Some(queued.turn_id.clone()),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: Some(queued.root_turn_id.clone()),
    };
    push_event(
        state,
        events,
        &definition,
        link.clone(),
        CollaborationEventKind::AgentTurnInterrupted,
    )?;
    set_status(
        state,
        agent_id,
        CollaborationAgentStatus::Interrupted {
            turn_id: queued.turn_id.clone(),
        },
        link,
        events,
    )?;
    state
        .agents
        .get_mut(agent_id)
        .expect("等待 Turn 的 Agent 在上方已校验")
        .last_turn = Some(completed_turn.clone());
    mark_activity(state, agent_id, actions)?;

    let tree_closed = state
        .roots
        .get(&queued.root_agent_id)
        .is_none_or(|root| root.lifecycle != RecoveredRootLifecycle::Open);
    if !tree_closed {
        if let Some(parent_agent_id) = definition.parent_agent_id.clone() {
            queue_completion_message(
                state,
                CompletionDraft {
                    source_definition: &definition,
                    target_agent_id: &parent_agent_id,
                    outcome: &outcome,
                    related_turn_id: &queued.turn_id,
                    parent_turn_id: queued.parent_turn_id,
                    root_turn_id: queued.root_turn_id,
                },
                events,
                actions,
            )?;
        }
        claim_followup_after_turn(state, agent_id, &completed_turn, events)?;
        schedule_available(state, events, actions)?;
    }
    Ok(())
}

/// 返回暂停时的 Agent 稳定顺序，保证子 Agent 先于根 Agent 收敛完成通知。
fn suspend_agent_order(
    state: &CoordinatorState,
    left_agent_id: &AgentId,
    right_agent_id: &AgentId,
) -> std::cmp::Ordering {
    let left = state
        .agents
        .get(left_agent_id)
        .map(|agent| (agent.definition.depth, agent.definition.path.clone()));
    let right = state
        .agents
        .get(right_agent_id)
        .map(|agent| (agent.definition.depth, agent.definition.path.clone()));
    match (left, right) {
        (Some((left_depth, left_path)), Some((right_depth, right_path))) => right_depth
            .cmp(&left_depth)
            .then_with(|| left_path.cmp(&right_path)),
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, None) => left_agent_id.cmp(right_agent_id),
    }
}

/// 释放指定 Turn 对 TriggerTurn mailbox 的临时归属，保留正文和动态输入 claim。
fn unclaim_mailbox_turn(state: &mut CoordinatorState, turn_id: &TurnId) {
    for agent in state.agents.values_mut() {
        for entry in &mut agent.mailbox {
            if entry.claimed_turn_id.as_ref() == Some(turn_id) {
                entry.claimed_turn_id = None;
            }
        }
    }
}

/// 在应用暂停期间将尚未预约容量的 Turn 收敛为 Interrupted，不创建 Followup。
fn interrupt_waiting_turn_for_suspend(
    state: &mut CoordinatorState,
    queued: &QueuedTurn,
    source_agent_id: &AgentId,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let position = state
        .pending_turns
        .iter()
        .position(|current| current.turn_id == queued.turn_id)
        .ok_or_else(|| CollaborationError::TurnMismatch {
            agent_id: queued.agent_id.clone(),
            turn_id: queued.turn_id.clone(),
        })?;
    let removed = state
        .pending_turns
        .remove(position)
        .expect("暂停时已查找到的等待 Turn 始终存在");
    if removed.agent_id != queued.agent_id
        || resident_agent(state, &queued.agent_id)?.status
            != (CollaborationAgentStatus::WaitingCapacity {
                turn_id: queued.turn_id.clone(),
            })
    {
        return Err(CollaborationError::TurnMismatch {
            agent_id: queued.agent_id.clone(),
            turn_id: queued.turn_id.clone(),
        });
    }
    let definition = resident_agent(state, &queued.agent_id)?.definition.clone();
    let outcome = AgentTurnOutcome::Interrupted;
    let completed_turn = TurnRecord {
        turn_id: queued.turn_id.clone(),
        cause: queued.cause.clone(),
        prompt: queued.prompt.clone(),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: queued.root_turn_id.clone(),
        outcome: outcome.clone(),
    };
    let link = EventLink {
        source_agent_id: source_agent_id.clone(),
        turn_id: Some(queued.turn_id.clone()),
        parent_turn_id: queued.parent_turn_id.clone(),
        root_turn_id: Some(queued.root_turn_id.clone()),
    };
    push_event(
        state,
        events,
        &definition,
        link.clone(),
        CollaborationEventKind::AgentTurnInterrupted,
    )?;
    set_status(
        state,
        &queued.agent_id,
        CollaborationAgentStatus::Interrupted {
            turn_id: queued.turn_id.clone(),
        },
        link,
        events,
    )?;
    state
        .agents
        .get_mut(&queued.agent_id)
        .expect("暂停时等待 Turn 的 Agent 在上方已校验")
        .last_turn = Some(completed_turn);
    mark_activity(state, &queued.agent_id, actions)?;
    if let Some(parent_agent_id) = definition.parent_agent_id.clone() {
        queue_completion_message(
            state,
            CompletionDraft {
                source_definition: &definition,
                target_agent_id: &parent_agent_id,
                outcome: &outcome,
                related_turn_id: &queued.turn_id,
                parent_turn_id: queued.parent_turn_id.clone(),
                root_turn_id: queued.root_turn_id.clone(),
            },
            events,
            actions,
        )?;
    }
    Ok(())
}

/// 在同一候选状态中同时预约全局与根级槽位。
fn schedule_available(
    state: &mut CoordinatorState,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    while state.global_in_use < state.global_limit {
        let Some(position) = state.pending_turns.iter().position(|queued| {
            state.roots.get(&queued.root_agent_id).is_some_and(|root| {
                root.lifecycle == RecoveredRootLifecycle::Open
                    && !root.suspended
                    && root.in_use < root.turn_limit
            })
        }) else {
            break;
        };
        let queued = state
            .pending_turns
            .remove(position)
            .expect("已查找到的队列位置始终存在");
        let agent = resident_agent(state, &queued.agent_id)?;
        if agent.status
            != (CollaborationAgentStatus::WaitingCapacity {
                turn_id: queued.turn_id.clone(),
            })
        {
            return Err(CollaborationError::InvalidRecovery {
                message: "待调度 Turn 与 Agent 状态不一致".to_owned(),
            });
        }
        let definition = agent.definition.clone();
        let cancellation = TurnCancellation::new();
        state.global_in_use = state
            .global_in_use
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        let root = state
            .roots
            .get_mut(&queued.root_agent_id)
            .expect("待调度 Turn 的根树在上方已校验");
        root.in_use = root
            .in_use
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        let active = ActiveTurn {
            agent_id: queued.agent_id.clone(),
            source_agent_id: queued.source_agent_id.clone(),
            root_agent_id: queued.root_agent_id.clone(),
            turn_id: queued.turn_id.clone(),
            parent_turn_id: queued.parent_turn_id.clone(),
            root_turn_id: queued.root_turn_id.clone(),
            cause: queued.cause.clone(),
            prompt: queued.prompt.clone(),
            plan_guard: queued.plan_guard,
            cancellation: cancellation.clone(),
        };
        state
            .active_turns
            .insert(queued.turn_id.clone(), active.clone());
        push_event(
            state,
            events,
            &definition,
            EventLink {
                source_agent_id: queued.source_agent_id.clone(),
                turn_id: Some(queued.turn_id.clone()),
                parent_turn_id: queued.parent_turn_id.clone(),
                root_turn_id: Some(queued.root_turn_id.clone()),
            },
            CollaborationEventKind::AgentTurnStarted {
                cause: queued.cause.clone(),
            },
        )?;
        set_status(
            state,
            &queued.agent_id,
            CollaborationAgentStatus::Running {
                turn_id: queued.turn_id.clone(),
            },
            EventLink {
                source_agent_id: queued.source_agent_id,
                turn_id: Some(queued.turn_id.clone()),
                parent_turn_id: queued.parent_turn_id.clone(),
                root_turn_id: Some(queued.root_turn_id.clone()),
            },
            events,
        )?;
        let launch = AgentTurnLaunch {
            agent: definition.clone(),
            turn_id: queued.turn_id.clone(),
            parent_turn_id: queued.parent_turn_id,
            root_turn_id: queued.root_turn_id,
            cause: queued.cause,
            prompt: queued.prompt,
            cancellation,
            plan_guard: queued.plan_guard,
            capabilities: AgentCapabilities {
                can_spawn_agent: definition.depth.can_spawn_child(),
            },
        };
        state
            .start_outbox
            .insert(launch.turn_id.clone(), launch.clone());
        actions.push(PostCommitAction::StartTurn(Box::new(launch)));
    }
    Ok(())
}

/// 递增 Agent 活动版本并安排提交后广播。
fn mark_activity(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let agent =
        state
            .agents
            .get_mut(agent_id)
            .ok_or_else(|| CollaborationError::AgentNotFound {
                agent_id: agent_id.clone(),
            })?;
    agent.activity_version = agent
        .activity_version
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    actions.push(PostCommitAction::NotifyWaiters {
        sender: agent.activity_sender.clone(),
        version: agent.activity_version,
    });
    Ok(())
}

/// 将同一 Turn 的安全边界信号合并到最新活动版本并登记可重试 outbox。
fn queue_turn_signal(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
    kind: AgentTurnSignalKind,
    actions: &mut Vec<PostCommitAction>,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    if agent.status.active_turn_id() != Some(turn_id) {
        return Err(CollaborationError::TurnMismatch {
            agent_id: agent_id.clone(),
            turn_id: turn_id.clone(),
        });
    }
    let signal = AgentTurnSignal {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        kind,
        activity_version: agent.activity_version,
    };
    state
        .signal_outbox
        .insert(AgentTurnSignalKey::from_signal(&signal), signal.clone());
    actions.push(PostCommitAction::SignalTurn(signal));
    Ok(())
}

/// 当前 Turn 已自行消费全部输入时作废尚未送达的冗余 SignalTurn outbox。
fn invalidate_quiet_turn_signal(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    turn_id: &TurnId,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let mailbox_claimed_through = agent
        .mailbox_claim
        .as_ref()
        .map_or(0, |claim| claim.through_sequence);
    let mailbox_empty = !agent
        .mailbox
        .iter()
        .any(|entry| entry.message.sequence > mailbox_claimed_through);
    let steer_claimed_through = agent
        .steer_claim
        .as_ref()
        .filter(|claim| &claim.turn_id == turn_id)
        .map_or(0, |claim| claim.through_sequence);
    let steer_empty = !agent
        .steers
        .iter()
        .any(|steer| &steer.turn_id == turn_id && steer.sequence > steer_claimed_through);
    let mailbox_key = AgentTurnSignalKey {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        kind: AgentTurnSignalKind::MailboxAvailable,
    };
    if mailbox_empty {
        state.signal_outbox.remove(&mailbox_key);
    }
    let steer_key = AgentTurnSignalKey {
        agent_id: agent_id.clone(),
        turn_id: turn_id.clone(),
        kind: AgentTurnSignalKind::UserSteer,
    };
    if steer_empty {
        state.signal_outbox.remove(&steer_key);
    }
    Ok(())
}

/// 作废一个 Agent Turn 的全部独立信号类型，不影响其他 Agent 或 Turn。
fn remove_turn_signals(state: &mut CoordinatorState, agent_id: &AgentId, turn_id: &TurnId) {
    state
        .signal_outbox
        .retain(|key, _signal| &key.agent_id != agent_id || &key.turn_id != turn_id);
}

/// 为目标 mailbox 分配单调序号，持久入队并唤醒活跃等待者。
fn queue_mailbox_message(
    state: &mut CoordinatorState,
    draft: MailboxDraft,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<MailboxMessage, CollaborationError> {
    if !matches!(draft.kind, MailboxMessageKind::AgentMessage) {
        return Err(CollaborationError::InvalidRecovery {
            message: "普通 mailbox 入口不能写入系统完成通知".to_owned(),
        });
    }
    validate_required_text(&draft.content, "mailbox 消息正文")?;
    if draft.message_id.as_str().len() > MAX_PROFILE_FIELD_BYTES {
        return Err(CollaborationError::TextTooLarge {
            field: "mailbox 消息标识",
            maximum_bytes: MAX_PROFILE_FIELD_BYTES,
        });
    }
    if state.agents.values().any(|agent| {
        agent
            .mailbox
            .iter()
            .any(|entry| entry.message.message_id == draft.message_id)
    }) {
        return Err(CollaborationError::IdentifierCollision {
            kind: "mailbox 消息",
        });
    }
    let target = resident_agent(state, &draft.target_agent_id)?;
    if !target.status.can_receive_messages() {
        return Err(CollaborationError::TargetStopped {
            agent_id: draft.target_agent_id,
        });
    }
    let user_count = target
        .mailbox
        .len()
        .checked_sub(target.completion_count)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "Agent 完成通知计数超过 mailbox 总数".to_owned(),
        })?;
    let user_bytes = target
        .mailbox_bytes
        .checked_sub(target.completion_bytes)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "Agent 完成通知字节超过 mailbox 总字节".to_owned(),
        })?;
    if user_count >= MAX_MAILBOX_MESSAGES_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单 Agent 未消费普通 mailbox 消息数量",
            maximum: MAX_MAILBOX_MESSAGES_PER_AGENT,
        });
    }
    let next_user_bytes = user_bytes
        .checked_add(draft.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    if next_user_bytes > MAX_MAILBOX_BYTES_PER_AGENT {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单 Agent 未消费普通 mailbox 正文字节数",
            maximum: MAX_MAILBOX_BYTES_PER_AGENT,
        });
    }
    let root = state
        .roots
        .get(&target.definition.root_agent_id)
        .expect("mailbox 目标所属根树应存在");
    let root_user_count = root
        .mailbox_count
        .checked_sub(root.completion_count)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "根树完成通知计数超过 mailbox 总数".to_owned(),
        })?;
    let root_user_bytes = root
        .mailbox_bytes
        .checked_sub(root.completion_bytes)
        .ok_or_else(|| CollaborationError::InvalidRecovery {
            message: "根树完成通知字节超过 mailbox 总字节".to_owned(),
        })?;
    if root_user_count >= MAX_USER_MAILBOX_MESSAGES_PER_TREE {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单棵根树未消费普通 mailbox 消息数量",
            maximum: MAX_USER_MAILBOX_MESSAGES_PER_TREE,
        });
    }
    let next_root_user_bytes = root_user_bytes
        .checked_add(draft.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    if next_root_user_bytes > MAX_USER_MAILBOX_BYTES_PER_TREE {
        return Err(CollaborationError::ResourceLimitExceeded {
            resource: "单棵根树未消费普通 mailbox 正文字节数",
            maximum: MAX_USER_MAILBOX_BYTES_PER_TREE,
        });
    }
    let next_mailbox_bytes = target
        .mailbox_bytes
        .checked_add(draft.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    let definition = target.definition.clone();
    let root_agent_id = definition.root_agent_id.clone();
    let sequence = target.next_mailbox_sequence;
    let next_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    let message = MailboxMessage {
        message_id: draft.message_id,
        sequence,
        source_agent_id: draft.source_agent_id.clone(),
        target_agent_id: draft.target_agent_id.clone(),
        delivery: draft.delivery,
        kind: draft.kind,
        content: draft.content,
        related_turn_id: draft.related_turn_id.clone(),
        parent_turn_id: draft.parent_turn_id.clone(),
        root_turn_id: draft.root_turn_id.clone(),
    };
    let target = state
        .agents
        .get_mut(&draft.target_agent_id)
        .expect("mailbox 目标在上方已校验");
    target.next_mailbox_sequence = next_sequence;
    target.mailbox_bytes = next_mailbox_bytes;
    target.mailbox.push_back(MailboxEntry {
        message: message.clone(),
        claimed_turn_id: draft.claimed_turn_id,
    });
    let root = state
        .roots
        .get_mut(&root_agent_id)
        .expect("mailbox 目标所属根树应存在");
    root.mailbox_count = root
        .mailbox_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.mailbox_bytes = root
        .mailbox_bytes
        .checked_add(message.content.len())
        .ok_or(CollaborationError::SequenceExhausted)?;
    push_event(
        state,
        events,
        &definition,
        EventLink {
            source_agent_id: draft.source_agent_id,
            turn_id: draft.related_turn_id,
            parent_turn_id: draft.parent_turn_id,
            root_turn_id: draft.root_turn_id,
        },
        CollaborationEventKind::AgentMessageQueued {
            message: message.clone(),
        },
    )?;
    mark_activity(state, &draft.target_agent_id, actions)?;
    Ok(message)
}

/// 在 UTF-8 边界内生成固定上限的可展示文本。
fn bounded_text(value: &str, maximum: usize) -> String {
    const SUFFIX: &str = "\n\n[内容已截断，完整结果保留在子 Agent 终态中]";
    bounded_utf8_with_suffix(value, maximum, SUFFIX)
}

/// 将完整 Turn 终态缩减为 mailbox 使用的有界通知终态。
fn bounded_completion_outcome(outcome: &AgentTurnOutcome) -> AgentTurnOutcome {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => AgentTurnOutcome::Completed {
            final_message: final_message
                .as_deref()
                .map(|message| bounded_text(message, MAX_COMPLETION_NOTIFICATION_BYTES / 2)),
        },
        AgentTurnOutcome::Interrupted => AgentTurnOutcome::Interrupted,
        AgentTurnOutcome::Failed { message } => AgentTurnOutcome::Failed {
            message: bounded_text(message, MAX_COMPLETION_NOTIFICATION_BYTES / 2),
        },
    }
}

/// 从不可复用 TurnId 派生不依赖可故障 ID 生成器的系统消息标识。
fn completion_message_id(turn_id: &TurnId, sequence: u64) -> MailboxMessageId {
    let digest = Sha256::digest(turn_id.as_str().as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("写入 String 不会失败");
    }
    MailboxMessageId(format!("completion-{hex}-{sequence}"))
}

/// 为子 Agent 终态写入不会挤占普通消息配额的有界、可恢复系统通知。
fn queue_completion_message(
    state: &mut CoordinatorState,
    draft: CompletionDraft<'_>,
    events: &mut Vec<CollaborationEvent>,
    actions: &mut Vec<PostCommitAction>,
) -> Result<MailboxMessage, CollaborationError> {
    let CompletionDraft {
        source_definition,
        target_agent_id,
        outcome,
        related_turn_id,
        parent_turn_id,
        root_turn_id,
    } = draft;
    let target = resident_agent(state, target_agent_id)?;
    if !target.status.can_receive_messages() {
        return Err(CollaborationError::TargetStopped {
            agent_id: target_agent_id.clone(),
        });
    }
    let running_turn_id = match &target.status {
        CollaborationAgentStatus::Running { turn_id } => Some(turn_id.clone()),
        _ => None,
    };
    let root_agent_id = target.definition.root_agent_id.clone();
    let target_definition = target.definition.clone();
    let claimed_through_sequence = target
        .mailbox_claim
        .as_ref()
        .map_or(0, |claim| claim.through_sequence);
    let mut superseded_position = target.mailbox.iter().position(|entry| {
        entry.message.sequence > claimed_through_sequence
            && entry.message.source_agent_id == source_definition.agent_id
            && matches!(
                entry.message.kind,
                MailboxMessageKind::ChildTurnFinished { .. }
            )
    });
    if superseded_position.is_none()
        && state
            .roots
            .get(&root_agent_id)
            .is_some_and(|root| root.completion_count >= MAX_COMPLETION_MESSAGES_PER_TREE)
    {
        superseded_position = target.mailbox.iter().position(|entry| {
            entry.message.sequence > claimed_through_sequence
                && matches!(
                    entry.message.kind,
                    MailboxMessageKind::ChildTurnFinished { .. }
                )
        });
    }
    if let Some(position) = superseded_position {
        let removed = state
            .agents
            .get_mut(target_agent_id)
            .expect("完成通知目标在上方已校验")
            .mailbox
            .remove(position)
            .expect("完成通知位置在上方已查找");
        let removed_bytes = removed.message.content.len();
        let target = state
            .agents
            .get_mut(target_agent_id)
            .expect("完成通知目标在上方已校验");
        target.mailbox_bytes =
            target
                .mailbox_bytes
                .checked_sub(removed_bytes)
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "替代完成通知时 Agent mailbox 字节下溢".to_owned(),
                })?;
        target.completion_count = target.completion_count.checked_sub(1).ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "替代完成通知时 Agent 完成计数下溢".to_owned(),
            }
        })?;
        target.completion_bytes = target
            .completion_bytes
            .checked_sub(removed_bytes)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "替代完成通知时 Agent 完成字节下溢".to_owned(),
            })?;
        let root = state
            .roots
            .get_mut(&root_agent_id)
            .expect("完成通知目标所属根树应存在");
        root.mailbox_count = root.mailbox_count.checked_sub(1).ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树 mailbox 计数下溢".to_owned(),
            }
        })?;
        root.mailbox_bytes = root
            .mailbox_bytes
            .checked_sub(removed_bytes)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树 mailbox 字节下溢".to_owned(),
            })?;
        root.completion_count = root.completion_count.checked_sub(1).ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树完成计数下溢".to_owned(),
            }
        })?;
        root.completion_bytes = root
            .completion_bytes
            .checked_sub(removed_bytes)
            .ok_or_else(|| CollaborationError::InvalidRecovery {
                message: "替代完成通知时根树完成字节下溢".to_owned(),
            })?;
        push_event(
            state,
            events,
            &target_definition,
            EventLink {
                source_agent_id: source_definition.agent_id.clone(),
                turn_id: Some(related_turn_id.clone()),
                parent_turn_id: parent_turn_id.clone(),
                root_turn_id: Some(root_turn_id.clone()),
            },
            CollaborationEventKind::AgentCompletionNotificationSuperseded {
                message_id: removed.message.message_id,
            },
        )?;
    }

    let notification_outcome = bounded_completion_outcome(outcome);
    let content = bounded_text(
        &child_completion_content(&source_definition.path, &notification_outcome),
        MAX_COMPLETION_NOTIFICATION_BYTES,
    );
    let target = resident_agent(state, target_agent_id)?;
    let sequence = target.next_mailbox_sequence;
    let next_sequence = sequence
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    let mut message_id = completion_message_id(related_turn_id, sequence);
    let mut collision_suffix = 0u64;
    while state.agents.values().any(|agent| {
        agent
            .mailbox
            .iter()
            .any(|entry| entry.message.message_id == message_id)
    }) {
        collision_suffix = collision_suffix
            .checked_add(1)
            .ok_or(CollaborationError::SequenceExhausted)?;
        message_id = MailboxMessageId(format!(
            "{}-{collision_suffix}",
            completion_message_id(related_turn_id, sequence).as_str()
        ));
    }
    let message = MailboxMessage {
        message_id,
        sequence,
        source_agent_id: source_definition.agent_id.clone(),
        target_agent_id: target_agent_id.clone(),
        delivery: MailboxDelivery::QueueOnly,
        kind: MailboxMessageKind::ChildTurnFinished {
            outcome: notification_outcome,
        },
        content,
        related_turn_id: Some(related_turn_id.clone()),
        parent_turn_id: parent_turn_id.clone(),
        root_turn_id: Some(root_turn_id.clone()),
    };
    let message_bytes = message.content.len();
    let target = state
        .agents
        .get_mut(target_agent_id)
        .expect("完成通知目标在上方已校验");
    target.next_mailbox_sequence = next_sequence;
    target.mailbox_bytes = target
        .mailbox_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    target.completion_count = target
        .completion_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    target.completion_bytes = target
        .completion_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    target.mailbox.push_back(MailboxEntry {
        message: message.clone(),
        claimed_turn_id: None,
    });
    let root = state
        .roots
        .get_mut(&root_agent_id)
        .expect("完成通知目标所属根树应存在");
    root.mailbox_count = root
        .mailbox_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.mailbox_bytes = root
        .mailbox_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.completion_count = root
        .completion_count
        .checked_add(1)
        .ok_or(CollaborationError::SequenceExhausted)?;
    root.completion_bytes = root
        .completion_bytes
        .checked_add(message_bytes)
        .ok_or(CollaborationError::SequenceExhausted)?;
    push_event(
        state,
        events,
        &target_definition,
        EventLink {
            source_agent_id: source_definition.agent_id.clone(),
            turn_id: Some(related_turn_id.clone()),
            parent_turn_id,
            root_turn_id: Some(root_turn_id),
        },
        CollaborationEventKind::AgentMessageQueued {
            message: message.clone(),
        },
    )?;
    mark_activity(state, target_agent_id, actions)?;
    if let Some(turn_id) = running_turn_id {
        queue_turn_signal(
            state,
            target_agent_id,
            &turn_id,
            AgentTurnSignalKind::MailboxAvailable,
            actions,
        )?;
    }
    Ok(message)
}

/// 当正在运行的 Turn 结束时，为未在安全边界消费的 Followup 创建一个后续 Turn。
fn claim_followup_after_turn(
    state: &mut CoordinatorState,
    agent_id: &AgentId,
    completed: &TurnRecord,
    events: &mut Vec<CollaborationEvent>,
) -> Result<(), CollaborationError> {
    let agent = resident_agent(state, agent_id)?;
    let root_agent_id = agent.definition.root_agent_id.clone();
    let plan_guard = agent.definition.profile.plan_guard;
    let Some(first) = agent
        .mailbox
        .iter()
        .find(|entry| {
            entry.message.delivery == MailboxDelivery::TriggerTurn
                && entry
                    .claimed_turn_id
                    .as_ref()
                    .is_none_or(|turn_id| turn_id == &completed.turn_id)
        })
        .cloned()
    else {
        return Ok(());
    };
    let next_turn_id = allocate_turn_id(state, &root_agent_id)?;
    let agent = state
        .agents
        .get_mut(agent_id)
        .expect("Followup 目标在上方已校验");
    for entry in &mut agent.mailbox {
        if entry.message.delivery == MailboxDelivery::TriggerTurn
            && entry
                .claimed_turn_id
                .as_ref()
                .is_none_or(|turn_id| turn_id == &completed.turn_id)
        {
            entry.claimed_turn_id = Some(next_turn_id.clone());
        }
    }
    rebind_pending_inputs(agent, &completed.turn_id, &next_turn_id);
    let queued = QueuedTurn {
        agent_id: agent_id.clone(),
        root_agent_id,
        turn_id: next_turn_id,
        source_agent_id: first.message.source_agent_id.clone(),
        parent_turn_id: first.message.parent_turn_id.clone(),
        root_turn_id: first.message.root_turn_id.clone().ok_or_else(|| {
            CollaborationError::InvalidRecovery {
                message: "Followup mailbox 缺少原始根 Turn 因果".to_owned(),
            }
        })?,
        cause: AgentTurnCause::Followup {
            message_id: first.message.message_id,
        },
        prompt: None,
        plan_guard,
    };
    queue_turn(state, queued, events)
}

/// 将执行器终态转换为 Agent 空闲状态。
fn outcome_status(turn_id: &TurnId, outcome: &AgentTurnOutcome) -> CollaborationAgentStatus {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => CollaborationAgentStatus::Completed {
            turn_id: turn_id.clone(),
            final_message: final_message.clone(),
        },
        AgentTurnOutcome::Interrupted => CollaborationAgentStatus::Interrupted {
            turn_id: turn_id.clone(),
        },
        AgentTurnOutcome::Failed { message } => CollaborationAgentStatus::Failed {
            turn_id: turn_id.clone(),
            message: message.clone(),
        },
    }
}

/// 将执行器终态转换为与 mailbox 入队分离的 Turn 终态事件。
fn terminal_event_kind(outcome: &AgentTurnOutcome) -> CollaborationEventKind {
    match outcome {
        AgentTurnOutcome::Completed { final_message } => {
            CollaborationEventKind::AgentTurnCompleted {
                final_message: final_message.clone(),
            }
        }
        AgentTurnOutcome::Interrupted => CollaborationEventKind::AgentTurnInterrupted,
        AgentTurnOutcome::Failed { message } => CollaborationEventKind::AgentTurnFailed {
            message: message.clone(),
        },
    }
}

/// 为子 Agent 终态 mailbox 生成稳定且简短的完整文本。
fn child_completion_content(path: &AgentPath, outcome: &AgentTurnOutcome) -> String {
    match outcome {
        AgentTurnOutcome::Completed {
            final_message: Some(message),
        } => format!("子 Agent {path} 已完成\n\n{message}"),
        AgentTurnOutcome::Completed {
            final_message: None,
        } => format!("子 Agent {path} 已完成"),
        AgentTurnOutcome::Interrupted => format!("子 Agent {path} 已中断"),
        AgentTurnOutcome::Failed { message } => {
            format!("子 Agent {path} 已失败\n\n{message}")
        }
    }
}

/// 将已经完整校验的冷恢复 Agent 转换为驻留状态与新的唤醒通道。
fn agent_entry_from_recovered(agent: &RecoveredAgent) -> AgentEntry {
    let (sender, _receiver) = watch::channel(0);
    let completion_count = agent
        .mailbox
        .iter()
        .filter(|entry| {
            matches!(
                &entry.message.kind,
                MailboxMessageKind::ChildTurnFinished { .. }
            )
        })
        .count();
    let completion_bytes = agent
        .mailbox
        .iter()
        .filter(|entry| {
            matches!(
                &entry.message.kind,
                MailboxMessageKind::ChildTurnFinished { .. }
            )
        })
        .map(|entry| entry.message.content.len())
        .sum();
    AgentEntry {
        definition: agent.definition.clone(),
        status: agent.status.clone(),
        mailbox_bytes: agent
            .mailbox
            .iter()
            .map(|entry| entry.message.content.len())
            .sum(),
        completion_count,
        completion_bytes,
        mailbox: agent
            .mailbox
            .iter()
            .cloned()
            .map(|entry| MailboxEntry {
                message: entry.message,
                claimed_turn_id: entry.claimed_turn_id,
            })
            .collect(),
        next_mailbox_sequence: agent.next_mailbox_sequence,
        mailbox_claim: agent
            .mailbox_claim_turn_id
            .clone()
            .zip(agent.mailbox_claim_through_sequence)
            .map(|(turn_id, through_sequence)| InputBatchClaim {
                turn_id,
                through_sequence,
            }),
        steers: agent.pending_steers.iter().cloned().collect(),
        steer_bytes: agent
            .pending_steers
            .iter()
            .map(|steer| steer.content.len())
            .sum(),
        next_steer_sequence: agent.next_steer_sequence,
        steer_claim: agent
            .steer_claim_turn_id
            .clone()
            .zip(agent.steer_claim_through_sequence)
            .map(|(turn_id, through_sequence)| InputBatchClaim {
                turn_id,
                through_sequence,
            }),
        last_turn: agent.last_turn.clone().map(|turn| TurnRecord {
            turn_id: turn.turn_id,
            cause: turn.cause,
            prompt: turn.prompt,
            parent_turn_id: turn.parent_turn_id,
            root_turn_id: turn.root_turn_id,
            outcome: turn.outcome,
        }),
        activity_version: 0,
        activity_sender: sender,
    }
}

/// 将驻留 Agent 投影为 Session Store 定期快照数据。
fn recovered_agent_from_entry(
    state: &CoordinatorState,
    agent: &AgentEntry,
) -> Result<RecoveredAgent, CollaborationError> {
    let current = match &agent.status {
        CollaborationAgentStatus::WaitingCapacity { turn_id } => {
            let queued = state
                .pending_turns
                .iter()
                .find(|queued| {
                    queued.agent_id == agent.definition.agent_id && &queued.turn_id == turn_id
                })
                .ok_or_else(|| CollaborationError::InvalidRecovery {
                    message: "等待容量 Agent 缺少 pending Turn".to_owned(),
                })?;
            Some((
                queued.source_agent_id.clone(),
                queued.cause.clone(),
                queued.prompt.clone(),
                queued.parent_turn_id.clone(),
                queued.root_turn_id.clone(),
                queued.plan_guard,
                false,
            ))
        }
        CollaborationAgentStatus::Running { turn_id }
        | CollaborationAgentStatus::Cancelling { turn_id } => {
            let active = state.active_turns.get(turn_id).ok_or_else(|| {
                CollaborationError::InvalidRecovery {
                    message: "活跃 Agent 缺少 active Turn".to_owned(),
                }
            })?;
            Some((
                active.source_agent_id.clone(),
                active.cause.clone(),
                active.prompt.clone(),
                active.parent_turn_id.clone(),
                active.root_turn_id.clone(),
                active.plan_guard,
                state.start_outbox.contains_key(turn_id),
            ))
        }
        _ => None,
    };
    Ok(RecoveredAgent {
        definition: agent.definition.clone(),
        status: agent.status.clone(),
        mailbox: agent
            .mailbox
            .iter()
            .map(|entry| RecoveredMailboxMessage {
                message: entry.message.clone(),
                claimed_turn_id: entry.claimed_turn_id.clone(),
            })
            .collect(),
        next_mailbox_sequence: agent.next_mailbox_sequence,
        mailbox_claim_turn_id: agent
            .mailbox_claim
            .as_ref()
            .map(|claim| claim.turn_id.clone()),
        mailbox_claim_through_sequence: agent
            .mailbox_claim
            .as_ref()
            .map(|claim| claim.through_sequence),
        next_steer_sequence: agent.next_steer_sequence,
        steer_claim_turn_id: agent
            .steer_claim
            .as_ref()
            .map(|claim| claim.turn_id.clone()),
        steer_claim_through_sequence: agent
            .steer_claim
            .as_ref()
            .map(|claim| claim.through_sequence),
        last_turn: agent.last_turn.as_ref().map(|turn| RecoveredTurn {
            turn_id: turn.turn_id.clone(),
            cause: turn.cause.clone(),
            prompt: turn.prompt.clone(),
            parent_turn_id: turn.parent_turn_id.clone(),
            root_turn_id: turn.root_turn_id.clone(),
            outcome: turn.outcome.clone(),
        }),
        current_source_agent_id: current.as_ref().map(|current| current.0.clone()),
        current_turn_cause: current.as_ref().map(|current| current.1.clone()),
        current_turn_prompt: current.as_ref().and_then(|current| current.2.clone()),
        current_parent_turn_id: current.as_ref().and_then(|current| current.3.clone()),
        current_root_turn_id: current.as_ref().map(|current| current.4.clone()),
        current_plan_guard: current.as_ref().map(|current| current.5),
        pending_steers: agent.steers.iter().cloned().collect(),
        start_pending: current.as_ref().is_some_and(|current| current.6),
    })
}
