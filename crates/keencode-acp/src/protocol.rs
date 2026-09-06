//! 标准 ACP 请求与 KeenCode 当前唯一扩展方法的严格解码。

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use agent_client_protocol_schema::{
    AgentRequest, AuthenticateRequest, AuthenticateResponse, CancelNotification,
    CreateElicitationRequest, CreateElicitationResponse, Error as AcpRpcError, ForkSessionRequest,
    ForkSessionResponse, InitializeRequest, JsonRpcMessage, ListSessionsRequest,
    ListSessionsResponse, LoadSessionRequest, LoadSessionResponse, Meta, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, Request as AcpRpcRequest, RequestId,
    Response as AcpRpcResponse, SessionId, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse,
};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{BackgroundTaskKind, McpOAuthEvent};
use crate::file_change::{ReadFileChangeRequest, ReadFileChangeResponse};
use crate::json::{
    JsonValueLimits, input_preserved, parse_raw_value, validate_identifier, validate_text,
    validate_value,
};
use crate::{AcpBoundaryError, ElicitationRouter, InitializeResponseDto};

/// 扩展协议标识允许的最大 UTF-8 字节数。
const MAX_IDENTIFIER_BYTES: usize = 256;
/// Session 用户可见标题允许的最大 UTF-8 字节数。
const MAX_TITLE_BYTES: usize = 512;
/// Steer 文本、Goal 目标和说明允许的最大 UTF-8 字节数。
const MAX_USER_TEXT_BYTES: usize = 64 * 1024;
/// OAuth 一次性字段允许的最大 UTF-8 字节数。
const MAX_OAUTH_FIELD_BYTES: usize = 8 * 1024;
/// OAuth 请求项目路径允许的最大 UTF-8 字节数。
const MAX_OAUTH_PROJECT_PATH_BYTES: usize = 4 * 1024;
/// JSON-RPC 字符串请求标识允许的最大 UTF-8 字节数。
const MAX_REQUEST_ID_BYTES: usize = 256;
/// 单页 Session 重放允许返回的最大权威事件数。
pub const MAX_REPLAY_EVENTS: u32 = 1_000;
/// 单次回退候选响应允许返回的最大消息锚点数量。
const MAX_REWIND_CANDIDATES: usize = 1_000;
/// 单次 MCP 状态响应允许返回的最大 Server 数量。
const MAX_MCP_SERVERS: usize = 512;
/// 单条 MCP OAuth JSON-RPC 通知允许的最大 JSON 字节数。
const MAX_MCP_OAUTH_NOTIFICATION_BYTES: usize = 64 * 1024;
/// MCP OAuth JSON-RPC 通知允许的最大 JSON 嵌套层数。
const MAX_MCP_OAUTH_NOTIFICATION_DEPTH: usize = 8;
/// MCP OAuth JSON-RPC 通知允许的最大 JSON 节点数。
const MAX_MCP_OAUTH_NOTIFICATION_NODES: usize = 32;
/// MCP OAuth JSON-RPC 通知的固定方法名。
const MCP_OAUTH_NOTIFICATION_METHOD: &str = "keencode/mcp/oauth";

/// ACP 请求参数和方法名的资源边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcpRequestLimits {
    /// 单个方法名允许的最大 ASCII 字节数。
    max_method_bytes: usize,
    /// 单个完整 JSON-RPC 信封或内部 params 允许的最大 JSON 字节数。
    max_payload_bytes: usize,
    /// 完整信封允许的最大 JSON 对象或数组嵌套层数。
    max_json_depth: usize,
    /// 完整信封允许的最大 JSON 值节点总数。
    max_json_nodes: usize,
}

impl AcpRequestLimits {
    /// 创建全部大于零的 ACP 请求边界。
    pub const fn new(
        max_method_bytes: usize,
        max_payload_bytes: usize,
        max_json_depth: usize,
        max_json_nodes: usize,
    ) -> Result<Self, AcpBoundaryError> {
        if max_method_bytes == 0
            || max_payload_bytes == 0
            || max_json_depth == 0
            || max_json_nodes == 0
        {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self {
            max_method_bytes,
            max_payload_bytes,
            max_json_depth,
            max_json_nodes,
        })
    }

    /// 返回方法名最大 ASCII 字节数。
    pub const fn max_method_bytes(&self) -> usize {
        self.max_method_bytes
    }

    /// 返回完整 JSON-RPC 信封的最大 JSON 字节数。
    pub const fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }

    /// 返回完整 JSON-RPC 信封的最大容器嵌套层数。
    pub const fn max_json_depth(&self) -> usize {
        self.max_json_depth
    }

    /// 返回完整 JSON-RPC 信封的最大 JSON 值节点数。
    pub const fn max_json_nodes(&self) -> usize {
        self.max_json_nodes
    }
}

impl Default for AcpRequestLimits {
    /// 返回适合进程内桌面传输的保守边界。
    fn default() -> Self {
        Self {
            max_method_bytes: 128,
            max_payload_bytes: 1024 * 1024,
            max_json_depth: 64,
            max_json_nodes: 65_536,
        }
    }
}

/// ACP JSON-RPC 响应序列化允许使用的资源边界。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcpResponseLimits {
    /// 单个完整响应信封允许的最大 JSON 字节数。
    max_payload_bytes: usize,
    /// 完整响应信封允许的最大 JSON 对象或数组嵌套层数。
    max_json_depth: usize,
    /// 完整响应信封允许的最大 JSON 值节点总数。
    max_json_nodes: usize,
}

impl AcpResponseLimits {
    /// 创建全部大于零的 ACP 响应边界。
    pub const fn new(
        max_payload_bytes: usize,
        max_json_depth: usize,
        max_json_nodes: usize,
    ) -> Result<Self, AcpBoundaryError> {
        if max_payload_bytes == 0 || max_json_depth == 0 || max_json_nodes == 0 {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self {
            max_payload_bytes,
            max_json_depth,
            max_json_nodes,
        })
    }

    /// 返回完整 JSON-RPC 响应信封的最大 JSON 字节数。
    pub const fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }

    /// 返回完整 JSON-RPC 响应信封的最大容器嵌套层数。
    pub const fn max_json_depth(&self) -> usize {
        self.max_json_depth
    }

    /// 返回完整 JSON-RPC 响应信封的最大 JSON 值节点数。
    pub const fn max_json_nodes(&self) -> usize {
        self.max_json_nodes
    }
}

impl Default for AcpResponseLimits {
    /// 返回适合进程内桌面传输的保守响应边界。
    fn default() -> Self {
        Self {
            max_payload_bytes: 1024 * 1024,
            max_json_depth: 64,
            max_json_nodes: 65_536,
        }
    }
}

/// 标准 `session/delete` 请求在 Rust 1.85 兼容 Schema 版本中的精确补充类型。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSessionRequest {
    /// 要从持久 Session 列表中删除的标准 ACP Session 标识。
    pub session_id: SessionId,
    /// ACP 为调用双方保留且不由 KeenCode 解释的扩展元数据。
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl DeleteSessionRequest {
    /// 使用明确 Session 标识创建不带扩展元数据的删除请求。
    pub fn new(session_id: impl Into<SessionId>) -> Self {
        Self {
            session_id: session_id.into(),
            meta: None,
        }
    }

    /// 附加由 ACP 保留且 KeenCode 不解释的扩展元数据。
    #[must_use]
    pub fn meta(mut self, meta: impl Into<Option<Meta>>) -> Self {
        self.meta = meta.into();
        self
    }
}

/// 标准 `session/delete` 成功响应在 Rust 1.85 兼容 Schema 版本中的精确补充类型。
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteSessionResponse {
    /// ACP 为调用双方保留且不由 KeenCode 解释的扩展元数据。
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}

impl DeleteSessionResponse {
    /// 创建不带扩展元数据的删除成功响应。
    pub fn new() -> Self {
        Self::default()
    }

    /// 附加由 ACP 保留且 KeenCode 不解释的扩展元数据。
    #[must_use]
    pub fn meta(mut self, meta: impl Into<Option<Meta>>) -> Self {
        self.meta = meta.into();
        self
    }
}

/// 已加载 Session 中一个仍在运行的后台 Shell 或单层子 Agent。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackgroundTaskInfo {
    /// 任务所属的根 Session。
    pub session_id: String,
    /// 精确取消时使用的稳定任务标识。
    pub task_id: String,
    /// 任务类别决定进程与子 Agent 字段的含义。
    pub kind: BackgroundTaskKind,
    /// 子 Agent 标识；Shell 固定为 null。
    pub child_thread_id: Option<String>,
    /// Runtime 已生成的单行任务摘要。
    pub summary: String,
    /// 任务开始时间，使用 UTC RFC 3339 文本。
    pub started_at: String,
    /// 已运行毫秒数，不代表任务已经结束。
    pub duration_ms: u64,
    /// Shell 根进程标识；Agent 固定为 null。
    pub pid: Option<u32>,
}

/// `keencode/background/list` 的显式 Session 作用域响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListBackgroundTasksResponse {
    /// 查询的唯一根 Session。
    pub session_id: String,
    /// 该 Session 当前仍在运行的有界任务列表。
    pub tasks: Vec<BackgroundTaskInfo>,
}

