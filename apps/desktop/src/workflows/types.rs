//! WorkflowHost 的桌面边界类型。
//!
//! 定义和执行类型全部来自 `keencode-workflow`。本模块只放桌面存储、Journal 适配和
//! ZCode v4 查询的投影，避免再造一套与纯 Rust 引擎不兼容的工作流 schema。

use keencode_workflow::{CompiledWorkflow, InputSpec, WorkflowDefinitionV1};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

pub use keencode_resources::{ArtifactUse, WorkflowJournalEvent};
#[allow(unused_imports)]
pub use keencode_workflow::{
    AgentNode, Condition, DriverError, DriverLeafKind, DriverRequest, EffectClass,
    ExecutionOptions, ExecutionRequest, InputType, Node, NodeAddress, NodeContext, NodeKind,
    PublicError, TypedOutput, ValueExpr, WorkflowGraph, WorkflowLimits, WorkflowMeta,
};

pub const WORKFLOW_VERSION: u32 = keencode_workflow::WORKFLOW_DEFINITION_VERSION;
pub const MAX_WORKFLOW_NAME_CHARS: usize = 64;
pub const MAX_WORKFLOW_EVENT_PAYLOAD_BYTES: usize = 256 * 1024;
pub const MAX_WORKFLOW_DEFINITION_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_WORKFLOW_EVENT_LIMIT: usize = 500;

/// 引擎唯一支持的 JSON v1 定义。
pub type WorkflowDefinition = WorkflowDefinitionV1;

/// 兼容桌面调用方的纯 JSON 定义校验入口；执行也必须再次调用 `compile`。
pub fn validate_definition(
    definition: &WorkflowDefinition,
) -> Result<CompiledWorkflow, WorkflowDefinitionError> {
    keencode_workflow::compile(definition.clone()).map_err(WorkflowDefinitionError::Validation)
}

pub fn canonical_hash(definition: &WorkflowDefinition) -> Result<String, WorkflowDefinitionError> {
    Ok(validate_definition(definition)?
        .definition_hash()
        .to_owned())
}

pub fn canonical_json(definition: &WorkflowDefinition) -> Result<Vec<u8>, WorkflowDefinitionError> {
    validate_definition(definition)?;
    serde_json::to_vec(definition)
        .map_err(|error| WorkflowDefinitionError::Serialization(error.to_string()))
}

pub fn validate_workflow_name(name: &str) -> Result<(), WorkflowDefinitionError> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(WorkflowDefinitionError::InvalidName(name.to_owned()));
    }
    if name.chars().count() > MAX_WORKFLOW_NAME_CHARS
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Err(WorkflowDefinitionError::InvalidName(name.to_owned()));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum WorkflowDefinitionError {
    InvalidName(String),
    Validation(keencode_workflow::ValidationError),
    Serialization(String),
}

impl fmt::Display for WorkflowDefinitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(formatter, "invalid workflow name: {name}"),
            Self::Validation(error) => error.fmt(formatter),
            Self::Serialization(error) => {
                write!(formatter, "workflow serialization failed: {error}")
            }
        }
    }
}

impl std::error::Error for WorkflowDefinitionError {}

