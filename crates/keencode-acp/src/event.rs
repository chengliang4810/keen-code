//! 标准 ACP 无法完整表达的 KeenCode 类型化生命周期事件。

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use url::{Host, Url};

use crate::AcpBoundaryError;
use crate::json::{
    JsonValueLimits, input_preserved, parse_raw_value, validate_identifier, validate_text,
};

/// 当前 KeenCode 扩展事件 Schema 版本。
pub const KEENCODE_EVENT_SCHEMA_VERSION: u16 = 1;
/// 事件标识允许的最大 UTF-8 字节数。
const MAX_EVENT_IDENTIFIER_BYTES: usize = 256;
/// Agent 路径允许的最大 UTF-8 字节数。
const MAX_AGENT_PATH_BYTES: usize = 1024;
/// 已脱敏用户可见事件说明允许的最大 UTF-8 字节数。
const MAX_EVENT_MESSAGE_BYTES: usize = 4096;
/// Agent 委派任务正文允许的最大 UTF-8 字节数。
const MAX_AGENT_TASK_BYTES: usize = 256 * 1024;
/// MCP OAuth 授权地址允许的最大 UTF-8 字节数。
const MAX_OAUTH_AUTHORIZATION_URL_BYTES: usize = 4096;
/// MCP OAuth 项目路径允许的最大 UTF-8 字节数。
const MAX_OAUTH_PROJECT_PATH_BYTES: usize = 4 * 1024;
/// 单次模型请求允许上报的最大重试次数。
const MAX_MODEL_RETRY_ATTEMPTS: u32 = 32;
/// 单次模型重试允许等待的最大毫秒数。
const MAX_MODEL_RETRY_DELAY_MS: u64 = 10 * 60 * 1000;
/// 单个后台任务允许记录的最大持续时间，当前为三十天。
const MAX_BACKGROUND_TASK_DURATION_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// 从持久字节恢复单个 KeenCode 事件时使用的资源边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeenCodeEventLimits {
    /// 单个事件信封允许的最大原始 JSON 字节数。
    max_bytes: usize,
    /// 事件 JSON 允许的最大容器嵌套层数。
    max_depth: usize,
    /// 事件 JSON 允许的最大值节点数。
    max_nodes: usize,
}

impl KeenCodeEventLimits {
    /// 创建所有上限均大于零的事件恢复边界。
    pub const fn new(
        max_bytes: usize,
        max_depth: usize,
        max_nodes: usize,
    ) -> Result<Self, AcpBoundaryError> {
        if max_bytes == 0 || max_depth == 0 || max_nodes == 0 {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self {
            max_bytes,
            max_depth,
            max_nodes,
        })
    }

    /// 返回单个事件信封的最大原始 JSON 字节数。
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// 返回事件 JSON 的最大容器嵌套层数。
    pub const fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// 返回事件 JSON 的最大值节点数。
    pub const fn max_nodes(&self) -> usize {
        self.max_nodes
    }

    /// 转换为共享严格 JSON 解码边界。
    const fn json_limits(self) -> JsonValueLimits {
        JsonValueLimits {
            max_bytes: self.max_bytes,
            max_depth: self.max_depth,
            max_nodes: self.max_nodes,
        }
    }
}

impl Default for KeenCodeEventLimits {
    /// 返回适合本地事件日志单条记录的保守边界。
    fn default() -> Self {
        Self {
            // 单条事件必须容纳最大 Agent 任务及其 JSON 信封开销。
            max_bytes: 512 * 1024,
            max_depth: 32,
            max_nodes: 16_384,
        }
    }
}

/// 创建或恢复 KeenCode 扩展事件信封所需的结构化参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeenCodeEventEnvelopeParams {
    /// 根 Session 的稳定标识。
    session_id: String,
    /// 产生事件的 Turn；Session 级事件没有该值。
    turn_id: Option<String>,
    /// 产生事件的根 Agent 或单层子 Agent；Session 级事件没有该值。
    source_agent_id: Option<String>,
    /// Session 当前 Runtime 内严格单调递增的投递序号。
    delivery_sequence: u64,
    /// 事件发生时的 UTC Unix 毫秒时间。
    occurred_at_ms: u64,
    /// 带稳定 `type` 判别字段的事件载荷。
    event: KeenCodeEvent,
}

impl KeenCodeEventEnvelopeParams {
    /// 创建不绑定 Turn 或 Agent 的 Session 级事件参数。
    pub fn for_session(
        session_id: impl Into<String>,
        delivery_sequence: u64,
        occurred_at_ms: u64,
        event: KeenCodeEvent,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            turn_id: None,
            source_agent_id: None,
            delivery_sequence,
            occurred_at_ms,
            event,
        }
    }

    /// 创建同时绑定明确 Turn 和来源 Agent 的 Turn 级事件参数。
    pub fn for_turn(
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
        source_agent_id: impl Into<String>,
        delivery_sequence: u64,
        occurred_at_ms: u64,
        event: KeenCodeEvent,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            turn_id: Some(turn_id.into()),
            source_agent_id: Some(source_agent_id.into()),
            delivery_sequence,
            occurred_at_ms,
            event,
        }
    }
}