impl ListBackgroundTasksResponse {
    /// 校验作用域、任务唯一性、类别字段及客户端安全数值边界。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        if self.tasks.len() > 1_024 {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        let mut seen = HashSet::with_capacity(self.tasks.len());
        for task in &self.tasks {
            validate_identifier(&task.task_id, MAX_IDENTIFIER_BYTES)?;
            validate_text(&task.summary, MAX_USER_TEXT_BYTES)?;
            validate_text(&task.started_at, 64)?;
            if task.session_id != self.session_id
                || !seen.insert(&task.task_id)
                || task.duration_ms > 9_007_199_254_740_991
                || task.pid == Some(0)
            {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
            match (&task.kind, &task.child_thread_id, task.pid) {
                (BackgroundTaskKind::Agent, Some(child), None) => {
                    validate_identifier(child, MAX_IDENTIFIER_BYTES)?;
                }
                (BackgroundTaskKind::Shell, None, _) => {}
                _ => return Err(AcpBoundaryError::InvalidSemanticValue),
            }
        }
        Ok(())
    }
}

/// `keencode/session/steer` 成功接管用户引导后的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SteerSessionResponse {
    /// 接收引导文本的 Session 标识。
    pub session_id: String,
    /// Runtime 是否已把引导放入当前 Turn 的安全边界队列。
    pub accepted: bool,
}

impl SteerSessionResponse {
    /// 创建一个已经接管引导的响应。
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            accepted: true,
        }
    }

    /// 校验 Session 标识且只允许成功响应声明已接管。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        if !self.accepted {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// `keencode/session/rename` 完成后的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameSessionResponse {
    /// 已修改标题的 Session 标识。
    pub session_id: String,
    /// Runtime 实际持久化的完整标题。
    pub title: String,
    /// 标题变化写入权威 Journal 后的序号。
    pub journal_sequence: u64,
}

impl RenameSessionResponse {
    /// 校验响应中的 Session、标题和权威 Journal 水位。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(&self.title, MAX_TITLE_BYTES)?;
        if self.journal_sequence == 0 {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// `keencode/session/title` 返回的已校验标题候选；不会隐式修改 Session 标题。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerateSessionTitleResponse {
    /// Runtime 实际生成或从同一操作持久收据恢复的标题。
    pub title: String,
}

impl GenerateSessionTitleResponse {
    /// 拒绝空白、多行或超出标题字节预算的结果。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_text(&self.title, MAX_TITLE_BYTES)?;
        if self.title.trim().is_empty() || self.title.chars().any(char::is_control) {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// 一个可以作为 `keencode/session/rewind` 目标的用户消息锚点。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewindCandidate {
    /// 用户消息的稳定标识。
    pub message_id: String,
    /// 用于确认回退位置的有界纯文本摘要。
    pub preview: String,
    /// 消息形成权威记录时的 UTC Unix 毫秒时间。
    pub created_at_ms: u64,
}

impl RewindCandidate {
    /// 校验回退锚点标识、摘要和时间。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.message_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(&self.preview, MAX_USER_TEXT_BYTES)?;
        if self.created_at_ms == 0 {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// `keencode/session/rewind_candidates` 的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewindCandidatesResponse {
    /// 候选消息所属的 Session 标识。
    pub session_id: String,
    /// 按 Transcript 时间顺序返回的用户消息锚点。
    pub candidates: Vec<RewindCandidate>,
}

impl RewindCandidatesResponse {
    /// 校验 Session、候选数量、候选内容和消息标识唯一性。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        if self.candidates.len() > MAX_REWIND_CANDIDATES {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        let mut identifiers = HashSet::with_capacity(self.candidates.len());
        for candidate in &self.candidates {
            candidate.validate()?;
            if !identifiers.insert(candidate.message_id.as_str()) {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
        }
        Ok(())
    }
}

/// `keencode/session/rewind` 完成后的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewindSessionResponse {
    /// 已回退的 Session 标识。
    pub session_id: String,
    /// 回退前完整历史的可恢复归档 Session 标识。
    pub archived_session_id: String,
    /// 回退完成后权威 Journal 的最后序号。
    pub through_journal_sequence: u64,
    /// 首版固定为 `false`，明确表示没有自动恢复项目文件。
    pub reverted_files: bool,
}

impl RewindSessionResponse {
    /// 校验 Session 水位并拒绝首版未实现的文件自动恢复声明。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.archived_session_id, MAX_IDENTIFIER_BYTES)?;
        if self.through_journal_sequence == 0
            || self.reverted_files
            || self.archived_session_id == self.session_id
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// `keencode/session/replay` 的类型化分页接管响应。
///
/// 实际历史继续通过标准 `SessionUpdateDeliveryEnvelope` 和类型化
/// `KeenCodeEventEnvelope` 投递；本响应只描述权威 Journal 游标，不内联事件子集。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaySessionResponse {
    /// 被重放的 Session 标识。
    pub session_id: String,
    /// 本页开始之前已由 Client 确认的 Journal 游标；零表示从日志起点开始。
    pub start_after: u64,
    /// 本页投递完成后 Client 可以确认的 Journal 游标。
    pub next_after: u64,
    /// 本次读取观察到的权威 Journal 末尾序号。
    pub through_journal_sequence: u64,
    /// 本页历史投影完成后 Client 可以确认的实际桌面投递序号。
    ///
    /// 该水位只包含本页及此前历史页已经成功投递的事件，不包含末页
    /// 释放的恢复期间实时事件；没有任何历史投影时可以为零。
    pub through_delivery_sequence: u64,
    /// 本页通过两种类型化投递信封实际发送的事件数量。
    pub replayed_events: u32,
    /// 当前页之后是否仍有不晚于读取水位的事件。
    pub has_more: bool,
}

impl ReplaySessionResponse {
    /// 校验 Session、事件数量和权威 Journal、桌面投递游标的一致性。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        if self.replayed_events > MAX_REPLAY_EVENTS
            || self.start_after > self.next_after
            || self.next_after > self.through_journal_sequence
            || u64::from(self.replayed_events) > self.through_delivery_sequence
            || (self.start_after == self.next_after && (self.replayed_events != 0 || self.has_more))
            || self.has_more != (self.next_after < self.through_journal_sequence)
        {
            return Err(AcpBoundaryError::InvalidSequence);
        }
        Ok(())
    }
}

/// `keencode/background/cancel` 的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelBackgroundTaskResponse {
    /// 后台任务所属的 Session 标识。
    pub session_id: String,
    /// 被请求取消的后台任务标识。
    pub task_id: String,
    /// 是否由本次请求首次发出取消信号。
    pub cancelled: bool,
}

impl CancelBackgroundTaskResponse {
    /// 校验 Session 与后台任务标识。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.task_id, MAX_IDENTIFIER_BYTES)
    }
}

/// 项目级 Goal 的固定作用域。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalScope {
    /// Goal 属于 Session 当前授权的项目。
    Project,
}

/// 项目级 Goal 的完整生命周期状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// Agent 仍应继续推进目标。
    Active,
    /// 目标已经完成且不可再次迁移。
    Completed,
    /// 目标被无法自行解决的外部条件阻塞。
    Blocked,
}

/// ACP wire 使用的完整项目 Goal 记录。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalRecord {
    /// 跨进程稳定且唯一的 Goal 标识。
    pub id: String,
    /// 用户可见的简短标题。
    pub title: String,
    /// 当前固定项目级作用域。
    pub scope: GoalScope,
    /// 当前生命周期状态。
    pub status: GoalStatus,
    /// 可选补充说明。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 可选人工进度百分比。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress_percent: Option<u8>,
    /// 可验证且完整的目标描述。
    pub objective: String,
    /// 可选 Token 预算。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    /// Provider 明确报告并累计的 Token 数。
    pub tokens_used: u64,
    /// 累计实际运行秒数。
    pub time_used_seconds: u64,
    /// 仅在阻塞状态存在的安全原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    /// 仅在完成状态存在的非空验收证据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_evidence: Option<String>,
    /// 创建时的 UTC Unix 毫秒时间。
    pub created_at_ms: u64,
    /// 最后变化时的 UTC Unix 毫秒时间。
    pub updated_at_ms: u64,
}

impl GoalRecord {
    /// 校验 Goal 字段长度、预算、进度、时间和终态原因不变量。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.id, MAX_IDENTIFIER_BYTES)?;
        validate_text(&self.title, MAX_TITLE_BYTES)?;
        validate_text(&self.objective, MAX_USER_TEXT_BYTES)?;
        if let Some(description) = &self.description {
            validate_text(description, MAX_USER_TEXT_BYTES)?;
        }
        if self.progress_percent.is_some_and(|progress| progress > 100)
            || self.token_budget == Some(0)
            || self.created_at_ms == 0
            || self.updated_at_ms < self.created_at_ms
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        match (
            self.status,
            self.blocked_reason.as_deref(),
            self.completion_evidence.as_deref(),
        ) {
            (GoalStatus::Active, None, None) => Ok(()),
            (GoalStatus::Blocked, Some(reason), None) => validate_text(reason, MAX_USER_TEXT_BYTES),
            (GoalStatus::Completed, None, Some(evidence)) => {
                validate_text(evidence, MAX_USER_TEXT_BYTES)
            }
            _ => Err(AcpBoundaryError::InvalidSemanticValue),
        }
    }
}

