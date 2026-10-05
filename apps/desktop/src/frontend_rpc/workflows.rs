//! WorkflowHost 的薄 RPC 适配。
//!
//! 这里不创建状态，也不把参数写入文件；根装配层把当前父会话的 `Arc<WorkflowHost>` 传入，
//! 本模块只负责 ZCode 方法名和 JSON 参数到 Host 方法的映射。

use crate::workflows::{
    WorkflowArtifact, WorkflowArtifactBytes, WorkflowArtifactPage, WorkflowDefinition,
    WorkflowDefinitionMeta, WorkflowEventPage, WorkflowGetResult, WorkflowGraphResult,
    WorkflowHost, WorkflowHostError, WorkflowListResult, WorkflowNodeResult,
    WorkflowRunDefinitionChangeRequest, WorkflowRunStartRequest, WorkflowRunStatus,
    WorkflowRunStopReason, WorkflowRunSummary, WorkflowScope, WorkflowStore, WorkflowStoreError,
    WorkflowWorkspaceNode, WorkflowWorkspaceResult,
};
use keencode_workflow::{Node, WorkflowLimits};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefinitionArgs {
    #[serde(default)]
    scope: WorkflowScope,
    definition: WorkflowDefinition,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefinitionNameArgs {
    #[serde(default)]
    scope: WorkflowScope,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefinitionMetaArgs {
    #[serde(default)]
    scope: WorkflowScope,
    name: String,
    #[serde(default)]
    meta: Option<DefinitionMetaPatch>,
    description: Option<String>,
    when_to_use: Option<String>,
    args: Option<std::collections::BTreeMap<String, keencode_workflow::InputSpec>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DefinitionMetaPatch {
    description: Option<String>,
    when_to_use: Option<String>,
    args: Option<std::collections::BTreeMap<String, keencode_workflow::InputSpec>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MoveDefinitionArgs {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunEventsArgs {
    run_id: String,
    after_sequence: Option<u64>,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RunListArgs {
    parent_session_id: Option<String>,
    name: Option<String>,
    scope: Option<WorkflowScope>,
    #[serde(default = "default_run_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NodeResultArgs {
    run_id: String,
    site_id: String,
    ordinal: u32,
    #[serde(default = "default_node_result_limit")]
    max_bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactItemsArgs {
    run_id: String,
    artifact_id: String,
    after_sequence: Option<u64>,
    #[serde(default = "default_event_limit")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactReadArgs {
    run_id: String,
    artifact_id: String,
    #[serde(default = "default_artifact_version")]
    version: u32,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_artifact_chunk")]
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QuestionArgs {
    #[serde(rename = "questionId")]
    qid: String,
    answer: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedWorkflowStartArgs {
    name: String,
    #[serde(default)]
    scope: WorkflowScope,
    #[serde(default = "default_inputs")]
    args: Value,
    parent_session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    project_storage: Option<PathBuf>,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    launch_input_id: Option<String>,
    #[serde(default)]
    model_selection: Option<Value>,
    #[serde(default)]
    budgets: Option<Value>,
}

/// `amendSettings` 只由已校验的 Session command 内部调用；仍使用严格 Source 命名，
/// 让未知字段和 snake_case 别名在进入 Journal 前失败，而不是被手工读取逻辑静默忽略。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AmendSettingsArgs {
    run_id: String,
    parent_session_id: String,
    cwd: PathBuf,
    #[serde(default)]
    subagent_model: Option<Option<String>>,
    #[serde(default)]
    max_concurrency: Option<Option<u16>>,
    model_selection: Value,
    tool_call_id: String,
    launch_input_id: String,
}

/// 定义管理方法没有会话上下文，根装配必须把已授权项目 Store 传给
/// [`call_definitions`]；调用方传入的 cwd/projectStorage 不在这里参与路径解析。
pub fn is_definition_method(method: &str) -> bool {
    matches!(
        method,
        "listSavedWorkflows"
            | "getSavedWorkflow"
            | "updateSavedWorkflowMeta"
            | "deleteSavedWorkflow"
            | "moveSavedWorkflow"
            | "list"
            | "listWorkflows"
            | "workflows.list"
            | "get"
            | "getWorkflow"
            | "workflows.get"
            | "save"
            | "createWorkflow"
            | "saveWorkflow"
            | "workflows.save"
            | "updateMeta"
            | "updateWorkflowMeta"
            | "workflows.updateMeta"
            | "delete"
            | "deleteWorkflow"
            | "workflows.delete"
            | "move"
            | "moveWorkflow"
            | "workflows.move"
    )
}

/// 运行面与定义面的统一方法判定，供 CompositeHandler 在进入具体 Host 前做路由。
pub fn is_workflow_method(method: &str) -> bool {
    is_definition_method(method)
        || matches!(
            method,
            "conversationWorkflowRunEvents"
                | "conversationWorkflowRunEventsV4"
                | "conversationWorkflowRuns"
                | "conversationWorkflowRunsV4"
                | "conversationWorkflowRunArtifacts"
                | "conversationWorkflowRunArtifactsV4"
                | "conversationWorkflowRunArtifactData"
                | "conversationWorkflowRunArtifactDataV4"
                | "conversationWorkflowRunArtifactRead"
                | "conversationWorkflowRunArtifactReadV4"
                | "conversationWorkflowRunWorkspace"
                | "conversationWorkflowRunWorkspaceV4"
                | "conversationWorkflowRunGraph"
                | "conversationWorkflowRunGraphV4"
                | "conversationWorkflowRunNodeResult"
                | "conversationWorkflowRunNodeResultV4"
                | "listSavedWorkflowRuns"
                | "listRuns"
                | "listWorkflowRuns"
                | "workflows.listRuns"
                | "runSummary"
                | "getWorkflowRun"
                | "workflows.getRun"
                | "events"
                | "listWorkflowRunEvents"
                | "workflows.events"
                | "progress"
                | "replayWorkflowRun"
                | "workflows.progress"
                | "graph"
                | "getWorkflowRunGraph"
                | "workflows.graph"
                | "workspace"
                | "workflows.workspace"
                | "nodeResult"
                | "workflowRunNodeResult"
                | "workflows.nodeResult"
                | "artifacts"
                | "workflowRunArtifacts"
                | "workflows.artifacts"
                | "artifactItems"
                | "workflowRunArtifactData"
                | "workflows.artifactItems"
                | "artifactRead"
                | "workflowRunArtifactRead"
                | "workflows.artifactRead"
        )
}

fn default_event_limit() -> usize {
    100
}

fn default_inputs() -> Value {
    json!({})
}

fn default_run_limit() -> usize {
    50
}

fn default_node_result_limit() -> usize {
    512 * 1024
}

fn default_artifact_version() -> u32 {
    1
}

fn default_artifact_chunk() -> usize {
    512 * 1024
}

const V4_WORKSPACE_RESULT_MAX_BYTES: usize = 32 * 1024;

fn settings_rejected(reason: &str, detail: impl Into<String>) -> WorkflowHostError {
    WorkflowHostError::SettingsRejected {
        reason: reason.to_owned(),
        detail: detail.into(),
    }
}

fn parse_amend_settings(args: Value) -> Result<AmendSettingsArgs, WorkflowHostError> {
    let input: AmendSettingsArgs = serde_json::from_value(args).map_err(|error| {
        settings_rejected(
            "missing_boundaries",
            format!("amendSettings 参数无效: {error}"),
        )
    })?;
    for (field, value) in [
        ("runId", input.run_id.as_str()),
        ("parentSessionId", input.parent_session_id.as_str()),
        ("toolCallId", input.tool_call_id.as_str()),
        ("launchInputId", input.launch_input_id.as_str()),
    ] {
        if value.trim().is_empty() {
            return Err(settings_rejected(
                "missing_boundaries",
                format!("amendSettings 缺少 {field}"),
            ));
        }
    }
    if let Some(Some(model)) = input.subagent_model.as_ref()
        && (model.trim().is_empty() || model.chars().count() > 256)
    {
        return Err(settings_rejected(
            "model_unavailable",
            "subagentModel 超出长度限制",
        ));
    }
    if input
        .max_concurrency
        .is_some_and(|value| value.is_some_and(|value| !(1..=1_024).contains(&value)))
    {
        return Err(settings_rejected(
            "not_configurable",
            "maxConcurrency 超出允许范围",
        ));
    }
    Ok(input)
}

fn amend_budgets(
    existing: Option<&Value>,
    change: Option<Option<u16>>,
) -> Result<Option<Value>, WorkflowHostError> {
    let Some(change) = change else {
        return Ok(existing.cloned());
    };
    let mut budgets = match existing {
        Some(value) => value.as_object().cloned().ok_or_else(|| {
            settings_rejected("not_configurable", "冻结预算不是对象，不能安全修订")
        })?,
        None => Map::new(),
    };
    budgets.remove("maxConcurrency");
    budgets.remove("max_concurrency");
    if let Some(value) = change {
        // WorkflowLimits 使用 Rust 的 snake_case serde 字段；Source 投影所需的
        // camelCase caps 会在 run-started 中由同一预算事实派生。
        budgets.insert("max_concurrency".to_owned(), json!(value));
    }
    Ok(Some(Value::Object(budgets)))
}

/// 配置修订会取消 predecessor 并铸造 successor；只有可证明无副作用的 Read 工具图
/// 才能安全重跑。Agent、Artifact、写工具即使声明 read_only，也不能在未复用完成结果
/// 的情况下自动重放，避免 Apply 重复真实副作用。
fn amendment_restart_is_read_only(definition: &WorkflowDefinition) -> bool {
    fn nodes_are_read_only(nodes: &[Node]) -> bool {
        nodes.iter().all(|node| match node {
            Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
                nodes_are_read_only(nodes)
            }
            Node::If {
                then_body,
                else_body,
                ..
            } => nodes_are_read_only(then_body) && nodes_are_read_only(else_body),
            Node::Foreach { body, .. } | Node::Repeat { body, .. } => nodes_are_read_only(body),
            Node::Tool(tool) => {
                tool.effect == keencode_workflow::EffectClass::ReadOnly && tool.name == "Read"
            }
            Node::Agent(_) | Node::Artifact(_) => false,
        })
    }

    nodes_are_read_only(&definition.body)
}

fn frozen_model_parts(value: &Value) -> Option<(&str, &str, Option<&str>)> {
    let provider = value.get("provider")?.as_object()?;
    let provider_id = provider.get("providerId")?.as_str()?;
    let model = provider.get("model")?.as_str()?;
    let reasoning = provider.get("reasoningEffort").and_then(Value::as_str);
    Some((provider_id, model, reasoning))
}

fn requested_model_parts(value: &str) -> Option<(&str, &str, Option<&str>)> {
    let (provider_id, model_and_level) = value.split_once('/')?;
    if provider_id.is_empty() || model_and_level.is_empty() {
        return None;
    }
    let (model, reasoning) = model_and_level
        .split_once('$')
        .map_or((model_and_level, None), |(model, reasoning)| {
            (model, Some(reasoning))
        });
    if model.is_empty() || reasoning.is_some_and(str::is_empty) {
        return None;
    }
    Some((provider_id, model, reasoning))
}

fn validate_requested_model(
    model_selection: &Value,
    requested: &str,
) -> Result<(), WorkflowHostError> {
    let Some((requested_provider, requested_model, requested_reasoning)) =
        requested_model_parts(requested)
    else {
        return Err(settings_rejected(
            "model_unavailable",
            "subagentModel 不是有效的规范模型串",
        ));
    };
    let Some((provider, model, reasoning)) = frozen_model_parts(model_selection) else {
        return Err(settings_rejected(
            "missing_boundaries",
            "缺少已授权的冻结模型快照",
        ));
    };
    if provider != requested_provider || model != requested_model {
        return Err(settings_rejected(
            "model_unavailable",
            "请求的子代理模型未绑定到当前已授权 Provider",
        ));
    }
    if requested_reasoning != reasoning {
        return Err(settings_rejected(
            "model_unavailable",
            "请求的推理档位未绑定到当前已授权 Provider",
        ));
    }
    Ok(())
}

async fn amend_settings(host: &Arc<WorkflowHost>, args: Value) -> Result<Value, WorkflowHostError> {
    let input = parse_amend_settings(args)?;
    let predecessor_run_id = input.run_id;
    let parent_session_id = input.parent_session_id;
    let cwd = input.cwd;
    let subagent_model = input.subagent_model;
    let max_concurrency = input.max_concurrency;
    if subagent_model.is_none() && max_concurrency.is_none() {
        return Err(settings_rejected("unchanged", "没有提供需要修改的设置"));
    }
    let summary = host
        .run_summary(&predecessor_run_id)?
        .ok_or_else(|| settings_rejected("not_found", "工作流运行不存在"))?;
    if matches!(summary.status, WorkflowRunStatus::Completed)
        || matches!(summary.status, WorkflowRunStatus::Stopped)
            && summary.stop_reason != Some(WorkflowRunStopReason::Superseded)
    {
        return Err(settings_rejected(
            "not_configurable",
            "该工作流运行已不能修改设置",
        ));
    }
    let frozen = host
        .frozen_run(&predecessor_run_id)?
        .ok_or_else(|| settings_rejected("not_found", "工作流运行缺少冻结快照"))?;
    if !amendment_restart_is_read_only(&frozen.definition) {
        return Err(settings_rejected(
            "not_configurable",
            "该工作流包含 Agent、写工具或产物节点，不能通过配置修订自动重跑",
        ));
    }
    if frozen.parent_session_id != parent_session_id || frozen.cwd != cwd {
        return Err(settings_rejected(
            "missing_boundaries",
            "工作流父会话或工作目录不匹配冻结事实",
        ));
    }
    let model_selection = input.model_selection;
    let next_subagent_model = match subagent_model {
        None => frozen.subagent_model.clone(),
        Some(None) => None,
        Some(Some(value)) => {
            validate_requested_model(&model_selection, &value)?;
            Some(value)
        }
    };
    let budgets = amend_budgets(frozen.budgets.as_ref(), max_concurrency)?;
    if let Some(budgets) = budgets.as_ref() {
        serde_json::from_value::<WorkflowLimits>(budgets.clone()).map_err(|error| {
            settings_rejected("compile_failed", format!("冻结预算校验失败: {error}"))
        })?;
    }
    let tool_call_id = input.tool_call_id;
    let launch_input_id = input.launch_input_id;
    let result = host
        .start_definition_change(WorkflowRunDefinitionChangeRequest {
            predecessor_run_id: predecessor_run_id.clone(),
            definition: frozen.definition,
            parent_session_id,
            tool_call_id: Some(tool_call_id.clone()),
            launch_input_id: Some(launch_input_id),
            inputs: frozen.resolved_inputs,
            cwd,
            model_selection: Some(model_selection),
            budgets,
            subagent_model: next_subagent_model,
        })
        .await?;
    let mut output = json!({
        "type": "amendWorkflowRunSettings",
        "runId": result.run_id,
        "toolCallId": tool_call_id,
    });
    if matches!(
        summary.status,
        WorkflowRunStatus::Running | WorkflowRunStatus::Pending
    ) {
        output["supersededRunId"] = Value::String(predecessor_run_id);
    }
    Ok(output)
}

fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, WorkflowHostError> {
    serde_json::from_value(args).map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))
}

/// 把无会话的已保存工作流 run 映射为 ZCode 中枢的严格结果。
///
/// `WorkflowRunSummary` 还包含 Rust 恢复门和定义摘要等宿主字段；这些字段不能直接
/// 序列化到 ZCode 的 `ZCodeSavedWorkflowRun`，否则 strict schema 会整页拒收。
pub fn project_saved_runs(
    runs: Vec<WorkflowRunSummary>,
    truncated: bool,
) -> Result<Value, WorkflowHostError> {
    let runs = runs
        .into_iter()
        .map(project_saved_run)
        .collect::<Result<Vec<_>, _>>()?;
    let mut result = json!({ "runs": runs });
    if truncated {
        result["truncated"] = Value::Bool(true);
    }
    Ok(result)
}

fn project_saved_run(run: WorkflowRunSummary) -> Result<Value, WorkflowHostError> {
    let mut result = Map::new();
    result.insert("runId".to_owned(), Value::String(run.run_id));
    if let Some(name) = non_empty_bounded(run.name, 160) {
        result.insert("name".to_owned(), Value::String(name));
    }
    result.insert("status".to_owned(), status_value(run.status));
    if let Some(stop_reason) = run.stop_reason {
        result.insert("stopReason".to_owned(), stop_reason_value(stop_reason));
    }
    result.insert("createdAt".to_owned(), json!(run.created_at));
    result.insert("updatedAt".to_owned(), json!(run.updated_at));
    result.insert("spentTokens".to_owned(), json!(run.spent_tokens));
    if let Some(parent_session_id) = non_empty_bounded(run.parent_session_id, 256) {
        result.insert(
            "parentSessionId".to_owned(),
            Value::String(parent_session_id),
        );
    }
    if let Some(tool_call_id) = non_empty_bounded(run.tool_call_id, 512) {
        result.insert("toolCallId".to_owned(), Value::String(tool_call_id));
    }
    if run.args.is_object() {
        result.insert("args".to_owned(), run.args);
    }
    if let Some(cwd) = run.cwd {
        result.insert(
            "cwd".to_owned(),
            Value::String(cwd.to_string_lossy().into_owned()),
        );
    }
    let artifacts = run
        .artifacts
        .iter()
        .map(saved_artifact_chip)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .take(8)
        .collect::<Vec<_>>();
    if !artifacts.is_empty() {
        result.insert("artifacts".to_owned(), Value::Array(artifacts));
    }
    Ok(Value::Object(result))
}

/// V4 会话 run 查询只允许读面摘要字段；它与无会话 saved-workflow run 行是两份不同契约。
fn project_v4_runs(runs: &[WorkflowRunSummary]) -> Value {
    let rows = runs
        .iter()
        .map(|run| {
            let mut result = Map::new();
            result.insert("runId".to_owned(), Value::String(run.run_id.clone()));
            if let Some(tool_call_id) = non_empty_bounded(run.tool_call_id.clone(), 512) {
                result.insert("toolCallId".to_owned(), Value::String(tool_call_id));
            }
            if let Some(label) = non_empty_bounded(run.name.clone(), 160) {
                result.insert("label".to_owned(), Value::String(label));
            }
            result.insert("updatedAt".to_owned(), json!(run.updated_at));
            result.insert("status".to_owned(), status_value(run.status.clone()));
            if let Some(stop_reason) = run.stop_reason.clone() {
                result.insert("stopReason".to_owned(), stop_reason_value(stop_reason));
            }
            result.insert("resumable".to_owned(), Value::Bool(run.resumable));
            Value::Object(result)
        })
        .collect::<Vec<_>>();
    json!({ "runs": rows })
}

fn status_value(status: WorkflowRunStatus) -> Value {
    Value::String(
        match status {
            WorkflowRunStatus::Pending => "pending",
            WorkflowRunStatus::Running => "running",
            WorkflowRunStatus::Completed => "completed",
            WorkflowRunStatus::Errored => "errored",
            WorkflowRunStatus::Stopped => "stopped",
        }
        .to_owned(),
    )
}

fn stop_reason_value(reason: WorkflowRunStopReason) -> Value {
    Value::String(
        match reason {
            WorkflowRunStopReason::User => "user",
            WorkflowRunStopReason::Model => "model",
            WorkflowRunStopReason::Provider => "provider",
            WorkflowRunStopReason::Interrupted => "interrupted",
            WorkflowRunStopReason::Superseded => "superseded",
        }
        .to_owned(),
    )
}

fn non_empty_bounded(value: Option<String>, max_chars: usize) -> Option<String> {
    let value = value?.trim().to_owned();
    (!value.is_empty()).then(|| value.chars().take(max_chars).collect())
}

fn saved_artifact_chip(value: &Value) -> Result<Value, WorkflowHostError> {
    let object = value.as_object().ok_or_else(|| {
        WorkflowHostError::FrozenRun("workflow artifact summary must be an object".to_owned())
    })?;
    let id = object
        .get("id")
        .or_else(|| object.get("artifactId"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(|id| id.chars().take(64).collect::<String>())
        .ok_or_else(|| {
            WorkflowHostError::FrozenRun("workflow artifact summary id is empty".to_owned())
        })?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| {
            matches!(
                *kind,
                "file" | "markdown" | "chart" | "table" | "metrics" | "board"
            )
        })
        .ok_or_else(|| {
            WorkflowHostError::FrozenRun(
                "workflow artifact summary kind is outside the source schema".to_owned(),
            )
        })?;
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .filter(|version| (1..=16).contains(version))
        .ok_or_else(|| {
            WorkflowHostError::FrozenRun(
                "workflow artifact summary version must be an integer in 1..=16".to_owned(),
            )
        })?;
    let mut chip = Map::new();
    chip.insert("id".to_owned(), Value::String(id));
    chip.insert("kind".to_owned(), Value::String(kind.to_owned()));
    if let Some(title) = object
        .get("title")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        chip.insert(
            "title".to_owned(),
            Value::String(title.chars().take(120).collect()),
        );
    }
    chip.insert("version".to_owned(), json!(version));
    if let Some(content_type) = object
        .get("contentType")
        .or_else(|| object.get("mediaType"))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        chip.insert(
            "contentType".to_owned(),
            Value::String(content_type.chars().take(128).collect()),
        );
    }
    Ok(Value::Object(chip))
}

fn project_events(page: WorkflowEventPage) -> Value {
    let events = page
        .events
        .into_iter()
        .map(protocol_event)
        .collect::<Vec<_>>();
    json!({ "events": events, "hasMore": page.has_more })
}

fn project_workspace(result: WorkflowWorkspaceResult) -> Value {
    let nodes = result
        .nodes
        .iter()
        .filter_map(project_workspace_node)
        .collect::<Vec<_>>();
    let mut response = json!({ "nodes": nodes });
    if result.truncated {
        response["truncated"] = Value::Bool(true);
    }
    response
}

fn project_workspace_node(node: &WorkflowWorkspaceNode) -> Option<Value> {
    let site_id = non_empty_bounded(Some(node.site_id.clone()), 64)?;
    let kind =
        matches!(node.kind.as_str(), "world-read" | "world-run").then_some(node.kind.as_str())?;
    let mut result = Map::new();
    result.insert("siteId".to_owned(), Value::String(site_id));
    result.insert("ordinal".to_owned(), json!(node.ordinal));
    result.insert("kind".to_owned(), Value::String(kind.to_owned()));
    if let Some(op) = node.op.as_deref().filter(|op| !op.is_empty()) {
        result.insert(
            "op".to_owned(),
            Value::String(op.chars().take(32).collect()),
        );
    }
    if let Some(args) = &node.args {
        result.insert(
            "args".to_owned(),
            Value::Array(args.iter().take(16).cloned().collect()),
        );
        if args.len() > 16 {
            result.insert("inputTruncated".to_owned(), Value::Bool(true));
        }
    }
    result.insert(
        "status".to_owned(),
        Value::String(workspace_status(&node.status).to_owned()),
    );
    if let Some(error) = node.error.as_ref().and_then(project_workspace_error) {
        result.insert("error".to_owned(), error);
    }
    if let Some(summary) = node.summary.as_ref() {
        result.insert("summary".to_owned(), project_workspace_summary(summary));
    }
    result.insert("createdAt".to_owned(), json!(node.created_at));
    result.insert("updatedAt".to_owned(), json!(node.updated_at));
    Some(Value::Object(result))
}

fn workspace_status(status: &str) -> &'static str {
    match status {
        "running" | "pending" | "started" | "prepared" => "running",
        "completed" | "succeeded" | "applied" => "completed",
        _ => "failed",
    }
}

fn project_workspace_error(value: &Value) -> Option<Value> {
    let object = value.as_object();
    let code = object
        .and_then(|object| object.get("code"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect::<String>())
        .unwrap_or_else(|| "workflow_error".to_owned());
    let message = object
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| value.to_string());
    Some(json!({
        "code": code,
        "message": message.chars().take(2_000).collect::<String>(),
    }))
}

fn project_workspace_summary(value: &Value) -> Value {
    let object = value.as_object();
    let result_bytes = object
        .and_then(|object| object.get("resultBytes"))
        .and_then(Value::as_u64)
        .or_else(|| {
            object
                .and_then(|object| object.get("result"))
                .and_then(|result| serde_json::to_vec(result).ok())
                .map(|bytes| bytes.len() as u64)
        })
        .unwrap_or(0);
    let mut summary = Map::new();
    summary.insert("resultBytes".to_owned(), json!(result_bytes));
    for (source, target) in [
        ("resultCount", "resultCount"),
        ("exitCode", "exitCode"),
        ("stdoutBytes", "stdoutBytes"),
        ("stderrBytes", "stderrBytes"),
    ] {
        if let Some(value) = object
            .and_then(|object| object.get(source))
            .and_then(Value::as_i64)
        {
            summary.insert(target.to_owned(), json!(value));
        }
    }
    Value::Object(summary)
}

fn project_node_result(result: WorkflowNodeResult) -> Value {
    let mut response = Map::new();
    response.insert(
        "status".to_owned(),
        Value::String(workspace_status(&result.status).to_owned()),
    );
    if let Some(value) = result.result {
        response.insert("result".to_owned(), value);
    }
    if let Some(error) = result.error.as_ref().and_then(project_workspace_error) {
        response.insert("error".to_owned(), error);
    }
    response.insert("truncated".to_owned(), Value::Bool(result.truncated));
    response.insert("totalBytes".to_owned(), json!(result.total_bytes));
    Value::Object(response)
}

fn project_artifacts(artifacts: Vec<WorkflowArtifact>) -> Result<Value, WorkflowHostError> {
    let projected = artifacts
        .iter()
        .map(project_artifact)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Array(projected))
}

fn project_artifact(artifact: &WorkflowArtifact) -> Result<Value, WorkflowHostError> {
    let id = non_empty_bounded(Some(artifact.id.clone()), 64).ok_or_else(|| {
        WorkflowHostError::FrozenRun("workflow artifact id is empty or too long".to_owned())
    })?;
    let kind = matches!(
        artifact.kind.as_str(),
        "file" | "markdown" | "chart" | "table" | "metrics" | "board"
    )
    .then_some(artifact.kind.as_str())
    .ok_or_else(|| {
        WorkflowHostError::FrozenRun(format!(
            "workflow artifact kind is outside the source schema: {}",
            artifact.kind
        ))
    })?;
    if !(1..=16).contains(&artifact.version) || artifact.versions.len() > 16 {
        return Err(WorkflowHostError::FrozenRun(format!(
            "workflow artifact version {} is outside 1..=16",
            artifact.version
        )));
    }
    let versions = artifact
        .versions
        .iter()
        .map(project_artifact_version)
        .collect::<Result<Vec<_>, _>>()?;
    let mut result = Map::new();
    result.insert("id".to_owned(), Value::String(id));
    result.insert("kind".to_owned(), Value::String(kind.to_owned()));
    insert_non_empty(&mut result, "title", artifact.title.as_deref(), 120);
    insert_non_empty(
        &mut result,
        "description",
        artifact.description.as_deref(),
        500,
    );
    insert_non_empty(
        &mut result,
        "contentType",
        artifact.content_type.as_deref(),
        128,
    );
    insert_non_empty(
        &mut result,
        "sourcePath",
        artifact.source_path.as_deref(),
        1_024,
    );
    if let Some(spec) = artifact.spec.as_ref().filter(|value| !value.is_null()) {
        result.insert("spec".to_owned(), spec.clone());
    }
    result.insert("version".to_owned(), json!(artifact.version));
    result.insert("versions".to_owned(), Value::Array(versions));
    result.insert("itemCount".to_owned(), json!(artifact.item_count));
    if artifact.primary {
        result.insert("primary".to_owned(), Value::Bool(true));
    }
    Ok(Value::Object(result))
}

fn project_artifact_version(value: &Value) -> Result<Value, WorkflowHostError> {
    let object = value.as_object().ok_or_else(|| {
        WorkflowHostError::FrozenRun("workflow artifact version must be an object".to_owned())
    })?;
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .filter(|value| (1..=16).contains(value))
        .ok_or_else(|| {
            WorkflowHostError::FrozenRun(
                "workflow artifact version must be an integer in 1..=16".to_owned(),
            )
        })?;
    let published_at = object
        .get("publishedAt")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            WorkflowHostError::FrozenRun(
                "workflow artifact version publishedAt must be a non-negative integer".to_owned(),
            )
        })?;
    let mut result = Map::new();
    result.insert("version".to_owned(), json!(version));
    insert_non_empty_from_value(&mut result, "title", object.get("title"), 120);
    insert_non_empty_from_value(&mut result, "description", object.get("description"), 500);
    insert_non_empty_from_value(&mut result, "contentType", object.get("contentType"), 128);
    insert_non_empty_from_value(&mut result, "sourcePath", object.get("sourcePath"), 1_024);
    if let Some(spec) = object.get("spec").filter(|value| !value.is_null()) {
        result.insert("spec".to_owned(), spec.clone());
    }
    if let Some(uri) = object
        .get("uri")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        result.insert(
            "uri".to_owned(),
            Value::String(uri.chars().take(512).collect()),
        );
    }
    if let Some(bytes) = object.get("bytes").and_then(Value::as_u64) {
        result.insert("bytes".to_owned(), json!(bytes));
    }
    if object.get("primary").and_then(Value::as_bool) == Some(true) {
        result.insert("primary".to_owned(), Value::Bool(true));
    }
    result.insert("publishedAt".to_owned(), json!(published_at));
    Ok(Value::Object(result))
}