/// 一个带独立 Journal/投递序号和来源身份的 KeenCode 扩展事件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeenCodeEventEnvelope {
    /// 扩展事件 Schema 版本，首版固定为一。
    schema_version: u16,
    /// 根 Session 的稳定标识。
    session_id: String,
    /// 产生事件的 Turn；Session 级事件没有该值。
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    /// 产生事件的根 Agent 或单层子 Agent；Session 级事件没有该值。
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_id: Option<String>,
    /// Session Journal 内严格单调递增的权威事件序号；临时事件必须省略。
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_sequence: Option<u64>,
    /// Session 当前 Runtime 内严格单调递增的投递序号。
    delivery_sequence: u64,
    /// 事件发生时的 UTC Unix 毫秒时间。
    occurred_at_ms: u64,
    /// 带稳定 `type` 判别字段的事件载荷。
    event: KeenCodeEvent,
}

impl KeenCodeEventEnvelope {
    /// 创建一个已写入权威 Journal 的首版扩展事件信封。
    pub fn new_authoritative(
        journal_sequence: u64,
        params: KeenCodeEventEnvelopeParams,
    ) -> Result<Self, AcpBoundaryError> {
        Self::restore_authoritative(KEENCODE_EVENT_SCHEMA_VERSION, journal_sequence, params)
    }

    /// 创建一个只存在于当前 Runtime 投递流、不写入 Journal 的首版临时事件信封。
    pub fn new_transient(params: KeenCodeEventEnvelopeParams) -> Result<Self, AcpBoundaryError> {
        Self::restore_transient(KEENCODE_EVENT_SCHEMA_VERSION, params)
    }

    /// 从权威 Journal 恢复事件信封，并拒绝临时事件和零 Journal 序号。
    pub fn restore_authoritative(
        schema_version: u16,
        journal_sequence: u64,
        params: KeenCodeEventEnvelopeParams,
    ) -> Result<Self, AcpBoundaryError> {
        Self::restore_with_journal(schema_version, Some(journal_sequence), params)
    }

    /// 恢复不写入 Journal 的临时事件信封，并拒绝权威事件。
    pub fn restore_transient(
        schema_version: u16,
        params: KeenCodeEventEnvelopeParams,
    ) -> Result<Self, AcpBoundaryError> {
        Self::restore_with_journal(schema_version, None, params)
    }