/// `keencode/goal/get` 的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalGetResponse {
    /// 提供项目作用域的 Session 标识。
    pub session_id: String,
    /// 每次实际 Goal 变化后单调递增的版本号。
    pub revision: u64,
    /// 当前唯一 Goal；尚未创建或已经清除时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalRecord>,
}

impl GoalGetResponse {
    /// 校验 Session 与可选 Goal 的完整字段。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        if self.goal.is_some() && self.revision == 0 {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        if let Some(goal) = &self.goal {
            goal.validate()?;
        }
        Ok(())
    }
}

/// Goal 创建、更新或终态迁移后的共享类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalMutationResponse {
    /// 提供项目作用域的 Session 标识。
    pub session_id: String,
    /// 成功操作后的 Goal 版本号。
    pub revision: u64,
    /// 操作完成后的完整 Goal。
    pub goal: GoalRecord,
    /// 本次结果是否来自相同请求 nonce 的幂等重放。
    pub deduplicated: bool,
}

impl GoalMutationResponse {
    /// 校验非零版本、Session 和完整 Goal。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        if self.revision == 0 {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        self.goal.validate()
    }
}

/// `keencode/goal/upsert` 的类型化响应。
pub type GoalUpsertResponse = GoalMutationResponse;

/// `keencode/goal/transition` 的类型化响应。
pub type GoalTransitionResponse = GoalMutationResponse;

/// `keencode/goal/clear` 的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalClearResponse {
    /// 提供项目作用域的 Session 标识。
    pub session_id: String,
    /// 成功清除后的 Goal 版本号。
    pub revision: u64,
    /// 被清除并进入墓碑集合的 Goal 标识。
    pub cleared_goal_id: String,
    /// 本次结果是否来自相同请求 nonce 的幂等重放。
    pub deduplicated: bool,
}

impl GoalClearResponse {
    /// 校验非零版本、Session 和被清除的 Goal 标识。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.cleared_goal_id, MAX_IDENTIFIER_BYTES)?;
        if self.revision == 0 {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// MCP Runtime 当前初始化阶段。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpRuntimePhase {
    /// 尚未启动任何 MCP 连接或子进程。
    Pending,
    /// 正在按需初始化已启用 Server。
    Initializing,
    /// 已完成当前配置的初始化尝试。
    Ready,
    /// Runtime 级初始化失败。
    Failed,
}

/// MCP Server 当前连接状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpConnectionStatus {
    /// 尚未按需初始化。
    Uninitialized,
    /// 正在建立连接并协商能力。
    Connecting,
    /// 已完成初始化并可调用。
    Connected,
    /// 曾经连接但当前已经断开。
    Disconnected,
    /// 最近一次连接或协议协商失败。
    Failed,
    /// 当前配置明确禁用。
    Disabled,
}

/// MCP Server 的 Provider 中立传输类型。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    /// 本地子进程标准输入输出传输。
    Stdio,
    /// MCP Streamable HTTP 传输。
    StreamableHttp,
}

/// MCP Server 当前 OAuth 阶段。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOAuthStatus {
    /// 当前 Server 不需要 OAuth。
    NotRequired,
    /// 支持 OAuth，但尚未发起授权。
    Idle,
    /// 已生成授权地址，正在等待浏览器回调。
    AwaitingAuthorization,
    /// 回调已校验，正在交换授权码。
    ExchangingCode,
    /// 已持有可用访问令牌。
    Authorized,
    /// 正在刷新访问令牌。
    Refreshing,
    /// 用户或授权服务拒绝了授权。
    Denied,
    /// 授权请求或令牌已经过期。
    Expired,
}

/// `keencode/mcp/list` 返回的单个 Server 类型化状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerStatus {
    /// MCP 配置中的稳定 Server 名称。
    pub name: String,
    /// 当前配置是否启用该 Server。
    pub enabled: bool,
    /// 当前使用的 MCP 传输类型。
    pub transport: McpTransportKind,
    /// 当前连接生命周期状态。
    pub connection_status: McpConnectionStatus,
    /// 已发现且可暴露给 Agent 的工具数量。
    pub tools_count: u32,
    /// 当前 OAuth 生命周期状态。
    pub oauth_status: McpOAuthStatus,
    /// 失败时可向用户展示的安全错误摘要。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl McpServerStatus {
    /// 校验名称、启用状态、连接状态、OAuth 状态和错误字段的一致性。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.name, MAX_IDENTIFIER_BYTES)?;
        if let Some(error) = &self.error {
            validate_text(error, MAX_USER_TEXT_BYTES)?;
        }
        if self.enabled == (self.connection_status == McpConnectionStatus::Disabled)
            || (self.connection_status == McpConnectionStatus::Failed) != self.error.is_some()
            || (self.connection_status != McpConnectionStatus::Connected && self.tools_count != 0)
            || (self.transport == McpTransportKind::Stdio
                && self.oauth_status != McpOAuthStatus::NotRequired)
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

/// `keencode/mcp/list` 的类型化连接池状态响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListResponse {
    /// MCP Runtime 当前初始化阶段。
    pub init_phase: McpRuntimePhase,
    /// 按 Server 名称稳定排序的完整运行态列表。
    pub servers: Vec<McpServerStatus>,
    /// Runtime 级初始化失败时的安全错误摘要。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl McpListResponse {
    /// 校验初始化阶段、Server 数量、排序、唯一性和全部运行态。
    pub fn validate(&self) -> Result<(), AcpBoundaryError> {
        if self.servers.len() > MAX_MCP_SERVERS
            || (self.init_phase == McpRuntimePhase::Failed) != self.error.is_some()
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        if let Some(error) = &self.error {
            validate_text(error, MAX_USER_TEXT_BYTES)?;
        }
        let mut previous_name: Option<&str> = None;
        for server in &self.servers {
            server.validate()?;
            if previous_name.is_some_and(|previous| previous >= server.name.as_str()) {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
            previous_name = Some(server.name.as_str());
        }
        Ok(())
    }
}

/// `keencode/mcp/oauth_start` 已接管异步授权流程的固定状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOAuthStartStatus {
    /// Runtime 正在生成授权请求，后续授权地址只通过类型化事件投递。
    Starting,
}

/// `keencode/mcp/oauth_start` 成功接管后的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpOAuthStartResponse {
    /// 固定为 `starting`，最终状态由 MCP OAuth 生命周期事件报告。
    pub status: McpOAuthStartStatus,
}

impl McpOAuthStartResponse {
    /// 创建不暴露授权地址、state、PKCE verifier 或令牌的接管响应。
    pub const fn new() -> Self {
        Self {
            status: McpOAuthStartStatus::Starting,
        }
    }
}

impl Default for McpOAuthStartResponse {
    /// 创建固定的异步授权接管响应。
    fn default() -> Self {
        Self::new()
    }
}

/// `keencode/mcp/oauth_callback` 已接管异步令牌交换的固定状态。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpOAuthCallbackStatus {
    /// Runtime 已验证回调形状并接管后续令牌交换。
    Accepted,
}

/// `keencode/mcp/oauth_callback` 成功接管后的类型化响应。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpOAuthCallbackResponse {
    /// 固定为 `accepted`，授权终态由类型化事件报告。
    pub status: McpOAuthCallbackStatus,
}

impl McpOAuthCallbackResponse {
    /// 创建不回显授权码、state 或令牌的回调接管响应。
    pub const fn new() -> Self {
        Self {
            status: McpOAuthCallbackStatus::Accepted,
        }
    }
}

impl Default for McpOAuthCallbackResponse {
    /// 创建固定的异步回调接管响应。
    fn default() -> Self {
        Self::new()
    }
}

/// `keencode/mcp/oauth_cancel` 的类型化响应。
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpOAuthCancelResponse {
    /// 是否存在待决流程且已由本次调用取消。
    pub cancelled: bool,
}

impl McpOAuthCancelResponse {
    /// 创建明确区分“已取消”和“没有待决流程”的响应。
    pub const fn new(cancelled: bool) -> Self {
        Self { cancelled }
    }
}

/// MCP OAuth 生命周期事件使用的独立 JSON-RPC 2.0 通知。
///
/// 该通知不携带 JSON-RPC ID，也不进入 Session 事件信封；`params` 直接承载
/// 严格类型化的 [`McpOAuthEvent`]。
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpOAuthNotification {
    /// 固定为 JSON-RPC 2.0 版本字符串。
    jsonrpc: String,
    /// 固定为 `keencode/mcp/oauth`，避免被伪装成其他通知方法。
    method: String,
    /// OAuth 生命周期事件参数。
    params: McpOAuthEvent,
}

impl fmt::Debug for McpOAuthNotification {
    /// 只显示固定方法和脱敏后的事件，不回显 OAuth 凭据内容。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthNotification")
            .field("jsonrpc", &self.jsonrpc)
            .field("method", &self.method)
            .field("params", &self.params)
            .finish()
    }
}

/// 只用于严格恢复 MCP OAuth JSON-RPC 通知的线格式。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpOAuthNotificationWire {
    /// JSON-RPC 版本字符串。
    jsonrpc: String,
    /// JSON-RPC 通知方法名。
    method: String,
    /// OAuth 生命周期事件参数。
    params: McpOAuthEvent,
}