fn insert_non_empty(
    target: &mut Map<String, Value>,
    key: &str,
    value: Option<&str>,
    max_chars: usize,
) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        target.insert(
            key.to_owned(),
            Value::String(value.chars().take(max_chars).collect()),
        );
    }
}

fn insert_non_empty_from_value(
    target: &mut Map<String, Value>,
    key: &str,
    value: Option<&Value>,
    max_chars: usize,
) {
    insert_non_empty(target, key, value.and_then(Value::as_str), max_chars);
}

fn project_artifact_page(page: WorkflowArtifactPage) -> Value {
    let items = page
        .items
        .into_iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            let site_id = object
                .get("siteId")
                .or_else(|| object.get("site_id"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| value.chars().take(64).collect::<String>())?;
            let sequence = object.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            let ordinal = object.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
            Some(json!({
                "sequence": sequence,
                "siteId": site_id,
                "ordinal": ordinal,
                "item": object.get("item").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect::<Vec<_>>();
    json!({ "items": items, "hasMore": page.has_more })
}

fn project_artifact_bytes(bytes: WorkflowArtifactBytes) -> Value {
    json!({
        "dataBase64": bytes.data_base64,
        "mediaType": bytes.media_type,
        "totalBytes": bytes.total_bytes,
        "nextOffset": bytes.next_offset,
    })
}

fn protocol_definition_meta(meta: &WorkflowDefinitionMeta) -> Value {
    let mut result = Map::new();
    result.insert("name".to_owned(), Value::String(meta.name.clone()));
    result.insert(
        "description".to_owned(),
        Value::String(
            meta.description
                .clone()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| meta.name.clone()),
        ),
    );
    insert_non_empty(&mut result, "whenToUse", meta.when_to_use.as_deref(), 500);
    let args = meta
        .args
        .iter()
        .map(|(name, spec)| {
            let mut value = Map::new();
            value.insert(
                "type".to_owned(),
                Value::String(input_type_name(&spec.value_type).to_owned()),
            );
            if spec.required {
                value.insert("required".to_owned(), Value::Bool(true));
            }
            if let Some(description) = spec.description.as_ref() {
                value.insert("description".to_owned(), Value::String(description.clone()));
            }
            if let Some(default) = spec.default.as_ref() {
                value.insert("default".to_owned(), default.clone());
            }
            (name.clone(), Value::Object(value))
        })
        .collect::<Map<_, _>>();
    if !args.is_empty() {
        result.insert("args".to_owned(), Value::Object(args));
    }
    result.insert(
        "scope".to_owned(),
        Value::String(scope_name(meta.scope).to_owned()),
    );
    result.insert(
        "path".to_owned(),
        Value::String(meta.path.to_string_lossy().into_owned()),
    );
    Value::Object(result)
}

fn protocol_saved_get_meta(meta: &WorkflowDefinitionMeta) -> Value {
    let mut result = Map::new();
    result.insert(
        "description".to_owned(),
        Value::String(
            meta.description
                .clone()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| meta.name.clone()),
        ),
    );
    insert_non_empty(&mut result, "whenToUse", meta.when_to_use.as_deref(), 500);
    let args = meta
        .args
        .iter()
        .map(|(name, spec)| {
            let mut value = Map::new();
            value.insert(
                "type".to_owned(),
                Value::String(input_type_name(&spec.value_type).to_owned()),
            );
            if spec.required {
                value.insert("required".to_owned(), Value::Bool(true));
            }
            if let Some(description) = spec.description.as_ref() {
                value.insert("description".to_owned(), Value::String(description.clone()));
            }
            if let Some(default) = spec.default.as_ref() {
                value.insert("default".to_owned(), default.clone());
            }
            (name.clone(), Value::Object(value))
        })
        .collect::<Map<_, _>>();
    if !args.is_empty() {
        result.insert("args".to_owned(), Value::Object(args));
    }
    Value::Object(result)
}

fn input_type_name(input_type: &keencode_workflow::InputType) -> &'static str {
    match input_type {
        keencode_workflow::InputType::String => "string",
        keencode_workflow::InputType::Number | keencode_workflow::InputType::Integer => "number",
        keencode_workflow::InputType::Boolean => "boolean",
        keencode_workflow::InputType::Any
        | keencode_workflow::InputType::Json
        | keencode_workflow::InputType::Object
        | keencode_workflow::InputType::Array
        | keencode_workflow::InputType::Null => "json",
    }
}

fn scope_name(scope: WorkflowScope) -> &'static str {
    match scope {
        WorkflowScope::Project => "project",
        WorkflowScope::Global => "global",
    }
}