    /// 按线格式中的可选 Journal 位置恢复信封，并统一执行全部不变量校验。
    fn restore_with_journal(
        schema_version: u16,
        journal_sequence: Option<u64>,
        params: KeenCodeEventEnvelopeParams,
    ) -> Result<Self, AcpBoundaryError> {
        let envelope = Self {
            schema_version,
            session_id: params.session_id,
            turn_id: params.turn_id,
            source_agent_id: params.source_agent_id,
            journal_sequence,
            delivery_sequence: params.delivery_sequence,
            occurred_at_ms: params.occurred_at_ms,
            event: params.event,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    /// 从未解析原始 JSON 恢复事件，并在分配 DOM 前执行默认字节边界。
    pub fn decode_raw(raw: &[u8]) -> Result<Self, AcpBoundaryError> {
        Self::decode_raw_with_limits(raw, KeenCodeEventLimits::default())
    }

    /// 从未解析原始 JSON 恢复事件，并拒绝超额、重复键和非法线格式。
    pub fn decode_raw_with_limits(
        raw: &[u8],
        limits: KeenCodeEventLimits,
    ) -> Result<Self, AcpBoundaryError> {
        let value = parse_raw_value(raw, limits.json_limits())?;
        let wire = serde_json::from_value::<KeenCodeEventEnvelopeWire>(value.clone())
            .map_err(|_| AcpBoundaryError::InvalidParams)?;
        let (schema_version, journal_sequence, params) = wire.into_parts();
        let envelope = Self::restore_with_journal(schema_version, journal_sequence, params)?;
        let normalized =
            serde_json::to_value(&envelope).map_err(|_| AcpBoundaryError::InvalidParams)?;
        if !input_preserved(&value, &normalized) {
            return Err(AcpBoundaryError::InvalidParams);
        }
        Ok(envelope)
    }

    /// 校验当前信封版本、公共身份、事件内部字段和跨字段不变量。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        if self.schema_version != KEENCODE_EVENT_SCHEMA_VERSION
            || self.delivery_sequence == 0
            || self.occurred_at_ms == 0
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        match (self.event.is_authoritative(), self.journal_sequence) {
            (true, Some(sequence)) if sequence > 0 => {}
            (false, None) => {}
            _ => return Err(AcpBoundaryError::InvalidSemanticValue),
        }
        validate_identifier(&self.session_id, MAX_EVENT_IDENTIFIER_BYTES)?;
        if let Some(turn_id) = &self.turn_id {
            validate_identifier(turn_id, MAX_EVENT_IDENTIFIER_BYTES)?;
        }
        if let Some(source_agent_id) = &self.source_agent_id {
            validate_identifier(source_agent_id, MAX_EVENT_IDENTIFIER_BYTES)?;
        }
        self.event.validate()?;
        if let (
            KeenCodeEvent::ContextCompactionCompleted {
                replaced_through_sequence,
                ..
            },
            Some(journal_sequence),
        ) = (&self.event, self.journal_sequence)
        {
            if *replaced_through_sequence >= journal_sequence {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
        }
        self.validate_identity()
    }

    /// 返回扩展事件 Schema 版本。
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// 返回根 Session 稳定标识。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 返回产生事件的可选 Turn 标识。
    pub fn turn_id(&self) -> Option<&str> {
        self.turn_id.as_deref()
    }

    /// 返回产生事件的可选 Agent 标识。
    pub fn source_agent_id(&self) -> Option<&str> {
        self.source_agent_id.as_deref()
    }

    /// 返回 Session Journal 内的权威事件序号；临时事件没有该值。
    pub const fn journal_sequence(&self) -> Option<u64> {
        self.journal_sequence
    }

    /// 判断信封是否承载已经写入权威 Journal 的事件。
    pub const fn is_authoritative(&self) -> bool {
        self.journal_sequence.is_some()
    }

    /// 返回 Session 当前 Runtime 内的投递序号。
    pub const fn delivery_sequence(&self) -> u64 {
        self.delivery_sequence
    }

    /// 返回事件发生时的 UTC Unix 毫秒时间。
    pub const fn occurred_at_ms(&self) -> u64 {
        self.occurred_at_ms
    }

    /// 返回类型化事件载荷。
    pub const fn event(&self) -> &KeenCodeEvent {
        &self.event
    }

    /// 校验 Session 级、双作用域和 Turn 级事件的身份形状及事件特定绑定。
    fn validate_identity(&self) -> Result<(), AcpBoundaryError> {
        let session_scoped = matches!(
            self.event,
            KeenCodeEvent::RecoveryStateChanged { .. }
                | KeenCodeEvent::GoalChanged { .. }
                | KeenCodeEvent::BackgroundTaskCompleted { .. }
        );
        if session_scoped {
            if self.turn_id.is_some() || self.source_agent_id.is_some() {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
            return Ok(());
        }
        if matches!(self.event, KeenCodeEvent::SystemNotification { .. }) {
            match (self.turn_id.as_ref(), self.source_agent_id.as_ref()) {
                (None, None) | (Some(_), Some(_)) => return Ok(()),
                _ => return Err(AcpBoundaryError::InvalidSemanticValue),
            }
        }
        let (Some(turn_id), Some(source_agent_id)) =
            (self.turn_id.as_deref(), self.source_agent_id.as_deref())
        else {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        };

        match &self.event {
            KeenCodeEvent::TurnStarted {
                root_turn_id,
                parent_turn_id,
            } => match parent_turn_id {
                None if root_turn_id != turn_id => {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
                Some(parent_turn_id)
                    if parent_turn_id == turn_id
                        || root_turn_id == turn_id
                        || parent_turn_id != root_turn_id =>
                {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
                _ => {}
            },
            KeenCodeEvent::AgentSpawned {
                parent_agent_id,
                parent_turn_id,
                ..
            } => {
                if parent_agent_id != source_agent_id || parent_turn_id != turn_id {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
            }
            KeenCodeEvent::AgentStatusChanged { agent_id, .. } => {
                if agent_id != source_agent_id {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
            }
            KeenCodeEvent::AgentMessageQueued { from_agent_id, .. }
                if from_agent_id != source_agent_id =>
            {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
            _ => {}
        }
        Ok(())
    }
}

/// 只用于严格反序列化再进入恢复校验入口的线格式。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KeenCodeEventEnvelopeWire {
    /// 扩展事件 Schema 版本。
    schema_version: u16,
    /// 根 Session 稳定标识。
    session_id: String,
    /// 可选 Turn 标识。
    turn_id: Option<String>,
    /// 可选来源 Agent 标识。
    source_agent_id: Option<String>,
    /// Session Journal 内的权威事件序号；临时事件必须省略。
    journal_sequence: Option<u64>,
    /// Session 当前 Runtime 内的投递序号。
    delivery_sequence: u64,
    /// UTC Unix 毫秒时间。
    occurred_at_ms: u64,
    /// 类型化事件载荷。
    event: KeenCodeEvent,
}

impl KeenCodeEventEnvelopeWire {
    /// 拆出 Schema、可选 Journal 位置和公共构造参数。
    fn into_parts(self) -> (u16, Option<u64>, KeenCodeEventEnvelopeParams) {
        (
            self.schema_version,
            self.journal_sequence,
            KeenCodeEventEnvelopeParams {
                session_id: self.session_id,
                turn_id: self.turn_id,
                source_agent_id: self.source_agent_id,
                delivery_sequence: self.delivery_sequence,
                occurred_at_ms: self.occurred_at_ms,
                event: self.event,
            },
        )
    }
}

/// Turn 失败的稳定安全分类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailureKind {
    /// Provider 或协议边界失败。
    Model,
    /// 上下文预算或压缩失败。
    Context,
    /// 工具或 Hook 执行失败。
    Tool,
    /// 持久化提交失败。
    Storage,
    /// Runtime 内部不变量失败。
    Internal,
}

/// 上下文压缩失败的稳定分类。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionFailureKind {
    /// 摘要模型请求失败。
    Model,
    /// 压缩结果无法满足上下文预算。
    Budget,
    /// 压缩记录无法持久提交。
    Storage,
    /// 压缩结果违反 Transcript 不变量。
    InvalidResult,
}

/// 单层 Agent 对桌面投影公开的生命周期状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLifecycleStatus {
    /// Agent 已创建但尚未开始 Turn。
    Pending,
    /// Agent 正在执行 Turn。
    Running,
    /// Agent 正等待 mailbox、用户输入或显式继续。
    Waiting,
    /// 最近一次 Turn 正常完成，身份仍可继续使用。
    Completed,
    /// 最近一次 Turn 被中断，身份仍可继续使用。
    Interrupted,
    /// 最近一次 Turn 失败，身份仍可重试。
    Failed,
    /// Agent 已永久关闭。
    Stopped,
}

/// Session 崩溃恢复和重放状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryState {
    /// 尚未开始恢复。
    Pending,
    /// 正在读取事件日志和快照。
    Replaying,
    /// 已恢复到一致快照。
    Ready,
    /// 发现不可恢复的日志或快照错误。
    Failed,
}

/// 系统通知允许驱动的三个稳定展示等级。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemNotificationLevel {
    /// 普通状态或提示信息。
    Info,
    /// 需要用户留意但未终止当前 Turn 的警告。
    Warning,
    /// 当前操作已经失败的错误提示。
    Error,
}

/// Session 级异步后台任务的稳定类别。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskKind {
    /// 由后台终端或非交互命令执行器拥有的任务。
    Shell,
    /// 由单层异步子 Agent 拥有的任务。
    Agent,
}

/// 后台任务完成事件允许使用的终态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskTerminalStatus {
    /// 后台任务正常完成。
    Succeeded,
    /// 后台任务执行失败。
    Failed,
    /// 后台任务被用户或 Runtime 取消。
    Cancelled,
}