impl<'de> Deserialize<'de> for McpOAuthNotification {
    /// 严格恢复固定版本、方法名和 OAuth 事件参数。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = McpOAuthNotificationWire::deserialize(deserializer)?;
        if wire.jsonrpc != "2.0" || wire.method != MCP_OAUTH_NOTIFICATION_METHOD {
            return Err(D::Error::custom("invalid MCP OAuth notification"));
        }
        Ok(Self {
            jsonrpc: wire.jsonrpc,
            method: wire.method,
            params: wire.params,
        })
    }
}

impl McpOAuthNotification {
    /// 使用一条已校验的 OAuth 事件创建固定 JSON-RPC 通知。
    pub fn new(event: McpOAuthEvent) -> Result<Self, AcpBoundaryError> {
        event.validate()?;
        Ok(Self {
            jsonrpc: "2.0".to_owned(),
            method: MCP_OAUTH_NOTIFICATION_METHOD.to_owned(),
            params: event,
        })
    }

    /// 返回固定 JSON-RPC 版本字符串。
    pub fn jsonrpc(&self) -> &str {
        &self.jsonrpc
    }

    /// 返回固定 MCP OAuth 通知方法名。
    pub fn method(&self) -> &str {
        &self.method
    }

    /// 返回 OAuth 事件参数。
    pub const fn params(&self) -> &McpOAuthEvent {
        &self.params
    }

    /// 返回 OAuth 事件参数；该别名便于调用方按事件语义读取通知。
    pub const fn event(&self) -> &McpOAuthEvent {
        &self.params
    }

    /// 将通知编码为严格、紧凑的 JSON-RPC 字节。
    pub fn encode(&self) -> Result<Vec<u8>, AcpBoundaryError> {
        if self.jsonrpc != "2.0" || self.method != MCP_OAUTH_NOTIFICATION_METHOD {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        self.params.validate()?;
        let value = serde_json::to_value(self).map_err(|_| AcpBoundaryError::InvalidParams)?;
        validate_value(
            &value,
            JsonValueLimits {
                max_bytes: MAX_MCP_OAUTH_NOTIFICATION_BYTES,
                max_depth: MAX_MCP_OAUTH_NOTIFICATION_DEPTH,
                max_nodes: MAX_MCP_OAUTH_NOTIFICATION_NODES,
            },
        )?;
        serde_json::to_vec(&value).map_err(|_| AcpBoundaryError::InvalidParams)
    }

    /// `encode` 的字节别名，供 JSON-RPC 传输适配器使用。
    pub fn to_vec(&self) -> Result<Vec<u8>, AcpBoundaryError> {
        self.encode()
    }

    /// 从未解析原始字节恢复严格 MCP OAuth JSON-RPC 通知。
    pub fn decode_raw(raw: &[u8]) -> Result<Self, AcpBoundaryError> {
        let value = parse_raw_value(
            raw,
            JsonValueLimits {
                max_bytes: MAX_MCP_OAUTH_NOTIFICATION_BYTES,
                max_depth: MAX_MCP_OAUTH_NOTIFICATION_DEPTH,
                max_nodes: MAX_MCP_OAUTH_NOTIFICATION_NODES,
            },
        )?;
        let wire = serde_json::from_value::<McpOAuthNotificationWire>(value.clone())
            .map_err(|_| AcpBoundaryError::InvalidParams)?;
        if wire.jsonrpc != "2.0" || wire.method != MCP_OAUTH_NOTIFICATION_METHOD {
            return Err(AcpBoundaryError::InvalidParams);
        }
        let notification = Self::new(wire.params)?;
        let normalized =
            serde_json::to_value(&notification).map_err(|_| AcpBoundaryError::InvalidParams)?;
        if !crate::json::input_preserved(&value, &normalized) {
            return Err(AcpBoundaryError::InvalidParams);
        }
        Ok(notification)
    }
}

/// 封闭响应载荷实现集合，禁止调用方把裸 JSON 值伪装成生产协议 DTO。
mod response_payload_sealed {
    /// 只有本 crate 明确登记的标准响应或 KeenCode 响应可以实现。
    pub trait Sealed {}
}

/// 可以穿过 ACP 成功响应边界的封闭类型化载荷。
///
/// 该接口不向外部 crate 开放实现。新增 ACP 方法时必须先在本模块定义或登记
/// 明确 DTO、语义校验和测试，不能直接传递 `serde_json::Value` 或 JSON 字符串。
pub trait AcpResponsePayload:
    Serialize + DeserializeOwned + response_payload_sealed::Sealed
{
    /// 在编码或恢复完整 JSON-RPC 信封前校验响应的跨字段语义。
    fn validate_response(&self) -> Result<(), AcpBoundaryError> {
        Ok(())
    }
}

macro_rules! impl_passthrough_response_payload {
    ($($response:ty),+ $(,)?) => {
        $(
            impl response_payload_sealed::Sealed for $response {}
            impl AcpResponsePayload for $response {}
        )+
    };
}

macro_rules! impl_validated_response_payload {
    ($($response:ty),+ $(,)?) => {
        $(
            impl response_payload_sealed::Sealed for $response {}
            impl AcpResponsePayload for $response {
                fn validate_response(&self) -> Result<(), AcpBoundaryError> {
                    self.validate()
                }
            }
        )+
    };
}

impl response_payload_sealed::Sealed for InitializeResponseDto {}

impl AcpResponsePayload for InitializeResponseDto {
    /// 校验初始化响应中的认证方式标识和唯一性。
    fn validate_response(&self) -> Result<(), AcpBoundaryError> {
        self.validate()
    }
}

impl_passthrough_response_payload!(
    AuthenticateResponse,
    LoadSessionResponse,
    PromptResponse,
    CreateElicitationResponse,
    DeleteSessionResponse,
    SetSessionConfigOptionResponse,
    SetSessionModeResponse,
    McpOAuthStartResponse,
    McpOAuthCallbackResponse,
    McpOAuthCancelResponse,
);

impl response_payload_sealed::Sealed for NewSessionResponse {}

impl AcpResponsePayload for NewSessionResponse {
    /// 校验新建 Session 响应中的稳定标识。
    fn validate_response(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id.0, MAX_IDENTIFIER_BYTES)
    }
}

impl response_payload_sealed::Sealed for ForkSessionResponse {}

impl AcpResponsePayload for ForkSessionResponse {
    /// 校验派生 Session 响应中的稳定标识。
    fn validate_response(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id.0, MAX_IDENTIFIER_BYTES)
    }
}

impl response_payload_sealed::Sealed for ListSessionsResponse {}

impl AcpResponsePayload for ListSessionsResponse {
    /// 校验 Session 列表中的标识、标题和分页游标。
    fn validate_response(&self) -> Result<(), AcpBoundaryError> {
        let mut identifiers = HashSet::with_capacity(self.sessions.len());
        for session in &self.sessions {
            validate_identifier(&session.session_id.0, MAX_IDENTIFIER_BYTES)?;
            if !identifiers.insert(session.session_id.0.as_ref()) {
                return Err(AcpBoundaryError::InvalidSemanticValue);
            }
            if let Some(title) = &session.title {
                validate_text(title, MAX_TITLE_BYTES)?;
            }
        }
        if let Some(cursor) = &self.next_cursor {
            validate_identifier(cursor, MAX_IDENTIFIER_BYTES)?;
        }
        Ok(())
    }
}

impl_validated_response_payload!(
    SteerSessionResponse,
    RenameSessionResponse,
    GenerateSessionTitleResponse,
    RewindCandidatesResponse,
    RewindSessionResponse,
    ReplaySessionResponse,
    CancelBackgroundTaskResponse,
    ListBackgroundTasksResponse,
    GoalGetResponse,
    GoalMutationResponse,
    GoalClearResponse,
    McpListResponse,
    ReadFileChangeResponse,
);

/// 一个已经严格解码的标准 ACP 或当前 KeenCode Session 扩展请求。
pub enum AcpRequest {
    /// 标准 `initialize` 请求。
    Initialize(InitializeRequest),
    /// 标准 `authenticate` 请求。
    Authenticate(AuthenticateRequest),
    /// 标准 `session/new` 请求。
    NewSession(NewSessionRequest),
    /// 标准 `session/load` 请求。
    LoadSession(LoadSessionRequest),
    /// 标准 `session/prompt` 请求。
    Prompt(PromptRequest),
    /// 标准 `session/delete` 请求。
    DeleteSession(DeleteSessionRequest),
    /// 标准 `session/set_config_option` 请求。
    SetSessionConfigOption(SetSessionConfigOptionRequest),
    /// 标准 `session/set_mode` 请求。
    SetSessionMode(SetSessionModeRequest),
    /// 标准 `session/list` 请求。
    ListSessions(ListSessionsRequest),
    /// 标准 `session/fork` 请求。
    ForkSession(ForkSessionRequest),
    /// 向正在运行的 Turn 安全注入下一条用户引导。
    SteerSession(SteerSessionRequest),
    /// 修改 Session 用户可见标题。
    RenameSession(RenameSessionRequest),
    /// 使用当前 Session 的 Provider 生成短标题候选。
    GenerateSessionTitle(GenerateSessionTitleRequest),
    /// 查询可以回退的用户消息锚点。
    RewindCandidates(RewindCandidatesRequest),
    /// 将 Session Transcript 回退到指定消息锚点。
    RewindSession(RewindSessionRequest),
    /// 从权威事件日志分页重放 Session。
    ReplaySession(ReplaySessionRequest),
    /// 取消 Session 内一个明确后台任务。
    CancelBackgroundTask(CancelBackgroundTaskRequest),
    /// 列出一个已授权 Session 的运行中后台任务。
    ListBackgroundTasks(ListBackgroundTasksRequest),
    /// 查询项目当前唯一 Goal。
    GoalGet(GoalGetRequest),
    /// 创建或更新项目当前唯一 Goal。
    GoalUpsert(GoalUpsertRequest),
    /// 把项目 Goal 迁移到不可逆终态。
    GoalTransition(GoalTransitionRequest),
    /// 清除已经进入终态的项目 Goal。
    GoalClear(GoalClearRequest),
    /// 查询当前项目或全局 MCP Server 连接状态。
    McpList(McpListRequest),
    /// 开始一个 MCP OAuth 授权流程。
    McpOAuthStart(McpOAuthServerRequest),
    /// 提交一个 MCP OAuth 回调。
    McpOAuthCallback(McpOAuthCallbackRequest),
    /// 取消一个 MCP OAuth 授权流程。
    McpOAuthCancel(McpOAuthServerRequest),
    /// 从权威文件变更快照按原始字节分页读取。
    ReadFileChange(ReadFileChangeRequest),
}