fn project_saved_list(result: WorkflowListResult) -> Value {
    json!({
        "workflows": result
            .workflows
            .iter()
            .map(protocol_definition_meta)
            .collect::<Vec<_>>(),
        "invalid": result.invalid.iter().map(|entry| json!({
            "path": entry.path.to_string_lossy(),
            "reason": entry.reason,
        })).collect::<Vec<_>>(),
        "dir": result.dir.to_string_lossy(),
    })
}

fn project_saved_get(result: WorkflowGetResult) -> Value {
    if !result.ok {
        let reason = result
            .reason
            .filter(|reason| {
                matches!(
                    reason.as_str(),
                    "invalid_name" | "not_found" | "parse_error" | "read_error"
                )
            })
            .unwrap_or_else(|| "read_error".to_owned());
        let mut failure = json!({ "ok": false, "reason": reason });
        if let Some(detail) = result.detail.filter(|detail| !detail.is_empty()) {
            failure["detail"] = Value::String(detail);
        }
        return failure;
    }
    let Some(meta) = result.meta.as_ref() else {
        return json!({ "ok": false, "reason": "read_error", "detail": "workflow metadata missing" });
    };
    let Some(definition) = result.definition.as_ref() else {
        return json!({ "ok": false, "reason": "read_error", "detail": "workflow definition missing" });
    };
    let script = match crate::workflows::canonical_json(definition)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
    {
        Some(script) => script,
        None => return json!({ "ok": false, "reason": "parse_error" }),
    };
    json!({
        "ok": true,
        "name": result.name.unwrap_or_else(|| meta.name.clone()),
        "path": result.path.map(|path| path.to_string_lossy().into_owned()).unwrap_or_default(),
        "scope": result.scope.map(scope_name).unwrap_or("project"),
        "meta": protocol_saved_get_meta(meta),
        "script": script,
    })
}