/// MCP OAuth 生命周期事件；通过独立 JSON-RPC 通知发送，不进入 Session 事件信封。
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum McpOAuthEvent {
    /// 一个 MCP Server 需要用户在浏览器中完成 OAuth 授权。
    #[serde(rename = "mcp_oauth_authorization_required")]
    AuthorizationRequired {
        /// 产生事件的项目路径。
        project_path: String,
        /// MCP Server 稳定名称。
        server_name: String,
        /// 不含用户凭据且可安全打开的授权地址。
        authorization_url: String,
    },
    /// 一个 MCP Server 已完成或恢复 OAuth 授权。
    #[serde(rename = "mcp_oauth_authorized")]
    Authorized {
        /// 产生事件的项目路径。
        project_path: String,
        /// MCP Server 稳定名称。
        server_name: String,
    },
    /// 一个 MCP Server 的 OAuth 授权失败。
    #[serde(rename = "mcp_oauth_failed")]
    Failed {
        /// 产生事件的项目路径。
        project_path: String,
        /// MCP Server 稳定名称。
        server_name: String,
        /// 已脱敏且可直接展示的失败说明。
        message: String,
    },
}

impl fmt::Debug for McpOAuthEvent {
    /// 只显示事件类型和字段存在性，不回显项目路径、Server 名称或 OAuth 内容。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("McpOAuthEvent");
        match self {
            Self::AuthorizationRequired {
                project_path,
                server_name,
                authorization_url,
            } => debug
                .field("type", &"mcp_oauth_authorization_required")
                .field("project_path_present", &!project_path.is_empty())
                .field("server_name_present", &!server_name.is_empty())
                .field("authorization_url_present", &!authorization_url.is_empty()),
            Self::Authorized {
                project_path,
                server_name,
            } => debug
                .field("type", &"mcp_oauth_authorized")
                .field("project_path_present", &!project_path.is_empty())
                .field("server_name_present", &!server_name.is_empty()),
            Self::Failed {
                project_path,
                server_name,
                message,
            } => debug
                .field("type", &"mcp_oauth_failed")
                .field("project_path_present", &!project_path.is_empty())
                .field("server_name_present", &!server_name.is_empty())
                .field("message_present", &!message.is_empty()),
        }
        .finish()
    }
}