impl AcpRequest {
    /// 返回该请求在当前协议中的唯一精确方法名。
    pub const fn method(&self) -> &'static str {
        match self {
            Self::Initialize(_) => "initialize",
            Self::Authenticate(_) => "authenticate",
            Self::NewSession(_) => "session/new",
            Self::LoadSession(_) => "session/load",
            Self::Prompt(_) => "session/prompt",
            Self::DeleteSession(_) => "session/delete",
            Self::SetSessionConfigOption(_) => "session/set_config_option",
            Self::SetSessionMode(_) => "session/set_mode",
            Self::ListSessions(_) => "session/list",
            Self::ForkSession(_) => "session/fork",
            Self::SteerSession(_) => "keencode/session/steer",
            Self::RenameSession(_) => "keencode/session/rename",
            Self::GenerateSessionTitle(_) => "keencode/session/title",
            Self::RewindCandidates(_) => "keencode/session/rewind_candidates",
            Self::RewindSession(_) => "keencode/session/rewind",
            Self::ReplaySession(_) => "keencode/session/replay",
            Self::CancelBackgroundTask(_) => "keencode/background/cancel",
            Self::ListBackgroundTasks(_) => "keencode/background/list",
            Self::GoalGet(_) => "keencode/goal/get",
            Self::GoalUpsert(_) => "keencode/goal/upsert",
            Self::GoalTransition(_) => "keencode/goal/transition",
            Self::GoalClear(_) => "keencode/goal/clear",
            Self::McpList(_) => "keencode/mcp/list",
            Self::McpOAuthStart(_) => "keencode/mcp/oauth_start",
            Self::McpOAuthCallback(_) => "keencode/mcp/oauth_callback",
            Self::McpOAuthCancel(_) => "keencode/mcp/oauth_cancel",
            Self::ReadFileChange(_) => "keencode/session/file-change/read",
        }
    }
}

impl fmt::Debug for AcpRequest {
    /// 只输出方法名，避免把 Prompt、Goal、OAuth code 或其他用户内容写入日志。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpRequest")
            .field("method", &self.method())
            .finish()
    }
}

/// 一个已经严格解码的标准 ACP 或当前 Session 扩展通知。
pub enum AcpNotification {
    /// 标准 `session/cancel` 通知。
    Cancel(CancelNotification),
    /// Provider、Skill、MCP 或项目配置发生当前格式变化。
    SessionConfigUpdate(SessionConfigUpdateNotification),
}

impl AcpNotification {
    /// 返回该通知在当前协议中的唯一精确方法名。
    pub const fn method(&self) -> &'static str {
        match self {
            Self::Cancel(_) => "session/cancel",
            Self::SessionConfigUpdate(_) => "keencode/config/update",
        }
    }
}

impl fmt::Debug for AcpNotification {
    /// 只输出方法名，避免把配置指纹或 Session 元数据写入日志。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpNotification")
            .field("method", &self.method())
            .finish()
    }
}

/// 一个已验证 JSON-RPC 2.0 信封和请求标识的 ACP 请求。
pub struct AcpRequestFrame {
    /// Client 提供且 Agent 必须原样回传的 JSON-RPC 请求标识。
    id: RequestId,
    /// 已经通过方法、形状和资源校验的 ACP 请求。
    request: AcpRequest,
}

impl AcpRequestFrame {
    /// 返回 Client 提供的 JSON-RPC 请求标识。
    pub const fn id(&self) -> &RequestId {
        &self.id
    }

    /// 返回已严格解码的 ACP 请求。
    pub const fn request(&self) -> &AcpRequest {
        &self.request
    }

    /// 拆分为必须原样回传的 JSON-RPC ID 和类型化请求。
    pub fn into_parts(self) -> (RequestId, AcpRequest) {
        (self.id, self.request)
    }
}

impl fmt::Debug for AcpRequestFrame {
    /// 只输出请求标识和方法，不输出 Prompt、Goal 或 OAuth 内容。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpRequestFrame")
            .field("id", &self.id)
            .field("method", &self.request.method())
            .finish()
    }
}

/// 从完整 JSON-RPC 2.0 字节入口安全解码的请求或通知。
#[derive(Debug)]
pub enum AcpIncomingFrame {
    /// 带有 JSON-RPC ID 且必须产生响应的请求；使用堆间接层限制信封枚举体积。
    Request(Box<AcpRequestFrame>),
    /// 不带 JSON-RPC ID 且不产生响应的通知。
    Notification(AcpNotification),
}

/// 把进程内 JSON 值严格转换为标准 ACP 和当前唯一扩展结构。
#[derive(Clone, Copy, Debug)]
pub struct AcpRequestDecoder {
    limits: AcpRequestLimits,
}

impl AcpRequestDecoder {
    /// 使用默认资源边界创建请求解码器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用显式资源边界创建请求解码器，并再次验证跨 FFI 或恢复边界的配置。
    pub const fn with_limits(limits: AcpRequestLimits) -> Result<Self, AcpBoundaryError> {
        if limits.max_method_bytes == 0
            || limits.max_payload_bytes == 0
            || limits.max_json_depth == 0
            || limits.max_json_nodes == 0
        {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self { limits })
    }

    /// 返回当前请求资源边界。
    pub const fn limits(&self) -> AcpRequestLimits {
        self.limits
    }

    /// 从完整 JSON-RPC 2.0 原始字节解码请求或通知。
    ///
    /// 字节上限在任何 Serde DOM 分配前执行；随后严格校验版本、ID、方法、
    /// params、重复键和未知信封字段。
    pub fn decode_raw(&self, raw: &[u8]) -> Result<AcpIncomingFrame, AcpBoundaryError> {
        let value = parse_raw_value(raw, self.json_limits())?;
        let Value::Object(mut frame) = value else {
            return Err(AcpBoundaryError::InvalidParams);
        };
        let is_request = frame.contains_key("id");
        let allowed = if is_request {
            ["jsonrpc", "id", "method", "params"].as_slice()
        } else {
            ["jsonrpc", "method", "params"].as_slice()
        };
        if frame.len() != allowed.len() || frame.keys().any(|key| !allowed.contains(&key.as_str()))
        {
            return Err(AcpBoundaryError::InvalidParams);
        }
        if frame.remove("jsonrpc") != Some(Value::String("2.0".to_owned())) {
            return Err(AcpBoundaryError::InvalidParams);
        }
        let method = frame
            .remove("method")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or(AcpBoundaryError::InvalidParams)?;
        let params = frame
            .remove("params")
            .ok_or(AcpBoundaryError::InvalidParams)?;

        if is_request {
            let id = serde_json::from_value::<RequestId>(
                frame.remove("id").ok_or(AcpBoundaryError::InvalidParams)?,
            )
            .map_err(|_| AcpBoundaryError::InvalidParams)?;
            self.validate_request_id(&id)?;
            let request = self.decode_request(&method, params)?;
            Ok(AcpIncomingFrame::Request(Box::new(AcpRequestFrame {
                id,
                request,
            })))
        } else {
            let notification = self.decode_notification(&method, params)?;
            Ok(AcpIncomingFrame::Notification(notification))
        }
    }