/// 已保存工作流的存储层级。
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowScope {
    Global,
    #[default]
    Project,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDefinitionMeta {
    pub name: String,
    pub description: Option<String>,
    pub when_to_use: Option<String>,
    pub args: BTreeMap<String, InputSpec>,
    pub scope: WorkflowScope,
    pub path: PathBuf,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidWorkflowDefinition {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowListResult {
    pub workflows: Vec<WorkflowDefinitionMeta>,
    pub invalid: Vec<InvalidWorkflowDefinition>,
    pub dir: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowGetResult {
    pub ok: bool,
    pub name: Option<String>,
    pub path: Option<PathBuf>,
    pub scope: Option<WorkflowScope>,
    pub meta: Option<WorkflowDefinitionMeta>,
    pub definition: Option<WorkflowDefinition>,
    pub reason: Option<String>,
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowRunStartRequest {
    pub name: String,
    pub scope: WorkflowScope,
    pub project_storage: Option<PathBuf>,
    pub parent_session_id: String,
    pub tool_call_id: Option<String>,
    pub launch_input_id: Option<String>,
    pub inputs: Value,
    pub cwd: PathBuf,
    pub model_selection: Option<Value>,
    pub budgets: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowRunDefinitionChangeRequest {
    pub predecessor_run_id: String,
    pub definition: WorkflowDefinition,
    pub parent_session_id: String,
    pub tool_call_id: Option<String>,
    pub launch_input_id: Option<String>,
    pub inputs: Value,
    pub cwd: PathBuf,
    pub model_selection: Option<Value>,
    pub budgets: Option<Value>,
    /// 子代理模型规范串；省略表示沿用 predecessor 的冻结选择，`null` 表示回到父会话模型。
    #[serde(default)]
    pub subagent_model: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunStartResult {
    pub run_id: String,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowRunStatus {
    Pending,
    Running,
    Completed,
    Errored,
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowRunStopReason {
    User,
    Model,
    Provider,
    Interrupted,
    Superseded,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunSummary {
    pub run_id: String,
    pub name: Option<String>,
    pub status: WorkflowRunStatus,
    pub stop_reason: Option<WorkflowRunStopReason>,
    pub created_at: u64,
    pub updated_at: u64,
    pub spent_tokens: u64,
    pub parent_session_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub args: Value,
    pub cwd: Option<PathBuf>,
    pub canonical_hash: Option<String>,
    pub artifacts: Vec<Value>,
    pub resumable: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunsResult {
    pub runs: Vec<WorkflowRunSummary>,
    pub truncated: bool,
}

/// 对外动态进度载荷，字段与 ZCode `DynamicWorkflowRunProgressPayload` 对齐。
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DynamicWorkflowRunProgressPayload {
    #[serde(rename = "runId")]
    pub run_id: String,
    #[serde(rename = "toolCallId", skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub sequence: u64,
    #[serde(rename = "eventType")]
    pub event_type: String,
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(rename = "actorSessionId", skip_serializing_if = "Option::is_none")]
    pub actor_session_id: Option<String>,
    #[serde(rename = "launchInputId", skip_serializing_if = "Option::is_none")]
    pub launch_input_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEventPage {
    pub events: Vec<WorkflowJournalEvent>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowGraphResult {
    pub run_id: String,
    pub definition: WorkflowDefinition,
    /// 对外只暴露 ZCode v4 的 ask/world-read 站点；控制节点和写入节点留在 Rust
    /// 引擎图与 workspace transcript 中，避免把内部 kind 泄露成第二套前端 schema。
    pub nodes: Vec<WorkflowGraphNode>,
    pub edges: Vec<WorkflowGraphEdge>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowGraphNode {
    pub id: String,
    pub kind: WorkflowGraphNodeKind,
    pub label: String,
    pub lane: String,
    pub depth: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WorkflowGraphNodeKind {
    #[serde(rename = "ask")]
    Ask,
    #[serde(rename = "world-read")]
    WorldRead,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowGraphEdge {
    pub from: String,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub back: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowWorkspaceNode {
    pub site_id: String,
    pub ordinal: u32,
    pub kind: String,
    pub op: Option<String>,
    pub args: Option<Vec<Value>>,
    pub status: String,
    pub error: Option<Value>,
    pub summary: Option<Value>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowWorkspaceResult {
    pub nodes: Vec<WorkflowWorkspaceNode>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowNodeResult {
    pub status: String,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub truncated: bool,
    pub total_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArtifact {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub content_type: Option<String>,
    pub source_path: Option<String>,
    pub spec: Option<Value>,
    pub version: u32,
    pub versions: Vec<Value>,
    pub item_count: u64,
    pub primary: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArtifactPage {
    pub items: Vec<Value>,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowArtifactBytes {
    pub data_base64: String,
    pub media_type: String,
    pub total_bytes: usize,
    pub next_offset: Option<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowJournalError(pub String);

impl fmt::Display for WorkflowJournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WorkflowJournalError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowRuntimeError(pub String);

impl fmt::Display for WorkflowRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WorkflowRuntimeError {}

/// 运行过程中需要冻结的定义、输入和父会话模型选择。
#[derive(Clone, Debug)]
pub struct FrozenWorkflowRun {
    pub definition: WorkflowDefinition,
    pub canonical_hash: String,
    pub input_hash: String,
    pub resolved_inputs: Value,
    pub parent_session_id: String,
    pub tool_call_id: String,
    pub launch_input_id: Option<String>,
    pub cwd: PathBuf,
    pub model_selection: Option<Value>,
    pub budgets: Option<Value>,
    /// 设置修订的可选模型覆盖，属于 run-started 冻结事实的一部分。
    pub subagent_model: Option<String>,
    /// 修订 run 的父运行身份同样属于冻结事实，resume 不能将其清空。
    pub predecessor_run_id: Option<String>,
}

pub fn progress_from_workflow_event(
    event: &WorkflowJournalEvent,
) -> DynamicWorkflowRunProgressPayload {
    DynamicWorkflowRunProgressPayload {
        run_id: event.run_id.clone(),
        tool_call_id: (!event.tool_call_id.is_empty()).then(|| event.tool_call_id.clone()),
        sequence: event.sequence,
        event_type: event.event_type.clone(),
        payload: event.payload.clone(),
        truncated: None,
        actor_session_id: event.actor_session_id.clone(),
        launch_input_id: event.launch_input_id.clone(),
    }
}