/// 仅供 Serde 严格构造 McpOAuthEvent 后进入语义校验的远端定义。
#[derive(Deserialize)]
#[serde(
    remote = "McpOAuthEvent",
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum McpOAuthEventDef {
    /// 一个 MCP Server 需要用户在浏览器中完成 OAuth 授权。
    #[serde(rename = "mcp_oauth_authorization_required")]
    AuthorizationRequired {
        /// 产生事件的项目路径。
        project_path: String,
        /// MCP Server 稳定名称。
        server_name: String,
        /// 不含用户凭据且可安全打开的授权地址。
        authorization_url: String,
    },
    /// 一个 MCP Server 已完成或恢复 OAuth 授权。
    #[serde(rename = "mcp_oauth_authorized")]
    Authorized {
        /// 产生事件的项目路径。
        project_path: String,
        /// MCP Server 稳定名称。
        server_name: String,
    },
    /// 一个 MCP Server 的 OAuth 授权失败。
    #[serde(rename = "mcp_oauth_failed")]
    Failed {
        /// 产生事件的项目路径。
        project_path: String,
        /// MCP Server 稳定名称。
        server_name: String,
        /// 已脱敏且可直接展示的失败说明。
        message: String,
    },
}

impl<'de> Deserialize<'de> for McpOAuthEvent {
    /// 严格反序列化 OAuth 事件，并在返回前执行路径、标识和 URL 校验。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let event = McpOAuthEventDef::deserialize(deserializer)?;
        event
            .validate()
            .map_err(|_| D::Error::custom("invalid MCP OAuth event"))?;
        Ok(event)
    }
}

impl McpOAuthEvent {
    /// 校验 OAuth 事件中的项目路径、Server 标识、授权地址和脱敏说明。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        match self {
            Self::AuthorizationRequired {
                project_path,
                server_name,
                authorization_url,
            } => {
                validate_oauth_project_path(project_path)?;
                validate_identifier(server_name, MAX_EVENT_IDENTIFIER_BYTES)?;
                validate_oauth_authorization_url(authorization_url)?;
            }
            Self::Authorized {
                project_path,
                server_name,
            } => {
                validate_oauth_project_path(project_path)?;
                validate_identifier(server_name, MAX_EVENT_IDENTIFIER_BYTES)?;
            }
            Self::Failed {
                project_path,
                server_name,
                message,
            } => {
                validate_oauth_project_path(project_path)?;
                validate_identifier(server_name, MAX_EVENT_IDENTIFIER_BYTES)?;
                validate_text(message, MAX_EVENT_MESSAGE_BYTES)?;
            }
        }
        Ok(())
    }
}