    /// 解码一个带 JSON-RPC ID 的请求，并拒绝未知字段、别名与旧拼写。
    pub(crate) fn decode_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<AcpRequest, AcpBoundaryError> {
        self.validate_method(method)?;
        self.validate_payload(&params)?;
        match method {
            "initialize" => self.decode(params).map(AcpRequest::Initialize),
            "authenticate" => {
                let request = self.decode::<AuthenticateRequest>(params)?;
                validate_identifier(&request.method_id.0, MAX_IDENTIFIER_BYTES)?;
                Ok(AcpRequest::Authenticate(request))
            }
            "session/new" => self.decode(params).map(AcpRequest::NewSession),
            "session/load" => self.decode(params).map(AcpRequest::LoadSession),
            "session/prompt" => self.decode(params).map(AcpRequest::Prompt),
            "session/delete" => self.decode_extension(params).map(AcpRequest::DeleteSession),
            "session/set_config_option" => {
                self.decode(params).map(AcpRequest::SetSessionConfigOption)
            }
            "session/set_mode" => {
                let request = self.decode::<SetSessionModeRequest>(params)?;
                validate_identifier(&request.session_id.0, MAX_IDENTIFIER_BYTES)?;
                validate_identifier(&request.mode_id.0, MAX_IDENTIFIER_BYTES)?;
                Ok(AcpRequest::SetSessionMode(request))
            }
            "session/list" => self.decode(params).map(AcpRequest::ListSessions),
            "session/fork" => self.decode(params).map(AcpRequest::ForkSession),
            "keencode/session/steer" => self.decode_extension(params).map(AcpRequest::SteerSession),
            "keencode/session/rename" => {
                self.decode_extension(params).map(AcpRequest::RenameSession)
            }
            "keencode/session/title" => self
                .decode_extension(params)
                .map(AcpRequest::GenerateSessionTitle),
            "keencode/session/rewind_candidates" => self
                .decode_extension(params)
                .map(AcpRequest::RewindCandidates),
            "keencode/session/rewind" => {
                self.decode_extension(params).map(AcpRequest::RewindSession)
            }
            "keencode/session/replay" => {
                self.decode_extension(params).map(AcpRequest::ReplaySession)
            }
            "keencode/background/cancel" => self
                .decode_extension(params)
                .map(AcpRequest::CancelBackgroundTask),
            "keencode/background/list" => self
                .decode_extension(params)
                .map(AcpRequest::ListBackgroundTasks),
            "keencode/goal/get" => self.decode_extension(params).map(AcpRequest::GoalGet),
            "keencode/goal/upsert" => self.decode_extension(params).map(AcpRequest::GoalUpsert),
            "keencode/goal/transition" => self
                .decode_extension(params)
                .map(AcpRequest::GoalTransition),
            "keencode/goal/clear" => self.decode_extension(params).map(AcpRequest::GoalClear),
            "keencode/mcp/list" => self.decode_extension(params).map(AcpRequest::McpList),
            "keencode/mcp/oauth_start" => {
                self.decode_extension(params).map(AcpRequest::McpOAuthStart)
            }
            "keencode/mcp/oauth_callback" => self
                .decode_extension(params)
                .map(AcpRequest::McpOAuthCallback),
            "keencode/mcp/oauth_cancel" => self
                .decode_extension(params)
                .map(AcpRequest::McpOAuthCancel),
            "keencode/session/file-change/read" => self
                .decode_extension(params)
                .map(AcpRequest::ReadFileChange),
            _ => Err(AcpBoundaryError::UnknownMethod),
        }
    }

    /// 解码一个无 JSON-RPC ID 的通知，并拒绝把请求方法伪装成通知。
    pub(crate) fn decode_notification(
        &self,
        method: &str,
        params: Value,
    ) -> Result<AcpNotification, AcpBoundaryError> {
        self.validate_method(method)?;
        self.validate_payload(&params)?;
        match method {
            "session/cancel" => self.decode(params).map(AcpNotification::Cancel),
            "keencode/config/update" => self
                .decode_extension(params)
                .map(AcpNotification::SessionConfigUpdate),
            _ => Err(AcpBoundaryError::UnknownMethod),
        }
    }

    /// 使用反序列化后再序列化的当前结构，拒绝被 Serde 忽略或默认化的输入字段。
    fn decode<T>(&self, params: Value) -> Result<T, AcpBoundaryError>
    where
        T: DeserializeOwned + Serialize,
    {
        let decoded = serde_json::from_value::<T>(params.clone())
            .map_err(|_| AcpBoundaryError::InvalidParams)?;
        let normalized =
            serde_json::to_value(&decoded).map_err(|_| AcpBoundaryError::InvalidParams)?;
        if !input_preserved(&params, &normalized) {
            return Err(AcpBoundaryError::InvalidParams);
        }
        Ok(decoded)
    }

    /// 严格解码 KeenCode 补充 DTO，并在返回前执行字段和跨字段语义校验。
    fn decode_extension<T>(&self, params: Value) -> Result<T, AcpBoundaryError>
    where
        T: DeserializeOwned + Serialize + ValidateAcpParams,
    {
        let decoded = self.decode::<T>(params)?;
        decoded.validate()?;
        Ok(decoded)
    }

    /// 校验标准及命名空间扩展方法需要的可见 ASCII；具体方法仍由封闭路由表验证。
    fn validate_method(&self, method: &str) -> Result<(), AcpBoundaryError> {
        if method.is_empty()
            || method.len() > self.limits.max_method_bytes
            || method.starts_with('/')
            || method.ends_with('/')
            || method.contains("//")
            || !method.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'_' | b'/' | b'-')
            })
        {
            return Err(AcpBoundaryError::InvalidMethod);
        }
        Ok(())
    }

    /// 校验 JSON-RPC ID 只使用规范允许的有界字符串、整数或 null。
    fn validate_request_id(&self, id: &RequestId) -> Result<(), AcpBoundaryError> {
        validate_json_rpc_id(id)
    }

    /// 在类型化反序列化前限制 params 的累计字节、节点和容器嵌套深度。
    fn validate_payload(&self, params: &Value) -> Result<(), AcpBoundaryError> {
        validate_value(params, self.json_limits())
    }

    /// 把公开解码器资源配置转换为共享 JSON 校验边界。
    const fn json_limits(&self) -> JsonValueLimits {
        JsonValueLimits {
            max_bytes: self.limits.max_payload_bytes,
            max_depth: self.limits.max_json_depth,
            max_nodes: self.limits.max_json_nodes,
        }
    }
}

impl Default for AcpRequestDecoder {
    /// 使用默认资源边界创建请求解码器。
    fn default() -> Self {
        Self {
            limits: AcpRequestLimits::default(),
        }
    }
}

/// 已通过严格恢复校验的 ACP 成功响应。
pub struct AcpResultFrame<T> {
    /// 必须与原请求完全一致的 JSON-RPC 标识。
    id: RequestId,
    /// 当前方法唯一允许的类型化响应载荷。
    result: T,
}

impl<T> AcpResultFrame<T> {
    /// 返回响应携带的原始 JSON-RPC 请求标识。
    pub const fn id(&self) -> &RequestId {
        &self.id
    }

    /// 返回经过严格形状和语义校验的类型化响应。
    pub const fn result(&self) -> &T {
        &self.result
    }

    /// 拆分为原始请求标识和类型化响应载荷。
    pub fn into_parts(self) -> (RequestId, T) {
        (self.id, self.result)
    }
}

/// 只用于严格恢复成功响应的内部 JSON-RPC 2.0 信封。
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictAcpResultEnvelope<T> {
    /// 必须精确等于 `2.0` 的 JSON-RPC 版本。
    jsonrpc: String,
    /// 必须与原请求完全一致的 JSON-RPC 标识。
    id: RequestId,
    /// 由调用点明确选择的类型化结果。
    result: T,
}

/// 从原始 JSON-RPC 字节恢复类型化 ACP 成功响应的严格解码器。
#[derive(Clone, Copy, Debug)]
pub struct AcpResponseDecoder {
    /// 当前响应恢复使用的资源边界。
    limits: AcpResponseLimits,
}

impl AcpResponseDecoder {
    /// 使用默认资源边界创建响应解码器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用显式资源边界创建响应解码器。
    pub const fn with_limits(limits: AcpResponseLimits) -> Result<Self, AcpBoundaryError> {
        if limits.max_payload_bytes == 0 || limits.max_json_depth == 0 || limits.max_json_nodes == 0
        {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self { limits })
    }

    /// 返回当前响应恢复资源边界。
    pub const fn limits(&self) -> AcpResponseLimits {
        self.limits
    }

    /// 严格恢复一个完整 JSON-RPC 2.0 成功响应。
    ///
    /// 调用方必须在编译期选择当前方法对应的封闭响应 DTO。该入口拒绝错误
    /// 信封、未知字段、重复键、旧字段别名、资源越界和 DTO 语义错误。
    pub fn decode_result<T>(&self, raw: &[u8]) -> Result<AcpResultFrame<T>, AcpBoundaryError>
    where
        T: AcpResponsePayload,
    {
        let value = parse_raw_value(raw, self.json_limits()).map_err(map_response_shape_error)?;
        let envelope = serde_json::from_value::<StrictAcpResultEnvelope<T>>(value.clone())
            .map_err(|_| AcpBoundaryError::InvalidResponse)?;
        let normalized =
            serde_json::to_value(&envelope).map_err(|_| AcpBoundaryError::InvalidResponse)?;
        if !input_preserved(&value, &normalized) || envelope.jsonrpc != "2.0" {
            return Err(AcpBoundaryError::InvalidResponse);
        }
        validate_json_rpc_id(&envelope.id)?;
        envelope.result.validate_response()?;
        Ok(AcpResultFrame {
            id: envelope.id,
            result: envelope.result,
        })
    }

    /// 把公开响应配置转换为共享 JSON 校验边界。
    const fn json_limits(&self) -> JsonValueLimits {
        JsonValueLimits {
            max_bytes: self.limits.max_payload_bytes,
            max_depth: self.limits.max_json_depth,
            max_nodes: self.limits.max_json_nodes,
        }
    }
}