#[derive(Clone, Copy)]
enum DefinitionOperation {
    Get,
    Update,
    Delete,
    Move,
}

fn definition_failure(reason: &str, detail: Option<String>) -> Value {
    let mut result = json!({ "ok": false, "reason": reason });
    if let Some(detail) = detail.filter(|value| !value.is_empty()) {
        result["detail"] = Value::String(detail);
    }
    result
}

/// Store 的业务失败必须保持 source 的 union 结果；授权存储、协议和未知 I/O
/// 上下文错误继续作为 RPC rejection，避免把权限故障伪装成用户可恢复的名字错误。
fn definition_store_failure(
    error: WorkflowStoreError,
    operation: DefinitionOperation,
) -> Result<Value, WorkflowHostError> {
    let (reason, detail) = match error {
        WorkflowStoreError::InvalidDefinition(error) => match error {
            crate::workflows::WorkflowDefinitionError::InvalidName(name) => (
                "invalid_name",
                Some(format!("invalid workflow name: {name}")),
            ),
            error => ("parse_error", Some(error.to_string())),
        },
        WorkflowStoreError::NotFound(name) => {
            ("not_found", Some(format!("workflow not found: {name}")))
        }
        WorkflowStoreError::TargetExists(path)
            if matches!(operation, DefinitionOperation::Move) =>
        {
            ("target_exists", Some(path))
        }
        WorkflowStoreError::Json(error) if !matches!(operation, DefinitionOperation::Move) => {
            ("parse_error", Some(error.to_string()))
        }
        WorkflowStoreError::Io(error) => {
            let reason = if matches!(operation, DefinitionOperation::Move) {
                "write_error"
            } else {
                "read_error"
            };
            (reason, Some(error.to_string()))
        }
        WorkflowStoreError::InvalidTarget(detail)
            if matches!(operation, DefinitionOperation::Update) =>
        {
            ("parse_error", Some(detail))
        }
        error => return Err(WorkflowHostError::Store(error)),
    };
    Ok(definition_failure(reason, detail))
}