/// 标准 ACP SessionUpdate 之外的 KeenCode 生命周期事件。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum KeenCodeEvent {
    /// 一个 Turn 已经持久创建并取得执行权。
    TurnStarted {
        /// 当前 Agent 树根 Turn 的稳定标识。
        root_turn_id: String,
        /// 子 Agent Turn 的直接父 Turn；根 Turn 没有该值。
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_turn_id: Option<String>,
    },
    /// 一个 Turn 已经持久提交唯一正常终态。
    TurnCompleted,
    /// 一个 Turn 已经持久提交唯一取消终态。
    TurnCancelled,
    /// 一个 Turn 已经持久提交唯一失败终态。
    TurnFailed {
        /// 不包含 Provider 正文、工具输出或凭据的错误分类。
        failure_kind: TurnFailureKind,
        /// 有界且已经脱敏的用户可见说明。
        message: String,
    },
    /// 根 Agent 创建了一个单层子 Agent 身份。
    AgentSpawned {
        /// 新子 Agent 的稳定标识。
        agent_id: String,
        /// 直接父 Agent 的稳定标识。
        parent_agent_id: String,
        /// 从根到当前 Agent 的稳定路径。
        agent_path: String,
        /// 父 Agent 直接下发的完整任务正文。
        task: String,
        /// 创建子 Agent 的父 Turn。
        parent_turn_id: String,
        /// 当前 Agent 树的根 Turn。
        root_turn_id: String,
    },
    /// 一个 Agent 的最近 Turn 或身份状态发生变化。
    AgentStatusChanged {
        /// 状态发生变化的 Agent。
        agent_id: String,
        /// 新生命周期状态。
        status: AgentLifecycleStatus,
    },
    /// 一条 exactly-once mailbox 消息已经持久排队。
    AgentMessageQueued {
        /// mailbox 消息稳定标识。
        message_id: String,
        /// 发送消息的 Agent。
        from_agent_id: String,
        /// 接收消息的 Agent。
        to_agent_id: String,
    },
    /// Runtime 开始一次上下文压缩尝试。
    ContextCompactionStarted {
        /// 压缩前估算的输入 Token。
        estimated_tokens: u64,
    },
    /// Runtime 原子提交了一次上下文压缩结果。
    ContextCompactionCompleted {
        /// 被摘要覆盖的最后权威事件序号。
        replaced_through_sequence: u64,
        /// 压缩后估算的输入 Token。
        estimated_tokens: u64,
    },
    /// 上下文压缩失败且原 Transcript 保持不变。
    ContextCompactionFailed {
        /// 不包含模型正文的稳定失败分类。
        failure_kind: CompactionFailureKind,
    },
    /// Session 崩溃恢复或重放状态发生变化。
    RecoveryStateChanged {
        /// 新恢复状态。
        state: RecoveryState,
    },
    /// 项目级持久 Goal 发生变化。
    GoalChanged {
        /// Goal 稳定标识；Goal 被清除时可以没有值。
        #[serde(skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
        /// 当前 Goal 单调修订号。
        revision: u64,
        /// 当前 Goal 状态；Goal 被清除时可以没有值。
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    /// Runtime 向 Session 准备阶段或当前 Turn 时间线发布一条用户可见系统通知。
    SystemNotification {
        /// 通知的稳定展示等级。
        level: SystemNotificationLevel,
        /// 已脱敏且可直接展示的通知正文。
        message: String,
    },
    /// 当前模型请求失败后已经安排下一次重试。
    ModelRetryScheduled {
        /// 刚刚失败的尝试序号，从一开始计数。
        attempt: u32,
        /// 当前模型请求允许的最大尝试次数。
        max_attempts: u32,
        /// 下一次尝试前等待的毫秒数。
        delay_ms: u64,
        /// 已脱敏且可直接展示的重试原因。
        message: String,
    },
    /// 一个 Session 级后台任务已经完成；Agent 任务由 Journal 权威记录驱动，Shell 任务为临时通知。
    BackgroundTaskCompleted {
        /// 后台任务稳定标识。
        task_id: String,
        /// 用于选择 UI 图标、详情和取消语义的任务类别。
        task_kind: BackgroundTaskKind,
        /// Agent 任务的稳定 Agent 标识；Shell 任务必须没有该值。
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        /// 后台任务已经提交的唯一终态。
        status: BackgroundTaskTerminalStatus,
        /// 从任务开始到提交终态的持续毫秒数。
        duration_ms: u64,
        /// 可选的已脱敏、有界展示摘要；缺失时在线格式中省略。
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// 当前模型请求已经收到首个流事件，用于本地延迟观测。
    ModelFirstStreamObserved,
}

/// 仅供 Serde 严格构造 KeenCodeEvent 后进入语义校验的远端定义。
#[derive(Deserialize)]
#[serde(
    remote = "KeenCodeEvent",
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum KeenCodeEventDef {
    /// 一个 Turn 已经持久创建并取得执行权。
    TurnStarted {
        /// 当前 Agent 树根 Turn 的稳定标识。
        root_turn_id: String,
        /// 子 Agent Turn 的直接父 Turn；根 Turn 没有该值。
        parent_turn_id: Option<String>,
    },
    /// 一个 Turn 已经持久提交唯一正常终态。
    TurnCompleted,
    /// 一个 Turn 已经持久提交唯一取消终态。
    TurnCancelled,
    /// 一个 Turn 已经持久提交唯一失败终态。
    TurnFailed {
        /// 不包含 Provider 正文、工具输出或凭据的错误分类。
        failure_kind: TurnFailureKind,
        /// 有界且已经脱敏的用户可见说明。
        message: String,
    },
    /// 根 Agent 创建了一个单层子 Agent 身份。
    AgentSpawned {
        /// 新子 Agent 的稳定标识。
        agent_id: String,
        /// 直接父 Agent 的稳定标识。
        parent_agent_id: String,
        /// 从根到当前 Agent 的稳定路径。
        agent_path: String,
        /// 父 Agent 直接下发的完整任务正文。
        task: String,
        /// 创建子 Agent 的父 Turn。
        parent_turn_id: String,
        /// 当前 Agent 树的根 Turn。
        root_turn_id: String,
    },
    /// 一个 Agent 的最近 Turn 或身份状态发生变化。
    AgentStatusChanged {
        /// 状态发生变化的 Agent。
        agent_id: String,
        /// 新生命周期状态。
        status: AgentLifecycleStatus,
    },
    /// 一条 exactly-once mailbox 消息已经持久排队。
    AgentMessageQueued {
        /// mailbox 消息稳定标识。
        message_id: String,
        /// 发送消息的 Agent。
        from_agent_id: String,
        /// 接收消息的 Agent。
        to_agent_id: String,
    },
    /// Runtime 开始一次上下文压缩尝试。
    ContextCompactionStarted {
        /// 压缩前估算的输入 Token。
        estimated_tokens: u64,
    },
    /// Runtime 原子提交了一次上下文压缩结果。
    ContextCompactionCompleted {
        /// 被摘要覆盖的最后权威事件序号。
        replaced_through_sequence: u64,
        /// 压缩后估算的输入 Token。
        estimated_tokens: u64,
    },
    /// 上下文压缩失败且原 Transcript 保持不变。
    ContextCompactionFailed {
        /// 不包含模型正文的稳定失败分类。
        failure_kind: CompactionFailureKind,
    },
    /// Session 崩溃恢复或重放状态发生变化。
    RecoveryStateChanged {
        /// 新恢复状态。
        state: RecoveryState,
    },
    /// 项目级持久 Goal 发生变化。
    GoalChanged {
        /// Goal 稳定标识；Goal 被清除时可以没有值。
        goal_id: Option<String>,
        /// 当前 Goal 单调修订号。
        revision: u64,
        /// 当前 Goal 状态；Goal 被清除时可以没有值。
        status: Option<String>,
    },
    /// Runtime 向 Session 准备阶段或当前 Turn 时间线发布一条用户可见系统通知。
    SystemNotification {
        /// 通知的稳定展示等级。
        level: SystemNotificationLevel,
        /// 已脱敏且可直接展示的通知正文。
        message: String,
    },
    /// 当前模型请求失败后已经安排下一次重试。
    ModelRetryScheduled {
        /// 刚刚失败的尝试序号，从一开始计数。
        attempt: u32,
        /// 当前模型请求允许的最大尝试次数。
        max_attempts: u32,
        /// 下一次尝试前等待的毫秒数。
        delay_ms: u64,
        /// 已脱敏且可直接展示的重试原因。
        message: String,
    },
    /// 一个 Session 级后台任务已经完成；Agent 任务由 Journal 权威记录驱动，Shell 任务为临时通知。
    BackgroundTaskCompleted {
        /// 后台任务稳定标识。
        task_id: String,
        /// 用于选择 UI 图标、详情和取消语义的任务类别。
        task_kind: BackgroundTaskKind,
        /// Agent 任务的稳定 Agent 标识；Shell 任务必须没有该值。
        agent_id: Option<String>,
        /// 后台任务已经提交的唯一终态。
        status: BackgroundTaskTerminalStatus,
        /// 从任务开始到提交终态的持续毫秒数。
        duration_ms: u64,
        /// 可选的已脱敏、有界展示摘要；缺失时在线格式中省略。
        summary: Option<String>,
    },
    /// 当前模型请求已经收到首个流事件，用于本地延迟观测。
    ModelFirstStreamObserved,
}

impl<'de> Deserialize<'de> for KeenCodeEvent {
    /// 严格反序列化事件内部字段，并在返回前执行语义校验。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let event = KeenCodeEventDef::deserialize(deserializer)?;
        event
            .validate()
            .map_err(|_| D::Error::custom("invalid KeenCode event"))?;
        Ok(event)
    }
}