impl Default for AcpResponseDecoder {
    /// 使用默认资源边界创建响应解码器。
    fn default() -> Self {
        Self {
            limits: AcpResponseLimits::default(),
        }
    }
}

/// Agent 发送给 ACP Client 的完整类型化 JSON-RPC 2.0 请求信封。
pub type AcpClientRequestFrame = JsonRpcMessage<AcpRpcRequest<AgentRequest>>;

/// 使用官方 ACP JSON-RPC 类型生成 Agent 到 Client 请求信封的编码器。
#[derive(Clone, Copy, Debug)]
pub struct AcpClientRequestEncoder {
    /// 当前出站 Client 请求序列化使用的资源边界。
    limits: AcpRequestLimits,
}

impl AcpClientRequestEncoder {
    /// 使用默认资源边界创建 Client 请求编码器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用显式资源边界创建 Client 请求编码器。
    pub const fn with_limits(limits: AcpRequestLimits) -> Result<Self, AcpBoundaryError> {
        if limits.max_method_bytes == 0
            || limits.max_payload_bytes == 0
            || limits.max_json_depth == 0
            || limits.max_json_nodes == 0
        {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self { limits })
    }

    /// 返回当前 Agent 到 Client 请求使用的资源边界。
    pub const fn limits(&self) -> AcpRequestLimits {
        self.limits
    }

    /// 校验 Client 能力、请求标识和完整载荷后构造标准 `elicitation/create` 请求。
    pub fn elicitation_request_frame(
        &self,
        request_id: RequestId,
        router: &ElicitationRouter,
        request: CreateElicitationRequest,
    ) -> Result<AcpClientRequestFrame, AcpBoundaryError> {
        validate_json_rpc_id(&request_id)?;
        let params = router.route_create_request(request)?;
        let method = Arc::<str>::from(params.method());
        if method.len() > self.limits.max_method_bytes {
            return Err(AcpBoundaryError::InvalidMethod);
        }
        let frame = JsonRpcMessage::wrap(AcpRpcRequest {
            id: request_id,
            method,
            params: Some(params),
        });
        self.encode_frame(&frame)?;
        Ok(frame)
    }

    /// 校验完整请求信封而不是仅校验 params，再输出确定性紧凑 JSON 字节。
    fn encode_frame<T>(&self, frame: &T) -> Result<Vec<u8>, AcpBoundaryError>
    where
        T: Serialize,
    {
        let value =
            serde_json::to_value(frame).map_err(|_| AcpBoundaryError::InvalidClientRequest)?;
        validate_value(&value, self.json_limits())?;
        serde_json::to_vec(&value).map_err(|_| AcpBoundaryError::InvalidClientRequest)
    }

    /// 把公开请求配置转换为共享 JSON 校验边界。
    const fn json_limits(&self) -> JsonValueLimits {
        JsonValueLimits {
            max_bytes: self.limits.max_payload_bytes,
            max_depth: self.limits.max_json_depth,
            max_nodes: self.limits.max_json_nodes,
        }
    }
}

impl Default for AcpClientRequestEncoder {
    /// 使用默认资源边界创建 Client 请求编码器。
    fn default() -> Self {
        Self {
            limits: AcpRequestLimits::default(),
        }
    }
}

/// 使用官方 ACP JSON-RPC 类型生成互斥 result/error 响应信封的编码器。
#[derive(Clone, Copy, Debug)]
pub struct AcpResponseEncoder {
    /// 当前响应序列化使用的资源边界。
    limits: AcpResponseLimits,
}

impl AcpResponseEncoder {
    /// 使用默认资源边界创建响应编码器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 使用显式资源边界创建响应编码器，并再次验证跨 FFI 边界的配置。
    pub const fn with_limits(limits: AcpResponseLimits) -> Result<Self, AcpBoundaryError> {
        if limits.max_payload_bytes == 0 || limits.max_json_depth == 0 || limits.max_json_nodes == 0
        {
            return Err(AcpBoundaryError::InvalidLimits);
        }
        Ok(Self { limits })
    }

    /// 返回当前响应资源边界。
    pub const fn limits(&self) -> AcpResponseLimits {
        self.limits
    }

    /// 编码一个保留原始请求 ID 的 JSON-RPC 成功响应。
    pub fn encode_result<T>(&self, id: RequestId, result: &T) -> Result<Vec<u8>, AcpBoundaryError>
    where
        T: AcpResponsePayload,
    {
        validate_json_rpc_id(&id)?;
        result.validate_response()?;
        self.encode_frame(&JsonRpcMessage::wrap(AcpRpcResponse::Result { id, result }))
    }

    /// 编码一个保留原始请求 ID 且不包含 result 字段的 JSON-RPC 错误响应。
    pub fn encode_error(
        &self,
        id: RequestId,
        error: &AcpRpcError,
    ) -> Result<Vec<u8>, AcpBoundaryError> {
        validate_json_rpc_id(&id)?;
        self.encode_frame(&JsonRpcMessage::wrap(AcpRpcResponse::<()>::Error {
            id,
            error: error.clone(),
        }))
    }

    /// 校验完整响应而不是仅校验 result，再输出确定性紧凑 JSON 字节。
    fn encode_frame<T>(&self, frame: &T) -> Result<Vec<u8>, AcpBoundaryError>
    where
        T: Serialize,
    {
        let value = serde_json::to_value(frame).map_err(|_| AcpBoundaryError::InvalidResponse)?;
        validate_value(&value, self.json_limits())?;
        serde_json::to_vec(&value).map_err(|_| AcpBoundaryError::InvalidResponse)
    }

    /// 把公开响应配置转换为共享 JSON 校验边界。
    const fn json_limits(&self) -> JsonValueLimits {
        JsonValueLimits {
            max_bytes: self.limits.max_payload_bytes,
            max_depth: self.limits.max_json_depth,
            max_nodes: self.limits.max_json_nodes,
        }
    }
}

impl Default for AcpResponseEncoder {
    /// 使用默认资源边界创建响应编码器。
    fn default() -> Self {
        Self {
            limits: AcpResponseLimits::default(),
        }
    }
}

/// 把原始 JSON 形状错误转换为响应边界分类，同时保留重复键和资源越界信息。
fn map_response_shape_error(error: AcpBoundaryError) -> AcpBoundaryError {
    match error {
        AcpBoundaryError::InvalidParams => AcpBoundaryError::InvalidResponse,
        other => other,
    }
}

/// 校验 JSON-RPC ID 只使用规范允许的有界字符串、整数或 null。
fn validate_json_rpc_id(id: &RequestId) -> Result<(), AcpBoundaryError> {
    match id {
        RequestId::Str(value) => validate_identifier(value, MAX_REQUEST_ID_BYTES),
        RequestId::Null | RequestId::Number(_) => Ok(()),
    }
}

/// Provider、Skill、MCP 或项目配置发生变化的 Session 级通知。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionConfigUpdateNotification {
    /// 配置目录单调修订号；Runtime 只接受比已应用值更新的修订。
    pub revision: u64,
}

/// 将用户消息注入当前正在运行的 Turn。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SteerSessionRequest {
    /// 目标 Session 标识。
    pub session_id: String,
    /// 在下一个安全模型消息边界加入的用户文本。
    pub text: String,
    /// ACP 为调用双方保留的扩展元数据；KeenCode 从中读取稳定 operationId。
    #[serde(skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<Meta>,
}

/// 修改 Session 用户可见标题。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenameSessionRequest {
    /// 目标 Session 标识。
    pub session_id: String,
    /// 新用户可见标题。
    pub title: String,
    /// ACP 为调用双方保留的扩展元数据；KeenCode 从中读取稳定 operationId。
    #[serde(skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<Meta>,
}

/// 根据首轮用户消息生成标题，不在同一请求中执行重命名。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerateSessionTitleRequest {
    /// 绑定 Provider 和标题生成缓存的目标 Session。
    pub session_id: String,
    /// 用于概括任务主题的有界用户消息。
    pub user_message: String,
    /// 稳定业务操作标识位于 `_meta["keencode/operationId"]`，与 RPC ID 无关。
    #[serde(skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<Meta>,
}

/// 查询 Session 可回退用户消息锚点。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewindCandidatesRequest {
    /// 目标 Session 标识。
    pub session_id: String,
}

/// 将 Session Transcript 回退到指定消息锚点。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RewindSessionRequest {
    /// 目标 Session 标识。
    pub session_id: String,
    /// 保留内容之前的用户消息稳定标识。
    pub target_message_id: String,
    /// 用户正在编辑的完整原文，用于独占事务中的并发与幂等校验。
    pub expected_text: String,
    /// 是否尝试恢复文件；首版桌面调用固定为 false。
    pub revert_files: bool,
    /// ACP 为调用双方保留的扩展元数据；KeenCode 从中读取稳定 operationId。
    #[serde(skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<Meta>,
}

/// 从权威事件日志分页重放 Session。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaySessionRequest {
    /// 目标 Session 标识。
    pub session_id: String,
    /// 上一页最后一个事件序号；缺失表示从当前快照水位开始。
    pub after: Option<u64>,
    /// 本页最多返回的权威事件数量。
    pub limit: u32,
}