fn ensure_definition_scope(
    store: &WorkflowStore,
    scope: WorkflowScope,
) -> Result<(), WorkflowHostError> {
    if matches!(scope, WorkflowScope::Project) {
        let Some(path) = store.authorized_project_storage() else {
            return Err(WorkflowHostError::FrozenRun(
                "project workflow requires authorized project storage".to_owned(),
            ));
        };
        if path.as_os_str().is_empty() {
            return Err(WorkflowHostError::FrozenRun(
                "authorized project storage path is empty".to_owned(),
            ));
        }
    }
    Ok(())
}

/// 无会话定义管理 RPC。项目路径只取 `store` 的授权配置，输入里的 workspace/cwd/path 字段
/// 即使存在也不会影响落盘位置。
pub fn call_definitions(
    store: &WorkflowStore,
    method: &str,
    args: Value,
) -> Result<Value, WorkflowHostError> {
    match method {
        "listSavedWorkflows" | "list" | "listWorkflows" | "workflows.list" => {
            let input: DefinitionNameArgs = parse(args)?;
            ensure_definition_scope(store, input.scope)?;
            let result = store.list(input.scope, store.authorized_project_storage())?;
            if method == "listSavedWorkflows" {
                Ok(project_saved_list(result))
            } else {
                Ok(serde_json::to_value(result)
                    .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
            }
        }
        "getSavedWorkflow" | "get" | "getWorkflow" | "workflows.get" => {
            let input: DefinitionNameArgs = parse(args)?;
            let name = input
                .name
                .ok_or_else(|| WorkflowHostError::FrozenRun("missing name".to_owned()))?;
            ensure_definition_scope(store, input.scope)?;
            let result = match store.get(input.scope, store.authorized_project_storage(), &name) {
                Ok(result) => result,
                Err(error) => return definition_store_failure(error, DefinitionOperation::Get),
            };
            if method == "getSavedWorkflow" {
                Ok(project_saved_get(result))
            } else {
                Ok(serde_json::to_value(result)
                    .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
            }
        }
        "createWorkflow" | "save" | "saveWorkflow" | "workflows.save" => {
            let input: DefinitionArgs = parse(args)?;
            ensure_definition_scope(store, input.scope)?;
            let result = store.save(
                input.scope,
                store.authorized_project_storage(),
                &input.definition,
            )?;
            Ok(serde_json::to_value(result)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "updateSavedWorkflowMeta"
        | "updateMeta"
        | "updateWorkflowMeta"
        | "workflows.updateMeta" => {
            let input: DefinitionMetaArgs = parse(args)?;
            ensure_definition_scope(store, input.scope)?;
            let (description, when_to_use, args) = input
                .meta
                .map_or((input.description, input.when_to_use, input.args), |meta| {
                    (meta.description, meta.when_to_use, meta.args)
                });
            let result = match store.update_meta(
                input.scope,
                store.authorized_project_storage(),
                &input.name,
                description,
                when_to_use,
                args,
            ) {
                Ok(result) => result,
                Err(error) => return definition_store_failure(error, DefinitionOperation::Update),
            };
            if method == "updateSavedWorkflowMeta" {
                Ok(json!({ "ok": true, "path": result.path }))
            } else {
                Ok(serde_json::to_value(result)
                    .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
            }
        }
        "deleteSavedWorkflow" | "delete" | "deleteWorkflow" | "workflows.delete" => {
            let input: DefinitionNameArgs = parse(args)?;
            let name = input
                .name
                .ok_or_else(|| WorkflowHostError::FrozenRun("missing name".to_owned()))?;
            ensure_definition_scope(store, input.scope)?;
            let path = match store.delete(input.scope, store.authorized_project_storage(), &name) {
                Ok(path) => path,
                Err(error) => return definition_store_failure(error, DefinitionOperation::Delete),
            };
            if method == "deleteSavedWorkflow" {
                Ok(json!({ "ok": true, "path": path }))
            } else {
                Ok(json!({ "deleted": true, "path": path }))
            }
        }
        "moveSavedWorkflow" | "move" | "moveWorkflow" | "workflows.move" => {
            let input: MoveDefinitionArgs = parse(args)?;
            ensure_definition_scope(store, WorkflowScope::Project)?;
            let project_storage = store.authorized_project_storage().ok_or_else(|| {
                WorkflowHostError::FrozenRun("project storage is not authorized".to_owned())
            })?;
            let (from, to) = match store.move_global_to_project(project_storage, &input.name) {
                Ok(paths) => paths,
                Err(error) => return definition_store_failure(error, DefinitionOperation::Move),
            };
            Ok(json!({ "ok": true, "from": from, "to": to }))
        }
        _ => Err(WorkflowHostError::FrozenRun(format!(
            "unknown workflow definition method: {method}"
        ))),
    }
}

/// 根 RPC Handler 调用的工作流方法表。
pub async fn call(
    host: Arc<WorkflowHost>,
    method: &str,
    args: Value,
) -> Result<Value, WorkflowHostError> {
    if is_definition_method(method) {
        return Err(WorkflowHostError::FrozenRun(
            "definition methods require the authorized call_definitions route".to_owned(),
        ));
    }
    match method {
        "startSavedWorkflow" => {
            let input: SavedWorkflowStartArgs = parse(args)?;
            let result = host.start(WorkflowRunStartRequest {
                name: input.name,
                scope: input.scope,
                project_storage: input.project_storage,
                parent_session_id: input.parent_session_id,
                tool_call_id: input.tool_call_id,
                launch_input_id: input.launch_input_id,
                inputs: input.args,
                cwd: input.cwd,
                model_selection: input.model_selection,
                budgets: input.budgets,
            })?;
            Ok(serde_json::to_value(result)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "start" | "run" | "startWorkflow" | "workflows.start" => {
            let input: WorkflowRunStartRequest = parse(args)?;
            Ok(serde_json::to_value(host.start(input)?)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        // Session command `amendWorkflowRunSettings` reaches this private method after
        // ownership and schema checks.  It must derive the successor solely from the
        // predecessor Journal, otherwise a page could replace frozen definition/inputs.
        "amendSettings" => amend_settings(&host, args).await,
        "cancelBackgroundWork" | "cancel" | "cancelWorkflow" | "workflows.cancel" => {
            let run_id = args
                .get("runId")
                .and_then(Value::as_str)
                .ok_or_else(|| WorkflowHostError::FrozenRun("missing runId/workId".to_owned()))?;
            Ok(json!({ "cancelled": host.cancel(run_id).await? }))
        }
        "resumeWorkflowRun" | "resume" | "resumeWorkflow" | "workflows.resume" => {
            let run_id = args
                .get("runId")
                .and_then(Value::as_str)
                .ok_or_else(|| WorkflowHostError::FrozenRun("missing runId/workId".to_owned()))?;
            Ok(serde_json::to_value(host.resume(run_id).await?)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "resolveQuestion" | "resolveWorkflowQuestion" | "workflows.resolveQuestion" => {
            let input: QuestionArgs = parse(args)?;
            host.resolve_question(input.qid, input.answer).await?;
            Ok(json!({ "ok": true }))
        }
        "conversationWorkflowRuns" | "conversationWorkflowRunsV4" => {
            let input: RunListArgs = parse(args)?;
            let result = host.list_runs(
                input.parent_session_id.as_deref(),
                input.name.as_deref(),
                input.scope,
                input.limit,
            )?;
            Ok(project_v4_runs(&result.runs))
        }
        "listSavedWorkflowRuns" => {
            let input: RunListArgs = parse(args)?;
            let result = host.list_runs(
                input.parent_session_id.as_deref(),
                input.name.as_deref(),
                input.scope,
                input.limit,
            )?;
            project_saved_runs(result.runs, result.truncated)
        }
        "listRuns" | "listWorkflowRuns" | "workflows.listRuns" => {
            let input: RunListArgs = parse(args)?;
            Ok(serde_json::to_value(host.list_runs(
                input.parent_session_id.as_deref(),
                input.name.as_deref(),
                input.scope,
                input.limit,
            )?)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "runSummary" | "getWorkflowRun" | "workflows.getRun" => {
            let run_id = string_field(&args, "runId")?;
            Ok(serde_json::to_value(host.run_summary(&run_id)?)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "conversationWorkflowRunEvents" | "conversationWorkflowRunEventsV4" => {
            let input: RunEventsArgs = parse(args)?;
            let page = host.list_events(&input.run_id, input.after_sequence, input.limit)?;
            Ok(project_events(page))
        }
        "events" | "listWorkflowRunEvents" | "workflows.events" => {
            let input: RunEventsArgs = parse(args)?;
            Ok(serde_json::to_value(host.list_events(
                &input.run_id,
                input.after_sequence,
                input.limit,
            )?)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "progress" | "replayWorkflowRun" | "workflows.progress" => {
            let input: RunEventsArgs = parse(args)?;
            Ok(serde_json::to_value(host.progress_events(
                &input.run_id,
                input.after_sequence,
                input.limit,
            )?)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "conversationWorkflowRunGraph" | "conversationWorkflowRunGraphV4" => {
            let run_id = string_field(&args, "runId")?;
            Ok(project_graph(host.graph(&run_id)?))
        }
        "graph" | "getWorkflowRunGraph" | "workflows.graph" => {
            let run_id = string_field(&args, "runId")?;
            Ok(project_graph(host.graph(&run_id)?))
        }
        "conversationWorkflowRunWorkspace" | "conversationWorkflowRunWorkspaceV4" => {
            let run_id = string_field(&args, "runId")?;
            let result = host.workspace(&run_id)?.unwrap_or(WorkflowWorkspaceResult {
                nodes: Vec::new(),
                truncated: false,
            });
            Ok(project_workspace(result))
        }
        "workspace" | "workflowRunWorkspace" | "workflows.workspace" => {
            let run_id = string_field(&args, "runId")?;
            Ok(serde_json::to_value(host.workspace(&run_id)?)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "conversationWorkflowRunNodeResult" | "conversationWorkflowRunNodeResultV4" => {
            let input: NodeResultArgs = parse(args)?;
            let result = host
                .node_result(
                    &input.run_id,
                    &input.site_id,
                    input.ordinal,
                    input.max_bytes.min(V4_WORKSPACE_RESULT_MAX_BYTES),
                )?
                .ok_or_else(|| WorkflowHostError::NotFound(input.run_id.clone()))?;
            Ok(project_node_result(result))
        }
        "nodeResult" | "workflowRunNodeResult" | "workflows.nodeResult" => {
            let input: NodeResultArgs = parse(args)?;
            Ok(serde_json::to_value(host.node_result(
                &input.run_id,
                &input.site_id,
                input.ordinal,
                input.max_bytes,
            )?)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "conversationWorkflowRunArtifacts" | "conversationWorkflowRunArtifactsV4" => {
            let run_id = string_field(&args, "runId")?;
            Ok(json!({
                "artifacts": match host.artifacts(&run_id)? {
                    Some(artifacts) => project_artifacts(artifacts)?,
                    None => Value::Array(Vec::new()),
                }
            }))
        }
        "artifacts" | "workflowRunArtifacts" | "workflows.artifacts" => {
            let run_id = string_field(&args, "runId")?;
            Ok(serde_json::to_value(host.artifacts(&run_id)?)
                .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "conversationWorkflowRunArtifactData" | "conversationWorkflowRunArtifactDataV4" => {
            let input: ArtifactItemsArgs = parse(args)?;
            let page = host.artifact_items(
                &input.run_id,
                &input.artifact_id,
                input.after_sequence,
                input.limit,
            )?;
            Ok(project_artifact_page(page))
        }
        "artifactItems" | "workflowRunArtifactData" | "workflows.artifactItems" => {
            let input: ArtifactItemsArgs = parse(args)?;
            Ok(serde_json::to_value(host.artifact_items(
                &input.run_id,
                &input.artifact_id,
                input.after_sequence,
                input.limit,
            )?)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        "conversationWorkflowRunArtifactRead" | "conversationWorkflowRunArtifactReadV4" => {
            let input: ArtifactReadArgs = parse(args)?;
            let bytes = host
                .read_artifact(
                    &input.run_id,
                    &input.artifact_id,
                    input.version,
                    input.offset,
                    input.limit.min(512 * 1024),
                )?
                .ok_or_else(|| WorkflowHostError::NotFound(input.artifact_id.clone()))?;
            Ok(project_artifact_bytes(bytes))
        }
        "artifactRead" | "workflowRunArtifactRead" | "workflows.artifactRead" => {
            let input: ArtifactReadArgs = parse(args)?;
            Ok(serde_json::to_value(host.read_artifact(
                &input.run_id,
                &input.artifact_id,
                input.version,
                input.offset,
                input.limit,
            )?)
            .map_err(|error| WorkflowHostError::FrozenRun(error.to_string()))?)
        }
        _ => Err(WorkflowHostError::FrozenRun(format!(
            "unknown workflow method: {method}"
        ))),
    }
}

fn string_field(args: &Value, name: &str) -> Result<String, WorkflowHostError> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| WorkflowHostError::FrozenRun(format!("missing {name}")))
}

fn protocol_event(event: keencode_resources::WorkflowJournalEvent) -> Value {
    let payload = if event.payload.is_object() {
        event.payload
    } else {
        json!({ "value": event.payload })
    };
    json!({
        "sequence": event.sequence,
        "type": event.event_type,
        "payload": payload,
    })
}

fn project_graph(graph: WorkflowGraphResult) -> Value {
    let nodes = graph
        .nodes
        .into_iter()
        .map(|node| {
            json!({
                "id": node.id.chars().take(128).collect::<String>(),
                "kind": node.kind,
                "label": node.label.chars().take(256).collect::<String>(),
                "lane": node.lane.chars().take(128).collect::<String>(),
                "depth": node.depth,
            })
        })
        .collect::<Vec<_>>();
    let edges = graph
        .edges
        .into_iter()
        .map(|edge| {
            let mut projected = Map::new();
            projected.insert(
                "from".to_owned(),
                Value::String(edge.from.chars().take(128).collect()),
            );
            projected.insert(
                "to".to_owned(),
                Value::String(edge.to.chars().take(128).collect()),
            );
            if edge.back == Some(true) {
                projected.insert("back".to_owned(), Value::Bool(true));
            }
            Value::Object(projected)
        })
        .collect::<Vec<_>>();
    json!({
        "runId": graph.run_id,
        "nodes": nodes,
        "edges": edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn summary() -> WorkflowRunSummary {
        WorkflowRunSummary {
            run_id: "run-1".to_owned(),
            name: Some("demo".to_owned()),
            status: WorkflowRunStatus::Completed,
            stop_reason: None,
            created_at: 1,
            updated_at: 2,
            spent_tokens: 3,
            parent_session_id: Some("session-1".to_owned()),
            tool_call_id: Some("tool-1".to_owned()),
            args: json!({"count": 1}),
            cwd: Some(PathBuf::from("C:\\workspace")),
            canonical_hash: Some("internal-hash".to_owned()),
            artifacts: vec![json!({
                "id": "artifact-1",
                "kind": "file",
                "version": 1,
                "mediaType": "text/plain",
                "bytes": 12,
                "itemCount": 3,
                "primary": true
            })],
            resumable: true,
        }
    }

    #[test]
    fn mutation_aliases_are_not_public_workflow_methods() {
        for method in [
            "startDefinitionChange",
            "amend",
            "amendWorkflowRun",
            "workflows.amend",
            "amendSettings",
            "startSavedWorkflow",
            "start",
            "run",
            "startWorkflow",
            "workflows.start",
            "cancelBackgroundWork",
            "cancel",
            "cancelWorkflow",
            "workflows.cancel",
            "resumeWorkflowRun",
            "resume",
            "resumeWorkflow",
            "workflows.resume",
            "resolveQuestion",
            "resolveWorkflowQuestion",
            "workflows.resolveQuestion",
        ] {
            assert!(
                !is_workflow_method(method),
                "{method} 必须只能通过已授权的 Session command 进入"
            );
        }
        assert!(is_workflow_method("conversationWorkflowRunsV4"));
        assert!(is_workflow_method("conversationWorkflowRunGraphV4"));
    }

    #[test]
    fn saved_run_projection_drops_internal_fields_and_null_optionals() {
        let projected = project_saved_runs(vec![summary()], false).unwrap();
        let row = &projected["runs"][0];
        assert_eq!(row["runId"], "run-1");
        assert_eq!(row["artifacts"][0]["kind"], "file");
        assert!(row["artifacts"][0].get("bytes").is_none());
        assert!(row["artifacts"][0].get("itemCount").is_none());
        assert!(row["artifacts"][0].get("primary").is_none());
        assert!(row.get("canonicalHash").is_none());
        assert!(row.get("resumable").is_none());
        assert!(row.get("stopReason").is_none());
        assert!(row.get("null").is_none());
    }

    #[test]
    fn saved_get_meta_drops_list_only_fields() {
        let meta = WorkflowDefinitionMeta {
            name: "demo".to_owned(),
            description: Some("description".to_owned()),
            when_to_use: Some("when useful".to_owned()),
            args: std::collections::BTreeMap::new(),
            scope: WorkflowScope::Project,
            path: PathBuf::from("C:\\workspace\\workflows\\demo.json"),
            canonical_hash: "hash".to_owned(),
        };
        let projected = protocol_saved_get_meta(&meta);
        assert_eq!(projected["description"], "description");
        assert_eq!(projected["whenToUse"], "when useful");
        assert!(projected.get("name").is_none());
        assert!(projected.get("scope").is_none());
        assert!(projected.get("path").is_none());
    }

    #[test]
    fn saved_meta_projection_preserves_source_input_description_and_json_type() {
        let mut args = std::collections::BTreeMap::new();
        args.insert(
            "filter".to_owned(),
            keencode_workflow::InputSpec {
                value_type: keencode_workflow::InputType::Json,
                description: Some("自由格式筛选条件".to_owned()),
                required: true,
                default: Some(json!({"enabled": true})),
            },
        );
        let meta = WorkflowDefinitionMeta {
            name: "demo".to_owned(),
            description: Some("description".to_owned()),
            when_to_use: None,
            args,
            scope: WorkflowScope::Project,
            path: PathBuf::from("C:\\workspace\\workflows\\demo.json"),
            canonical_hash: "hash".to_owned(),
        };
        let projected = protocol_definition_meta(&meta);
        assert_eq!(projected["args"]["filter"]["type"], "json");
        assert_eq!(
            projected["args"]["filter"]["description"],
            "自由格式筛选条件"
        );
        assert_eq!(
            projected["args"]["filter"]["default"],
            json!({"enabled": true})
        );
    }

    #[test]
    fn definition_business_errors_use_source_unions_and_context_errors_reject() {
        let root = tempfile::tempdir().expect("global workflow root");
        let project = tempfile::tempdir().expect("project workflow root");
        let store = WorkflowStore::with_project_storage(root.path(), project.path().to_path_buf());

        let get_missing = call_definitions(
            &store,
            "getSavedWorkflow",
            json!({"scope": "global", "name": "missing"}),
        )
        .expect("missing get is a business result");
        assert_eq!(get_missing["ok"], false);
        assert_eq!(get_missing["reason"], "not_found");

        let invalid_name = call_definitions(
            &store,
            "deleteSavedWorkflow",
            json!({"scope": "global", "name": "../escape"}),
        )
        .expect("invalid name is a business result");
        assert_eq!(invalid_name["ok"], false);
        assert_eq!(invalid_name["reason"], "invalid_name");

        let update_missing = call_definitions(
            &store,
            "updateSavedWorkflowMeta",
            json!({
                "scope": "global",
                "name": "missing",
                "meta": {"description": "new description"}
            }),
        )
        .expect("missing update is a business result");
        assert_eq!(update_missing["reason"], "not_found");

        let malformed_path = root.path().join("workflows").join("broken.json");
        std::fs::create_dir_all(malformed_path.parent().expect("workflow directory"))
            .expect("workflow directory");
        std::fs::write(&malformed_path, b"{not-json").expect("malformed workflow definition");
        let update_malformed = call_definitions(
            &store,
            "updateSavedWorkflowMeta",
            json!({
                "scope": "global",
                "name": "broken",
                "meta": {"description": "new description"}
            }),
        )
        .expect("malformed update is a business result");
        assert_eq!(update_malformed["reason"], "parse_error");

        let delete_missing = call_definitions(
            &store,
            "deleteSavedWorkflow",
            json!({"scope": "global", "name": "missing"}),
        )
        .expect("missing delete is a business result");
        assert_eq!(delete_missing["reason"], "not_found");

        let move_missing =
            call_definitions(&store, "moveSavedWorkflow", json!({"name": "missing"}))
                .expect("missing move is a business result");
        assert_eq!(move_missing["reason"], "not_found");

        let definition = WorkflowDefinition {
            version: crate::workflows::WORKFLOW_VERSION,
            meta: keencode_workflow::WorkflowMeta {
                id: None,
                name: "shared".to_owned(),
                description: Some("shared".to_owned()),
                when_to_use: None,
                tags: Vec::new(),
            },
            inputs: std::collections::BTreeMap::new(),
            body: Vec::new(),
        };
        store
            .save(WorkflowScope::Global, None, &definition)
            .expect("global definition");
        store
            .save(WorkflowScope::Project, None, &definition)
            .expect("project definition");
        let move_conflict =
            call_definitions(&store, "moveSavedWorkflow", json!({"name": "shared"}))
                .expect("target conflict is a business result");
        assert_eq!(move_conflict["reason"], "target_exists");

        let unauthorized_store = WorkflowStore::new(root.path().to_path_buf());
        let unauthorized = call_definitions(
            &unauthorized_store,
            "getSavedWorkflow",
            json!({"scope": "project", "name": "missing"}),
        );
        assert!(matches!(
            unauthorized,
            Err(WorkflowHostError::FrozenRun(message))
                if message.contains("authorized project storage")
        ));
    }

    #[test]
    fn artifact_projection_rejects_unknown_kind_and_out_of_range_versions() {
        let mut artifact = WorkflowArtifact {
            id: "artifact-1".to_owned(),
            kind: "file".to_owned(),
            title: None,
            description: None,
            content_type: None,
            source_path: None,
            spec: None,
            version: 1,
            versions: vec![json!({"version": 1, "publishedAt": 42})],
            item_count: 0,
            primary: false,
        };
        artifact.kind = "unknown".to_owned();
        assert!(project_artifacts(vec![artifact.clone()]).is_err());
        artifact.kind = "file".to_owned();
        artifact.version = 17;
        assert!(project_artifacts(vec![artifact.clone()]).is_err());
        artifact.version = 1;
        artifact.versions = vec![json!({"version": 17})];
        assert!(project_artifacts(vec![artifact]).is_err());

        let missing_published_at = WorkflowArtifact {
            id: "artifact-1".to_owned(),
            kind: "file".to_owned(),
            title: None,
            description: None,
            content_type: None,
            source_path: None,
            spec: None,
            version: 1,
            versions: vec![json!({"version": 1})],
            item_count: 0,
            primary: false,
        };
        assert!(project_artifacts(vec![missing_published_at]).is_err());

        let mut run = summary();
        run.artifacts[0]["kind"] = Value::String("unknown".to_owned());
        assert!(project_saved_runs(vec![run.clone()], false).is_err());
        run.artifacts[0]["kind"] = Value::String("file".to_owned());
        run.artifacts[0]["version"] = json!(17);
        assert!(project_saved_runs(vec![run], false).is_err());
    }

    #[test]
    fn v4_run_projection_keeps_only_conversation_summary_fields() {
        let projected = project_v4_runs(&[summary()]);
        let row = &projected["runs"][0];
        assert_eq!(row["label"], "demo");
        assert_eq!(row["resumable"], true);
        assert!(row.get("canonicalHash").is_none());
        assert!(row.get("args").is_none());
    }

    #[test]
    fn workspace_projection_uses_strict_optional_fields() {
        let projected = project_workspace(WorkflowWorkspaceResult {
            nodes: vec![WorkflowWorkspaceNode {
                site_id: "site".to_owned(),
                ordinal: 0,
                kind: "world-run".to_owned(),
                op: None,
                args: None,
                status: "completed".to_owned(),
                error: Some(json!({"code": "failed", "message": "bad"})),
                summary: None,
                created_at: 1,
                updated_at: 2,
            }],
            truncated: false,
        });
        let row = &projected["nodes"][0];
        assert_eq!(row["kind"], "world-run");
        assert_eq!(row["error"]["code"], "failed");
        assert!(row.get("op").is_none());
        assert!(row.get("args").is_none());
        assert!(row.get("summary").is_none());
    }

    #[test]
    fn graph_projection_does_not_expose_definition_or_internal_fields() {
        let projected = project_graph(WorkflowGraphResult {
            run_id: "run-1".to_owned(),
            definition: WorkflowDefinition {
                version: crate::workflows::WORKFLOW_VERSION,
                meta: keencode_workflow::WorkflowMeta::default(),
                inputs: std::collections::BTreeMap::new(),
                body: Vec::new(),
            },
            nodes: vec![crate::workflows::WorkflowGraphNode {
                id: "ask".to_owned(),
                kind: crate::workflows::WorkflowGraphNodeKind::Ask,
                label: "Ask".to_owned(),
                lane: "agent".to_owned(),
                depth: 0,
            }],
            edges: vec![crate::workflows::WorkflowGraphEdge {
                from: "ask".to_owned(),
                to: "read".to_owned(),
                back: None,
            }],
        });
        assert_eq!(projected["runId"], "run-1");
        assert_eq!(projected["nodes"][0]["kind"], "ask");
        assert!(projected.get("definition").is_none());
    }

    /// 导出完整的 V4 查询投影，供 source Zod 在 Node 侧做 strict contract 校验。
    /// 默认不写文件；设置 `KEENCODE_RPC_CONTRACT_FIXTURES` 后才落盘，避免单测污染仓库。
    #[test]
    fn export_workflow_source_contract_fixtures_when_requested() {
        let Ok(directory) = std::env::var("KEENCODE_RPC_CONTRACT_FIXTURES") else {
            return;
        };
        let directory = Path::new(&directory);
        std::fs::create_dir_all(directory).expect("应创建工作流 RPC 契约 fixture 目录");

        let artifact = WorkflowArtifact {
            id: "artifact-contract".to_owned(),
            kind: "markdown".to_owned(),
            title: Some("Contract report".to_owned()),
            description: Some("A bounded source contract artifact".to_owned()),
            content_type: Some("text/markdown".to_owned()),
            source_path: None,
            spec: None,
            version: 1,
            versions: vec![json!({
                "version": 1,
                "title": "Contract report",
                "contentType": "text/markdown",
                "bytes": 5,
                "publishedAt": 42,
                "primary": true
            })],
            item_count: 1,
            primary: true,
        };
        let artifacts = json!({
            "artifacts": project_artifacts(vec![artifact.clone()]).unwrap()
        });
        let artifact_data = project_artifact_page(WorkflowArtifactPage {
            items: vec![json!({
                "sequence": 7,
                "siteId": "report#1",
                "ordinal": 0,
                "item": {"value": 1}
            })],
            has_more: false,
        });
        let artifact_read = project_artifact_bytes(WorkflowArtifactBytes {
            data_base64: "aGVsbG8=".to_owned(),
            media_type: "text/markdown".to_owned(),
            total_bytes: 5,
            next_offset: None,
        });
        let workspace = project_workspace(WorkflowWorkspaceResult {
            nodes: vec![WorkflowWorkspaceNode {
                site_id: "read".to_owned(),
                ordinal: 0,
                kind: "world-read".to_owned(),
                op: Some("read-file".to_owned()),
                args: Some(vec![json!("README.md")]),
                status: "completed".to_owned(),
                error: None,
                summary: Some(json!({"resultBytes": 2, "resultCount": 1})),
                created_at: 1,
                updated_at: 2,
            }],
            truncated: false,
        });
        let node_result = project_node_result(WorkflowNodeResult {
            status: "completed".to_owned(),
            result: Some(json!({"text": "ok"})),
            error: None,
            truncated: false,
            total_bytes: 13,
        });
        let graph = project_graph(WorkflowGraphResult {
            run_id: "run-contract".to_owned(),
            definition: WorkflowDefinition {
                version: crate::workflows::WORKFLOW_VERSION,
                meta: keencode_workflow::WorkflowMeta::default(),
                inputs: std::collections::BTreeMap::new(),
                body: Vec::new(),
            },
            nodes: vec![
                crate::workflows::WorkflowGraphNode {
                    id: "ask".to_owned(),
                    kind: crate::workflows::WorkflowGraphNodeKind::Ask,
                    label: "Ask".to_owned(),
                    lane: "agent:reader".to_owned(),
                    depth: 0,
                },
                crate::workflows::WorkflowGraphNode {
                    id: "read".to_owned(),
                    kind: crate::workflows::WorkflowGraphNodeKind::WorldRead,
                    label: "Read".to_owned(),
                    lane: "workspace".to_owned(),
                    depth: 1,
                },
            ],
            edges: vec![crate::workflows::WorkflowGraphEdge {
                from: "ask".to_owned(),
                to: "read".to_owned(),
                back: None,
            }],
        });
        let run_timeline = project_events(WorkflowEventPage {
            events: vec![keencode_resources::WorkflowJournalEvent {
                run_id: "run-contract".to_owned(),
                tool_call_id: "tool-contract".to_owned(),
                sequence: 1,
                event_type: "run-started".to_owned(),
                payload: json!({"status": "running"}),
                artifacts: Vec::new(),
                actor_session_id: None,
                launch_input_id: None,
            }],
            has_more: false,
        });
        let runs = project_v4_runs(&[summary()]);

        for (name, value) in [
            ("workflow_artifacts.json", artifacts),
            ("workflow_artifact_data.json", artifact_data),
            ("workflow_artifact_read.json", artifact_read),
            ("workflow_workspace.json", workspace),
            ("workflow_node_result.json", node_result),
            ("workflow_graph.json", graph),
            ("workflow_run_timeline.json", run_timeline),
            ("workflow_runs.json", runs),
        ] {
            let bytes = serde_json::to_vec_pretty(&value).expect("工作流 fixture 应为 JSON");
            std::fs::write(directory.join(name), bytes).expect("应写入工作流 RPC 契约 fixture");
        }
    }

    #[test]
    fn amend_model_accepts_only_the_authorized_provider_snapshot() {
        let selection = json!({
            "provider": {
                "providerId": "native-live-deepseek",
                "model": "deepseek-v4.1-flash",
                "reasoningEffort": null
            },
            "planEnabled": false
        });
        validate_requested_model(&selection, "native-live-deepseek/deepseek-v4.1-flash")
            .expect("同一冻结 provider/model 应可修订");
        assert!(matches!(
            validate_requested_model(&selection, "other-provider/model"),
            Err(WorkflowHostError::SettingsRejected { reason, .. }) if reason == "model_unavailable"
        ));
        assert!(matches!(
            validate_requested_model(&selection, "native-live-deepseek/deepseek-v4.1-flash$high"),
            Err(WorkflowHostError::SettingsRejected { reason, .. }) if reason == "model_unavailable"
        ));
    }

    #[test]
    fn amend_budgets_preserves_frozen_limits_and_uses_executor_field_name() {
        let existing = json!({"max_nodes": 8, "max_concurrency": 4});
        let amended = amend_budgets(Some(&existing), Some(Some(2)))
            .expect("预算修订应成功")
            .expect("预算对象应保留");
        assert_eq!(amended["max_nodes"], 8);
        assert_eq!(amended["max_concurrency"], 2);
        assert!(amended.get("maxConcurrency").is_none());

        let reset = amend_budgets(Some(&amended), Some(None))
            .expect("清除并发上限应成功")
            .expect("其他预算仍应保留");
        assert_eq!(reset["max_nodes"], 8);
        assert!(reset.get("max_concurrency").is_none());
    }

    #[test]
    fn amendment_restart_rejects_agent_and_write_side_effects() {
        let mut definition = WorkflowDefinition {
            version: crate::workflows::WORKFLOW_VERSION,
            meta: keencode_workflow::WorkflowMeta {
                id: None,
                name: "read-only".to_owned(),
                description: None,
                when_to_use: None,
                tags: Vec::new(),
            },
            inputs: std::collections::BTreeMap::new(),
            body: vec![Node::Tool(keencode_workflow::ToolNode {
                node_id: "read".to_owned(),
                name: "Read".to_owned(),
                input: keencode_workflow::ValueExpr::Literal { value: json!({}) },
                config: Value::Null,
                output_type: None,
                effect: keencode_workflow::EffectClass::ReadOnly,
            })],
        };
        assert!(amendment_restart_is_read_only(&definition));
        let read_only_successor_budget =
            amend_budgets(Some(&json!({ "max_concurrency": 4 })), Some(Some(1)))
                .expect("只读 successor 的并发预算应可修订")
                .expect("只读 successor 应保留冻结预算");
        assert_eq!(read_only_successor_budget["max_concurrency"], 1);
        if let Node::Tool(tool) = &mut definition.body[0] {
            tool.name = "Write".to_owned();
        }
        assert!(!amendment_restart_is_read_only(&definition));

        let agent_definition = WorkflowDefinition {
            version: crate::workflows::WORKFLOW_VERSION,
            meta: keencode_workflow::WorkflowMeta {
                id: None,
                name: "agent-side-effect".to_owned(),
                description: None,
                when_to_use: None,
                tags: Vec::new(),
            },
            inputs: std::collections::BTreeMap::new(),
            body: vec![Node::Agent(keencode_workflow::AgentNode {
                node_id: "agent".to_owned(),
                name: "planner".to_owned(),
                input: keencode_workflow::ValueExpr::Literal { value: json!({}) },
                config: Value::Null,
                output_type: None,
                effect: keencode_workflow::EffectClass::ReadOnly,
            })],
        };
        assert!(!amendment_restart_is_read_only(&agent_definition));
    }

    #[test]
    fn requested_model_parser_keeps_empty_reasoning_invalid() {
        assert_eq!(
            requested_model_parts("provider/model$high"),
            Some(("provider", "model", Some("high")))
        );
        assert!(requested_model_parts("provider/model$").is_none());
        assert!(requested_model_parts("provider-only").is_none());
    }

    #[test]
    fn workflow_rpc_inputs_require_camel_case_source_fields() {
        assert!(
            parse::<RunEventsArgs>(json!({
                "run_id": "run-1"
            }))
            .is_err()
        );
        // Host 清理后只剩 Source 字段；严格 DTO 必须接受清理结果并拒绝仍带
        // workspaceIdentity 的宿主 envelope，防止查询字段被静默吞掉或绕过校验。
        assert!(
            parse::<RunEventsArgs>(json!({
                "runId": "run-1"
            }))
            .is_ok()
        );
        assert!(
            parse::<RunEventsArgs>(json!({
                "runId": "run-1",
                "workspaceIdentity": "workspace-key"
            }))
            .is_err()
        );
        assert!(
            parse::<NodeResultArgs>(json!({
                "runId": "run-1",
                "site_id": "site-1",
                "ordinal": 0
            }))
            .is_err()
        );
        assert!(
            parse::<QuestionArgs>(json!({
                "questionId": "question-1",
                "answer": "yes"
            }))
            .is_ok()
        );
        assert!(
            parse::<QuestionArgs>(json!({
                "question_id": "question-1",
                "answer": "yes"
            }))
            .is_err()
        );
        assert!(
            parse::<WorkflowRunStartRequest>(json!({
                "name": "workflow",
                "scope": "project",
                "parent_session_id": "session-1",
                "cwd": "C:\\workspace",
                "inputs": {}
            }))
            .is_err()
        );
        assert!(
            parse::<WorkflowRunStartRequest>(json!({
                "name": "workflow",
                "scope": "project",
                "parentSessionId": "session-1",
                "cwd": "C:\\workspace",
                "inputs": {}
            }))
            .is_ok()
        );
        let amend = json!({
            "runId": "run-1",
            "parentSessionId": "session-1",
            "cwd": "C:\\workspace",
            "subagentModel": null,
            "maxConcurrency": 2,
            "modelSelection": {},
            "toolCallId": "tool-1",
            "launchInputId": "launch-1"
        });
        assert!(serde_json::from_value::<AmendSettingsArgs>(amend.clone()).is_ok());
        let mut snake_alias = amend;
        snake_alias["run_id"] = json!("run-1");
        assert!(serde_json::from_value::<AmendSettingsArgs>(snake_alias).is_err());
    }
}