impl KeenCodeEvent {
    /// 判断事件是否必须先提交到权威 Journal，再向桌面投递。
    pub const fn is_authoritative(&self) -> bool {
        matches!(
            self,
            Self::TurnStarted { .. }
                | Self::TurnCompleted
                | Self::TurnCancelled
                | Self::TurnFailed { .. }
                | Self::AgentSpawned { .. }
                | Self::AgentStatusChanged { .. }
                | Self::AgentMessageQueued { .. }
                | Self::ContextCompactionCompleted { .. }
        )
    }

    /// 判断事件是否只属于当前 Runtime 的临时投递流，不得伪造 Journal 位置。
    pub const fn is_transient(&self) -> bool {
        !self.is_authoritative()
    }

    /// 校验每种事件内部字段、长度和必要的跨字段关系。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        match self {
            Self::TurnStarted {
                root_turn_id,
                parent_turn_id,
            } => {
                validate_identifier(root_turn_id, MAX_EVENT_IDENTIFIER_BYTES)?;
                if let Some(parent_turn_id) = parent_turn_id {
                    validate_identifier(parent_turn_id, MAX_EVENT_IDENTIFIER_BYTES)?;
                }
            }
            Self::TurnFailed { message, .. } => {
                validate_text(message, MAX_EVENT_MESSAGE_BYTES)?;
            }
            Self::AgentSpawned {
                agent_id,
                parent_agent_id,
                agent_path,
                task,
                parent_turn_id,
                root_turn_id,
            } => {
                for identifier in [agent_id, parent_agent_id, parent_turn_id, root_turn_id] {
                    validate_identifier(identifier, MAX_EVENT_IDENTIFIER_BYTES)?;
                }
                validate_identifier(agent_path, MAX_AGENT_PATH_BYTES)?;
                validate_text(task, MAX_AGENT_TASK_BYTES)?;
                if agent_id == parent_agent_id || parent_turn_id != root_turn_id {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
            }
            Self::AgentStatusChanged { agent_id, .. } => {
                validate_identifier(agent_id, MAX_EVENT_IDENTIFIER_BYTES)?;
            }
            Self::AgentMessageQueued {
                message_id,
                from_agent_id,
                to_agent_id,
            } => {
                for identifier in [message_id, from_agent_id, to_agent_id] {
                    validate_identifier(identifier, MAX_EVENT_IDENTIFIER_BYTES)?;
                }
                if from_agent_id == to_agent_id {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
            }
            Self::ContextCompactionCompleted {
                replaced_through_sequence,
                ..
            } if *replaced_through_sequence == 0 => {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
            Self::GoalChanged {
                goal_id,
                revision,
                status,
            } => {
                if *revision == 0 || goal_id.is_some() != status.is_some() {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
                if let Some(goal_id) = goal_id {
                    validate_identifier(goal_id, MAX_EVENT_IDENTIFIER_BYTES)?;
                }
                if let Some(status) = status {
                    if !matches!(status.as_str(), "active" | "completed" | "blocked") {
                        return Err(AcpBoundaryError::InvalidSemanticValue);
                    }
                }
            }
            Self::SystemNotification { message, .. } => {
                validate_text(message, MAX_EVENT_MESSAGE_BYTES)?;
            }
            Self::ModelRetryScheduled {
                attempt,
                max_attempts,
                delay_ms,
                message,
            } => {
                if *attempt == 0
                    || *max_attempts == 0
                    || *attempt >= *max_attempts
                    || *max_attempts > MAX_MODEL_RETRY_ATTEMPTS
                    || *delay_ms > MAX_MODEL_RETRY_DELAY_MS
                {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
                validate_text(message, MAX_EVENT_MESSAGE_BYTES)?;
            }
            Self::BackgroundTaskCompleted {
                task_id,
                task_kind,
                agent_id,
                duration_ms,
                summary,
                ..
            } => {
                validate_identifier(task_id, MAX_EVENT_IDENTIFIER_BYTES)?;
                match (task_kind, agent_id) {
                    (BackgroundTaskKind::Agent, Some(agent_id)) => {
                        validate_identifier(agent_id, MAX_EVENT_IDENTIFIER_BYTES)?;
                    }
                    (BackgroundTaskKind::Shell, None) => {}
                    _ => return Err(AcpBoundaryError::InvalidSemanticValue),
                }
                if *duration_ms > MAX_BACKGROUND_TASK_DURATION_MS {
                    return Err(AcpBoundaryError::InvalidSemanticValue);
                }
                if let Some(summary) = summary {
                    validate_redacted_summary(summary)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// 校验后台任务摘要非空、有界，且不含常见的未脱敏凭据线格式。
fn validate_redacted_summary(value: &str) -> Result<(), AcpBoundaryError> {
    validate_text(value, MAX_EVENT_MESSAGE_BYTES)?;
    let normalized = value.to_ascii_lowercase();
    const SENSITIVE_LABELS: [&str; 10] = [
        "authorization",
        "x-api-key",
        "api-key",
        "api_key",
        "apikey",
        "client_secret",
        "access_token",
        "refresh_token",
        "id_token",
        "cookie",
    ];
    if SENSITIVE_LABELS
        .iter()
        .any(|label| contains_unredacted_labeled_value(&normalized, label))
        || contains_prefixed_secret(&normalized, "sk-")
    {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    Ok(())
}

/// 判断文本是否包含标签后未替换为明确占位符的凭据值。
fn contains_unredacted_labeled_value(value: &str, label: &str) -> bool {
    value.match_indices(label).any(|(offset, _)| {
        let mut remainder = &value[offset + label.len()..];
        remainder = remainder.trim_start();
        if let Some(after_quote) = remainder.strip_prefix(['"', '\'']) {
            remainder = after_quote.trim_start();
        }
        let Some(after_separator) = remainder.strip_prefix([':', '=']) else {
            return false;
        };
        let candidate = after_separator
            .trim_start()
            .trim_start_matches(['"', '\'', ' ']);
        !candidate.is_empty()
            && !candidate.starts_with("[redacted]")
            && !candidate.starts_with("<redacted>")
            && !candidate.starts_with("[已脱敏]")
            && !candidate.starts_with("bearer [redacted]")
            && !candidate.starts_with("basic [redacted]")
            && !candidate.starts_with("***")
    })
}

/// 判断文本是否包含长度足以构成凭据的常见固定前缀 Token。
fn contains_prefixed_secret(value: &str, prefix: &str) -> bool {
    value.match_indices(prefix).any(|(offset, _)| {
        value[offset..]
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
            .count()
            >= 20
    })
}

/// 校验 OAuth 事件项目路径非空、有界且不含控制字符。
fn validate_oauth_project_path(value: &str) -> Result<(), AcpBoundaryError> {
    validate_identifier(value, MAX_OAUTH_PROJECT_PATH_BYTES)
}

/// 校验 OAuth 授权地址有界、无凭据，且只允许 HTTPS 或本机 HTTP。
fn validate_oauth_authorization_url(value: &str) -> Result<(), AcpBoundaryError> {
    validate_text(value, MAX_OAUTH_AUTHORIZATION_URL_BYTES)?;
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    let parsed = Url::parse(value).map_err(|_| AcpBoundaryError::InvalidSemanticValue)?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.fragment().is_some() {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    let Some(host) = parsed.host() else {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    };
    let safe_scheme =
        parsed.scheme() == "https" || (parsed.scheme() == "http" && is_loopback_host(host));
    if !safe_scheme
        || parsed
            .query_pairs()
            .any(|(name, _)| is_sensitive_query_name(&name))
    {
        return Err(AcpBoundaryError::InvalidSemanticValue);
    }
    Ok(())
}

/// 判断授权地址主机是否为本机回环地址。
fn is_loopback_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain == "localhost" || domain.ends_with(".localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

/// 判断 OAuth 查询参数名是否明确承载凭据。
fn is_sensitive_query_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "access_token"
            | "refresh_token"
            | "id_token"
            | "client_secret"
            | "api_key"
            | "apikey"
            | "authorization"
    )
}