/// 查询一个明确 Session 的后台任务，不允许隐式使用桌面焦点。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListBackgroundTasksRequest {
    /// 待查询的根 Session。
    pub session_id: String,
}

/// 取消 Session 内一个明确后台任务。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelBackgroundTaskRequest {
    /// 任务所属 Session。
    pub session_id: String,
    /// 后台任务稳定标识。
    pub task_id: String,
}

/// 查询项目当前唯一 Goal。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalGetRequest {
    /// 提供项目作用域的目标 Session。
    pub session_id: String,
}

/// 创建或更新 Goal 时允许由用户或 Agent 提交的字段。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalInput {
    /// 输入框上方展示的简短标题。
    pub title: String,
    /// 可验证且完整的目标描述。
    pub objective: String,
    /// 可选补充说明。
    pub description: Option<String>,
    /// 可选人工进度百分比。
    pub progress_percent: Option<u8>,
    /// 可选 Token 预算。
    pub token_budget: Option<u64>,
}

/// 创建或更新项目当前唯一 Goal。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalUpsertRequest {
    /// 提供项目作用域的目标 Session。
    pub session_id: String,
    /// 用户可修改的 Goal 字段。
    pub goal: GoalInput,
    /// 比较交换预期修订号；首次创建必须为零。
    pub expected_revision: u64,
    /// 调用方生成的幂等请求标识。
    pub request_nonce: String,
}

/// Goal 允许的两个不可逆终态。
#[derive(Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalTransitionStatus {
    /// 目标已经实际完成。
    Completed,
    /// 目标因为无法自行解决的外部条件阻塞。
    Blocked,
}

/// 把项目 Goal 迁移到不可逆终态。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalTransitionRequest {
    /// 提供项目作用域的目标 Session。
    pub session_id: String,
    /// 当前 Goal 稳定标识。
    pub goal_id: String,
    /// 目标的新不可逆终态。
    pub status: GoalTransitionStatus,
    /// 进入 blocked 时必须提供的安全原因；completed 时必须为空。
    pub reason: Option<String>,
    /// 进入 completed 时必须提供的非空验收证据；blocked 时必须为空。
    pub completion_evidence: Option<String>,
    /// 比较交换预期修订号。
    pub expected_revision: u64,
    /// 调用方生成的幂等请求标识。
    pub request_nonce: String,
}

/// 清除已经进入终态的项目 Goal。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GoalClearRequest {
    /// 提供项目作用域的目标 Session。
    pub session_id: String,
    /// 比较交换预期修订号。
    pub expected_revision: u64,
    /// 调用方生成的幂等请求标识。
    pub request_nonce: String,
}

/// 查询当前项目或全局 MCP Server 连接状态。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpListRequest {
    /// 可选项目路径；缺失或显式 `null` 仅表示全局配置只读视图。
    pub project_path: Option<String>,
}

/// 指向一个明确项目和 MCP Server 的 OAuth 控制面请求。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpOAuthServerRequest {
    /// MCP 配置所属项目路径。
    pub project_path: String,
    /// MCP 配置中的稳定 Server 名称。
    pub server_name: String,
}

impl fmt::Debug for McpOAuthServerRequest {
    /// 只显示项目路径和 Server 名称是否存在，不回显其内容。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthServerRequest")
            .field("project_path_present", &!self.project_path.is_empty())
            .field("server_name_present", &!self.server_name.is_empty())
            .finish()
    }
}

/// 宿主收到并提交给 MCP OAuth 状态机的授权回调。
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpOAuthCallbackRequest {
    /// MCP 配置所属项目路径。
    pub project_path: String,
    /// MCP 配置中的稳定 Server 名称。
    pub server_name: String,
    /// 授权服务返回的一次性授权码。
    pub code: String,
    /// 必须与当前待决授权常量时间匹配的 CSRF state。
    pub state: String,
}

impl fmt::Debug for McpOAuthCallbackRequest {
    /// 只显示字段是否存在，不回显路径、Server 名称、授权码或 state。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpOAuthCallbackRequest")
            .field("project_path_present", &!self.project_path.is_empty())
            .field("server_name_present", &!self.server_name.is_empty())
            .field("code_present", &!self.code.is_empty())
            .field("state_present", &!self.state.is_empty())
            .finish()
    }
}

/// KeenCode 补充 DTO 在类型形状之外必须满足的协议语义校验接口。
pub trait ValidateAcpParams {
    /// 校验字段长度、标识和值之间的跨字段不变量。
    fn validate(&self) -> Result<(), AcpBoundaryError>;
}

impl ValidateAcpParams for DeleteSessionRequest {
    /// 校验标准删除请求中的 Session 标识。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id.0, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for SessionConfigUpdateNotification {
    /// 配置修订号必须从一开始单调递增。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        if self.revision == 0 {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

impl ValidateAcpParams for SteerSessionRequest {
    /// 校验目标 Session 和要注入的有界用户文本。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(&self.text, MAX_USER_TEXT_BYTES)
    }
}

impl ValidateAcpParams for GenerateSessionTitleRequest {
    /// 标题请求必须明确指向 Session，并携带非空白的有界用户正文。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(&self.user_message, MAX_USER_TEXT_BYTES)?;
        if self.user_message.trim().is_empty() {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

impl ValidateAcpParams for RenameSessionRequest {
    /// 校验目标 Session 和非空有界标题。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(&self.title, MAX_TITLE_BYTES)
    }
}

impl ValidateAcpParams for RewindCandidatesRequest {
    /// 校验目标 Session 标识。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for RewindSessionRequest {
    /// 校验消息锚点，并拒绝首版不支持的文件恢复。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.target_message_id, MAX_IDENTIFIER_BYTES)?;
        validate_text(&self.expected_text, MAX_USER_TEXT_BYTES)?;
        if self.revert_files {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

impl ValidateAcpParams for ReplaySessionRequest {
    /// 校验重放水位和单页数量边界。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        if self.after == Some(0) || !(1..=MAX_REPLAY_EVENTS).contains(&self.limit) {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

impl ValidateAcpParams for CancelBackgroundTaskRequest {
    /// 校验 Session 和后台任务标识。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.task_id, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for ListBackgroundTasksRequest {
    /// 查询必须具有合法且明确的根 Session 标识。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for ReadFileChangeRequest {
    /// 校验文件变更读取的身份、侧别和有界原始字节区间。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        ReadFileChangeRequest::validate(self)
    }
}

impl ValidateAcpParams for GoalGetRequest {
    /// 校验提供项目作用域的 Session 标识。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for GoalUpsertRequest {
    /// 校验 Goal 内容、幂等标识和项目作用域。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        self.goal.validate()?;
        validate_identifier(&self.request_nonce, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for GoalInput {
    /// 校验 Goal 可编辑内容的文本、进度和可选 Token 预算。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_text(&self.title, MAX_TITLE_BYTES)?;
        validate_text(&self.objective, MAX_USER_TEXT_BYTES)?;
        if let Some(description) = &self.description {
            validate_text(description, MAX_USER_TEXT_BYTES)?;
        }
        if self.progress_percent.is_some_and(|progress| progress > 100)
            || self.token_budget == Some(0)
        {
            return Err(AcpBoundaryError::InvalidSemanticValue);
        }
        Ok(())
    }
}

impl ValidateAcpParams for GoalTransitionRequest {
    /// 校验 Goal 终态与原因、验收证据的互斥关系。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.goal_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.request_nonce, MAX_IDENTIFIER_BYTES)?;
        match (&self.status, &self.reason, &self.completion_evidence) {
            (GoalTransitionStatus::Completed, None, Some(evidence)) => {
                validate_text(evidence, MAX_USER_TEXT_BYTES)
            }
            (GoalTransitionStatus::Blocked, Some(reason), None) => {
                validate_text(reason, MAX_USER_TEXT_BYTES)
            }
            _ => Err(AcpBoundaryError::InvalidSemanticValue),
        }
    }
}

impl ValidateAcpParams for GoalClearRequest {
    /// 校验 Goal 清理作用域和幂等标识。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.session_id, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.request_nonce, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for McpListRequest {
    /// 校验可选项目路径；缺失路径只代表全局配置只读视图。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        if let Some(project_path) = &self.project_path {
            validate_identifier(project_path, MAX_OAUTH_PROJECT_PATH_BYTES)?;
        }
        Ok(())
    }
}

impl ValidateAcpParams for McpOAuthServerRequest {
    /// 校验项目路径和 MCP Server 稳定名称。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.project_path, MAX_OAUTH_PROJECT_PATH_BYTES)?;
        validate_identifier(&self.server_name, MAX_IDENTIFIER_BYTES)
    }
}

impl ValidateAcpParams for McpOAuthCallbackRequest {
    /// 校验项目路径、MCP Server、一次性授权码和 CSRF state。
    fn validate(&self) -> Result<(), AcpBoundaryError> {
        validate_identifier(&self.project_path, MAX_OAUTH_PROJECT_PATH_BYTES)?;
        validate_identifier(&self.server_name, MAX_IDENTIFIER_BYTES)?;
        validate_identifier(&self.code, MAX_OAUTH_FIELD_BYTES)?;
        validate_identifier(&self.state, MAX_OAUTH_FIELD_BYTES)
    }
}
