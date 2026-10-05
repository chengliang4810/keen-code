//! Runtime/Journal 到 ZCode V4 的只读投影。
//!
//! 投影函数只读取 `AgentRuntime` 与 `RuntimeSession`，不建立前端状态缓存。行的
//! `rowId/entityId` 由 Journal 中的稳定 message identity 派生，编辑、重连和进程
//! 恢复不会依赖 localStorage 别名。

use super::protocol::{
    DEFAULT_ROWS_WINDOW, MAX_ROWS_RANGE, SNAPSHOT_PROTOCOL_VERSION, now_epoch_ms,
};
use crate::agent_runtime::AgentRuntime;
use crate::elicitation::{PendingElicitationAutoResolution, PendingElicitationView};
use crate::frontend_rpc::attachments;
use crate::permissions::{PendingPermissionView, PermissionMode};
use crate::session_commands::open_authorized_session;
use crate::workflows::{RuntimeWorkflowJournal, WorkflowJournalPort};
use keencode_acp::ConnectionId;
use keencode_agent::{GoalController, GoalStatus as AgentGoalStatus};
use keencode_resources::{
    AgentId, AssistantFeedback, AssistantFeedbackRecord, FollowupMode, ROOT_AGENT_ID,
    SessionInputDelivery, SessionInputDispatch, SessionInputKind, SessionState, SubAgentStatus,
    TranscriptRecord, TurnStatus, WorkflowJournalEvent,
};
use keencode_runtime::{PersistentAgentState, RuntimeModelRetryScheduled, RuntimeSession};
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, fs, path::Path};

/// 每个持久消息预留一个有界的内容块区间。rowId 来自完整 Transcript 的事件顺序，
/// 不依赖消息文本或前端生成的 alias；块区间只用于让同一消息内的 reasoning/tool 行
/// 也保持单调排序。
const ROW_ID_PART_STRIDE: u64 = 4_096;

/// 文件变更与附件读取共用的 CAS 失败分类；调用方只能把真实快照中的目标行
/// 作为实体继续处理，不能用一个存在的 `entityId` 搭配任意 `rowId` 绕过绑定。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConversationQueryError {
    StaleRevision,
    StaleLogEpoch,
    TargetNotFound,
}

/// 普通 core subagent 没有独立 Runtime Session；此作用域只是在父 Journal 上
/// 建立一个严格校验的只读视图。`requested_session_id` 保留给协议和 row 路由，
/// `parent_session_id` 才是实际读取和事件订阅的 Runtime 身份。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConversationScope {
    pub(crate) requested_session_id: String,
    pub(crate) parent_session_id: String,
    pub(crate) agent_id: Option<AgentId>,
}

/// 生成不会与真实 Session ID 混淆的普通 core subagent view ID。
pub(crate) fn virtual_agent_view_id(parent_session_id: &str, agent_id: &AgentId) -> String {
    format!("agent:{parent_session_id}:{}", agent_id.as_str())
}

/// 解析并严格限制 virtual-agent ID 的形状。以 `agent:` 开头但不符合完整三段式
/// 的值必须报错，不能降级成普通 Session ID 继续查找。
pub(crate) fn parse_virtual_agent_view_id(
    value: &str,
) -> Result<Option<(String, AgentId)>, String> {
    if !value.starts_with("agent:") {
        return Ok(None);
    }
    let parts = value.split(':').collect::<Vec<_>>();
    if parts.len() != 3 || parts[1].is_empty() || parts[2].is_empty() {
        return Err("virtual agent sessionId 必须为 agent:<parentSessionId>:<agentId>".to_owned());
    }
    let agent_id = AgentId::new(parts[2].to_owned())
        .map_err(|error| format!("virtual agent agentId 无效：{error}"))?;
    if virtual_agent_view_id(parts[1], &agent_id) != value {
        return Err("virtual agent sessionId 不是规范的父会话视图标识".to_owned());
    }
    Ok(Some((parts[1].to_owned(), agent_id)))
}

/// 解析 conversation topic 的 Runtime 父会话与可选子 Agent。子 Agent 只能是
/// Journal 中已经注册、且 parent_agent_id 固定为 root 的一层 Agent；这里不会
/// 创建 Session、读取任意跨 Session ID 或接受前端伪造的 agent 路径。
pub(crate) fn resolve_conversation_scope(
    runtime: &AgentRuntime,
    app: &tauri::AppHandle,
    requested_session_id: &str,
    workspace_path: &str,
) -> Result<ConversationScope, String> {
    let Some((parent_session_id, agent_id)) = parse_virtual_agent_view_id(requested_session_id)?
    else {
        validate_workspace(runtime, requested_session_id, workspace_path)?;
        return Ok(ConversationScope {
            requested_session_id: requested_session_id.to_owned(),
            parent_session_id: requested_session_id.to_owned(),
            agent_id: None,
        });
    };
    validate_workspace(runtime, &parent_session_id, workspace_path)?;
    let parent = open_authorized_session(runtime, app, &parent_session_id)?;
    let state = parent.snapshot().map_err(|error| error.to_string())?.state;
    let Some(agent) = state.sub_agents.get(&agent_id) else {
        return Err("virtual agent 不属于请求的父 Session".to_owned());
    };
    if agent.parent_agent_id.as_str() != ROOT_AGENT_ID {
        return Err("virtual agent 只能读取 root 的一层子 Agent".to_owned());
    }
    Ok(ConversationScope {
        requested_session_id: requested_session_id.to_owned(),
        parent_session_id,
        agent_id: Some(agent_id),
    })
}

pub(crate) fn is_virtual_agent_view_id(value: &str) -> bool {
    value.starts_with("agent:")
}

fn serialized_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => value.to_string().trim_matches('"').to_owned(),
    }
}

fn status_string(value: &Value) -> String {
    serialized_string(value).to_ascii_lowercase()
}

fn text_from_content(value: &Value) -> String {
    let Some(parts) = value.as_array() else {
        return String::new();
    };
    let mut result = String::new();
    for part in parts {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            result.push_str(text);
        } else if let Some(content) = part.get("content")
            && let Some(text) = content.as_str()
        {
            result.push_str(text);
        }
    }
    result
}

fn role_name(message: &Value) -> String {
    message
        .get("role")
        .map(status_string)
        .unwrap_or_else(|| "user".to_owned())
}

/// SessionMessage 使用 camelCase 序列化，但其 content 内的 MessagePart 明确使用
/// snake_case。投影边界必须按实际资源记录读取这两种层级，不能把缺字段误判为
/// orphan tool result，否则工具名、参数与终态都会退化成 unknown/running。
fn json_field<'a>(value: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    value.get(camel).or_else(|| value.get(snake))
}

fn json_string_field<'a>(value: &'a Value, camel: &str, snake: &str) -> Option<&'a str> {
    json_field(value, camel, snake).and_then(Value::as_str)
}

fn attachment_mime(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mov") => "video/quicktime",
        Some("pdf") => "application/pdf",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("js" | "mjs" | "cjs") => "application/javascript",
        Some("toml") => "application/toml",
        Some("yaml" | "yml") => "application/yaml",
        Some("csv") => "text/csv",
        Some("md" | "markdown") => "text/markdown",
        Some("txt" | "log") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// 从权威用户消息的已授权文件引用恢复 V4 行附件元数据；plugin URI 是普通
/// 资源选择，不属于 composer AttachmentRef，不能伪装成可预览文件暴露给 UI。
fn message_attachments(message: &Value) -> Option<Value> {
    let references = message.get("references")?.as_array()?;
    let attachments = references
        .iter()
        .filter_map(|reference| {
            let path = reference.get("path").and_then(Value::as_str)?;
            if path.is_empty() || path.contains("://") {
                return None;
            }
            let file_name = reference
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .unwrap_or(path);
            let bytes = fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            Some(json!({
                "ref": path,
                "fileName": file_name,
                "mime": attachment_mime(path),
                "bytes": bytes,
            }))
        })
        .collect::<Vec<_>>();
    (!attachments.is_empty()).then_some(Value::Array(attachments))
}

/// 为完整 Transcript 中的每条消息分配稳定、单调且永不复用的 rowId 基址。
///
/// `RuntimeSession::transcript()` 是当前有效消息视图，压缩或分支后可能丢掉早期
/// 位置；这里刻意遍历 `SessionState::transcript` 的完整提交顺序，避免重新编号造成
/// rows/range 的游标回退。Compaction 本身不生成行，所以只消耗真实消息的位置。
fn transcript_row_bases(state: &SessionState) -> BTreeMap<String, u64> {
    let mut bases = BTreeMap::new();
    let mut message_ordinal = 0_u64;
    let mut visit = |message: &keencode_resources::SessionMessage| {
        message_ordinal = message_ordinal.saturating_add(1);
        bases.entry(message.message_id.clone()).or_insert_with(|| {
            message_ordinal
                .saturating_mul(ROW_ID_PART_STRIDE)
                .saturating_sub(ROW_ID_PART_STRIDE)
                .saturating_add(1)
        });
    };
    for record in &state.transcript {
        match record {
            TranscriptRecord::MessageAdded(message) => visit(message),
            TranscriptRecord::SegmentCommitted(segment) => {
                for message in &segment.messages {
                    visit(message);
                }
            }
            TranscriptRecord::CompactionApplied(_) => {}
        }
    }
    bases
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 计算 conversation 订阅和文件查询使用的同一 log epoch。
pub(crate) fn conversation_log_epoch(session_id: &str, workspace_path: &str) -> String {
    format!(
        "conversation-{:016x}",
        stable_hash(&format!("{workspace_path}\0{session_id}"))
    )
}

/// 校验文件/附件查询的 revision、epoch 和完整目标行身份。
///
/// `rows_for_transcript` 是前端看到的唯一 row 投影出口；直接复用它可以同时
/// 校验消息行、工具行、reasoning 行的 `rowId/entityId` 配对，避免只检查实体
/// 标识而接受伪造的行号。
pub(crate) fn validate_conversation_query_target(
    state: &SessionState,
    session_id: &str,
    base_revision: u64,
    base_log_epoch: &str,
    target_row_id: u64,
    target_entity_id: &str,
) -> Result<String, ConversationQueryError> {
    if state.transcript_revision != base_revision {
        return Err(ConversationQueryError::StaleRevision);
    }
    if conversation_log_epoch(session_id, &state.project_root) != base_log_epoch {
        return Err(ConversationQueryError::StaleLogEpoch);
    }
    conversation_row_turn(state, target_row_id, target_entity_id)
        .ok_or(ConversationQueryError::TargetNotFound)
}

/// 在不带 CAS 字段的附件读取请求中，仅验证目标行的真实 row/entity 配对。
pub(crate) fn conversation_row_turn(
    state: &SessionState,
    target_row_id: u64,
    target_entity_id: &str,
) -> Option<String> {
    conversation_row_value(state, target_row_id, target_entity_id).and_then(|row| {
        row.get("turnId")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

/// 返回当前唯一权威行投影，用于同时校验 rowId/entityId 和行种类。
fn conversation_row_value(
    state: &SessionState,
    target_row_id: u64,
    target_entity_id: &str,
) -> Option<Value> {
    let transcript = state
        .raw_transcript_messages()
        .into_iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let rows = rows_for_transcript(&transcript, state.created_at_unix_ms, state);
    rows.iter()
        .find(|row| {
            row.get("rowId").and_then(Value::as_u64) == Some(target_row_id)
                && row.get("entityId").and_then(Value::as_str) == Some(target_entity_id)
        })
        .cloned()
}

/// 反馈只能写入 assistantText 行，不能把 user/tool/reasoning 行伪装成 Assistant 回复。
pub(crate) fn conversation_row_is_assistant(
    state: &SessionState,
    target_row_id: u64,
    target_entity_id: &str,
) -> bool {
    conversation_row_value(state, target_row_id, target_entity_id)
        .and_then(|row| {
            row.get("kind")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .as_deref()
        == Some("assistantText")
}

fn row_common(
    row_id: u64,
    turn_id: &str,
    entity_id: &str,
    created_at: u64,
    created_at_seq: u64,
) -> Value {
    json!({
        "rowId": row_id,
        "turnId": turn_id,
        "entityId": entity_id,
        "createdAt": created_at.saturating_add(created_at_seq),
        "createdAtSeq": created_at_seq,
        "visibility": "visible",
    })
}

fn lifecycle_values(state: &Value) -> BTreeMap<String, Value> {
    let mut values = BTreeMap::new();
    let Some(tools) = state.get("tools").and_then(Value::as_object) else {
        return values;
    };
    for lifecycle in tools.values() {
        let Some(request) = lifecycle.get("request") else {
            continue;
        };
        let Some(call_id) = json_string_field(request, "modelToolCallId", "model_tool_call_id")
        else {
            continue;
        };
        if !call_id.is_empty() {
            values.insert(call_id.to_owned(), lifecycle.clone());
        }
    }
    values
}

fn tool_result_values(transcript: &[Value]) -> BTreeMap<String, Value> {
    let mut values = BTreeMap::new();
    for message in transcript {
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(call_id) = json_string_field(part, "toolCallId", "tool_call_id") else {
                continue;
            };
            if !call_id.is_empty() {
                values.insert(call_id.to_owned(), part.clone());
            }
        }
    }
    values
}

fn tool_call_ids(transcript: &[Value]) -> BTreeMap<String, ()> {
    let mut values = BTreeMap::new();
    for message in transcript {
        let Some(parts) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("tool_call") {
                continue;
            }
            if let Some(call_id) = json_string_field(part, "toolCallId", "tool_call_id")
                .filter(|value| !value.is_empty())
            {
                values.insert(call_id.to_owned(), ());
            }
        }
    }
    values
}

fn tool_status(lifecycle: Option<&Value>, result: Option<&Value>) -> &'static str {
    if let Some(status) = lifecycle
        .and_then(|value| value.pointer("/outcome/status"))
        .and_then(Value::as_str)
    {
        return match status {
            "succeeded" => "success",
            "failed" | "side_effect_unknown" => "error",
            "cancelled" => "cancelled",
            _ => "error",
        };
    }
    if let Some(is_error) = result
        .and_then(|value| json_field(value, "isError", "is_error"))
        .and_then(Value::as_bool)
    {
        // Transcript 中已经写入 tool_result 就是终态事实；即使冷恢复时
        // ToolLifecycle 尚未回填，也不能把它继续显示成“执行中”。
        return if is_error { "error" } else { "success" };
    }
    if lifecycle
        .and_then(|value| value.get("executionStarted"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        "running"
    } else if lifecycle.is_some() {
        // ToolLifecycle 没有把“等待准入”和“尚未越过执行起点”拆成两个终态；
        // 没有可绑定的 pendingInteractionId 时显示为运行中，避免制造不可操作的
        // pendingApproval 卡片。
        "running"
    } else {
        // 已落盘的 ToolCall 可能在 Runtime 崩溃前尚未形成 ToolLifecycle；不能把
        // “没有结果”投影成成功，否则 UI 会制造一条假的终态事实。
        "running"
    }
}

fn tool_output_text(value: &Value) -> String {
    value
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// 工具行字段来自同一条 transcript part；集中传递可避免长参数列表的错位。
struct ToolRowInput<'a> {
    message: &'a Value,
    part_index: usize,
    row_id: u64,
    created_at: u64,
    call_id: &'a str,
    tool_name: &'a str,
    arguments: Value,
    lifecycle: Option<&'a Value>,
    result: Option<&'a Value>,
}

/// 将已成功的 Rust CreateWorkflow 定义图挂到工具行的 Source display 通道。
///
/// Source 详情页按发起 `toolCallId` 查找 `row.display.causalityGraph`；独立的
/// `conversationWorkflowRunGraphV4` 查询不参与这条 join。图的节点、边和标签只能来自
/// 已校验的 Rust WorkflowDefinition，超出 Source display 上界的尾部标记为 truncated，
/// 不用前端临时状态或模型输出补图。
fn create_workflow_display(tool_name: &str, arguments: &Value) -> Option<Value> {
    if !matches!(tool_name, "CreateWorkflow" | "createWorkflow") {
        return None;
    }
    let definition_value = arguments.get("definition")?.clone();
    let definition =
        serde_json::from_value::<crate::workflows::WorkflowDefinition>(definition_value).ok()?;
    crate::workflows::validate_definition(&definition).ok()?;
    let (nodes, edges) = crate::workflows::project_graph_for_display(&definition);

    // Source display 的 strict schema 比 Rust 引擎允许的节点预算更小；保留稳定的
    // 前缀，并把省略的节点/车道通过 truncated 明确告诉 UI。
    let mut selected_nodes = Vec::new();
    let mut lanes = BTreeMap::<String, String>::new();
    let mut truncated = false;
    for node in nodes {
        let id = node.id;
        let source_lane = node.lane;
        if id.is_empty()
            || id.chars().count() > 64
            || source_lane.is_empty()
            || source_lane.chars().count() > 64
        {
            truncated = true;
            continue;
        }
        if selected_nodes.len() >= 64 {
            truncated = true;
            continue;
        }
        // Runtime actor 的唯一身份是节点 id；`agent:<名称>` 只是静态图的展示车道，
        // 与 Journal 的 `nodeId` 不同会让时间轴无法把 actorSessionId 绑定回胶囊。
        // 保留名称作为 lane.name，避免把稳定身份泄露成用户看到的文本。
        let lane = match node.kind {
            crate::workflows::WorkflowGraphNodeKind::Ask => id.clone(),
            crate::workflows::WorkflowGraphNodeKind::WorldRead => source_lane.clone(),
        };
        if !lanes.contains_key(&lane) && lanes.len() >= 32 {
            truncated = true;
            continue;
        }
        let label = node.label.chars().take(128).collect::<String>();
        if label.is_empty() {
            truncated = true;
            continue;
        }
        let lane_name = match node.kind {
            crate::workflows::WorkflowGraphNodeKind::Ask => label.clone(),
            crate::workflows::WorkflowGraphNodeKind::WorldRead => source_lane,
        };
        lanes.entry(lane.clone()).or_insert(lane_name);
        selected_nodes.push((id, node.kind, label, lane));
    }

    let selected_ids = selected_nodes
        .iter()
        .map(|(id, _, _, _)| id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let lane_by_node = selected_nodes
        .iter()
        .map(|(id, _, _, lane)| (id.as_str(), lane.as_str()))
        .collect::<BTreeMap<_, _>>();
    let steps = selected_nodes
        .iter()
        .map(|(id, kind, label, lane)| {
            let kind = match kind {
                crate::workflows::WorkflowGraphNodeKind::Ask => "ask",
                crate::workflows::WorkflowGraphNodeKind::WorldRead => "world-read",
            };
            json!({
                "id": id,
                "kind": kind,
                "label": label,
                "lane": lane,
            })
        })
        .collect::<Vec<_>>();
    if steps.is_empty() {
        return Some(json!({
            "kind": "create_workflow",
            "ok": true,
            "errorCount": 0,
            "diagnostics": [],
        }));
    }

    let lanes = lanes
        .into_iter()
        .map(|(id, name)| json!({"id": id, "name": name}))
        .collect::<Vec<_>>();
    let participants = lanes
        .iter()
        .filter_map(|lane| lane.get("id").and_then(Value::as_str))
        .map(|lane| {
            let step_ids = selected_nodes
                .iter()
                .filter(|(_, _, _, node_lane)| node_lane == lane)
                .map(|(id, _, _, _)| json!(id))
                .collect::<Vec<_>>();
            json!({
                "id": lane,
                "phase": "unphased",
                "lane": lane,
                "steps": step_ids,
            })
        })
        .collect::<Vec<_>>();

    let mut handoff_set = std::collections::BTreeSet::new();
    for edge in edges {
        if !selected_ids.contains(edge.from.as_str()) || !selected_ids.contains(edge.to.as_str()) {
            truncated = true;
            continue;
        }
        let Some(from) = lane_by_node.get(edge.from.as_str()) else {
            continue;
        };
        let Some(to) = lane_by_node.get(edge.to.as_str()) else {
            continue;
        };
        if from == to {
            continue;
        }
        handoff_set.insert((
            (*from).to_owned(),
            (*to).to_owned(),
            edge.back.unwrap_or(false),
        ));
    }
    let all_handoffs = handoff_set.into_iter().collect::<Vec<_>>();
    if all_handoffs.len() > 256 {
        truncated = true;
    }
    let handoffs = all_handoffs
        .into_iter()
        .take(256)
        .map(|(from, to, back)| {
            let mut value = json!({"from": from, "to": to});
            if back {
                value["back"] = Value::Bool(true);
            }
            value
        })
        .collect::<Vec<_>>();

    let mut graph = json!({
        "steps": steps,
        "lanes": lanes,
        "participants": participants,
        "handoffs": handoffs,
    });
    if truncated {
        graph["truncated"] = Value::Bool(true);
    }
    Some(json!({
        "kind": "create_workflow",
        "ok": true,
        "errorCount": 0,
        "diagnostics": [],
        "causalityGraph": graph,
    }))
}

fn tool_row(input: ToolRowInput<'_>) -> Value {
    let ToolRowInput {
        message,
        part_index,
        row_id,
        created_at,
        call_id,
        tool_name,
        arguments,
        lifecycle,
        result,
    } = input;
    let turn_id = message
        .get("turnId")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let status = tool_status(lifecycle, result);
    let input_text = if arguments.is_null() {
        String::new()
    } else {
        arguments.to_string()
    };
    let mut row = json!({
        "kind": "toolCall",
        "assistantResponseId": message.get("messageId").cloned().unwrap_or(Value::String(call_id.to_owned())),
        "toolCallId": call_id,
        "toolName": tool_name,
        "status": status,
        "inputText": input_text,
        "input": arguments,
        "row": row_common(row_id, turn_id, call_id, created_at, row_id.saturating_add(part_index as u64)),
    });
    if let Some(result) = result {
        let output_text = tool_output_text(result);
        row["output"] = json!({"text": output_text});
        if json_field(result, "isError", "is_error").and_then(Value::as_bool) == Some(true) {
            row["error"] = json!({"code": "tool_failed", "message": output_text});
        } else if let Some(display) = create_workflow_display(tool_name, &arguments) {
            row["display"] = display;
        }
    } else if let Some(outcome) = lifecycle.and_then(|value| value.get("outcome")) {
        let output = outcome.get("result").cloned().unwrap_or(Value::Null);
        let output_text = tool_output_text(&output);
        row["output"] = json!({"text": output_text});
        if status == "error" {
            row["error"] = json!({"code": "tool_failed", "message": output_text});
        } else if let Some(display) = create_workflow_display(tool_name, &arguments) {
            row["display"] = display;
        }
    }
    row
}

fn flatten_row(row: Value) -> Value {
    let mut row_object = row
        .get("row")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(object) = row.as_object() {
        for (key, value) in object {
            if key != "row" {
                row_object.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(row_object)
}

fn apply_assistant_feedback(
    row: &mut Value,
    is_assistant: bool,
    row_id: u64,
    entity_id: &str,
    records: &[AssistantFeedbackRecord],
) {
    if !is_assistant {
        return;
    }
    let Some(feedback) = records
        .iter()
        .find(|record| record.row_id == row_id && record.entity_id == entity_id)
        .map(|record| match record.feedback {
            AssistantFeedback::Like => "like",
            AssistantFeedback::Dislike => "dislike",
        })
    else {
        return;
    };
    // 取消反馈通过删除事实记录实现；projection 省略字段，符合 Source strict row schema。
    row["feedback"] = Value::String(feedback.to_owned());
}

struct MessageProjectionContext<'a> {
    lifecycle: &'a BTreeMap<String, Value>,
    results: &'a BTreeMap<String, Value>,
    call_ids: &'a BTreeMap<String, ()>,
    assistant_feedback: &'a [AssistantFeedbackRecord],
}

fn rows_for_message(
    message: &Value,
    fallback_index: usize,
    created_at: u64,
    row_base: u64,
    context: &MessageProjectionContext<'_>,
) -> Vec<Value> {
    if message
        .get("isMeta")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Vec::new();
    }
    let entity_id = message
        .get("messageId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("message-unknown");
    let role = role_name(message);
    if role == "system" {
        return Vec::new();
    }
    let turn_id = message
        .get("turnId")
        .and_then(Value::as_str)
        .unwrap_or("session")
        .to_owned();
    let parts = message
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let user_attachments = (role == "user")
        .then(|| message_attachments(message))
        .flatten();
    let mut attachments_emitted = false;
    let mut rows = Vec::new();
    let mut saw_tool = false;
    for (part_index, part) in parts.iter().enumerate() {
        let kind = part.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "tool_call" => {
                let call_id = json_string_field(part, "toolCallId", "tool_call_id")
                    .filter(|value| !value.is_empty())
                    .unwrap_or(entity_id);
                let lifecycle_value = context.lifecycle.get(call_id);
                let tool_name = json_string_field(part, "toolName", "tool_name")
                    .or_else(|| {
                        lifecycle_value
                            .and_then(|value| value.get("request"))
                            .and_then(|request| json_string_field(request, "toolName", "tool_name"))
                    })
                    .unwrap_or("unknown");
                let arguments = part
                    .get("arguments")
                    .cloned()
                    .or_else(|| {
                        lifecycle_value
                            .and_then(|value| value.get("request"))
                            .and_then(|request| json_field(request, "arguments", "arguments"))
                            .cloned()
                    })
                    .unwrap_or(Value::Null);
                rows.push(flatten_row(tool_row(ToolRowInput {
                    message,
                    part_index,
                    row_id: row_base.saturating_add(part_index as u64),
                    created_at,
                    call_id,
                    tool_name,
                    arguments,
                    lifecycle: lifecycle_value,
                    result: context.results.get(call_id),
                })));
                saw_tool = true;
            }
            "tool_result" => {
                let call_id = json_string_field(part, "toolCallId", "tool_call_id")
                    .filter(|value| !value.is_empty())
                    .unwrap_or(entity_id);
                if !context.call_ids.contains_key(call_id) {
                    rows.push(flatten_row(tool_row(ToolRowInput {
                        message,
                        part_index,
                        row_id: row_base.saturating_add(part_index as u64),
                        created_at,
                        call_id,
                        tool_name: "unknown",
                        arguments: Value::Null,
                        lifecycle: None,
                        result: Some(part),
                    })));
                }
                saw_tool = true;
            }
            "reasoning" => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                let row_entity_id = format!("{entity_id}:reasoning:{part_index}");
                rows.push(flatten_row(json!({
                    "kind": "reasoning",
                    "text": text,
                    "state": "complete",
                    "assistantResponseId": entity_id,
                    "row": row_common(row_base.saturating_add(part_index as u64), &turn_id, &row_entity_id, created_at, row_base.saturating_add(part_index as u64)),
                })));
            }
            "text" => {
                let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                let row_entity_id = if part_index == 0 {
                    entity_id.to_owned()
                } else {
                    format!("{entity_id}:part:{part_index}")
                };
                let mut row = if role == "assistant" {
                    json!({
                        "kind": "assistantText",
                        "text": text,
                        "state": "complete",
                        "assistantResponseId": entity_id,
                        "actions": {"canFork": true, "canRetry": true},
                        "row": row_common(row_base.saturating_add(part_index as u64), &turn_id, &row_entity_id, created_at, row_base.saturating_add(part_index as u64)),
                    })
                } else {
                    json!({
                        "kind": "userInput",
                        "text": text,
                        "origin": "realUser",
                        "sourceCommandId": entity_id,
                        "actions": {"canEdit": true},
                        "row": row_common(row_base.saturating_add(part_index as u64), &turn_id, &row_entity_id, created_at, row_base.saturating_add(part_index as u64)),
                    })
                };
                apply_assistant_feedback(
                    &mut row,
                    role == "assistant",
                    row_base.saturating_add(part_index as u64),
                    &row_entity_id,
                    context.assistant_feedback,
                );
                if role == "user"
                    && !attachments_emitted
                    && let Some(attachments) = user_attachments.as_ref()
                {
                    row["attachments"] = attachments.clone();
                    attachments_emitted = true;
                }
                rows.push(flatten_row(row));
            }
            _ => {}
        }
    }
    if rows.is_empty() && !saw_tool {
        let text = text_from_content(message.get("content").unwrap_or(&Value::Null));
        let mut row = if role == "assistant" {
            json!({
                "kind": "assistantText",
                "text": text,
                "state": "complete",
                "assistantResponseId": entity_id,
                "actions": {"canFork": true, "canRetry": true},
                "row": row_common(row_base, &turn_id, entity_id, created_at, row_base),
            })
        } else {
            json!({
                "kind": "userInput",
                "text": text,
                "origin": "realUser",
                "sourceCommandId": entity_id,
                "actions": {"canEdit": true},
                "row": row_common(row_base, &turn_id, entity_id, created_at, row_base),
            })
        };
        apply_assistant_feedback(
            &mut row,
            role == "assistant",
            row_base,
            entity_id,
            context.assistant_feedback,
        );
        if role == "user"
            && let Some(attachments) = user_attachments.as_ref()
        {
            row["attachments"] = attachments.clone();
        }
        rows.push(flatten_row(row));
    }
    if rows.is_empty() && fallback_index == 0 {
        return Vec::new();
    }
    rows
}

/// 测试使用单条消息的最小投影；生产路径传入完整 SessionState。
#[cfg(test)]
fn row_for_message(message: &Value, index: usize, created_at: u64) -> Option<Value> {
    let lifecycle = BTreeMap::new();
    let results = BTreeMap::new();
    let call_ids = BTreeMap::new();
    let context = MessageProjectionContext {
        lifecycle: &lifecycle,
        results: &results,
        call_ids: &call_ids,
        assistant_feedback: &[],
    };
    rows_for_message(
        message,
        index,
        created_at,
        (index as u64 + 1).saturating_mul(ROW_ID_PART_STRIDE),
        &context,
    )
    .into_iter()
    .next()
}

fn rows_for_transcript_base(
    transcript: &[Value],
    created_at: u64,
    state: &SessionState,
) -> Vec<Value> {
    let row_bases = transcript_row_bases(state);
    let state_value = serde_json::to_value(state).unwrap_or_else(|_| json!({}));
    let lifecycle = lifecycle_values(&state_value);
    let results = tool_result_values(transcript);
    let call_ids = tool_call_ids(transcript);
    let context = MessageProjectionContext {
        lifecycle: &lifecycle,
        results: &results,
        call_ids: &call_ids,
        assistant_feedback: &state.assistant_feedback,
    };
    transcript
        .iter()
        .enumerate()
        .flat_map(|(index, message)| {
            let base = message
                .get("messageId")
                .and_then(Value::as_str)
                .and_then(|id| row_bases.get(id).copied())
                .unwrap_or_else(|| (index as u64 + 1).saturating_mul(ROW_ID_PART_STRIDE));
            rows_for_message(message, index, created_at, base, &context)
        })
        .collect()
}

fn turn_header_state(turn: &keencode_resources::TurnState) -> &'static str {
    match turn.status {
        TurnStatus::Running => "running",
        TurnStatus::Completed => "completedSuccess",
        TurnStatus::Cancelled => "completedInterrupted",
        TurnStatus::Failed => "failed",
    }
}

/// 在首条真实消息前插入 Source 的权威 turnHeader。Header 的 row/entity 身份来自
/// 同一条消息行区间，文件摘要稍后由 Runtime Artifact 读取补齐；没有 TurnState 的
/// 损坏/旧消息不创建猜测行，避免把 frontend 行伪装成持久事实。
fn insert_turn_headers(rows: Vec<Value>, state: &SessionState) -> Vec<Value> {
    let mut seen = BTreeMap::<String, ()>::new();
    let mut projected = Vec::with_capacity(rows.len().saturating_add(state.turns.len()));
    for row in rows {
        let Some(turn_id) = row.get("turnId").and_then(Value::as_str) else {
            projected.push(row);
            continue;
        };
        if !seen.contains_key(turn_id)
            && let Some(turn) = state
                .turns
                .values()
                .find(|turn| turn.turn_id.as_str() == turn_id)
        {
            seen.insert(turn_id.to_owned(), ());
            let first_row_id = row.get("rowId").and_then(Value::as_u64).unwrap_or(0);
            let source_command_id = row
                .get("sourceCommandId")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or(turn_id);
            let origin = if turn.parent_turn_id.is_some()
                || turn.source_agent_id.as_str() != ROOT_AGENT_ID
            {
                "backgroundResult"
            } else {
                "userInput"
            };
            let history_round_count = state
                .model_rounds
                .iter()
                .filter(|round| round.turn_id == turn.turn_id)
                .count();
            let mut header = flatten_row(json!({
                "kind": "turnHeader",
                "origin": origin,
                "executionKind": "agent",
                "sourceCommandId": source_command_id,
                "historyRoundCount": history_round_count,
                "state": turn_header_state(turn),
                "startedAt": turn.started_at_unix_ms,
                "endedAt": turn.completed_at_unix_ms,
                "row": row_common(
                    first_row_id.saturating_sub(1),
                    turn_id,
                    turn_id,
                    turn.started_at_unix_ms,
                    first_row_id.saturating_sub(1),
                ),
            }));
            // Source schema 将可选字段缺省解释为未提供；不要给不存在的 activeMs
            // 或文件事实填充默认值，真实摘要在带 RuntimeSession 的生产路径补齐。
            if turn.completed_at_unix_ms.is_none()
                && let Some(object) = header.as_object_mut()
            {
                object.remove("endedAt");
            }
            projected.push(header);
        }
        projected.push(row);
    }
    projected
}

fn workflow_launch_row_state(events: &[WorkflowJournalEvent]) -> (&'static str, Option<u64>) {
    let mut ordered = events.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|event| event.sequence);
    let mut state = "running";
    let mut ended_at = None;
    for event in ordered {
        match event.event_type.as_str() {
            "run-started" | "run-resumed" => {
                state = "running";
                ended_at = None;
            }
            "run-settled" => {
                state = match event.payload.get("status").and_then(Value::as_str) {
                    Some("completed" | "succeeded") => "completedSuccess",
                    Some("stopped" | "cancelled") => "completedInterrupted",
                    _ => "failed",
                };
                ended_at = event
                    .payload
                    .get("updatedAt")
                    .or_else(|| event.payload.get("updated_at"))
                    .and_then(Value::as_u64);
            }
            _ => {}
        }
    }
    (state, ended_at)
}

/// 直接启动的工作流没有模型 Turn，但 Source 仍要求一个带 graph 的 controlOnly
/// 轮。这里从已经归属当前 Session 的 `run-started` 冻结事实生成该行投影；runId、
/// toolCallId、参数和定义均来自同一事实，不创建新的 Session/Journal 事实源。
fn workflow_launch_metadata(started: &WorkflowJournalEvent) -> Option<Value> {
    if started.run_id.is_empty()
        || started.run_id.chars().count() > 128
        || started.tool_call_id.is_empty()
        || started.tool_call_id.chars().count() > 128
    {
        return None;
    }
    let definition = started.payload.get("definition");
    let display = definition.and_then(|definition| {
        create_workflow_display("CreateWorkflow", &json!({"definition": definition}))
    });
    let name = started
        .payload
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            definition.and_then(|value| value.pointer("/meta/name").and_then(Value::as_str))
        })
        .map(ToOwned::to_owned)
        .and_then(|value| bounded_text(Some(value), 200));
    let scope = started
        .payload
        .get("scope")
        .and_then(Value::as_str)
        .filter(|value| matches!(*value, "project" | "global"));
    let path = definition
        .and_then(|value| value.pointer("/meta/path"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.chars().count() <= 1024);
    let args = started
        .payload
        .get("inputs")
        .or_else(|| started.payload.get("args"))
        .filter(|value| value.is_object())
        .filter(|value| {
            serde_json::to_vec(value)
                .map(|bytes| bytes.len() <= 4096)
                .unwrap_or(false)
        });
    let description = definition
        .and_then(|value| value.pointer("/meta/description"))
        .and_then(Value::as_str)
        .map(|value| value.to_owned())
        .and_then(|value| bounded_text(Some(value), 500));
    let script = started
        .payload
        .get("script")
        .and_then(Value::as_str)
        .filter(|value| value.chars().count() <= 256_000);

    let mut metadata = json!({
        "runId": started.run_id,
        "toolCallId": started.tool_call_id,
    });
    if let Some(name) = name {
        metadata["name"] = Value::String(name);
    }
    if let Some(scope) = scope {
        metadata["scope"] = Value::String(scope.to_owned());
    }
    if let Some(path) = path {
        metadata["path"] = Value::String(path.to_owned());
    }
    if let Some(args) = args {
        metadata["args"] = args.clone();
    }
    if let Some(description) = description {
        metadata["description"] = Value::String(description);
    }
    if let Some(display) = display {
        metadata["display"] = display;
    }
    if let Some(script) = script {
        metadata["script"] = Value::String(script.to_owned());
    }
    Some(metadata)
}

fn append_workflow_launch_rows(mut rows: Vec<Value>, state: &SessionState) -> Vec<Value> {
    let mut launches = state
        .workflow_events
        .iter()
        .filter_map(|(run_id, events)| {
            let started = events
                .iter()
                .find(|event| event.event_type == "run-started")?;
            if started.run_id != *run_id
                || started.launch_input_id.as_deref().is_none_or(str::is_empty)
                || started
                    .payload
                    .get("parentSessionId")
                    .and_then(Value::as_str)
                    != Some(state.session_id.as_str())
            {
                return None;
            }
            let started_at = started
                .payload
                .get("createdAt")
                .or_else(|| started.payload.get("created_at"))
                .and_then(Value::as_u64)?;
            let metadata = workflow_launch_metadata(started)?;
            Some((started.sequence, started_at, events.as_slice(), metadata))
        })
        .collect::<Vec<_>>();
    launches.sort_by_key(|(sequence, _, events, _)| {
        (
            *sequence,
            events
                .first()
                .map(|event| event.run_id.clone())
                .unwrap_or_default(),
        )
    });

    let existing_run_ids = rows
        .iter()
        .filter_map(|row| row.get("workflowLaunch"))
        .filter_map(|launch| launch.get("runId"))
        .filter_map(Value::as_str)
        .map(ToOwned::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    let launch_base = (rows.len() as u64 + 1).saturating_mul(ROW_ID_PART_STRIDE);
    for (index, (_, started_at, events, metadata)) in launches.into_iter().enumerate() {
        let Some(run_id) = metadata.get("runId").and_then(Value::as_str) else {
            continue;
        };
        if existing_run_ids.contains(run_id) {
            continue;
        }
        let Some(source_command_id) = events
            .iter()
            .find(|event| event.event_type == "run-started")
            .and_then(|event| event.launch_input_id.as_deref())
        else {
            continue;
        };
        let (row_state, ended_at) = workflow_launch_row_state(events);
        let turn_id = format!("workflow-launch:{run_id}");
        let user_entity_id = format!("{turn_id}:input");
        let base = launch_base.saturating_add((index as u64).saturating_mul(ROW_ID_PART_STRIDE));
        let mut header = flatten_row(json!({
            "kind": "turnHeader",
            "origin": "workflowLaunch",
            "executionKind": "controlOnly",
            "sourceCommandId": source_command_id,
            "historyRoundCount": 0,
            "state": row_state,
            "startedAt": started_at,
            "endedAt": ended_at,
            "workflowLaunch": metadata.clone(),
            "row": row_common(base, &turn_id, &turn_id, started_at, base),
        }));
        if ended_at.is_none()
            && let Some(object) = header.as_object_mut()
        {
            object.remove("endedAt");
        }
        rows.push(header);
        rows.push(flatten_row(json!({
            "kind": "userInput",
            "text": "Start saved workflow",
            "origin": "workflowLaunch",
            "sourceCommandId": source_command_id,
            "workflowLaunch": metadata,
            "row": row_common(
                base.saturating_add(1),
                &turn_id,
                &user_entity_id,
                started_at,
                base.saturating_add(1),
            ),
        })));
    }
    rows
}

fn rows_for_transcript_with_launches(
    transcript: &[Value],
    created_at: u64,
    state: &SessionState,
    include_workflow_launches: bool,
) -> Vec<Value> {
    let rows = insert_turn_headers(
        rows_for_transcript_base(transcript, created_at, state),
        state,
    );
    if include_workflow_launches {
        append_workflow_launch_rows(rows, state)
    } else {
        rows
    }
}

fn rows_for_transcript(transcript: &[Value], created_at: u64, state: &SessionState) -> Vec<Value> {
    rows_for_transcript_with_launches(transcript, created_at, state, true)
}

/// 生产快照/分页在同一 RuntimeSession 上读取文件快照和 rewind 事务，保持
/// turnHeader 与 conversationFileChangesV4 共用真实 before/after 事实。
fn rows_for_transcript_with_file_changes(
    transcript: &[Value],
    created_at: u64,
    state: &SessionState,
    session: &RuntimeSession,
    reverted_turn_ids: &std::collections::BTreeSet<String>,
    include_workflow_launches: bool,
) -> Result<Vec<Value>, String> {
    let mut rows =
        rows_for_transcript_with_launches(transcript, created_at, state, include_workflow_launches);
    for row in &mut rows {
        if row.get("kind").and_then(Value::as_str) != Some("turnHeader") {
            continue;
        }
        let Some(turn_id) = row.get("turnId").and_then(Value::as_str) else {
            continue;
        };
        let Some(summary) = attachments::file_change_summary_for_turn(
            session,
            state,
            turn_id,
            reverted_turn_ids.contains(turn_id),
        )
        .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let is_reverted = summary.get("state").and_then(Value::as_str) == Some("reverted");
        row["fileChanges"] = summary;
        if !is_reverted && row.get("state").and_then(Value::as_str) != Some("running") {
            row["actions"] = json!({"canRewindFiles": true});
        }
    }
    Ok(rows)
}

fn phase_for_state(state: &Value, active: bool) -> &'static str {
    if active {
        return "running";
    }
    let session_status = state.get("status").map(status_string).unwrap_or_default();
    if matches!(session_status.as_str(), "running" | "waiting") {
        return "running";
    }
    let latest_turn = state
        .get("turns")
        .and_then(Value::as_object)
        .and_then(|turns| {
            // 父 Session 的列表阶段只代表根任务；虚拟 child view 由
            // `phase_for_agent` 单独投影，不能让子 Turn 的晚到终态污染父任务通知。
            turns
                .values()
                .filter(|turn| {
                    turn.get("sourceAgentId").and_then(Value::as_str) == Some(ROOT_AGENT_ID)
                })
                .max_by_key(|turn| {
                    turn.get("startedAtUnixMs")
                        .and_then(Value::as_u64)
                        .unwrap_or(0)
                })
        });
    match latest_turn
        .and_then(|turn| turn.get("status"))
        .map(status_string)
        .as_deref()
    {
        Some("failed") => "error",
        Some("cancelled") => "completedInterrupted",
        Some("completed") => "completedSuccess",
        Some("running") => "running",
        _ if session_status == "closed" => "completedSuccess",
        _ => "draft",
    }
}

fn active_turn(state: &Value) -> bool {
    state
        .get("turns")
        .and_then(Value::as_object)
        .map(|turns| {
            turns.values().any(|turn| {
                turn.get("status")
                    .map(status_string)
                    .is_some_and(|status| status == "running" || status == "pending")
            })
        })
        .unwrap_or(false)
}

fn active_turn_for_agent(state: &SessionState, agent_id: &AgentId) -> bool {
    state.turns.values().any(|turn| {
        turn.source_agent_id == *agent_id
            && matches!(turn.status, keencode_resources::TurnStatus::Running)
    })
}

fn phase_for_agent(state: &SessionState, agent_id: &AgentId, active: bool) -> &'static str {
    if active {
        return "running";
    }
    match state.sub_agents.get(agent_id).map(|agent| &agent.status) {
        Some(SubAgentStatus::Failed) => "error",
        Some(SubAgentStatus::Interrupted | SubAgentStatus::Stopped) => "completedInterrupted",
        Some(SubAgentStatus::Completed) => "completedSuccess",
        Some(SubAgentStatus::Running | SubAgentStatus::Waiting) => "running",
        Some(SubAgentStatus::Pending) | None => "draft",
    }
}

/// 只从 Runtime/Journal 的最新终态 Turn 构造控制区错误；active Turn 存在时，
/// 旧失败不能覆盖当前进行中的请求。错误正文再次经过模型层脱敏，避免将 provider
/// URL、Authorization 或凭据回显到 V4 snapshot。
fn session_last_error(
    runtime_state: &SessionState,
    active: bool,
    agent_id: Option<&AgentId>,
    terminal_sequences: Option<&BTreeMap<String, (u64, u32)>>,
) -> Value {
    if active {
        return Value::Null;
    }
    let latest = runtime_state
        .turns
        .values()
        .filter(|turn| {
            agent_id.map_or(turn.source_agent_id.as_str() == ROOT_AGENT_ID, |id| {
                turn.source_agent_id == *id
            })
        })
        .max_by_key(|turn| {
            // 生产 snapshot 传入 Journal 终态顺序，保证相同时间戳时仍按真实提交顺序
            // 选择最新错误；纯状态测试没有 Journal 时才使用稳定字段作为 fallback。
            let journal_order = terminal_sequences
                .and_then(|sequences| sequences.get(turn.turn_id.as_str()))
                .copied();
            (
                journal_order,
                turn.completed_at_unix_ms.unwrap_or(turn.started_at_unix_ms),
                turn.started_at_unix_ms,
                turn.turn_id.as_str(),
            )
        });
    let Some(turn) = latest.filter(|turn| turn.status == TurnStatus::Failed) else {
        return Value::Null;
    };
    let message = turn
        .outcome_message
        .as_deref()
        .map(|value| keencode_model::redact_error_secrets_bounded(value, 2048))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "会话执行失败".to_owned());
    let (code, recoverable) = match turn.stop_reason {
        Some(keencode_resources::TurnStopReason::LimitReached) => ("limit_reached", true),
        Some(keencode_resources::TurnStopReason::ContextBlocked) => ("context_blocked", true),
        Some(keencode_resources::TurnStopReason::ModelOutputLimit) => ("model_output_limit", true),
        Some(keencode_resources::TurnStopReason::ModelRefusal) => ("model_refusal", false),
        Some(keencode_resources::TurnStopReason::Cancelled) => ("cancelled", false),
        Some(keencode_resources::TurnStopReason::Failed) | None => ("turn_failed", true),
    };
    json!({
        "code": code,
        "message": message,
        "recoverable": recoverable,
        "at": turn.completed_at_unix_ms.unwrap_or(runtime_state.updated_at_unix_ms),
        "source": "runtime",
        "traceId": format!("turn:{}", turn.turn_id.as_str()),
    })
}

fn model_config(
    state: &Value,
    followup_mode: FollowupMode,
    permission_mode: Option<PermissionMode>,
) -> Value {
    let provider = state.get("provider").unwrap_or(&Value::Null);
    let provider_id = provider
        .get("providerId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model_id = provider
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let thought = provider
        .get("reasoningEffort")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plan_enabled = state
        .get("plan")
        .and_then(|plan| plan.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let model_selection = if !provider_id.is_empty() && !model_id.is_empty() {
        Some(json!({
            "providerId": provider_id,
            "modelId": model_id,
            "options": if thought.is_empty() { json!({}) } else { json!({"reasoningLevel": thought}) },
        }))
    } else {
        None
    };
    let followup_mode = match followup_mode {
        FollowupMode::Queue => "queue",
        FollowupMode::Guide => "guide",
    };
    // 持久化 Plan 是硬只读事实；即使冷恢复尚未重放 mode authority，也不能把它投影为可写。
    let mode = if plan_enabled {
        "plan"
    } else {
        permission_mode.map(permission_mode_wire).unwrap_or("build")
    };
    let mut config = json!({
        "provider": provider_id,
        "model": model_id,
        "thought": thought,
        "thoughtLevels": if thought.is_empty() { json!([]) } else { json!([thought]) },
        "followupMode": followup_mode,
        "mode": mode,
        "planEnabled": plan_enabled,
    });
    // `modelSelection` 是 optional 而不是 nullable；显式 null 会被 source
    // zod schema 拒绝，未绑定 provider 时应省略该键。
    if let Some(selection) = model_selection {
        config["modelSelection"] = selection;
    }
    config
}

pub(crate) fn permission_mode_wire(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Build => "build",
        PermissionMode::Edit => "edit",
        PermissionMode::Plan => "plan",
        PermissionMode::Yolo => "yolo",
    }
}

/// 将资源层持久队列转换为源 V4 的完整 input intent 形状。
fn input_queue_value(state: &SessionState) -> Value {
    let items = state
        .input_queue
        .items
        .iter()
        .enumerate()
        .map(|(queue_position, item)| {
            let kind = match item.kind {
                SessionInputKind::SendText => "sendText",
                SessionInputKind::SendGoalCommand => "sendGoalCommand",
                SessionInputKind::Compact => "compact",
            };
            let requested = match item.requested_delivery {
                SessionInputDelivery::Queue => "queue",
                SessionInputDelivery::Guide => "guide",
            };
            let admitted = match item.admitted_delivery {
                SessionInputDelivery::Queue => "queue",
                SessionInputDelivery::Guide => "guide",
            };
            let dispatch = match item.dispatch {
                SessionInputDispatch::Queued => "queued",
                SessionInputDispatch::Reserved => "reserved",
                SessionInputDispatch::Promoting => "promoting",
            };
            let mut value = json!({
                "sourceCommandId": item.source_command_id,
                "queueItemId": item.queue_item_id,
                "clientId": item.client_id.as_deref().unwrap_or("desktop"),
                "kind": kind,
                "text": item.text,
                "attachments": item.attachments,
                "delivery": {
                    "requested": requested,
                    "admitted": admitted,
                },
                "order": {
                    "admissionSeq": item.admission_seq,
                    "queuePosition": queue_position,
                },
                "steer": {
                    "state": if matches!(item.admitted_delivery, SessionInputDelivery::Guide) {
                        "guided"
                    } else {
                        "notRequested"
                    },
                },
                "dispatch": {"state": dispatch},
                "admittedAt": item.admitted_at_unix_ms,
                "provenance": {
                    "sourceCommandId": item.source_command_id,
                    "queueItemId": item.queue_item_id,
                    "clientId": item.client_id.as_deref().unwrap_or("desktop"),
                },
            });
            if let Some(selection) = item.model_selection.clone() {
                value["modelSelection"] = selection;
            }
            if let Some(mode) = item.mode.clone() {
                value["mode"] = Value::String(mode);
            }
            if item.plan_enabled {
                value["planEnabled"] = Value::Bool(true);
            }
            value
        })
        .collect::<Vec<_>>();
    let mut queue = json!({
        "items": items,
        "autoDrain": state.input_queue.auto_drain,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    if let Some(reason) = &state.input_queue.pause_reason {
        queue.insert("pauseReason".to_owned(), Value::String(reason.clone()));
    }
    Value::Object(queue)
}

fn usage_snapshot(state: &Value) -> Value {
    let mut input = 0_u64;
    let mut output = 0_u64;
    let mut cache_read = 0_u64;
    let mut cache_write = 0_u64;
    if let Some(rounds) = state.get("modelRounds").and_then(Value::as_array) {
        for round in rounds {
            if let Some(usage) = round.get("usage") {
                input = input.saturating_add(
                    usage
                        .get("inputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                output = output.saturating_add(
                    usage
                        .get("outputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                cache_read = cache_read.saturating_add(
                    usage
                        .get("cacheReadTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                cache_write = cache_write.saturating_add(
                    usage
                        .get("cacheWriteTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
            }
        }
    }
    json!({
        "contextWindow": Value::Null,
        "cumulative": {
            "inputTokens": input,
            "outputTokens": output,
            "cacheReadTokens": cache_read,
            "cacheWriteTokens": cache_write,
        },
    })
}

fn value_field<'a>(value: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    value.get(camel).or_else(|| value.get(snake))
}

fn string_field(value: &Value, camel: &str, snake: &str) -> Option<String> {
    value_field(value, camel, snake)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn object_field<'a>(
    value: &'a Value,
    camel: &str,
    snake: &str,
) -> Option<&'a serde_json::Map<String, Value>> {
    value_field(value, camel, snake).and_then(Value::as_object)
}

fn instance_ref(payload: &Value) -> Option<(String, u64)> {
    let reference = object_field(payload, "instance", "instance")
        .or_else(|| object_field(payload, "node", "node"))
        .or_else(|| object_field(payload, "address", "address"))?;
    let site_id = string_field(&Value::Object(reference.clone()), "siteId", "site_id")
        .or_else(|| string_field(&Value::Object(reference.clone()), "nodeId", "node_id"))?;
    let ordinal = value_field(&Value::Object(reference.clone()), "ordinal", "ordinal")
        .and_then(Value::as_u64)
        .or_else(|| {
            value_field(
                &Value::Object(reference.clone()),
                "invocation",
                "invocation",
            )
            .and_then(Value::as_array)
            .and_then(|items| items.last())
            .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    Some((site_id, ordinal))
}

fn actor_ref(payload: &Value) -> Option<(String, u64)> {
    let actor = object_field(payload, "actor", "actor")?;
    let actor_value = Value::Object(actor.clone());
    let site_id = string_field(&actor_value, "siteId", "site_id")?;
    let ordinal = value_field(&actor_value, "ordinal", "ordinal")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some((site_id, ordinal))
}

/// Runtime workflow actor 事件使用 `nodeId`/`nodeAddress`，而不是 V4 事件的
/// `instance`/`actor`。这里只接受 actor 运行时实际写入的节点身份；控制节点没有
/// `actorSessionId`，不会通过这个适配器伪造成可见步骤。
fn runtime_node_ref(payload: &Value) -> Option<(String, u64)> {
    // actor-started/agent-result 是 Runtime 唯一发布的协议形状；不接受旧的
    // snake_case 顶层别名或旧 address 字段，否则会把过期记录误投影成新步骤。
    let address = payload.get("nodeAddress")?.as_object()?;
    let site_id = payload.get("nodeId")?.as_str()?;
    let ordinal = address
        .get("invocation")
        .and_then(Value::as_array)
        .and_then(|items| items.last())
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some((site_id.to_owned(), ordinal))
}

/// Native actor 结果将模型轮次用量嵌在 `output.usage` 中。该字段是每个轮次的
/// 实际 `totalTokens`，没有该事实时返回 None，调用方继续保留 0 而不是猜测进度。
fn runtime_agent_result_tokens(payload: &Value) -> Option<u64> {
    let items = payload.pointer("/output/usage")?.as_array()?;
    let mut total = 0u64;
    let mut found = false;
    for item in items {
        let value = item.get("totalTokens").and_then(Value::as_u64);
        if let Some(value) = value {
            total = total.saturating_add(value);
            found = true;
        }
    }
    found.then_some(total)
}

fn node_phase(event_type: &str) -> Option<&'static str> {
    Some(match event_type {
        "node-queued" => "queued",
        "node-dispatched" => "dispatched",
        "node-executing" => "executing",
        "node-waiting" => "waiting",
        "node-repairing" => "repairing",
        "node-nudged" => "nudged",
        "node-settled" => "settled",
        _ => return None,
    })
}

fn bounded_text(value: Option<String>, max: usize) -> Option<String> {
    value
        .map(|text| text.chars().take(max).collect())
        .filter(|text: &String| !text.is_empty())
}

fn workflow_status_from_event_type(event_type: &str, current: &str, payload: &Value) -> String {
    match event_type {
        "run-started" => "running".to_owned(),
        "run-settled" => match string_field(payload, "status", "status").as_deref() {
            Some("completed") | Some("succeeded") => "completed".to_owned(),
            Some("stopped") | Some("cancelled") => "stopped".to_owned(),
            Some("errored") | Some("failed") => "errored".to_owned(),
            _ => "errored".to_owned(),
        },
        _ => current.to_owned(),
    }
}

/// 将一个 Journal run 形成为 V4 strict workflowRun。
///
/// status/resumable/spentTokens 优先来自 WorkflowHost 的 `RuntimeWorkflowJournal` 摘要；
/// actors/nodes 只读取同一组有序事件，并且只接收 ask/world-read 叶节点。控制节点保留在
/// Journal 查询中，不进入 strict `nodes` 数组，避免用不完整字段伪造协议实体。
fn workflow_run_from_events(
    run_id: &str,
    raw_events: &[Value],
    host_summary: Option<&Value>,
) -> Value {
    let mut ordered = raw_events.to_vec();
    ordered.sort_by_key(|event| event.get("sequence").and_then(Value::as_u64).unwrap_or(0));
    let mut status = host_summary
        .and_then(|summary| summary.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("pending")
        .to_owned();
    let mut last_sequence = 0u64;
    let mut spent_tokens = host_summary
        .and_then(|summary| {
            summary
                .pointer("/usage/spentTokens")
                .or_else(|| summary.get("spent_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut agent_result_tokens = 0u64;
    let mut has_agent_result_tokens = false;
    let mut nodes_used = 0u64;
    let mut dispatched = BTreeMap::<(String, u64), bool>::new();
    let mut actors = BTreeMap::<(String, u64), Value>::new();
    let mut nodes = BTreeMap::<(String, u64), Value>::new();
    let mut error = None;
    let mut result_preview = None;

    for event in &ordered {
        let event_type = event
            .get("eventType")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let payload = event.get("payload").unwrap_or(&Value::Null);
        last_sequence =
            last_sequence.max(event.get("sequence").and_then(Value::as_u64).unwrap_or(0));
        status = workflow_status_from_event_type(event_type, &status, payload);

        if let Some(value) =
            value_field(payload, "spentTokens", "spent_tokens").and_then(Value::as_u64)
        {
            spent_tokens = value;
        }
        if let Some(value) = value_field(payload, "error", "error") {
            error = value
                .as_str()
                .map(|text| text.chars().take(2_048).collect::<String>())
                .or_else(|| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(|text| text.chars().take(2_048).collect::<String>())
                });
        }
        if let Some(value) = string_field(payload, "resultPreview", "result_preview") {
            result_preview = bounded_text(Some(value), 2_048);
        }

        // 父 WorkflowJournal 的 native actor 结果把每个模型轮次的真实 usage 放在
        // `agent-result.output.usage`；该事件没有再发 V4 的顶层 usage-updated。
        // 只在字段存在时汇总，避免把缺失 usage 当成一个估算值。
        if event_type == "agent-result"
            && let Some(value) = runtime_agent_result_tokens(payload)
        {
            agent_result_tokens = agent_result_tokens.saturating_add(value);
            has_agent_result_tokens = true;
        }

        // Native actor 生命周期是 Runtime 的内部事件名。一个 actor 对应一个
        // V4 ask 节点；parallel/artifact 等控制节点没有 actorSessionId，仍由下面
        // 的通用 node-* 分支过滤，不会被计入步数。
        if event_type == "actor-started"
            && payload
                .get("actorSessionId")
                .and_then(Value::as_str)
                .is_some()
            && let Some((site_id, ordinal)) = runtime_node_ref(payload)
        {
            let actor_key = (site_id.clone(), ordinal);
            let actor = actors.entry(actor_key.clone()).or_insert_with(|| {
                json!({
                    "siteId": site_id,
                    "ordinal": ordinal,
                    "status": "running",
                })
            });
            if let Some(session_id) = string_field(payload, "actorSessionId", "actor_session_id") {
                actor["sessionId"] = Value::String(session_id);
            }

            let node = nodes.entry(actor_key.clone()).or_insert_with(|| {
                nodes_used = nodes_used.saturating_add(1);
                json!({
                    "siteId": actor_key.0.clone(),
                    "ordinal": actor_key.1,
                    "kind": "ask",
                    "phase": "executing",
                    "actorSiteId": actor_key.0.clone(),
                    "actorOrdinal": actor_key.1,
                })
            });
            node["kind"] = Value::String("ask".to_owned());
            node["phase"] = Value::String("executing".to_owned());
            node["actorSiteId"] = Value::String(actor_key.0);
            node["actorOrdinal"] = json!(actor_key.1);
        }

        // actor-result 是 actor 节点已经完成的权威事实。先于同一 run 的
        // node-settled 到达时也能建立严格节点，之后的生命周期事件只会更新它。
        if event_type == "agent-result"
            && let Some((site_id, ordinal)) = runtime_node_ref(payload)
        {
            let key = (site_id.clone(), ordinal);
            let node = nodes.entry(key.clone()).or_insert_with(|| {
                nodes_used = nodes_used.saturating_add(1);
                json!({
                    "siteId": site_id.clone(),
                    "ordinal": ordinal,
                    "kind": "ask",
                    "phase": "settled",
                    "actorSiteId": key.0.clone(),
                    "actorOrdinal": key.1,
                })
            });
            node["kind"] = Value::String("ask".to_owned());
            node["phase"] = Value::String("settled".to_owned());
            node["actorSiteId"] = Value::String(key.0.clone());
            node["actorOrdinal"] = json!(key.1);
            let outcome = match string_field(payload, "status", "status").as_deref() {
                Some("completed") | Some("succeeded") => "ok",
                Some("cancelled") | Some("stopped") => "cancelled",
                _ => "failed",
            };
            node["outcome"] = Value::String(outcome.to_owned());
        }

        if event_type == "actor-created"
            && let Some((site_id, ordinal)) = actor_ref(payload)
        {
            let actor_value = json!({
                "siteId": site_id,
                "ordinal": ordinal,
                "status": "waiting",
            });
            let mut actor = actor_value;
            if let Some(name) = bounded_text(string_field(payload, "name", "name"), 128) {
                actor["name"] = Value::String(name);
            }
            if let Some(session_id) = string_field(event, "actorSessionId", "actor_session_id") {
                actor["sessionId"] = Value::String(session_id);
            }
            actors.insert((site_id, ordinal), actor);
        }

        if let Some(phase) = node_phase(event_type) {
            let kind = string_field(payload, "kind", "kind");
            let Some((site_id, ordinal)) = instance_ref(payload) else {
                continue;
            };
            let key = (site_id.clone(), ordinal);
            // `node-started`/`node-settled` from the pure engine are control nodes. They
            // intentionally stay in the event query and never enter the strict leaf schema.
            // Native actor node-settled 只有 address，没有 kind；它必须复用前面的
            // actor-started 节点身份，而不能因为缺 kind 把真实完成事实过滤掉。
            let kind = kind.or_else(|| {
                nodes
                    .get(&key)
                    .and_then(|node| node.get("kind"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            });
            if !matches!(kind.as_deref(), Some("ask") | Some("world-read")) {
                continue;
            }
            // 某些 Runtime 事件流不会单独发 actor-created；叶节点上的 actor
            // 引用仍是权威绑定，先建立最小 actor，再由后面的状态归约更新它。
            if let Some((actor_site, actor_ordinal)) = actor_ref(payload) {
                actors
                    .entry((actor_site.clone(), actor_ordinal))
                    .or_insert_with(|| {
                        json!({
                            "siteId": actor_site,
                            "ordinal": actor_ordinal,
                            "status": "waiting",
                        })
                    });
            }
            if event_type == "node-dispatched" && dispatched.insert(key.clone(), true).is_none() {
                nodes_used = nodes_used.saturating_add(1);
            }
            let previous = nodes.get(&key).cloned().unwrap_or_else(|| {
                json!({
                    "siteId": site_id,
                    "ordinal": ordinal,
                })
            });
            let mut node = previous;
            node["phase"] = Value::String(phase.to_owned());
            node["kind"] = Value::String(kind.unwrap_or_else(|| "ask".to_owned()));
            if let Some(outcome) = string_field(payload, "outcome", "outcome") {
                if matches!(outcome.as_str(), "ok" | "failed" | "cancelled") {
                    node["outcome"] = Value::String(outcome);
                }
            } else if event_type == "node-settled" {
                let outcome = match string_field(payload, "status", "status").as_deref() {
                    Some("succeeded") | Some("completed") => "ok",
                    Some("cancelled") | Some("stopped") => "cancelled",
                    Some("failed") | Some("errored") => "failed",
                    _ => "failed",
                };
                node["outcome"] = Value::String(outcome.to_owned());
            }
            if payload.get("cached").and_then(Value::as_bool) == Some(true) {
                node["cached"] = Value::Bool(true);
            }
            if let Some((actor_site, actor_ordinal)) = actor_ref(payload) {
                node["actorSiteId"] = Value::String(actor_site);
                node["actorOrdinal"] = json!(actor_ordinal);
            }
            for (camel, snake, limit) in [
                ("phaseName", "phase_name", 128usize),
                ("instructionsHead", "instructions_head", 240usize),
            ] {
                if let Some(value) = bounded_text(string_field(payload, camel, snake), limit) {
                    node[camel] = Value::String(value);
                }
            }
            for (camel, snake) in [("turn", "turn"), ("toolCalls", "tool_calls")] {
                if let Some(value) = value_field(payload, camel, snake).and_then(Value::as_u64) {
                    node[camel] = json!(value);
                }
            }
            if let Some(last_tool) = value_field(payload, "lastTool", "last_tool")
                && last_tool.is_object()
            {
                node["lastTool"] = last_tool.clone();
            }
            nodes.insert(key, node);
        }
    }

    // `usage-updated`（若存在）仍是引擎直接给出的总量；只有 native actor 事件
    // 没有该事件时，才采用上面从每个 actor 的真实 model-round usage 汇总出的值。
    if has_agent_result_tokens
        && !ordered
            .iter()
            .any(|event| event.get("eventType").and_then(Value::as_str) == Some("usage-updated"))
    {
        spent_tokens = agent_result_tokens;
    }

    // actor-created 事件可能包含内部控制 actor。strict V4 只暴露至少绑定到一个
    // 已接受叶节点的 actor，避免把控制节点或半成品 actor 冒充用户可见执行者。
    actors.retain(|(site_id, ordinal), _| {
        nodes.values().any(|node| {
            node.get("actorSiteId").and_then(Value::as_str) == Some(site_id.as_str())
                && node.get("actorOrdinal").and_then(Value::as_u64) == Some(*ordinal)
        })
    });
    for actor in actors.values_mut() {
        let actor_ref = (
            actor.get("siteId").and_then(Value::as_str),
            actor.get("ordinal").and_then(Value::as_u64),
        );
        let running = nodes.values().any(|node| {
            node.get("actorSiteId").and_then(Value::as_str) == actor_ref.0
                && node.get("actorOrdinal").and_then(Value::as_u64) == actor_ref.1
                && matches!(
                    node.get("phase").and_then(Value::as_str),
                    Some("executing" | "repairing" | "nudged")
                )
        });
        let waiting = nodes.values().any(|node| {
            node.get("actorSiteId").and_then(Value::as_str) == actor_ref.0
                && node.get("actorOrdinal").and_then(Value::as_u64) == actor_ref.1
                && matches!(
                    node.get("phase").and_then(Value::as_str),
                    Some("queued" | "dispatched" | "waiting")
                )
        });
        actor["status"] = Value::String(
            if running {
                "running"
            } else if waiting || status == "running" {
                "waiting"
            } else {
                "completed"
            }
            .to_owned(),
        );
    }

    let mut run = json!({
        "runId": run_id,
        "status": status.clone(),
        "usage": {"spentTokens": spent_tokens, "nodesUsed": nodes_used},
        "actors": [],
        "nodes": [],
        "lastEventSequence": last_sequence,
    });
    if let Some(summary) = host_summary {
        if let Some(tool_call_id) = summary
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            run["toolCallId"] = Value::String(tool_call_id.to_owned());
        }
        if summary.get("resumable").and_then(Value::as_bool) == Some(true) {
            run["resumable"] = Value::Bool(true);
        }
    }
    run["status"] = Value::String(status.clone());
    run["usage"] = json!({"spentTokens": spent_tokens, "nodesUsed": nodes_used});
    run["actors"] = Value::Array(actors.into_values().collect());
    run["nodes"] = Value::Array(nodes.into_values().collect());
    run["lastEventSequence"] = json!(last_sequence);
    // `workflowRuns[].artifacts` 是活投影的刷新信号：详情 hook 先读它，再从
    // Journal 补齐版本/spec。不能只依赖一次完成后的 Journal 查询，否则运行中
    // 先返回空清单时没有新的摘要签名触发重查，产物区会永久缺席。
    if let Some(artifacts) = workflow_artifact_summaries(host_summary) {
        run["artifacts"] = artifacts;
    }
    if let Some(value) = error {
        run["error"] = Value::String(value);
    }
    if let Some(value) = result_preview {
        run["resultPreview"] = Value::String(value);
    }
    run
}

/// 将同一父 Journal 的完整产物元数据收敛为 Source 的高频摘要。
///
/// `WorkflowRunSummary.artifacts` 来自 `RuntimeWorkflowJournal`，是唯一权威的
/// Journal 读面；这里仅裁剪掉 versions/spec/sourcePath 等冷查询字段，不从原始
/// 事件另造一份产物事实。无效条目被过滤，避免把严格 V4 状态键变成整帧拒收。
fn workflow_artifact_summaries(host_summary: Option<&Value>) -> Option<Value> {
    let artifacts = host_summary?.get("artifacts")?.as_array()?;
    let summaries = artifacts
        .iter()
        .filter_map(|artifact| {
            let object = artifact.as_object()?;
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.chars().count() <= 64)?;
            let kind = object.get("kind").and_then(Value::as_str).filter(|kind| {
                matches!(
                    *kind,
                    "file" | "markdown" | "chart" | "table" | "metrics" | "board"
                )
            })?;
            let version = object
                .get("version")
                .and_then(Value::as_u64)
                .filter(|value| (1..=16).contains(value))?;
            let mut summary = Map::new();
            summary.insert("id".to_owned(), Value::String(id.to_owned()));
            summary.insert("kind".to_owned(), Value::String(kind.to_owned()));
            summary.insert("version".to_owned(), json!(version));
            for (field, max_chars) in [("title", 120usize), ("contentType", 128usize)] {
                if let Some(value) = object
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                {
                    summary.insert(
                        field.to_owned(),
                        Value::String(value.chars().take(max_chars).collect()),
                    );
                }
            }
            let bytes = object
                .get("versions")
                .and_then(Value::as_array)
                .and_then(|versions| {
                    versions.iter().rev().find(|record| {
                        record.get("version").and_then(Value::as_u64) == Some(version)
                    })
                })
                .and_then(|record| record.get("bytes"))
                .and_then(Value::as_u64);
            if let Some(bytes) = bytes {
                summary.insert("bytes".to_owned(), json!(bytes));
            }
            if let Some(item_count) = object.get("itemCount").and_then(Value::as_u64) {
                summary.insert("itemCount".to_owned(), json!(item_count));
            }
            if object.get("primary").and_then(Value::as_bool) == Some(true) {
                summary.insert("primary".to_owned(), Value::Bool(true));
            }
            Some(Value::Object(summary))
        })
        .collect::<Vec<_>>();
    (!summaries.is_empty()).then_some(Value::Array(summaries))
}

fn workflow_host_summaries(session: &RuntimeSession) -> BTreeMap<String, Value> {
    RuntimeWorkflowJournal::new(session.clone())
        .list_runs(Some(session.session_id().as_str()), None, None, 8)
        .ok()
        .map(|result| {
            result
                .runs
                .into_iter()
                .filter_map(|summary| {
                    let run_id = summary.run_id.clone();
                    serde_json::to_value(summary)
                        .ok()
                        .map(|value| (run_id, value))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 从同一份 workflowRuns 投影派生 Composer 的后台工作摘要。
///
/// `workflowRuns` 和 `backgroundWorks` 都来自父 Session 的 WorkflowJournal；前者给详情
/// 面板使用，后者给 Composer badge/停止入口使用。两者必须按 runId 对齐，不能把前端看到的
/// 运行状态重新存成一份后台任务事实。终态 run 不再属于后台工作，pending 也必须投影成
/// `running`，因为 Source 的 BackgroundWorkSummary 没有 pending 值域。
fn workflow_background_works(
    runtime_state: &keencode_resources::SessionState,
    workflow_snapshot: &Value,
) -> Value {
    let Some(runs) = workflow_snapshot.get("runs").and_then(Value::as_array) else {
        return Value::Array(Vec::new());
    };
    let mut works = Vec::new();
    for run in runs {
        let Some(status) = run.get("status").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(status, "pending" | "running") {
            continue;
        }
        let Some(run_id) = run.get("runId").and_then(Value::as_str) else {
            continue;
        };
        let Some(started) = runtime_state
            .workflow_events
            .get(run_id)
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| event.event_type == "run-started")
            })
        else {
            // 没有 run-started 冻结事实的损坏/半成品 run 不能生成严格的后台摘要。
            continue;
        };
        if started
            .payload
            .get("parentSessionId")
            .and_then(Value::as_str)
            != Some(runtime_state.session_id.as_str())
        {
            continue;
        }
        let Some(started_at) = started.payload.get("createdAt").and_then(Value::as_u64) else {
            continue;
        };
        let title = started
            .payload
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(run_id);
        works.push(json!({
            "workId": run_id,
            "kind": "workflow",
            "title": title,
            "status": "running",
            "startedAt": started_at,
            "cancellable": true,
            "anchorRowId": Value::Null,
        }));
    }
    Value::Array(works)
}

fn workflow_runs(session: &RuntimeSession, state: &Value) -> Value {
    let summaries = workflow_host_summaries(session);
    let mut runs = state
        .get("workflowEvents")
        .and_then(Value::as_object)
        .map(|events| {
            events
                .iter()
                .filter_map(|(run_id, raw_events)| {
                    let event_values = raw_events.as_array()?;
                    Some(workflow_run_from_events(
                        run_id,
                        event_values,
                        summaries.get(run_id),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    runs.sort_by_key(|run| {
        run.get("lastEventSequence")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    });
    if runs.len() > 8 {
        runs = runs.split_off(runs.len() - 8);
    }
    json!({
        "revision": runs.iter().filter_map(|run| run.get("lastEventSequence").and_then(Value::as_u64)).max().unwrap_or(0),
        "runs": runs,
    })
}

fn conversation_state_snapshot(
    session_id: &str,
    session: &RuntimeSession,
    runtime_snapshot: &keencode_runtime::RuntimeSnapshot,
    transcript: &[Value],
    log_epoch: &str,
    goal: Value,
    terminal_sequences: Option<&BTreeMap<String, (u64, u32)>>,
) -> Value {
    let workflow_snapshot = workflow_runs(
        session,
        &serde_json::to_value(&runtime_snapshot.state).unwrap_or_else(|_| json!({})),
    );
    let mut snapshot = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
        ConversationSnapshotInput {
            session_id,
            runtime_state: &runtime_snapshot.state,
            transcript,
            log_epoch,
            workflow_snapshot,
            pending_interactions: Value::Array(Vec::new()),
            permission_mode: None,
            goal,
            agent_id: None,
            workspace_hook_admission: Value::Null,
            terminal_sequences,
        },
    );
    snapshot["control"]["apiRetry"] =
        active_model_retry_value(session, &runtime_snapshot.state, None);
    snapshot
}

fn active_model_retry_value(
    session: &RuntimeSession,
    state: &SessionState,
    agent_id: Option<&AgentId>,
) -> Value {
    let expected_agent = agent_id.map(|id| id.as_str()).unwrap_or(ROOT_AGENT_ID);
    let turn = state
        .turns
        .values()
        .filter(|turn| {
            turn.status == TurnStatus::Running && turn.source_agent_id.as_str() == expected_agent
        })
        .max_by_key(|turn| (turn.started_at_unix_ms, turn.turn_id.as_str()));
    let Some(turn) = turn else {
        return Value::Null;
    };
    let retry = session
        .model_retry_for_turn(turn.turn_id.as_str(), turn.source_agent_id.as_str())
        .ok()
        .flatten();
    retry.map_or(Value::Null, |retry| retry_state_value(&retry))
}

fn retry_state_value(retry: &RuntimeModelRetryScheduled) -> Value {
    json!({
        "attempt": retry.attempt,
        "maxAttempts": retry.max_attempts,
        "nextRetryAt": retry.occurred_at_ms.saturating_add(retry.delay_ms),
        "reasonCode": "provider_retry",
    })
}

/// 契约夹具复用生产状态投影，不为测试另建 TypeScript 协议事实。
#[cfg(test)]
fn conversation_snapshot_from_state_with_pending_and_goal(
    session_id: &str,
    runtime_state: &keencode_resources::SessionState,
    transcript: &[Value],
    log_epoch: &str,
    workflow_snapshot: Value,
    pending_interactions: Value,
    goal: Value,
) -> Value {
    conversation_snapshot_from_state_with_pending_and_mode_for_agent(ConversationSnapshotInput {
        session_id,
        runtime_state,
        transcript,
        log_epoch,
        workflow_snapshot,
        pending_interactions,
        permission_mode: None,
        goal,
        agent_id: None,
        workspace_hook_admission: Value::Null,
        terminal_sequences: None,
    })
}

struct ConversationSnapshotInput<'a> {
    session_id: &'a str,
    runtime_state: &'a keencode_resources::SessionState,
    transcript: &'a [Value],
    log_epoch: &'a str,
    workflow_snapshot: Value,
    pending_interactions: Value,
    permission_mode: Option<PermissionMode>,
    goal: Value,
    agent_id: Option<&'a AgentId>,
    workspace_hook_admission: Value,
    terminal_sequences: Option<&'a BTreeMap<String, (u64, u32)>>,
}

/// 将持久 Goal 转为 Source 的严格 `GoalState`。GoalFileStore 只保存生命周期和
/// 用量事实；Source 的验证历史目前没有对应的 Rust 事实，因此只能稳定投影为空，
/// 不能用前端缓存或猜测值补齐。
fn goal_projection_value_from_snapshot(
    snapshot: &keencode_agent::GoalSnapshot,
    session_id: &str,
) -> Result<Value, String> {
    let Some(goal) = snapshot.goal.as_ref() else {
        return Ok(Value::Null);
    };
    if goal.owner_session_id != session_id {
        return Err(format!(
            "Goal owner_session_id 与当前 Session 不一致：{}",
            goal.owner_session_id
        ));
    }
    let status = match goal.status {
        AgentGoalStatus::Active => "active",
        AgentGoalStatus::Paused => "paused",
        // Source 没有 completed/blocked，分别使用其终态展示值；终态不会再提供
        // pause/resume 操作，避免 UI 对 Rust 不支持的状态迁移作出乐观假设。
        AgentGoalStatus::Completed => "verified",
        AgentGoalStatus::Blocked => "failed",
    };
    Ok(json!({
        "targetId": goal.id,
        "objective": goal.objective,
        "summaryTitle": goal.title,
        "timeUsedSeconds": goal.time_used_seconds,
        "activeRunStartedAtMs": Value::Null,
        "status": status,
        "iteration": 1,
        "verifications": [],
        "iterations": [],
    }))
}

/// 从同一 Session 的持久控制器读取 Goal。读取失败或 owner 不匹配必须向上返回，
/// 不能把故障伪装成 `goal: null`，否则 Source 会错误地显示无 Goal 并允许错误操作。
fn goal_projection_value(
    session: &RuntimeSession,
    goal_root: &Path,
    session_id: &str,
    child_view: bool,
) -> Result<Value, String> {
    if child_view {
        return Ok(Value::Null);
    }
    let persistent_state = PersistentAgentState::open_with_goal_root(session.clone(), goal_root)
        .map_err(|error| format!("读取 Session Goal 存储失败：{error}"))?;
    let snapshot = persistent_state
        .goal_snapshot()
        .map_err(|error| format!("读取 Session Goal 快照失败：{error}"))?;
    goal_projection_value_from_snapshot(&snapshot, session_id)
}

fn goal_action_availability_value(goal: &Value, child_view: bool) -> (Value, Value) {
    if child_view {
        let unavailable = json!({"allowed": false, "reasonCode": "agent.readOnly"});
        return (unavailable.clone(), unavailable);
    }
    match goal.get("status").and_then(Value::as_str) {
        Some("active" | "verifying" | "notSatisfied") => (
            json!({"allowed": true}),
            json!({"allowed": false, "reasonCode": "goal.notPaused"}),
        ),
        Some("paused") => (
            json!({"allowed": false, "reasonCode": "goal.notActive"}),
            json!({"allowed": true}),
        ),
        Some("verified" | "failed") => {
            let unavailable = json!({"allowed": false, "reasonCode": "goal.terminal"});
            (unavailable.clone(), unavailable)
        }
        _ => {
            let unavailable = json!({"allowed": false, "reasonCode": "goal.notFound"});
            (unavailable.clone(), unavailable)
        }
    }
}

/// 将父 Session 的单层 core subagent 事实投影为 Source 的 snapshot 目录。
///
/// 普通 subagent 没有第二个 Runtime Session；`childSessionIds` 使用稳定的
/// virtual view ID，前端再按该 ID 请求父 Journal 上的只读视图。child view
/// 必须保持空目录，避免把父级 Agent 再递归显示成自己的子 Agent。
fn subagent_projection_value(runtime_state: &SessionState, child_view: bool) -> Value {
    if child_view {
        return json!({
            "revision": 0,
            "childSessionIds": [],
            "running": [],
            "endedTotal": 0,
        });
    }

    let mut child_session_ids = Vec::new();
    let mut running = Vec::new();
    let mut ended_total = 0usize;

    for agent in runtime_state.sub_agents.values() {
        // reducer 已禁止递归 Agent；这里再次过滤可避免损坏 Journal 泄露嵌套
        // view，并与 listSessionSubagents 的单层协议保持一致。
        if agent.parent_agent_id.as_str() != ROOT_AGENT_ID {
            continue;
        }

        let child_session_id =
            virtual_agent_view_id(runtime_state.session_id.as_str(), &agent.agent_id);
        child_session_ids.push(child_session_id.clone());
        let turn = agent
            .current_turn_id
            .as_ref()
            .and_then(|turn_id| runtime_state.turns.get(turn_id));
        let status = match agent.status {
            SubAgentStatus::Pending | SubAgentStatus::Waiting => Some("waiting"),
            SubAgentStatus::Running => Some("running"),
            SubAgentStatus::Completed
            | SubAgentStatus::Failed
            | SubAgentStatus::Interrupted
            | SubAgentStatus::Stopped => None,
        };

        let Some(status) = status else {
            ended_total += 1;
            continue;
        };

        let mut item = json!({
            "childSessionId": child_session_id,
            "agentId": agent.agent_id.as_str(),
            "subagentType": "agent",
            "title": agent.agent_path,
            "status": status,
        });
        if let Some(summary) = &agent.result_summary {
            item["summary"] = summary.clone().into();
        }
        if let Some(turn) = turn {
            item["startedAt"] = turn.started_at_unix_ms.into();
        }
        running.push(item);
    }

    json!({
        "revision": runtime_state.last_sequence,
        "childSessionIds": child_session_ids,
        "running": running,
        "endedTotal": ended_total,
    })
}

fn conversation_snapshot_from_state_with_pending_and_mode_for_agent(
    input: ConversationSnapshotInput<'_>,
) -> Value {
    let ConversationSnapshotInput {
        session_id,
        runtime_state,
        transcript,
        log_epoch,
        workflow_snapshot,
        pending_interactions,
        permission_mode,
        goal,
        agent_id,
        workspace_hook_admission,
        terminal_sequences,
    } = input;
    let state = serde_json::to_value(runtime_state).unwrap_or_else(|_| json!({}));
    let active = agent_id.map_or_else(
        || active_turn(&state),
        |id| active_turn_for_agent(runtime_state, id),
    );
    let phase = agent_id.map_or_else(
        || phase_for_state(&state, active),
        |id| phase_for_agent(runtime_state, id, active),
    );
    let all_rows = rows_for_transcript_with_launches(
        transcript,
        runtime_state.created_at_unix_ms,
        runtime_state,
        agent_id.is_none(),
    );
    let first_row_id = all_rows
        .first()
        .and_then(|row| row.get("rowId"))
        .cloned()
        .unwrap_or(Value::Null);
    let rows = if all_rows.len() > DEFAULT_ROWS_WINDOW {
        all_rows[all_rows.len() - DEFAULT_ROWS_WINDOW..].to_vec()
    } else {
        all_rows.clone()
    };
    let plan = state
        .get("plan")
        .filter(|value| {
            value.get("enabled").and_then(Value::as_bool) == Some(true)
                || value
                    .get("planArtifact")
                    .is_some_and(|artifact| !artifact.is_null())
        })
        .map(|_| {
            // Runtime 的 PlanState 以 ArtifactUse 持有正文，V4 snapshot 的 plan
            // 只接受结构化 item；正文不在这里猜测，完整产物由 conversationPlans
            // 从同一 Artifact/Transcript 事实源返回。
            json!({"items": [], "updatedAt": runtime_state.updated_at_unix_ms})
        })
        .unwrap_or(Value::Null);
    let title = state
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let title_source = state
        .get("titleSource")
        .map(status_string)
        .map(|source| match source.as_str() {
            "manual" => "custom".to_owned(),
            "automatic" | "message_prefix" => "generated".to_owned(),
            _ => "default".to_owned(),
        })
        .unwrap_or_else(|| "default".to_owned());
    let child_view = agent_id.is_some();
    let goal = if child_view { Value::Null } else { goal };
    let workspace_hook_admission = if child_view {
        Value::Null
    } else {
        workspace_hook_admission
    };
    let last_error = session_last_error(runtime_state, active, agent_id, terminal_sequences);
    let (pause_goal, resume_goal) = goal_action_availability_value(&goal, child_view);
    let availability = if child_view {
        json!({
            "fork": {"allowed": false, "reasonCode": "agent.readOnly"},
            "compact": {"allowed": false, "reasonCode": "agent.readOnly"},
            "switchModelConfig": {"allowed": false, "reasonCode": "agent.readOnly"},
            "setFollowupMode": {"allowed": false, "reasonCode": "agent.readOnly"},
            "queueEdit": {"allowed": false, "reasonCode": "agent.readOnly"},
            "sendQueuedNow": {"allowed": false, "reasonCode": "agent.readOnly"},
            "pauseGoal": pause_goal,
            "resumeGoal": resume_goal,
        })
    } else {
        json!({
            "fork": {"allowed": true},
            "compact": {"allowed": true},
            "switchModelConfig": {"allowed": true},
            "setFollowupMode": {"allowed": true},
            "queueEdit": {"allowed": true},
            "sendQueuedNow": {"allowed": true},
            "pauseGoal": pause_goal,
            "resumeGoal": resume_goal,
        })
    };
    let input_routing = if child_view {
        "reject"
    } else if active {
        match runtime_state.followup_mode {
            FollowupMode::Queue => "enqueue",
            FollowupMode::Guide => "guide",
        }
    } else if !runtime_state.input_queue.auto_drain && !runtime_state.input_queue.items.is_empty() {
        "choice"
    } else {
        "startNow"
    };
    let title = if let Some(agent_id) = agent_id {
        runtime_state
            .sub_agents
            .get(agent_id)
            .map(|agent| agent.agent_path.as_str())
            .unwrap_or("subagent")
    } else {
        title
    };
    let background_works = if child_view {
        Value::Array(Vec::new())
    } else {
        workflow_background_works(runtime_state, &workflow_snapshot)
    };
    json!({
        "protocolVersion": SNAPSHOT_PROTOCOL_VERSION,
        "sessionId": session_id,
        "logEpoch": log_epoch,
        "seq": runtime_state.last_sequence,
        "revision": runtime_state.transcript_revision,
        "control": {
            "phase": phase,
            "sessionEnded": matches!(phase, "completedSuccess" | "completedInterrupted" | "error"),
            "canStop": active,
            "stopState": if active { "stoppable" } else { "idle" },
            "stopTargetKind": if active { "assistant" } else { "unknown" },
            "activeWorks": if active { json!([{"kind":"primaryTurn","startedAt": runtime_state.updated_at_unix_ms}]) } else { json!([]) },
            "lastError": last_error,
            "apiRetry": Value::Null,
        },
        "availability": availability,
        "inputRouting": if child_view {
            json!({"mode": "reject", "reasonCode": "agent.readOnly"})
        } else {
            json!({"mode": input_routing})
        },
        "meta": {"title": title, "titleSource": title_source},
        "config": model_config(&state, runtime_state.followup_mode, permission_mode),
        "modelTransition": Value::Null,
        "usage": usage_snapshot(&state),
        "queue": if child_view { json!({"items": [], "autoDrain": false}) } else { input_queue_value(runtime_state) },
        "pendingInteractions": if child_view { Value::Array(Vec::new()) } else { pending_interactions },
        "pendingCommands": [],
        "backgroundWorks": background_works,
        "subagents": subagent_projection_value(runtime_state, child_view),
        "workflowRuns": if child_view { json!({"revision": 0, "runs": []}) } else { workflow_snapshot },
        "goal": goal,
        "plan": if child_view { Value::Null } else { plan },
        "workspaceHookAdmission": workspace_hook_admission,
        "rows": {"window": rows, "totalCount": all_rows.len(), "firstRowId": first_row_id},
    })
}

/// 将 Coordinator 的自动继续状态投影为 shared V4 绝对时间形状。
fn auto_resolution_value(auto_resolution: &PendingElicitationAutoResolution) -> Value {
    match auto_resolution {
        PendingElicitationAutoResolution::Active {
            started_at_unix_ms,
            visible_at_unix_ms,
            deadline_at_unix_ms,
        } => json!({
            "state": if now_epoch_ms() < *visible_at_unix_ms {
                "hiddenGrace"
            } else {
                "visibleCountdown"
            },
            "startedAt": started_at_unix_ms,
            "visibleAt": visible_at_unix_ms,
            "deadlineAt": deadline_at_unix_ms,
        }),
        PendingElicitationAutoResolution::Snoozed {
            started_at_unix_ms,
            snoozed_at_unix_ms,
        } => json!({
            "state": "snoozed",
            "startedAt": started_at_unix_ms,
            "snoozedAt": snoozed_at_unix_ms,
        }),
    }
}

/// 将 Coordinator 的真实问题 Schema 投影为 V4 userInput 形状。
fn pending_interaction_value(view: &PendingElicitationView) -> Value {
    let first = view.questions.first();
    let prompt = first
        .map(|question| question.prompt.clone())
        .unwrap_or_default();
    let first_options = first
        .map(|question| {
            question
                .options
                .iter()
                .map(|option| {
                    json!({"optionId": option.label.clone(), "label": option.label.clone()})
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let questions = view
        .questions
        .iter()
        .map(|question| {
            let options = question
                .options
                .iter()
                .map(|option| {
                    let mut value = json!({
                        "value": option.label.clone(),
                        "label": option.label.clone(),
                    });
                    if let Some(description) = &option.description {
                        value["description"] = Value::String(description.clone());
                    }
                    value
                })
                .collect::<Vec<_>>();
            json!({
                "question": question.prompt.clone(),
                "header": question.id.clone(),
                "options": options,
                "multiSelect": question.multi_select,
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "interactionId": view.request_id.clone(),
        "kind": "userInput",
        "anchorRowId": Value::Null,
        "createdAt": view.created_at_unix_ms,
        "payload": {
            "kind": "userInput",
            "prompt": prompt,
            "freeText": first.map(|question| question.allow_custom).unwrap_or(false),
            "options": first_options,
            "toolName": "AskUserQuestion",
            "toolCallId": view.tool_call_id.clone(),
            "questions": questions,
            "currentQuestionIndex": 0,
            "answerDrafts": {},
        },
    });
    if let Some(auto_resolution) = view.auto_resolution.as_ref() {
        value["autoResolution"] = auto_resolution_value(auto_resolution);
    }
    value
}

fn pending_permission_value(view: &PendingPermissionView) -> Value {
    view.to_v4_value()
}

/// 读取经 Runtime 按父 Session、连接及 actor 路由授权的 pending Interaction 数组。
pub fn pending_interactions_for_connection(
    runtime: &AgentRuntime,
    session_id: &str,
    connection_id: &ConnectionId,
) -> Result<Value, String> {
    let elicitation_views = runtime
        .pending_elicitation_views(session_id, connection_id)
        .map_err(|error| error.to_string())?;
    let permission_views = runtime
        .pending_permission_views(session_id, connection_id)
        .map_err(|error| error.to_string())?;
    let mut pending = elicitation_views
        .iter()
        .filter(|view| &view.connection_id == connection_id)
        .map(|view| {
            (
                view.created_at_unix_ms,
                view.request_id.as_str(),
                pending_interaction_value(view),
            )
        })
        .chain(permission_views.iter().map(|view| {
            (
                view.created_at_unix_ms,
                view.interaction_id.as_str(),
                pending_permission_value(view),
            )
        }))
        .collect::<Vec<_>>();
    pending.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    Ok(Value::Array(
        pending.into_iter().map(|(_, _, value)| value).collect(),
    ))
}

/// workspace Hook review 不属于 AskUser/permission coordinator；这里只把独立
/// Journal 中已绑定当前 session/connection 的 immutable payload 投影成 V4 interaction。
fn append_workspace_hook_pending(
    pending: &mut Value,
    app: &tauri::AppHandle,
    session_id: &str,
    connection_id: &ConnectionId,
) -> Result<(), String> {
    let values = pending
        .as_array_mut()
        .ok_or_else(|| "pending interactions 必须是数组".to_owned())?;
    let coordinator =
        crate::frontend_rpc::workspace_hook_review::WorkspaceHookReviewCoordinator::for_app(app)?;
    let hook_payloads = coordinator.pending_for_session(
        session_id,
        Some(connection_id.as_str()),
        now_epoch_ms(),
    )?;
    values.extend(hook_payloads.into_iter().map(|payload| {
        json!({
            "interactionId": payload.get("interactionId").cloned().unwrap_or(Value::Null),
            "kind": "workspaceHookReview",
            "anchorRowId": Value::Null,
            "createdAt": payload.get("createdAt").cloned().unwrap_or(Value::Null),
            "payload": payload,
        })
    }));
    values.sort_by(|left, right| {
        left.get("createdAt")
            .and_then(Value::as_u64)
            .cmp(&right.get("createdAt").and_then(Value::as_u64))
            .then_with(|| {
                left.get("interactionId")
                    .and_then(Value::as_str)
                    .cmp(&right.get("interactionId").and_then(Value::as_str))
            })
    });
    Ok(())
}

fn session_snapshot_for_runtime(
    runtime: &AgentRuntime,
    app: &tauri::AppHandle,
    requested_session_id: &str,
    parent_session_id: &str,
    agent_id: Option<&AgentId>,
    log_epoch: &str,
    connection_id: Option<&ConnectionId>,
) -> Result<Value, String> {
    let session = open_authorized_session(runtime, app, parent_session_id)?;
    let runtime_snapshot = session.snapshot().map_err(|error| error.to_string())?;
    // TurnState 不复制 Journal sequence；一次分页读取提供相同时间戳下的真实终态顺序，
    // 同一份 map 同时供有连接和无连接 snapshot 使用，避免重复读日志。
    let terminal_sequences = session
        .turn_terminal_sequences()
        .map_err(|error| error.to_string())?;
    let reverted_turn_ids = super::command_handlers::completed_rewind_turn_ids(
        runtime.storage_root(),
        parent_session_id,
    )?;
    let goal = goal_projection_value(
        &session,
        runtime.storage_root(),
        parent_session_id,
        agent_id.is_some(),
    )?;
    let transcript_messages = match agent_id {
        Some(agent_id) => runtime_snapshot
            .state
            .effective_transcript(agent_id)
            .map_err(|error| error.to_string())?,
        None => session.transcript().map_err(|error| error.to_string())?,
    };
    let transcript = transcript_messages
        .into_iter()
        .map(|message| serde_json::to_value(message).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    // Hook admission 只从已授权 Session 的持久 project_root 读取；virtual agent
    // view 不得看到父会话的 workspace trust banner，也不得从 renderer 参数推断路径。
    let workspace_hook_admission = if agent_id.is_some() {
        Value::Null
    } else {
        let metadata = runtime
            .runtime_manager()
            .stored_session_metadata(parent_session_id)
            .map_err(|error| error.to_string())?;
        crate::frontend_rpc::workspace_hook_review::workspace_hook_admission(
            app,
            Path::new(&metadata.project_root),
        )?
    };
    if let Some(connection_id) = connection_id {
        // Pending Interaction 没有 core-agent 身份字段；把父 Session 的问题投影到
        // virtual view 会造成跨 Agent 泄露，因此只在真实父会话中显示。
        let pending = if agent_id.is_some() {
            Value::Array(Vec::new())
        } else {
            let mut pending =
                pending_interactions_for_connection(runtime, parent_session_id, connection_id)?;
            append_workspace_hook_pending(&mut pending, app, parent_session_id, connection_id)?;
            pending
        };
        let workflow_snapshot = workflow_runs(
            &session,
            &serde_json::to_value(&runtime_snapshot.state).unwrap_or_else(|_| json!({})),
        );
        let permission_mode = runtime.permission_mode(parent_session_id).ok();
        let mut snapshot = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
            ConversationSnapshotInput {
                session_id: requested_session_id,
                runtime_state: &runtime_snapshot.state,
                transcript: &transcript,
                log_epoch,
                workflow_snapshot: if agent_id.is_some() {
                    json!({"revision": 0, "runs": []})
                } else {
                    workflow_snapshot
                },
                pending_interactions: pending,
                permission_mode,
                goal: goal.clone(),
                agent_id,
                workspace_hook_admission,
                terminal_sequences: Some(&terminal_sequences),
            },
        );
        let projected_rows = rows_for_transcript_with_file_changes(
            &transcript,
            runtime_snapshot.state.created_at_unix_ms,
            &runtime_snapshot.state,
            &session,
            &reverted_turn_ids,
            agent_id.is_none(),
        )?;
        let first_row_id = projected_rows
            .first()
            .and_then(|row| row.get("rowId"))
            .cloned()
            .unwrap_or(Value::Null);
        let window = if projected_rows.len() > DEFAULT_ROWS_WINDOW {
            projected_rows[projected_rows.len() - DEFAULT_ROWS_WINDOW..].to_vec()
        } else {
            projected_rows.clone()
        };
        snapshot["rows"] = json!({
            "window": window,
            "totalCount": projected_rows.len(),
            "firstRowId": first_row_id,
        });
        snapshot["control"]["apiRetry"] =
            active_model_retry_value(&session, &runtime_snapshot.state, agent_id);
        Ok(snapshot)
    } else {
        Ok(conversation_state_snapshot(
            requested_session_id,
            &session,
            &runtime_snapshot,
            &transcript,
            log_epoch,
            goal,
            Some(&terminal_sequences),
        ))
    }
}

/// Journal-backed workflow run enumeration used by the standalone V4 query.
pub fn workflow_runs_query(
    session: &RuntimeSession,
    requested_limit: usize,
) -> Result<Value, String> {
    let snapshot = session.snapshot().map_err(|error| error.to_string())?;
    let state = serde_json::to_value(snapshot.state).map_err(|error| error.to_string())?;
    let mut runs = workflow_runs(session, &state)
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    runs.reverse();
    runs.truncate(requested_limit.clamp(1, 64));
    Ok(json!({"runs": runs}))
}

/// 为真实父 Session 或已授权的 core subagent view 生成首帧/重同步快照。
pub(crate) fn conversation_frame_for_connection_with_agent(
    runtime: &AgentRuntime,
    app: &tauri::AppHandle,
    scope: &ConversationScope,
    connection_id: &str,
    topic: &str,
    subscription_id: &str,
    log_epoch: &str,
) -> Result<Value, String> {
    let connection_id = keencode_acp::ConnectionId::new(connection_id.to_owned())
        .map_err(|error| error.to_string())?;
    let snapshot = session_snapshot_for_runtime(
        runtime,
        app,
        &scope.requested_session_id,
        &scope.parent_session_id,
        scope.agent_id.as_ref(),
        log_epoch,
        Some(&connection_id),
    )?;
    let to_seq = snapshot.get("seq").and_then(Value::as_u64).unwrap_or(0);
    Ok(json!({
        "topic": topic,
        "subscriptionId": subscription_id,
        "fromSeq": 0,
        "toSeq": to_seq,
        "sentAt": now_epoch_ms(),
        "payload": {"kind": "snapshot", "snapshot": snapshot},
    }))
}

/// 读取父 Journal 中指定 core subagent 的有效 Transcript 行；不创建或打开子 Session。
pub(crate) fn rows_range_with_agent(
    runtime: &AgentRuntime,
    app: &tauri::AppHandle,
    scope: &ConversationScope,
    log_epoch: &str,
    before_row_id: Option<u64>,
    requested_limit: usize,
) -> Result<Value, String> {
    let limit = requested_limit.clamp(1, MAX_ROWS_RANGE);
    let session = open_authorized_session(runtime, app, &scope.parent_session_id)?;
    let runtime_snapshot = session.snapshot().map_err(|error| error.to_string())?;
    let transcript_messages = match scope.agent_id.as_ref() {
        Some(agent_id) => runtime_snapshot
            .state
            .effective_transcript(agent_id)
            .map_err(|error| error.to_string())?,
        None => session.transcript().map_err(|error| error.to_string())?,
    };
    let transcript = transcript_messages
        .into_iter()
        .map(|message| serde_json::to_value(message).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let reverted_turn_ids = super::command_handlers::completed_rewind_turn_ids(
        runtime.storage_root(),
        &scope.parent_session_id,
    )?;
    let rows = rows_for_transcript_with_file_changes(
        &transcript,
        runtime_snapshot.state.created_at_unix_ms,
        &runtime_snapshot.state,
        &session,
        &reverted_turn_ids,
        scope.agent_id.is_none(),
    );
    let rows = rows?;
    let mut candidates = rows
        .iter()
        .filter(|row| {
            before_row_id.is_none_or(|before| {
                row.get("rowId")
                    .and_then(Value::as_u64)
                    .is_some_and(|id| id < before)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let has_more = candidates.len() > limit;
    if has_more {
        let start = candidates.len() - limit;
        candidates = candidates.split_off(start);
    }
    let next_before = candidates
        .first()
        .and_then(|row| row.get("rowId"))
        .and_then(Value::as_u64);
    let at_seq = runtime_snapshot.state.last_sequence;
    let at_revision = runtime_snapshot.state.transcript_revision;
    Ok(json!({
        "rows": candidates,
        "atSeq": at_seq,
        "atRevision": at_revision,
        "atLogEpoch": log_epoch,
        "sessionId": scope.requested_session_id,
        "hasMore": has_more && next_before.is_some(),
    }))
}

/// 读取当前有效分支中的 ExitPlanMode 终态行。
///
/// 计划目录不能从尾窗推导；它必须从完整 Transcript 读取已提交工具调用，
/// `SessionState::plan.plan_artifact` 则是重启后仍存在但尚未物化为消息的真实计划
/// 产物兜底。两者都保留原始 Artifact/参数，不在前端复制计划事实。
pub fn conversation_plans(session: &RuntimeSession, log_epoch: &str) -> Result<Value, String> {
    let snapshot = session.snapshot().map_err(|error| error.to_string())?;
    let transcript = session
        .transcript()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|message| serde_json::to_value(message).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = rows_for_transcript(
        &transcript,
        snapshot.state.created_at_unix_ms,
        &snapshot.state,
    );
    let mut plans = rows
        .into_iter()
        .filter(|row| {
            row.get("kind") == Some(&Value::String("toolCall".to_owned()))
                && row
                    .get("toolName")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.eq_ignore_ascii_case("exitplanmode"))
                && row.get("status").and_then(Value::as_str) == Some("success")
        })
        .collect::<Vec<_>>();
    if plans.is_empty()
        && let Some(artifact_ref) = snapshot.state.plan.plan_artifact.as_ref()
        && let Some(artifact) = serde_json::to_value(artifact_ref).ok()
    {
        let artifact_id = artifact
            .get("artifactId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("plan-artifact");
        // 计划正文只能由 Runtime 按当前 state.plan.plan_artifact 授权读取；前端传入的
        // Artifact ID 不参与读取，避免把任意 Session 产物暴露为 plan 行。
        let markdown = session
            .read_plan_artifact()
            .map_err(|error| error.to_string())?
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|text| !text.trim().is_empty());
        // Plan artifact 没有单独的 Transcript message identity，使用专用持久投影
        // 区间，避免它与未来追加的普通消息基址相撞。
        let row_id = 1_u64 << 51;
        let arguments = markdown
            .as_deref()
            .map(|content| json!({"plan": content, "artifactId": artifact_id}))
            .unwrap_or_else(|| json!({"planArtifact": artifact}));
        plans.push(flatten_row(json!({
            "kind": "toolCall",
            "assistantResponseId": format!("plan-{artifact_id}"),
            "toolCallId": format!("plan-{artifact_id}"),
            "toolName": "ExitPlanMode",
            "status": "success",
            "inputText": arguments.to_string(),
            "input": arguments,
            "row": row_common(
                row_id,
                "session",
                &format!("plan-{artifact_id}"),
                snapshot.state.updated_at_unix_ms,
                row_id,
            ),
        })));
    }
    plans.sort_by_key(|row| {
        std::cmp::Reverse(row.get("rowId").and_then(Value::as_u64).unwrap_or(0))
    });
    Ok(json!({
        "plans": plans,
        "atSeq": snapshot.state.last_sequence,
        "atLogEpoch": log_epoch,
    }))
}

fn metadata_phase(metadata: &Value, snapshot_state: Option<&Value>, active: bool) -> &'static str {
    // metadata.json 只有 SessionStatus；真实 RuntimeSnapshot 才带 turns，能够
    // 区分 completedSuccess、completedInterrupted 和尚未开始的 cold draft。
    phase_for_state(snapshot_state.unwrap_or(metadata), active)
}

/// 判断 Session 是否已经产生了可进入 tasks-index 的持久用户事实。
///
/// `SessionCreated` 只代表 workspace 预热，不能凭 metadata 行生成侧栏任务。首个
/// 用户输入可能先进入可冷恢复队列而尚未形成 Turn，因此同时采信 Journal 中的
/// queue/admission、Transcript、Turn 以及它们派生的持久事实；不采信标题、模型选择、
/// pin/archive 或 createSession command receipt，避免把未提升的 draft 当成任务。
pub(crate) fn is_promoted_task_state(state: &SessionState) -> bool {
    !state.turns.is_empty()
        || !state.transcript.is_empty()
        || !state.input_queue.items.is_empty()
        || !state.input_queue.completions.is_empty()
        || !state.dynamic_input_receipts.is_empty()
        || !state.model_rounds.is_empty()
        || !state.tools.is_empty()
        || !state.terminals.is_empty()
        || !state.todos.items.is_empty()
        || state.plan.plan_artifact.is_some()
        || !state.workflow_events.is_empty()
        || !state.sub_agents.is_empty()
        || !state.mailbox.is_empty()
        || !state.worktrees.is_empty()
}

pub fn sessions_index_snapshot(
    runtime: &AgentRuntime,
    workspace_path: &str,
    workspace_id: &str,
    log_epoch: &str,
) -> Result<Value, String> {
    sessions_index_snapshot_for_connection(runtime, workspace_path, workspace_id, log_epoch, None)
}

/// 生成按连接裁剪 pendingInteraction 摘要的 sessions-index 快照。
///
/// 列表只携带 kind/count 和工具名，不泄露问题正文；完整问题仍只从对应
/// conversation topic 的 connection-scoped snapshot 读取。
pub fn sessions_index_snapshot_for_connection(
    runtime: &AgentRuntime,
    workspace_path: &str,
    workspace_id: &str,
    log_epoch: &str,
    connection_id: Option<&ConnectionId>,
) -> Result<Value, String> {
    // Session metadata 使用 Runtime 创建时的规范根目录；前端可能传入同目录的
    // Windows 8.3/长路径别名或不同分隔符，先按真实文件系统归一化才能命中索引。
    let workspace_root = crate::workspace::canonical_session_root(workspace_path)?;
    let workspace_path = workspace_root.to_string_lossy().into_owned();
    let metadata = runtime
        .stored_sessions_for_project(Some(&workspace_path))
        .map_err(|error| error.to_string())?;
    let mut sessions = Vec::new();
    for item in metadata {
        if item.corrupt {
            continue;
        }
        let value = serde_json::to_value(&item).map_err(|error| error.to_string())?;
        let active = runtime
            .session_has_active_work(item.session_id.as_str())
            .unwrap_or(false);
        let snapshot_state = runtime
            .runtime_manager()
            .stored_session_snapshot(item.session_id.as_str())
            .ok()
            .and_then(|snapshot| serde_json::to_value(snapshot.state).ok());
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title_source = value
            .get("titleSource")
            .map(status_string)
            .map(|source| match source.as_str() {
                "manual" => "custom",
                "automatic" | "message_prefix" => "generated",
                _ => "default",
            })
            .unwrap_or("default");
        let phase = metadata_phase(&value, snapshot_state.as_ref(), active);
        let preview = runtime
            .session_transcript(item.session_id.as_str())
            .ok()
            .and_then(|messages| {
                messages.into_iter().rev().find_map(|message| {
                    let value = serde_json::to_value(message).ok()?;
                    (role_name(&value) == "assistant").then(|| {
                        let text = text_from_content(value.get("content").unwrap_or(&Value::Null));
                        text.chars().take(120).collect::<String>()
                    })
                })
            });
        let pending_views = connection_id
            .and_then(|connection_id| {
                runtime
                    .pending_elicitation_views(item.session_id.as_str(), connection_id)
                    .ok()
            })
            .unwrap_or_default();
        let pending_permission_views = connection_id
            .and_then(|connection_id| {
                runtime
                    .pending_permission_views(item.session_id.as_str(), connection_id)
                    .ok()
            })
            .unwrap_or_default();
        // 只有当前连接已经读取到权限正文时才读取总数，避免错误连接看到另一个窗口的计数。
        let pending_permission_count = if pending_permission_views.is_empty() {
            0
        } else {
            runtime.pending_permission_count(item.session_id.as_str())
        };
        let pending_summary = if pending_views.is_empty() && pending_permission_count == 0 {
            None
        } else {
            Some(json!({
                "permissionCount": pending_permission_count,
                "userInputCount": pending_views.len(),
            }))
        };
        let user_pending_summary = |user: &PendingElicitationView| {
            let mut value = json!({
                "interactionId": user.request_id,
                "kind": "userInput",
                "toolName": "AskUserQuestion",
            });
            if let Some(auto_resolution) = user.auto_resolution.as_ref() {
                value["autoResolution"] = auto_resolution_value(auto_resolution);
            }
            value
        };
        let pending_interaction = match (pending_views.first(), pending_permission_views.first()) {
            (Some(user), Some(permission))
                if user.created_at_unix_ms <= permission.created_at_unix_ms =>
            {
                Some(user_pending_summary(user))
            }
            (Some(user), None) => Some(user_pending_summary(user)),
            (_, Some(permission)) => Some(json!({
                "interactionId": permission.interaction_id,
                "kind": "permission",
                "toolName": permission.tool_name,
            })),
            (None, None) => None,
        };
        let mut summary = json!({
            "sessionId": item.session_id,
            "workspaceId": workspace_id,
            "title": title,
            "titleSource": title_source,
            "phase": phase,
            "sessionEnded": matches!(phase, "completedSuccess" | "completedInterrupted" | "error"),
            "hasBackgroundWork": active,
            "lastActivityAt": item.updated_at_unix_ms,
            "createdAt": item.created_at_unix_ms,
        });
        // source schema 将该字段定义为 optional；没有 assistant 文本时省略，
        // 不能序列化成 null 让严格解析器误判为 malformed snapshot。
        if let Some(preview) = preview {
            summary["lastAssistantPreview"] = Value::String(preview);
        }
        if let Some(pending_summary) = pending_summary {
            summary["pendingInteractionSummary"] = pending_summary;
        }
        if let Some(pending_interaction) = pending_interaction {
            summary["pendingInteraction"] = pending_interaction;
        }
        sessions.push(summary);
    }
    Ok(json!({
        "protocolVersion": SNAPSHOT_PROTOCOL_VERSION,
        "workspaceId": workspace_id,
        "logEpoch": log_epoch,
        "sessions": sessions,
    }))
}

/// 为已绑定连接生成包含 pendingInteraction 摘要的 sessions-index 帧。
pub fn sessions_index_frame_for_connection(
    runtime: &AgentRuntime,
    workspace_path: &str,
    workspace_id: &str,
    subscription_id: &str,
    log_epoch: &str,
    connection_id: Option<&ConnectionId>,
) -> Result<Value, String> {
    let snapshot = sessions_index_snapshot_for_connection(
        runtime,
        workspace_path,
        workspace_id,
        log_epoch,
        connection_id,
    )?;
    Ok(json!({
        "topic": format!("sessions-index/{workspace_id}"),
        "subscriptionId": subscription_id,
        "fromSeq": 0,
        "toSeq": 0,
        "sentAt": now_epoch_ms(),
        "payload": {"kind": "snapshot", "snapshot": snapshot},
    }))
}

pub fn workspace_config_snapshot(
    app: &tauri::AppHandle,
    workspace_id: &str,
) -> Result<Value, String> {
    // 模型目录属于 conversation.config 的 Session 事实；workspace-config 只发布
    // workspace mode 与 slash command 目录，和源 V4 `v4-workspace-config.ts` 同构。
    let _ = app;
    Ok(workspace_config_snapshot_value(workspace_id))
}

fn workspace_config_snapshot_value(workspace_id: &str) -> Value {
    json!({
        "protocolVersion": SNAPSHOT_PROTOCOL_VERSION,
        "workspaceId": workspace_id,
        "logEpoch": format!("workspace-{workspace_id}"),
        "config": {
            "configOptions": [
                {
                    "id": "mode",
                    "name": "Mode",
                    "category": "mode",
                    "type": "select",
                    "currentValue": "build",
                    "options": [
                        {"value":"build","name":"Ask before changes","description":"Ask before each file changes."},
                        {"value":"edit","name":"Edit automatically","description":"Edit selected files or relevant workspace files automatically."},
                        {"value":"plan","name":"Plan mode","description":"Inspect the code and present a plan before editing."},
                        {"value":"yolo","name":"Full access","description":"Edit and run commands with fewer confirmations."},
                    ],
                },
            ],
            "slashCommands": protocol_slash_commands(),
        },
    })
}

/// 与源 `listAppProtocolBuiltinSlashCommands` 同顺序的内置协议命令。
///
/// 自定义 command 由根装配的已注册 catalog 追加；这里保留源内置目录，确保冷启动时
/// `readWorkspacePresentation` 和 workspace-config 不再返回无意义空目录。
pub fn protocol_slash_commands() -> Vec<Value> {
    vec![
        json!({
            "name": "goal",
            "description": "Show or set the current session goal.",
            "inputHint": "/goal [pause|resume|clear|replace <objective>|<objective>]",
            "source": "builtin",
        }),
        json!({
            "name": "workflow",
            "description": "Design and launch a dynamic workflow for a task.",
            "inputHint": "/workflow [what the workflow should accomplish]",
            "source": "builtin",
        }),
        json!({
            "name": "compact",
            "description": "Compact the current conversation with optional instructions.",
            "inputHint": "/compact [instructions]",
            "source": "builtin",
        }),
        json!({
            "name": "init",
            "description": "Create or update workspace AGENTS.md instructions.",
            "inputHint": "/init [notes]",
            "source": "builtin",
        }),
        json!({
            "name": "plan",
            "description": "Switch to Plan mode and optionally send a task.",
            "inputHint": "/plan [task]",
            "source": "builtin",
        }),
    ]
}

/// `workspace/readPresentation` 的严格结果。mode 和 slash command 字段与源协议保持
/// 相同值域，workspace identity 原样透传以便前端做归属校验。
pub fn workspace_presentation(workspace: Value) -> Value {
    json!({
        "workspace": workspace,
        "mode": "build",
        "slashCommands": protocol_slash_commands(),
    })
}

pub fn validate_workspace(
    runtime: &AgentRuntime,
    session_id: &str,
    workspace_path: &str,
) -> Result<(), String> {
    let metadata = runtime
        .runtime_manager()
        .stored_session_metadata(session_id)
        .map_err(|error| error.to_string())?;
    // 前端路径可能来自 Windows 8.3/长路径别名、斜杠转换或 verbatim 前缀；词法比较
    // 会把同一个目录误判成跨 workspace。两边都经过真实文件系统规范化后再比较，
    // 同时保留目录不存在时的拒绝行为，避免把路径别名修复成越权旁路。
    let stored_root = crate::workspace::canonical_session_root(&metadata.project_root)?;
    let requested_root = crate::workspace::canonical_session_root(workspace_path)?;
    if stored_root != requested_root {
        return Err("Session 不属于当前 workspace".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::command_receipt_query_result;
    use super::{
        ConversationSnapshotInput, MessageProjectionContext, conversation_log_epoch,
        conversation_row_is_assistant, conversation_row_turn,
        conversation_snapshot_from_state_with_pending_and_goal,
        conversation_snapshot_from_state_with_pending_and_mode_for_agent,
        goal_action_availability_value, goal_projection_value_from_snapshot,
        is_promoted_task_state, metadata_phase, parse_virtual_agent_view_id,
        pending_interaction_value, pending_permission_value, phase_for_state, row_for_message,
        rows_for_message, rows_for_transcript, rows_for_transcript_with_launches,
        session_last_error, sessions_index_snapshot_for_connection, tool_call_ids,
        tool_result_values, validate_conversation_query_target, virtual_agent_view_id,
        workflow_background_works, workflow_run_from_events, workspace_config_snapshot_value,
    };
    use crate::agent_runtime::AgentRuntime;
    use crate::elicitation::PendingElicitationView;
    use crate::permissions::PendingPermissionView;
    use crate::workflows::types::{
        WorkflowRunStatus, WorkflowRunSummary, progress_from_workflow_event,
    };
    use keencode_acp::ConnectionId;
    use keencode_agent::{GoalController, GoalDraft};
    use keencode_resources::{
        AgentId, AssistantFeedback, AssistantFeedbackRecord, COMMAND_RECEIPT_SCHEMA,
        CommandReceipt, CommandReceiptStatus, MessagePart, MessageRole, ProviderProtocolSnapshot,
        ProviderSnapshot, ROOT_AGENT_ID, ReasoningEffortSnapshot, SessionId, SessionInputDelivery,
        SessionInputDispatch, SessionInputKind, SessionInputQueueItem, SessionMessage,
        SessionState, SubAgentState, SubAgentStatus, ToolResultPart, TranscriptRecord, TurnId,
        TurnState, TurnStatus, WorkflowJournalEvent,
    };
    use keencode_runtime::PersistentAgentState;
    use keencode_tools::{UserQuestion, UserQuestionOption};
    use keencode_workflow::{EffectClass, Node, ToolNode, ValueExpr};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::env;
    use std::fs;
    use std::path::Path;

    #[test]
    fn sessions_index_phase_uses_snapshot_turn_terminal_state() {
        let metadata = json!({"status": "idle"});
        let completed = json!({
            "status": "closed",
            "turns": {
                "turn-1": {
                    "sourceAgentId": ROOT_AGENT_ID,
                    "startedAtUnixMs": 1,
                    "status": "completed"
                }
            }
        });
        let cancelled = json!({
            "status": "closed",
            "turns": {
                "turn-1": {
                    "sourceAgentId": ROOT_AGENT_ID,
                    "startedAtUnixMs": 1,
                    "status": "cancelled"
                }
            }
        });
        let cold_draft = json!({"status": "idle", "turns": {}});

        assert_eq!(
            metadata_phase(&metadata, Some(&completed), false),
            "completedSuccess"
        );
        assert_eq!(
            metadata_phase(&metadata, Some(&cancelled), false),
            "completedInterrupted"
        );
        assert_eq!(metadata_phase(&metadata, Some(&cold_draft), false), "draft");
        assert_eq!(metadata_phase(&metadata, None, false), "draft");
    }

    #[test]
    fn sessions_index_phase_ignores_child_turn_after_root_success() {
        let state = json!({
            "status": "closed",
            "turns": {
                "root-turn": {
                    "sourceAgentId": ROOT_AGENT_ID,
                    "startedAtUnixMs": 10,
                    "status": "completed"
                },
                "child-turn": {
                    "sourceAgentId": "child-agent",
                    "startedAtUnixMs": 20,
                    "status": "failed"
                }
            }
        });

        assert_eq!(phase_for_state(&state, false), "completedSuccess");
    }

    #[test]
    fn task_projection_requires_a_persisted_promotion_fact() {
        let mut prewarm = SessionState::empty(SessionId::new("prewarm-draft").unwrap());
        prewarm.created = true;
        assert!(!is_promoted_task_state(&prewarm));

        let mut workflow_parent = SessionState::empty(SessionId::new("workflow-parent").unwrap());
        workflow_parent.workflow_events.insert(
            "run-1".to_owned(),
            vec![WorkflowJournalEvent {
                run_id: "run-1".to_owned(),
                tool_call_id: "tool-1".to_owned(),
                sequence: 1,
                event_type: "run_started".to_owned(),
                payload: json!({}),
                artifacts: Vec::new(),
                actor_session_id: None,
                launch_input_id: None,
            }],
        );
        assert!(is_promoted_task_state(&workflow_parent));

        let mut queued = SessionState::empty(SessionId::new("queued-input").unwrap());
        queued.input_queue.items.push(SessionInputQueueItem {
            queue_item_id: "queue-1".to_owned(),
            source_command_id: "send-1".to_owned(),
            client_id: None,
            kind: SessionInputKind::SendText,
            text: "尚未绑定模型的输入".to_owned(),
            attachments: Vec::new(),
            model_selection: None,
            mode: None,
            plan_enabled: false,
            requested_delivery: SessionInputDelivery::Queue,
            admitted_delivery: SessionInputDelivery::Queue,
            admission_seq: 1,
            reserve_attempt: 0,
            dispatch: SessionInputDispatch::Queued,
            promoted_turn_id: None,
            admitted_at_unix_ms: 1,
        });
        assert!(is_promoted_task_state(&queued));

        let mut cold_real_task = SessionState::empty(SessionId::new("cold-real-task").unwrap());
        let turn_id = TurnId::new("turn-1").unwrap();
        cold_real_task.turns.insert(
            turn_id.clone(),
            keencode_resources::TurnState {
                turn_id: turn_id.clone(),
                source_agent_id: keencode_resources::AgentId::new(ROOT_AGENT_ID).unwrap(),
                root_turn_id: turn_id,
                parent_turn_id: None,
                prompt_summary: "冷恢复后的真实输入".to_owned(),
                started_at_unix_ms: 1,
                completed_at_unix_ms: Some(2),
                status: keencode_resources::TurnStatus::Completed,
                stop_reason: None,
                outcome_message: None,
            },
        );
        assert!(is_promoted_task_state(&cold_real_task));
    }

    #[test]
    fn goal_projection_reads_persisted_lifecycle_and_is_session_scoped() {
        let storage = tempfile::tempdir().expect("应创建 Goal 存储目录");
        let project = tempfile::tempdir().expect("应创建 Goal 项目目录");
        let runtime = AgentRuntime::new_for_control_test(storage.path())
            .expect("应创建 Goal projection Runtime");
        let first = runtime
            .open_or_create_session(project.path(), None, "goal-projection-first")
            .expect("首个 Goal Session 应创建");
        let second = runtime
            .open_or_create_session(project.path(), None, "goal-projection-second")
            .expect("第二 Goal Session 应创建");
        let first_state =
            PersistentAgentState::open_with_goal_root(first.clone(), runtime.storage_root())
                .expect("首个 Goal 控制器应打开");
        first_state
            .create_goal(
                "goal-projection-create",
                GoalDraft {
                    title: "投影生命周期".to_owned(),
                    objective: "验证 Goal 快照按 Session 持久隔离并可暂停恢复".to_owned(),
                    description: None,
                    token_budget: None,
                    progress_percent: None,
                },
            )
            .expect("Goal 应持久创建");

        let active_snapshot = first_state.goal_snapshot().expect("应读取 active Goal");
        let active =
            goal_projection_value_from_snapshot(&active_snapshot, first.session_id().as_str())
                .expect("active Goal 应可投影");
        assert_eq!(active["status"], "active");
        assert_eq!(active["summaryTitle"], "投影生命周期");
        assert_eq!(
            goal_action_availability_value(&active, false),
            (
                json!({"allowed": true}),
                json!({"allowed": false, "reasonCode": "goal.notPaused"})
            )
        );

        first_state
            .pause_goal("goal-projection-pause")
            .expect("Goal 应持久暂停");
        let paused_snapshot = first_state.goal_snapshot().expect("应读取 paused Goal");
        let paused =
            goal_projection_value_from_snapshot(&paused_snapshot, first.session_id().as_str())
                .expect("paused Goal 应可投影");
        assert_eq!(paused["status"], "paused");
        assert_eq!(
            goal_action_availability_value(&paused, false),
            (
                json!({"allowed": false, "reasonCode": "goal.notActive"}),
                json!({"allowed": true})
            )
        );

        let second_state =
            PersistentAgentState::open_with_goal_root(second.clone(), runtime.storage_root())
                .expect("第二个 Goal 控制器应打开");
        assert!(
            second_state
                .goal_snapshot()
                .expect("第二 Session Goal 应读取")
                .goal
                .is_none()
        );
        assert!(
            goal_projection_value_from_snapshot(&paused_snapshot, second.session_id().as_str())
                .is_err()
        );
        assert_eq!(
            goal_action_availability_value(&Value::Null, false),
            (
                json!({"allowed": false, "reasonCode": "goal.notFound"}),
                json!({"allowed": false, "reasonCode": "goal.notFound"})
            )
        );
    }

    #[test]
    fn workspace_aliases_resolve_to_the_same_session_root() {
        let base = env::temp_dir().join(format!(
            "keencode-workspace-alias-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&base).expect("create workspace alias fixture");
        let canonical = fs::canonicalize(&base).expect("canonicalize workspace alias fixture");
        let alternate = format!("{}{}", canonical.display(), std::path::MAIN_SEPARATOR);
        assert_eq!(
            crate::workspace::canonical_session_root(&alternate).expect("canonicalize alternate"),
            canonical
        );

        #[cfg(windows)]
        {
            let frontend_path = crate::path_utils::path_to_frontend(&canonical);
            let verbatim = format!(r"\\?\{}", frontend_path.replace('/', "\\"));
            assert_eq!(
                crate::workspace::canonical_session_root(&verbatim)
                    .expect("canonicalize verbatim alias"),
                canonical
            );
        }

        fs::remove_dir_all(&base).expect("remove workspace alias fixture");
    }

    #[test]
    fn virtual_agent_view_id_is_strictly_parent_scoped() {
        let agent_id = AgentId::new("child-reader").unwrap();
        let view_id = virtual_agent_view_id("session-parent", &agent_id);
        assert_eq!(view_id, "agent:session-parent:child-reader");
        let (parent, parsed_agent) = parse_virtual_agent_view_id(&view_id)
            .unwrap()
            .expect("virtual view 应解析");
        assert_eq!(parent, "session-parent");
        assert_eq!(parsed_agent, agent_id);
        assert!(parse_virtual_agent_view_id("agent:session-parent").is_err());
        assert!(parse_virtual_agent_view_id("agent:other:child-reader:extra").is_err());
        assert!(
            parse_virtual_agent_view_id("session-parent")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn virtual_agent_snapshot_is_read_only_and_has_no_parent_queue_or_workflow() {
        let mut state = SessionState::empty(SessionId::new("session-parent").unwrap());
        let agent_id = AgentId::new("child-reader").unwrap();
        state.sub_agents.insert(
            agent_id.clone(),
            SubAgentState {
                agent_id: agent_id.clone(),
                parent_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                agent_path: "/root/reader".to_owned(),
                task: "读取项目结构".to_owned(),
                status: SubAgentStatus::Running,
                current_turn_id: None,
                result_summary: None,
            },
        );
        let snapshot = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
            ConversationSnapshotInput {
                session_id: "agent:session-parent:child-reader",
                runtime_state: &state,
                transcript: &[],
                log_epoch: "epoch-agent",
                workflow_snapshot: json!({"revision": 0, "runs": []}),
                pending_interactions: json!([{"interactionId": "parent-only"}]),
                permission_mode: None,
                goal: Value::Null,
                agent_id: Some(&agent_id),
                workspace_hook_admission: Value::Null,
                terminal_sequences: None,
            },
        );
        assert_eq!(snapshot["sessionId"], "agent:session-parent:child-reader");
        assert_eq!(snapshot["control"]["phase"], "running");
        assert_eq!(snapshot["inputRouting"]["mode"], "reject");
        assert_eq!(snapshot["inputRouting"]["reasonCode"], "agent.readOnly");
        assert_eq!(snapshot["availability"]["fork"]["allowed"], false);
        assert_eq!(snapshot["queue"]["items"], json!([]));
        assert_eq!(snapshot["pendingInteractions"], json!([]));
        assert_eq!(snapshot["workflowRuns"]["runs"], json!([]));
        assert!(snapshot["usage"]["contextWindow"].is_null());
        assert_eq!(
            snapshot["usage"]["cumulative"],
            json!({
                "inputTokens": 0,
                "outputTokens": 0,
                "cacheReadTokens": 0,
                "cacheWriteTokens": 0,
            })
        );
        assert_eq!(
            snapshot["subagents"],
            json!({
                "revision": 0,
                "childSessionIds": [],
                "running": [],
                "endedTotal": 0,
            })
        );
    }

    #[test]
    fn session_error_projection_is_redacted_and_clears_for_newer_terminal_state() {
        let mut state = SessionState::empty(SessionId::new("error-projection").unwrap());
        state.updated_at_unix_ms = 300;
        let failed_id = TurnId::new("turn-failed").unwrap();
        state.turns.insert(
            failed_id.clone(),
            keencode_resources::TurnState {
                turn_id: failed_id,
                source_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                root_turn_id: TurnId::new("turn-failed-root").unwrap(),
                parent_turn_id: None,
                prompt_summary: "失败请求".to_owned(),
                started_at_unix_ms: 100,
                completed_at_unix_ms: Some(200),
                status: TurnStatus::Failed,
                stop_reason: Some(keencode_resources::TurnStopReason::Failed),
                outcome_message: Some(
                    "Authorization: Bearer super-secret; provider failed".to_owned(),
                ),
            },
        );
        let snapshot = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
            ConversationSnapshotInput {
                session_id: "error-projection",
                runtime_state: &state,
                transcript: &[],
                log_epoch: "epoch-error",
                workflow_snapshot: json!({"revision": 0, "runs": []}),
                pending_interactions: json!([]),
                permission_mode: None,
                goal: Value::Null,
                agent_id: None,
                workspace_hook_admission: Value::Null,
                terminal_sequences: None,
            },
        );
        assert_eq!(snapshot["control"]["lastError"]["code"], "turn_failed");
        assert_eq!(snapshot["control"]["lastError"]["source"], "runtime");
        assert!(
            snapshot["control"]["lastError"]["message"]
                .as_str()
                .expect("错误消息应为字符串")
                .contains("[REDACTED]")
        );
        assert!(
            !snapshot["control"]["lastError"]["message"]
                .as_str()
                .unwrap()
                .contains("super-secret")
        );

        let completed_id = TurnId::new("turn-completed").unwrap();
        state.turns.insert(
            completed_id.clone(),
            keencode_resources::TurnState {
                turn_id: completed_id,
                source_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                root_turn_id: TurnId::new("turn-completed-root").unwrap(),
                parent_turn_id: None,
                prompt_summary: "新请求".to_owned(),
                started_at_unix_ms: 400,
                completed_at_unix_ms: Some(500),
                status: TurnStatus::Completed,
                stop_reason: None,
                outcome_message: None,
            },
        );
        let cleared = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
            ConversationSnapshotInput {
                session_id: "error-projection",
                runtime_state: &state,
                transcript: &[],
                log_epoch: "epoch-error",
                workflow_snapshot: json!({"revision": 0, "runs": []}),
                pending_interactions: json!([]),
                permission_mode: None,
                goal: Value::Null,
                agent_id: None,
                workspace_hook_admission: Value::Null,
                terminal_sequences: None,
            },
        );
        assert_eq!(cleared["control"]["lastError"], Value::Null);

        // 后台子 Agent 在根 Turn 终态之后失败时，父视图不能被子错误污染；
        // 只有显式 child view 才能看到该 Agent 自己的失败事实。
        let child_id = AgentId::new("child-late-failure").unwrap();
        let child_turn_id = TurnId::new("turn-child-late-failure").unwrap();
        state.sub_agents.insert(
            child_id.clone(),
            SubAgentState {
                agent_id: child_id.clone(),
                parent_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                agent_path: "/root/child-late-failure".to_owned(),
                task: "后台失败".to_owned(),
                status: SubAgentStatus::Failed,
                current_turn_id: Some(child_turn_id.clone()),
                result_summary: None,
            },
        );
        state.turns.insert(
            child_turn_id.clone(),
            keencode_resources::TurnState {
                turn_id: child_turn_id,
                source_agent_id: child_id.clone(),
                root_turn_id: TurnId::new("turn-completed").unwrap(),
                parent_turn_id: Some(TurnId::new("turn-completed").unwrap()),
                prompt_summary: "后台失败".to_owned(),
                started_at_unix_ms: 600,
                completed_at_unix_ms: Some(700),
                status: TurnStatus::Failed,
                stop_reason: Some(keencode_resources::TurnStopReason::Failed),
                outcome_message: Some("child secret Authorization: Bearer hidden".to_owned()),
            },
        );
        let parent_after_child_failure =
            conversation_snapshot_from_state_with_pending_and_mode_for_agent(
                ConversationSnapshotInput {
                    session_id: "error-projection",
                    runtime_state: &state,
                    transcript: &[],
                    log_epoch: "epoch-error",
                    workflow_snapshot: json!({"revision": 0, "runs": []}),
                    pending_interactions: json!([]),
                    permission_mode: None,
                    goal: Value::Null,
                    agent_id: None,
                    workspace_hook_admission: Value::Null,
                    terminal_sequences: None,
                },
            );
        assert_eq!(
            parent_after_child_failure["control"]["lastError"],
            Value::Null
        );
        let child_snapshot = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
            ConversationSnapshotInput {
                session_id: "agent:error-projection:child-late-failure",
                runtime_state: &state,
                transcript: &[],
                log_epoch: "epoch-error-child",
                workflow_snapshot: json!({"revision": 0, "runs": []}),
                pending_interactions: json!([]),
                permission_mode: None,
                goal: Value::Null,
                agent_id: Some(&child_id),
                workspace_hook_admission: Value::Null,
                terminal_sequences: None,
            },
        );
        assert_eq!(
            child_snapshot["control"]["lastError"]["code"],
            "turn_failed"
        );
        assert!(
            child_snapshot["control"]["lastError"]["message"]
                .as_str()
                .unwrap()
                .contains("[REDACTED]")
        );
    }

    #[test]
    fn session_error_projection_uses_journal_order_for_equal_timestamps() {
        let mut state = SessionState::empty(SessionId::new("error-order").unwrap());
        for (turn_id, message) in [
            ("turn-first", "first journal failure"),
            ("turn-latest", "latest journal failure"),
        ] {
            let turn_id = TurnId::new(turn_id).unwrap();
            state.turns.insert(
                turn_id.clone(),
                keencode_resources::TurnState {
                    turn_id,
                    source_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                    root_turn_id: TurnId::new("root").unwrap(),
                    parent_turn_id: None,
                    prompt_summary: "same timestamp".to_owned(),
                    started_at_unix_ms: 100,
                    completed_at_unix_ms: Some(200),
                    status: TurnStatus::Failed,
                    stop_reason: Some(keencode_resources::TurnStopReason::Failed),
                    outcome_message: Some(message.to_owned()),
                },
            );
        }
        let terminal_sequences = BTreeMap::from([
            ("turn-first".to_owned(), (10, 0)),
            ("turn-latest".to_owned(), (11, 0)),
        ]);
        let error = session_last_error(&state, false, None, Some(&terminal_sequences));
        assert_eq!(error["message"], "latest journal failure");
    }

    #[test]
    fn parent_snapshot_projects_single_level_subagents_from_session_state() {
        let mut state = SessionState::empty(SessionId::new("session-parent").unwrap());
        state.last_sequence = 19;
        let running_id = AgentId::new("child-running").unwrap();
        let running_turn_id = TurnId::new("turn-child-running").unwrap();
        state.sub_agents.insert(
            running_id.clone(),
            SubAgentState {
                agent_id: running_id.clone(),
                parent_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                agent_path: "/root/reader".to_owned(),
                task: "读取项目结构".to_owned(),
                status: SubAgentStatus::Running,
                current_turn_id: Some(running_turn_id.clone()),
                result_summary: None,
            },
        );
        state.turns.insert(
            running_turn_id.clone(),
            keencode_resources::TurnState {
                turn_id: running_turn_id,
                source_agent_id: running_id,
                root_turn_id: TurnId::new("turn-root").unwrap(),
                parent_turn_id: None,
                prompt_summary: "读取项目结构".to_owned(),
                started_at_unix_ms: 1_234,
                completed_at_unix_ms: None,
                status: keencode_resources::TurnStatus::Running,
                stop_reason: None,
                outcome_message: None,
            },
        );
        let ended_id = AgentId::new("child-ended").unwrap();
        state.sub_agents.insert(
            ended_id.clone(),
            SubAgentState {
                agent_id: ended_id,
                parent_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                agent_path: "/root/finished".to_owned(),
                task: "完成检查".to_owned(),
                status: SubAgentStatus::Completed,
                current_turn_id: None,
                result_summary: Some("已完成".to_owned()),
            },
        );
        let nested_id = AgentId::new("nested-hidden").unwrap();
        state.sub_agents.insert(
            nested_id.clone(),
            SubAgentState {
                agent_id: nested_id,
                parent_agent_id: AgentId::new("child-running").unwrap(),
                agent_path: "/root/reader/nested".to_owned(),
                task: "不应递归展示".to_owned(),
                status: SubAgentStatus::Running,
                current_turn_id: None,
                result_summary: None,
            },
        );

        let snapshot = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
            ConversationSnapshotInput {
                session_id: "session-parent",
                runtime_state: &state,
                transcript: &[],
                log_epoch: "epoch-parent",
                workflow_snapshot: json!({"revision": 0, "runs": []}),
                pending_interactions: json!([]),
                permission_mode: None,
                goal: Value::Null,
                agent_id: None,
                workspace_hook_admission: Value::Null,
                terminal_sequences: None,
            },
        );

        assert_eq!(snapshot["subagents"]["revision"], 19);
        assert_eq!(
            snapshot["subagents"]["childSessionIds"],
            json!([
                "agent:session-parent:child-ended",
                "agent:session-parent:child-running"
            ])
        );
        assert_eq!(snapshot["subagents"]["endedTotal"], 1);
        assert_eq!(
            snapshot["subagents"]["running"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            snapshot["subagents"]["running"][0],
            json!({
                "childSessionId": "agent:session-parent:child-running",
                "agentId": "child-running",
                "subagentType": "agent",
                "title": "/root/reader",
                "status": "running",
                "startedAt": 1234,
            })
        );
    }

    #[test]
    fn row_identity_is_monotonic_and_number_safe() {
        let first = row_for_message(
            &json!({
                "messageId": "message-1",
                "turnId": "turn-1",
                "role": "user",
                "content": [{"type": "text", "text": "first"}]
            }),
            0,
            100,
        )
        .unwrap()["rowId"]
            .as_u64()
            .unwrap();
        let second = row_for_message(
            &json!({
                "messageId": "message-2",
                "turnId": "turn-2",
                "role": "user",
                "content": [{"type": "text", "text": "second"}]
            }),
            1,
            100,
        )
        .unwrap()["rowId"]
            .as_u64()
            .unwrap();
        assert!(first > 0 && second > first && second < (1u64 << 53));
    }

    #[test]
    fn transcript_rows_start_each_turn_with_authoritative_header() {
        let session_id = SessionId::new("header-session").unwrap();
        let turn_id = TurnId::new("header-turn").unwrap();
        let mut state = SessionState::empty(session_id);
        state.turns.insert(
            turn_id.clone(),
            TurnState {
                turn_id: turn_id.clone(),
                source_agent_id: AgentId::new(ROOT_AGENT_ID).unwrap(),
                root_turn_id: turn_id.clone(),
                parent_turn_id: None,
                prompt_summary: "修改文件".to_owned(),
                started_at_unix_ms: 100,
                completed_at_unix_ms: Some(200),
                status: TurnStatus::Completed,
                stop_reason: None,
                outcome_message: None,
            },
        );
        let message = SessionMessage {
            is_meta: false,
            references: Vec::new(),
            message_id: "header-message".to_owned(),
            turn_id: Some(turn_id),
            agent_id: Some(AgentId::new(ROOT_AGENT_ID).unwrap()),
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: "修改文件".to_owned(),
            }],
        };
        state
            .transcript
            .push(TranscriptRecord::MessageAdded(message.clone()));
        let transcript = vec![serde_json::to_value(message).unwrap()];
        let rows = rows_for_transcript(&transcript, 0, &state);

        assert_eq!(rows[0]["kind"], "turnHeader");
        assert_eq!(rows[0]["turnId"], "header-turn");
        assert_eq!(rows[0]["entityId"], "header-turn");
        assert_eq!(rows[0]["sourceCommandId"], "header-message");
        assert_eq!(rows[0]["state"], "completedSuccess");
        assert_eq!(rows[1]["kind"], "userInput");
    }

    #[test]
    fn saved_workflow_launch_projects_control_only_graph_rows_from_rust_fact() {
        let session_id = SessionId::new("workflow-launch-session").unwrap();
        let mut state = SessionState::empty(session_id.clone());
        let definition = crate::workflows::WorkflowDefinition {
            version: keencode_workflow::WORKFLOW_DEFINITION_VERSION,
            meta: crate::workflows::WorkflowMeta {
                id: None,
                name: "inspect".to_owned(),
                description: Some("Inspect the workspace".to_owned()),
                when_to_use: None,
                tags: Vec::new(),
            },
            inputs: BTreeMap::new(),
            body: vec![Node::Tool(ToolNode {
                node_id: "read".to_owned(),
                name: "Read".to_owned(),
                input: ValueExpr::literal(json!({"file_path": "README.md"})),
                config: Value::Null,
                output_type: None,
                effect: EffectClass::ReadOnly,
            })],
        };
        state.workflow_events.insert(
            "run-launch".to_owned(),
            vec![
                WorkflowJournalEvent {
                    run_id: "run-launch".to_owned(),
                    tool_call_id: "launch-command".to_owned(),
                    sequence: 1,
                    event_type: "run-started".to_owned(),
                    payload: json!({
                        "name": "inspect",
                        "scope": "project",
                        "definition": serde_json::to_value(&definition).unwrap(),
                        "inputs": {},
                        "parentSessionId": session_id.as_str(),
                        "createdAt": 10,
                    }),
                    artifacts: Vec::new(),
                    actor_session_id: None,
                    launch_input_id: Some("command-1".to_owned()),
                },
                WorkflowJournalEvent {
                    run_id: "run-launch".to_owned(),
                    tool_call_id: "launch-command".to_owned(),
                    sequence: 2,
                    event_type: "run-settled".to_owned(),
                    payload: json!({"status": "completed", "updatedAt": 20}),
                    artifacts: Vec::new(),
                    actor_session_id: None,
                    launch_input_id: Some("command-1".to_owned()),
                },
            ],
        );

        let rows = rows_for_transcript(&[], 0, &state);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["kind"], "turnHeader");
        assert_eq!(rows[0]["origin"], "workflowLaunch");
        assert_eq!(rows[0]["executionKind"], "controlOnly");
        assert_eq!(rows[0]["state"], "completedSuccess");
        assert_eq!(rows[0]["endedAt"], 20);
        assert_eq!(rows[0]["workflowLaunch"]["runId"], "run-launch");
        assert_eq!(rows[0]["workflowLaunch"]["toolCallId"], "launch-command");
        assert_eq!(
            rows[0]["workflowLaunch"]["display"]["causalityGraph"]["steps"][0]["id"],
            "read"
        );
        assert_eq!(rows[1]["kind"], "userInput");
        assert_eq!(rows[1]["origin"], "workflowLaunch");
        assert_eq!(rows[1]["workflowLaunch"], rows[0]["workflowLaunch"]);

        let child_rows = rows_for_transcript_with_launches(&[], 0, &state, false);
        assert!(child_rows.is_empty());
    }

    #[test]
    fn user_message_projects_to_a_real_row() {
        let message = json!({
            "messageId": "m1",
            "turnId": "t1",
            "role": "user",
            "content": [{"text": "hello"}]
        });
        let row = row_for_message(&message, 0, 100).unwrap();
        assert_eq!(row["kind"], "userInput");
        assert_eq!(row["text"], "hello");
        assert_eq!(row["entityId"], "m1");
    }

    #[test]
    fn assistant_feedback_is_projected_and_cancelled_without_null_field() {
        let session_id = SessionId::new("feedback-projection").unwrap();
        let turn_id = TurnId::new("feedback-turn").unwrap();
        let mut state = SessionState::empty(session_id);
        state.transcript = vec![TranscriptRecord::MessageAdded(SessionMessage {
            is_meta: false,
            references: Vec::new(),
            message_id: "assistant-message".to_owned(),
            turn_id: Some(turn_id),
            agent_id: Some(AgentId::new(ROOT_AGENT_ID).unwrap()),
            role: MessageRole::Assistant,
            content: vec![MessagePart::Text {
                text: "有帮助的答复".to_owned(),
            }],
        })];
        state.assistant_feedback.push(AssistantFeedbackRecord {
            row_id: 1,
            entity_id: "assistant-message".to_owned(),
            feedback: AssistantFeedback::Like,
        });
        let transcript = state
            .raw_transcript_messages()
            .into_iter()
            .map(|message| serde_json::to_value(message).unwrap())
            .collect::<Vec<_>>();
        assert!(conversation_row_is_assistant(
            &state,
            1,
            "assistant-message"
        ));
        assert!(!conversation_row_is_assistant(&state, 1, "foreign-entity"));
        let rows = rows_for_transcript(&transcript, 0, &state);
        assert_eq!(rows[0]["feedback"], "like");

        state.assistant_feedback.clear();
        let transcript = state
            .raw_transcript_messages()
            .into_iter()
            .map(|message| serde_json::to_value(message).unwrap())
            .collect::<Vec<_>>();
        let rows = rows_for_transcript(&transcript, 0, &state);
        assert!(rows[0].get("feedback").is_none());
    }

    #[test]
    fn user_file_reference_projects_as_attachment_metadata() {
        let path = env::temp_dir().join(format!(
            "keencode-projection-attachment-{}.md",
            std::process::id()
        ));
        fs::write(&path, b"# attachment\n").expect("write attachment fixture");
        let path_text = path.to_string_lossy().into_owned();
        let message = json!({
            "messageId": "attachment-message",
            "turnId": "attachment-turn",
            "role": "user",
            "references": [{"name": "notes.md", "path": path_text}],
            "content": [{"type": "text", "text": "请读取附件"}]
        });
        let row = row_for_message(&message, 0, 100).unwrap();
        assert_eq!(row["attachments"][0]["fileName"], "notes.md");
        assert_eq!(row["attachments"][0]["mime"], "text/markdown");
        assert_eq!(row["attachments"][0]["bytes"], 13);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn file_query_requires_real_epoch_revision_and_row_entity_pair() {
        let session_id = SessionId::new("query-session").unwrap();
        let turn_id = TurnId::new("query-turn").unwrap();
        let mut state = SessionState::empty(session_id.clone());
        state.project_root = "C:/query-workspace".to_owned();
        state.transcript_revision = 7;
        state.transcript = vec![TranscriptRecord::MessageAdded(SessionMessage {
            is_meta: false,
            references: Vec::new(),
            message_id: "query-message".to_owned(),
            turn_id: Some(turn_id),
            agent_id: Some(AgentId::new(ROOT_AGENT_ID).unwrap()),
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: "inspect".to_owned(),
            }],
        })];
        let epoch = conversation_log_epoch(session_id.as_str(), &state.project_root);
        assert_eq!(
            validate_conversation_query_target(
                &state,
                session_id.as_str(),
                7,
                &epoch,
                1,
                "query-message",
            )
            .unwrap(),
            "query-turn"
        );
        assert_eq!(
            validate_conversation_query_target(
                &state,
                session_id.as_str(),
                7,
                "conversation-0000000000000000",
                1,
                "query-message",
            ),
            Err(super::ConversationQueryError::StaleLogEpoch)
        );
        assert_eq!(
            validate_conversation_query_target(
                &state,
                session_id.as_str(),
                7,
                &epoch,
                2,
                "query-message",
            ),
            Err(super::ConversationQueryError::TargetNotFound)
        );
        assert_eq!(
            conversation_row_turn(&state, 1, "query-message").as_deref(),
            Some("query-turn")
        );
    }

    #[test]
    fn tool_projection_keeps_real_name_arguments_and_failure() {
        let message = json!({
            "messageId": "m-tool",
            "turnId": "t1",
            "role": "assistant",
            "content": [{
                "type": "tool_call",
                "toolCallId": "call-1",
                "toolName": "Read",
                "arguments": {"path": "src/lib.rs"}
            }]
        });
        let result = json!({
            "type": "tool_result",
            "toolCallId": "call-1",
            "content": [{"type": "text", "text": "permission denied"}],
            "isError": true
        });
        let lifecycle = BTreeMap::new();
        let results = BTreeMap::from([(String::from("call-1"), result)]);
        let call_ids = BTreeMap::from([(String::from("call-1"), ())]);
        let context = MessageProjectionContext {
            lifecycle: &lifecycle,
            results: &results,
            call_ids: &call_ids,
            assistant_feedback: &[],
        };
        let row = rows_for_message(&message, 0, 100, 1, &context)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(row["toolName"], "Read");
        assert_eq!(row["toolCallId"], "call-1");
        assert_eq!(row["input"]["path"], "src/lib.rs");
        assert_eq!(row["status"], "error");
        assert_eq!(row["output"]["text"], "permission denied");
    }

    #[test]
    fn tool_projection_reads_persisted_message_part_fields_and_completed_lifecycle() {
        // MessagePart 的 serde 约定是 snake_case；SessionMessage/ToolLifecycle 外层仍是
        // camelCase。该 fixture 覆盖真实 Journal 记录的混合层级，防止工具卡再次退化为
        // unknown、空参数或永久 running。
        let message = json!({
            "messageId": "m-tool-snake",
            "turnId": "t1",
            "role": "assistant",
            "content": [{
                "type": "tool_call",
                "tool_call_id": "call-snake",
                "tool_name": "Write",
                "arguments": {"file_path": "evidence.txt", "content": "ok"}
            }]
        });
        let result_message = json!({
            "messageId": "m-tool-snake-result",
            "turnId": "t1",
            "role": "tool",
            "content": [{
                "type": "tool_result",
                "tool_call_id": "call-snake",
                "content": [{"type": "text", "text": "写入完成"}],
                "is_error": false
            }]
        });
        let lifecycle = json!({
            "request": {
                "modelToolCallId": "call-snake",
                "toolName": "Write",
                "arguments": {"file_path": "evidence.txt", "content": "ok"}
            },
            "executionStarted": true,
            "outcome": {
                "status": "succeeded",
                "result": {
                    "toolCallId": "call-snake",
                    "content": [{"type": "text", "text": "写入完成"}],
                    "isError": false
                }
            }
        });
        let lifecycle = BTreeMap::from([(String::from("call-snake"), lifecycle)]);
        let results = tool_result_values(std::slice::from_ref(&result_message));
        let call_ids = tool_call_ids(std::slice::from_ref(&message));
        let context = MessageProjectionContext {
            lifecycle: &lifecycle,
            results: &results,
            call_ids: &call_ids,
            assistant_feedback: &[],
        };
        let row = rows_for_message(&message, 0, 100, 1, &context)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(row["toolName"], "Write");
        assert_eq!(row["toolCallId"], "call-snake");
        assert_eq!(row["input"]["file_path"], "evidence.txt");
        assert_eq!(row["input"]["content"], "ok");
        assert_eq!(row["status"], "success");
        assert_eq!(row["output"]["text"], "写入完成");
        assert!(row.get("error").is_none());

        // transcript 的 tool_result 已经是完整终态，即使冷恢复时生命周期索引尚未
        // 建好，也必须保持 completed 语义，不能回退到“工具调用执行中”。
        let empty_lifecycle = BTreeMap::new();
        let result_only_context = MessageProjectionContext {
            lifecycle: &empty_lifecycle,
            results: &results,
            call_ids: &call_ids,
            assistant_feedback: &[],
        };
        let row_from_result_only = rows_for_message(&message, 0, 100, 1, &result_only_context)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(row_from_result_only["status"], "success");
    }

    #[test]
    fn create_workflow_tool_row_projects_source_causality_graph_from_rust_definition() {
        let message = json!({
            "messageId": "m-workflow",
            "turnId": "t-workflow",
            "role": "assistant",
            "content": [{
                "type": "tool_call",
                "toolCallId": "workflow-call",
                "toolName": "CreateWorkflow",
                "arguments": {
                    "definition": {
                        "version": 1,
                        "meta": {"name": "inspect"},
                        "inputs": {},
                        "body": [
                            {
                                "type": "agent",
                                "node_id": "ask",
                                "name": "Researcher",
                                "input": {"type": "literal", "value": null},
                                "effect": "read_only"
                            },
                            {
                                "type": "tool",
                                "node_id": "read",
                                "name": "Read",
                                "input": {"type": "literal", "value": null},
                                "effect": "read_only"
                            }
                        ]
                    }
                }
            }]
        });
        let result = json!({
            "type": "tool_result",
            "toolCallId": "workflow-call",
            "content": [{"type": "text", "text": "saved"}],
            "isError": false
        });
        let lifecycle = BTreeMap::new();
        let results = BTreeMap::from([(String::from("workflow-call"), result)]);
        let call_ids = BTreeMap::from([(String::from("workflow-call"), ())]);
        let context = MessageProjectionContext {
            lifecycle: &lifecycle,
            results: &results,
            call_ids: &call_ids,
            assistant_feedback: &[],
        };

        let row = rows_for_message(&message, 0, 100, 1, &context)
            .into_iter()
            .next()
            .expect("CreateWorkflow 工具行应生成");
        assert_eq!(row["toolCallId"], "workflow-call");
        assert_eq!(row["display"]["kind"], "create_workflow");
        assert_eq!(row["display"]["ok"], true);
        assert_eq!(
            row["display"]["causalityGraph"]["steps"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(row["display"]["causalityGraph"]["steps"][0]["kind"], "ask");
        assert_eq!(row["display"]["causalityGraph"]["steps"][0]["lane"], "ask");
        assert_eq!(
            row["display"]["causalityGraph"]["lanes"][0],
            json!({"id": "ask", "name": "Researcher"})
        );
        assert_eq!(
            row["display"]["causalityGraph"]["participants"][0]["lane"],
            "ask"
        );
        assert_eq!(
            row["display"]["causalityGraph"]["steps"][1]["kind"],
            "world-read"
        );
        assert_eq!(
            row["display"]["causalityGraph"]["participants"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn pending_user_input_projection_preserves_real_question_schema() {
        let view = PendingElicitationView {
            request_id: "elicitation-7".to_owned(),
            session_id: "actor-1".to_owned(),
            display_session_id: Some("parent-1".to_owned()),
            connection_id: ConnectionId::new("connection-1").unwrap(),
            questions: vec![UserQuestion {
                id: "strategy".to_owned(),
                prompt: "选择实现策略".to_owned(),
                options: vec![UserQuestionOption {
                    label: "直接实现".to_owned(),
                    description: Some("立即修改当前模块".to_owned()),
                }],
                multi_select: false,
                allow_custom: true,
            }],
            created_at_unix_ms: 123,
            tool_call_id: "tool-ask-1".to_owned(),
            auto_resolution: None,
        };
        let interaction = pending_interaction_value(&view);
        assert_eq!(interaction["interactionId"], "elicitation-7");
        assert_eq!(interaction["kind"], "userInput");
        assert_eq!(interaction["payload"]["toolCallId"], "tool-ask-1");
        assert_eq!(interaction["payload"]["freeText"], true);
        assert_eq!(interaction["payload"]["options"][0]["optionId"], "直接实现");
        assert_eq!(
            interaction["payload"]["questions"][0]["options"][0]["description"],
            "立即修改当前模块"
        );
        assert!(
            interaction["payload"]["questions"][0]["options"][0]
                .get("missing")
                .is_none()
        );
    }

    #[test]
    fn permission_pending_projection_preserves_strict_allow_options() {
        let view = PendingPermissionView {
            interaction_id: "permission-contract".to_owned(),
            session_id: "contract-session".to_owned(),
            display_session_id: "contract-session".to_owned(),
            connection_id: ConnectionId::new("contract-connection").unwrap(),
            tool_call_id: "tool-write-1".to_owned(),
            tool_name: "Shell".to_owned(),
            summary: "允许工具 Shell 修改工作区或外部状态？".to_owned(),
            detail: json!({"effect": "write", "input": {"command": "echo test"}}),
            created_at_unix_ms: 123,
        };
        let value = pending_permission_value(&view);
        assert_eq!(value["kind"], "permission");
        assert_eq!(value["payload"]["toolCallId"], "tool-write-1");
        assert_eq!(value["payload"]["options"].as_array().unwrap().len(), 3);
        assert_eq!(
            value["payload"]["options"][1]["response"]["permissionUpdates"][0]["type"],
            "addRules"
        );
    }

    #[test]
    fn node_settled_does_not_complete_the_run() {
        let run = workflow_run_from_events(
            "run-1",
            &[
                json!({
                    "eventType": "run-started",
                    "sequence": 1,
                    "payload": {}
                }),
                json!({
                    "eventType": "node-queued",
                    "sequence": 2,
                    "payload": {
                        "kind": "ask",
                        "instance": {"siteId": "reader", "ordinal": 0},
                        "actor": {"siteId": "reader", "ordinal": 0},
                        "instructionsHead": "inspect the source"
                    }
                }),
                json!({
                    "eventType": "node-settled",
                    "sequence": 3,
                    "payload": {
                        "kind": "ask",
                        "instance": {"siteId": "reader", "ordinal": 0},
                        "outcome": "ok"
                    }
                }),
            ],
            None,
        );
        assert_eq!(run["status"], "running");
        assert_eq!(run["actors"][0]["status"], "waiting");
        assert_eq!(run["nodes"][0]["phase"], "settled");
        assert_eq!(run["nodes"][0]["instructionsHead"], "inspect the source");
        assert_eq!(run["usage"]["nodesUsed"], 0);
    }

    #[test]
    fn run_settled_and_usage_are_projected_from_exact_events() {
        let run = workflow_run_from_events(
            "run-2",
            &[
                json!({
                    "eventType": "run-started",
                    "sequence": 1,
                    "payload": {}
                }),
                json!({
                    "eventType": "node-dispatched",
                    "sequence": 2,
                    "payload": {
                        "kind": "world-read",
                        "instance": {"siteId": "workspace", "ordinal": 1}
                    }
                }),
                json!({
                    "eventType": "usage-updated",
                    "sequence": 3,
                    "payload": {"spentTokens": 42}
                }),
                json!({
                    "eventType": "run-settled",
                    "sequence": 4,
                    "payload": {"status": "completed"}
                }),
            ],
            None,
        );
        assert_eq!(run["status"], "completed");
        assert_eq!(run["usage"]["spentTokens"], 42);
        assert_eq!(run["usage"]["nodesUsed"], 1);
        assert_eq!(run["nodes"][0]["kind"], "world-read");
        assert_eq!(run["nodes"][0]["phase"], "dispatched");
    }

    #[test]
    fn workflow_terminal_error_reaches_live_run_details() {
        let run = workflow_run_from_events(
            "run-budget-failure",
            &[
                json!({"eventType": "run-started", "sequence": 1, "payload": {}}),
                json!({
                    "eventType": "run-settled",
                    "sequence": 2,
                    "payload": {
                        "status": "failed",
                        "error": {
                            "code": "workflow_budget",
                            "message": "工作流事件预算已耗尽"
                        }
                    }
                }),
            ],
            None,
        );
        // 原生详情从实时 run.error 读取失败原因，不能只保留终态而丢掉 Journal 错误正文。
        assert_eq!(run["status"], "errored");
        assert_eq!(run["error"], "工作流事件预算已耗尽");
        assert_eq!(run["lastEventSequence"], 2);
    }

    #[test]
    fn workflow_run_projection_exposes_journal_artifact_summary_as_refresh_signal() {
        let run = workflow_run_from_events(
            "run-artifact",
            &[
                json!({
                    "eventType": "run-started",
                    "sequence": 1,
                    "payload": {}
                }),
                json!({
                    "eventType": "artifact-committed",
                    "sequence": 2,
                    "payload": {
                        "artifactId": "artifact-1",
                        "kind": "markdown",
                        "version": 1
                    }
                }),
            ],
            Some(&json!({
                "artifacts": [{
                    "id": "artifact-1",
                    "kind": "markdown",
                    "contentType": "text/markdown",
                    "version": 1,
                    "versions": [{
                        "version": 1,
                        "bytes": 12,
                        "publishedAt": 42
                    }],
                    "itemCount": 0,
                    "primary": true
                }]
            })),
        );
        assert_eq!(
            run["artifacts"],
            json!([{
                "id": "artifact-1",
                "kind": "markdown",
                "contentType": "text/markdown",
                "version": 1,
                "bytes": 12,
                "itemCount": 0,
                "primary": true
            }])
        );
    }

    #[test]
    fn native_actor_events_project_real_steps_and_round_usage() {
        let run = workflow_run_from_events(
            "native-run",
            &[
                json!({
                    "eventType": "run-started",
                    "sequence": 1,
                    "payload": {}
                }),
                // parallel 本身是控制节点；没有 actorSessionId，不应占一个步骤。
                json!({
                    "eventType": "node-started",
                    "sequence": 2,
                    "payload": {
                        "address": {"invocation": [], "node_id": "reviews"},
                        "effect": "read_only"
                    }
                }),
                json!({
                    "eventType": "actor-started",
                    "sequence": 3,
                    "payload": {
                        "actorSessionId": "actor-left",
                        "nodeId": "review_left",
                        "nodeAddress": {"invocation": [], "node_id": "review_left"}
                    }
                }),
                json!({
                    "eventType": "actor-started",
                    "sequence": 4,
                    "payload": {
                        "actorSessionId": "actor-right",
                        "nodeId": "review_right",
                        "nodeAddress": {"invocation": [], "node_id": "review_right"}
                    }
                }),
                json!({
                    "eventType": "agent-result",
                    "sequence": 5,
                    "payload": {
                        "nodeId": "review_left",
                        "status": "completed",
                        "output": {"usage": [{"totalTokens": 12}, {"totalTokens": 8}]}
                    }
                }),
                json!({
                    "eventType": "node-settled",
                    "sequence": 6,
                    "payload": {
                        "address": {"invocation": [], "node_id": "review_left"},
                        "status": "succeeded"
                    }
                }),
                json!({
                    "eventType": "agent-result",
                    "sequence": 7,
                    "payload": {
                        "nodeId": "review_right",
                        "status": "completed",
                        "output": {"usage": [{"totalTokens": 9}]}
                    }
                }),
                json!({
                    "eventType": "node-settled",
                    "sequence": 8,
                    "payload": {
                        "address": {"invocation": [], "node_id": "review_right"},
                        "status": "succeeded"
                    }
                }),
                json!({
                    "eventType": "run-settled",
                    "sequence": 9,
                    "payload": {"status": "succeeded"}
                }),
            ],
            Some(&json!({"status": "completed", "usage": {"spentTokens": 0}})),
        );

        assert_eq!(run["status"], "completed");
        assert_eq!(run["usage"]["spentTokens"], 29);
        assert_eq!(run["usage"]["nodesUsed"], 2);
        assert_eq!(run["actors"].as_array().unwrap().len(), 2);
        assert_eq!(run["nodes"].as_array().unwrap().len(), 2);
        assert!(run["actors"].as_array().unwrap().iter().any(|actor| {
            actor["siteId"] == "review_left"
                && actor["ordinal"] == 0
                && actor["sessionId"] == "actor-left"
        }));
        assert!(run["actors"].as_array().unwrap().iter().any(|actor| {
            actor["siteId"] == "review_right"
                && actor["ordinal"] == 0
                && actor["sessionId"] == "actor-right"
        }));
        assert!(run["nodes"].as_array().unwrap().iter().all(|node| {
            node["kind"] == "ask" && node["phase"] == "settled" && node["outcome"] == "ok"
        }));
    }

    #[test]
    fn native_runtime_projection_rejects_legacy_aliases() {
        let run = workflow_run_from_events(
            "native-legacy-shape",
            &[
                json!({
                    "eventType": "run-started",
                    "sequence": 1,
                    "payload": {}
                }),
                json!({
                    "eventType": "actor-started",
                    "sequence": 2,
                    "payload": {
                        "actorSessionId": "legacy-actor",
                        "node_id": "legacy-node",
                        "node_address": {"invocation": [], "node_id": "legacy-node"}
                    }
                }),
                json!({
                    "eventType": "agent-result",
                    "sequence": 3,
                    "payload": {
                        "node_id": "legacy-node",
                        "node_address": {"invocation": [], "node_id": "legacy-node"},
                        "output": {"usage": [{"total_tokens": 11}]}
                    }
                }),
                json!({
                    "eventType": "run-settled",
                    "sequence": 4,
                    "payload": {"status": "completed"}
                }),
            ],
            None,
        );

        assert_eq!(run["usage"]["spentTokens"], 0);
        assert_eq!(run["usage"]["nodesUsed"], 0);
        assert!(run["actors"].as_array().unwrap().is_empty());
        assert!(run["nodes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn workflow_run_projection_preserves_rust_summary_tool_call_id_camel_case() {
        let summary = WorkflowRunSummary {
            run_id: "run-summary".to_owned(),
            name: Some("summary contract".to_owned()),
            status: WorkflowRunStatus::Completed,
            stop_reason: None,
            created_at: 1,
            updated_at: 2,
            spent_tokens: 3,
            parent_session_id: Some("session-summary".to_owned()),
            tool_call_id: Some("tool-summary".to_owned()),
            args: json!({}),
            cwd: None,
            canonical_hash: None,
            artifacts: Vec::new(),
            resumable: false,
        };
        let serialized = serde_json::to_value(&summary).expect("WorkflowRunSummary 应可序列化");
        assert_eq!(serialized["toolCallId"], json!("tool-summary"));
        assert!(serialized.get("tool_call_id").is_none());

        let run = workflow_run_from_events("run-summary", &[], Some(&serialized));
        assert_eq!(run["runId"], json!("run-summary"));
        assert_eq!(run["status"], json!("completed"));
        assert_eq!(run["toolCallId"], json!("tool-summary"));
        assert!(run.get("tool_call_id").is_none());
        assert!(run["usage"].is_object());
        assert!(run["actors"].is_array());
        assert!(run["nodes"].is_array());
    }

    #[test]
    fn background_work_projection_tracks_live_runs_and_filters_terminal_runs() {
        let session_id = SessionId::new("background-work-session").unwrap();
        let mut state = SessionState::empty(session_id.clone());
        state.workflow_events.insert(
            "run-live".to_owned(),
            vec![WorkflowJournalEvent {
                run_id: "run-live".to_owned(),
                tool_call_id: "tool-live".to_owned(),
                sequence: 1,
                event_type: "run-started".to_owned(),
                payload: json!({
                    "name": "读取工作区",
                    "parentSessionId": session_id.as_str(),
                    "createdAt": 1234,
                }),
                artifacts: Vec::new(),
                actor_session_id: None,
                launch_input_id: None,
            }],
        );
        state.workflow_events.insert(
            "run-pending".to_owned(),
            vec![WorkflowJournalEvent {
                run_id: "run-pending".to_owned(),
                tool_call_id: "tool-pending".to_owned(),
                sequence: 2,
                event_type: "run-started".to_owned(),
                payload: json!({
                    "parentSessionId": session_id.as_str(),
                    "createdAt": 1235,
                }),
                artifacts: Vec::new(),
                actor_session_id: None,
                launch_input_id: None,
            }],
        );
        state.workflow_events.insert(
            "run-completed".to_owned(),
            vec![WorkflowJournalEvent {
                run_id: "run-completed".to_owned(),
                tool_call_id: "tool-completed".to_owned(),
                sequence: 3,
                event_type: "run-started".to_owned(),
                payload: json!({
                    "parentSessionId": session_id.as_str(),
                    "createdAt": 1236,
                }),
                artifacts: Vec::new(),
                actor_session_id: None,
                launch_input_id: None,
            }],
        );

        let snapshot = json!({
            "runs": [
                {"runId": "run-live", "status": "running", "toolCallId": "tool-live"},
                {"runId": "run-pending", "status": "pending", "toolCallId": "tool-pending"},
                {"runId": "run-completed", "status": "completed", "toolCallId": "tool-completed"},
            ]
        });
        let background = workflow_background_works(&state, &snapshot);
        assert_eq!(
            background,
            json!([
                {
                    "workId": "run-live",
                    "kind": "workflow",
                    "title": "读取工作区",
                    "status": "running",
                    "startedAt": 1234,
                    "cancellable": true,
                    "anchorRowId": null,
                },
                {
                    "workId": "run-pending",
                    "kind": "workflow",
                    "title": "run-pending",
                    "status": "running",
                    "startedAt": 1235,
                    "cancellable": true,
                    "anchorRowId": null,
                },
            ])
        );
    }

    #[test]
    fn actors_are_filtered_to_leaf_node_bindings() {
        let run = workflow_run_from_events(
            "run-actors",
            &[
                json!({
                    "eventType": "actor-created",
                    "sequence": 1,
                    "payload": {
                        "actor": {"siteId": "control", "ordinal": 0},
                        "name": "internal-control"
                    }
                }),
                json!({
                    "eventType": "actor-created",
                    "sequence": 2,
                    "payload": {
                        "actor": {"siteId": "reader", "ordinal": 0},
                        "name": "reader"
                    }
                }),
                json!({
                    "eventType": "node-queued",
                    "sequence": 3,
                    "payload": {
                        "kind": "ask",
                        "instance": {"siteId": "reader", "ordinal": 0},
                        "actor": {"siteId": "reader", "ordinal": 0}
                    }
                }),
            ],
            None,
        );
        let actors = run["actors"].as_array().expect("actors 必须为数组");
        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0]["siteId"], "reader");
        assert_eq!(actors[0]["ordinal"], 0);
        assert_eq!(run["nodes"].as_array().unwrap().len(), 1);
    }

    /// 在显式环境目录中导出生产投影的真实形状，供源仓库 shared zod schema 直接验收。
    /// 默认不写文件，避免普通 Rust 单测污染工作区；fixture 内容不含路径、凭据或用户正文。
    #[test]
    fn export_source_contract_fixtures_when_requested() {
        let Ok(directory) = env::var("KEENCODE_RPC_CONTRACT_FIXTURES") else {
            return;
        };
        let directory = Path::new(&directory);
        fs::create_dir_all(directory).expect("应创建 RPC 契约 fixture 目录");

        let mut state = SessionState::empty(
            SessionId::new("contract-session").expect("fixture session id 应合法"),
        );
        state.created = true;
        state.project_root = "C:/contract-workspace".to_owned();
        state.title = "Contract fixture".to_owned();
        state.last_sequence = 8;
        state.created_at_unix_ms = 100;
        state.updated_at_unix_ms = 123;
        state.transcript_revision = 2;
        let contract_turn = TurnId::new("contract-turn").expect("fixture turn id 应合法");
        let contract_workflow_definition = json!({
            "version": 1,
            "meta": {"name": "contract-graph"},
            "inputs": {},
            "body": [
                {
                    "type": "agent",
                    "node_id": "ask",
                    "name": "Researcher",
                    "input": {"type": "literal", "value": null},
                    "effect": "read_only"
                },
                {
                    "type": "tool",
                    "node_id": "read",
                    "name": "Read",
                    "input": {"type": "literal", "value": null},
                    "effect": "read_only"
                }
            ]
        });
        state.transcript = vec![
            TranscriptRecord::MessageAdded(SessionMessage {
                is_meta: false,
                references: Vec::new(),
                message_id: "contract-user-message".to_owned(),
                turn_id: Some(contract_turn.clone()),
                agent_id: None,
                role: MessageRole::User,
                content: vec![MessagePart::Text {
                    text: "contract input".to_owned(),
                }],
            }),
            TranscriptRecord::MessageAdded(SessionMessage {
                is_meta: false,
                references: Vec::new(),
                message_id: "contract-assistant-message".to_owned(),
                turn_id: Some(contract_turn.clone()),
                agent_id: Some(AgentId::new(ROOT_AGENT_ID).expect("root agent id 应合法")),
                role: MessageRole::Assistant,
                content: vec![MessagePart::Text {
                    text: "contract response".to_owned(),
                }],
            }),
            TranscriptRecord::MessageAdded(SessionMessage {
                is_meta: false,
                references: Vec::new(),
                message_id: "contract-workflow-call-message".to_owned(),
                turn_id: Some(contract_turn.clone()),
                agent_id: Some(AgentId::new(ROOT_AGENT_ID).expect("root agent id 应合法")),
                role: MessageRole::Assistant,
                content: vec![MessagePart::ToolCall {
                    tool_call_id: "contract-workflow-call".to_owned(),
                    tool_name: "CreateWorkflow".to_owned(),
                    arguments: json!({"definition": contract_workflow_definition}),
                }],
            }),
            TranscriptRecord::MessageAdded(SessionMessage {
                is_meta: false,
                references: Vec::new(),
                message_id: "contract-workflow-result-message".to_owned(),
                turn_id: Some(contract_turn),
                agent_id: None,
                role: MessageRole::Tool,
                content: vec![MessagePart::ToolResult {
                    tool_call_id: "contract-workflow-call".to_owned(),
                    content: vec![ToolResultPart::Text {
                        text: "workflow saved".to_owned(),
                    }],
                    is_error: false,
                }],
            }),
        ];
        // 以真实资源层 Provider 快照生成非空 modelSelection，确保源 schema 校验的不是
        // 未绑定会话的退化分支。快照只含无凭据配置身份，不会把 secret 写入 fixture。
        state.provider = Some(ProviderSnapshot {
            provider_id: "provider-contract".to_owned(),
            model: "model-contract".to_owned(),
            context_window: Some(128_000),
            protocol: ProviderProtocolSnapshot::OpenAiResponses,
            config_fingerprint: "fingerprint-contract".to_owned(),
            reasoning_effort: Some(ReasoningEffortSnapshot::Low),
        });
        state.sub_agents.insert(
            AgentId::new("contract-child-ended").expect("fixture agent id 应合法"),
            SubAgentState {
                agent_id: AgentId::new("contract-child-ended").expect("fixture agent id 应合法"),
                parent_agent_id: AgentId::new(ROOT_AGENT_ID).expect("root agent id 应合法"),
                agent_path: "/root/contract-ended".to_owned(),
                task: "contract ended child".to_owned(),
                status: SubAgentStatus::Completed,
                current_turn_id: None,
                result_summary: Some("contract ended".to_owned()),
            },
        );
        state.sub_agents.insert(
            AgentId::new("contract-child-waiting").expect("fixture agent id 应合法"),
            SubAgentState {
                agent_id: AgentId::new("contract-child-waiting").expect("fixture agent id 应合法"),
                parent_agent_id: AgentId::new(ROOT_AGENT_ID).expect("root agent id 应合法"),
                agent_path: "/root/contract-waiting".to_owned(),
                task: "contract waiting child".to_owned(),
                status: SubAgentStatus::Waiting,
                current_turn_id: None,
                result_summary: None,
            },
        );
        // 同一份真实 Session Journal 同时放一条活动 run 和一条终态 run；Source 契约
        // fixture 必须证明 Composer 后台摘要只保留活动工作，而 workflowRuns 仍保留历史。
        state.workflow_events.insert(
            "contract-workflow-live".to_owned(),
            vec![WorkflowJournalEvent {
                run_id: "contract-workflow-live".to_owned(),
                tool_call_id: "tool-workflow-live".to_owned(),
                sequence: 6,
                event_type: "run-started".to_owned(),
                payload: json!({
                    "name": "契约活动工作流",
                    "parentSessionId": "contract-session",
                    "createdAt": 124,
                }),
                artifacts: Vec::new(),
                actor_session_id: None,
                launch_input_id: None,
            }],
        );
        state.workflow_events.insert(
            "contract-workflow-completed".to_owned(),
            vec![
                WorkflowJournalEvent {
                    run_id: "contract-workflow-completed".to_owned(),
                    tool_call_id: "tool-workflow-completed".to_owned(),
                    sequence: 7,
                    event_type: "run-started".to_owned(),
                    payload: json!({
                        "name": "契约终态工作流",
                        "parentSessionId": "contract-session",
                        "createdAt": 125,
                    }),
                    artifacts: Vec::new(),
                    actor_session_id: None,
                    launch_input_id: None,
                },
                WorkflowJournalEvent {
                    run_id: "contract-workflow-completed".to_owned(),
                    tool_call_id: "tool-workflow-completed".to_owned(),
                    sequence: 8,
                    event_type: "run-settled".to_owned(),
                    payload: json!({"status": "completed"}),
                    artifacts: Vec::new(),
                    actor_session_id: None,
                    launch_input_id: None,
                },
            ],
        );
        let pending_view = PendingElicitationView {
            request_id: "elicitation-contract".to_owned(),
            session_id: "contract-session".to_owned(),
            display_session_id: None,
            connection_id: ConnectionId::new("contract-connection").unwrap(),
            questions: vec![keencode_tools::UserQuestion {
                id: "contract-choice".to_owned(),
                prompt: "选择契约测试路径".to_owned(),
                options: vec![keencode_tools::UserQuestionOption {
                    label: "测试".to_owned(),
                    description: Some("执行严格 schema 校验".to_owned()),
                }],
                multi_select: false,
                allow_custom: true,
            }],
            created_at_unix_ms: 123,
            tool_call_id: "tool-contract-ask".to_owned(),
            auto_resolution: None,
        };
        let transcript = state
            .raw_transcript_messages()
            .into_iter()
            .map(|message| serde_json::to_value(message).expect("fixture transcript 应可序列化"))
            .collect::<Vec<_>>();
        let contract_goal_snapshot = keencode_agent::GoalSnapshot {
            revision: 1,
            goal: Some(keencode_agent::GoalRecord {
                id: "contract-goal".to_owned(),
                owner_session_id: "contract-session".to_owned(),
                title: "Contract goal".to_owned(),
                scope: "project".to_owned(),
                status: keencode_agent::GoalStatus::Active,
                description: None,
                progress_percent: Some(20),
                objective: "验证 Goal projection contract".to_owned(),
                token_budget: None,
                tokens_used: 0,
                time_used_seconds: 12,
                blocked_reason: None,
                completion_evidence: None,
                created_at_unix_ms: 100,
                updated_at_unix_ms: 123,
            }),
        };
        let contract_goal =
            goal_projection_value_from_snapshot(&contract_goal_snapshot, "contract-session")
                .expect("Goal fixture 应由 Rust 持久字段生成");
        // 工作流摘要必须经过真实 Rust 类型的 camelCase 序列化，再进入会话投影；
        // 不能在 fixture 中手写 toolCallId，否则会漏掉宿主字段命名与投影的联接错误。
        let live_summary = WorkflowRunSummary {
            run_id: "contract-workflow-live".to_owned(),
            name: Some("契约活动工作流".to_owned()),
            status: WorkflowRunStatus::Running,
            stop_reason: None,
            created_at: 124,
            updated_at: 124,
            spent_tokens: 0,
            parent_session_id: Some("contract-session".to_owned()),
            tool_call_id: Some("tool-workflow-live".to_owned()),
            args: json!({}),
            cwd: None,
            canonical_hash: None,
            artifacts: Vec::new(),
            resumable: false,
        };
        let completed_summary = WorkflowRunSummary {
            run_id: "contract-workflow-completed".to_owned(),
            name: Some("契约终态工作流".to_owned()),
            status: WorkflowRunStatus::Completed,
            stop_reason: None,
            created_at: 125,
            updated_at: 125,
            spent_tokens: 1,
            parent_session_id: Some("contract-session".to_owned()),
            tool_call_id: Some("tool-workflow-completed".to_owned()),
            args: json!({}),
            cwd: None,
            canonical_hash: None,
            artifacts: Vec::new(),
            resumable: false,
        };
        let project_workflow_run = |summary: &WorkflowRunSummary| {
            let events = state
                .workflow_events
                .get(&summary.run_id)
                .expect("fixture workflow events 应存在")
                .iter()
                .map(|event| serde_json::to_value(event).expect("WorkflowJournalEvent 应可序列化"))
                .collect::<Vec<_>>();
            let summary = serde_json::to_value(summary).expect("WorkflowRunSummary 应可序列化");
            workflow_run_from_events(
                summary["runId"].as_str().expect("摘要必须带 runId"),
                &events,
                Some(&summary),
            )
        };
        let workflow_snapshot = json!({
            "revision": 8,
            "runs": [
                project_workflow_run(&live_summary),
                project_workflow_run(&completed_summary),
            ],
        });
        let mut conversation = conversation_snapshot_from_state_with_pending_and_goal(
            "contract-session",
            &state,
            &transcript,
            "epoch-contract",
            workflow_snapshot,
            json!([pending_interaction_value(&pending_view)]),
            contract_goal,
        );
        // 复用生产 retry_state_value 生成非空 V4 控制字段；fixture 不手写 JSON，
        // 这样 Rust 热 authority 的字段改动会与 Source strict schema 同步暴露。
        let contract_retry = keencode_runtime::RuntimeModelRetryScheduled {
            turn_id: "contract-turn".to_owned(),
            source_agent_id: ROOT_AGENT_ID.to_owned(),
            attempt: 2,
            max_attempts: 4,
            delay_ms: 750,
            occurred_at_ms: 10_000,
            message: "fixture retry detail is not projected".to_owned(),
        };
        let api_retry = super::retry_state_value(&contract_retry);
        conversation["control"]["apiRetry"] = api_retry.clone();
        assert_eq!(api_retry["attempt"], json!(2));
        assert_eq!(api_retry["maxAttempts"], json!(4));
        assert_eq!(api_retry["nextRetryAt"], json!(10_750));
        assert_eq!(api_retry["reasonCode"], json!("provider_retry"));
        let child_agent_id =
            AgentId::new("contract-child-waiting").expect("fixture child agent id 应合法");
        let child_conversation = conversation_snapshot_from_state_with_pending_and_mode_for_agent(
            ConversationSnapshotInput {
                session_id: "agent:contract-session:contract-child-waiting",
                runtime_state: &state,
                transcript: &transcript,
                log_epoch: "epoch-contract",
                workflow_snapshot: json!({"revision": 0, "runs": []}),
                pending_interactions: json!([]),
                permission_mode: None,
                goal: Value::Null,
                agent_id: Some(&child_agent_id),
                workspace_hook_admission: Value::Null,
                terminal_sequences: None,
            },
        );
        assert!(child_conversation["usage"]["contextWindow"].is_null());
        assert!(child_conversation["usage"]["cumulative"].is_object());
        assert_eq!(
            conversation["backgroundWorks"],
            json!([{
                "workId": "contract-workflow-live",
                "kind": "workflow",
                "title": "契约活动工作流",
                "status": "running",
                "startedAt": 124,
                "cancellable": true,
                "anchorRowId": null,
            }])
        );
        // sessions-index 必须走与生产首帧相同的 Runtime 查询，不能在测试中复制
        // StoredSessionMetadata 的 JSON 形状。临时目录只用于生成 metadata，路径不会进入
        // 对外投影，因此 fixture 仍然是确定且不含本机路径的契约数据。
        let storage = tempfile::tempdir().expect("应创建 Runtime fixture 存储目录");
        let project = tempfile::tempdir().expect("应创建 workspace fixture 目录");
        let runtime =
            AgentRuntime::new_for_control_test(storage.path()).expect("应创建 Runtime fixture");
        runtime
            .open_or_create_session(project.path(), None, "contract-index-fixture")
            .expect("应创建 sessions-index fixture Session");
        // 使用 Runtime 读回的权威 project_root 作为过滤键；创建路径经过 Runtime
        // canonicalize，直接拿 tempfile 路径会让列表误判为空。
        let project_path = runtime
            .stored_sessions()
            .expect("应读取 sessions-index fixture metadata")
            .into_iter()
            .next()
            .map(|metadata| metadata.project_root)
            .expect("sessions-index fixture 应至少包含刚创建的 Session");
        let sessions_index = sessions_index_snapshot_for_connection(
            &runtime,
            &project_path,
            "local:C:/contract-workspace",
            "workspace-local:C:/contract-workspace",
            None,
        )
        .expect("sessions-index fixture 应由生产投影生成");
        let workspace_config = workspace_config_snapshot_value("local:C:/contract-workspace");
        // 命令对账 fixture 必须来自与生产 query 相同的 Receipt->wire 映射：
        // completed/rejected 只能带 Source commandAck，admitted 与未命中都只能是
        // literal "unknown"，这样前端不会把在途状态误当作失败 ACK。
        let rejected_receipt = CommandReceipt {
            schema: COMMAND_RECEIPT_SCHEMA.to_owned(),
            scope: "session:contract-session".to_owned(),
            command_id: "command-rejected".to_owned(),
            command_type: "renameSession".to_owned(),
            payload_sha256: "0".repeat(64),
            status: CommandReceiptStatus::Rejected {
                ack: json!({
                    "commandId": "command-rejected",
                    "status": "rejected",
                    "reasonCode": "rpc.invalidParams",
                    "revisionAtDecision": 2,
                }),
            },
        };
        let admitted_receipt = CommandReceipt {
            schema: COMMAND_RECEIPT_SCHEMA.to_owned(),
            scope: "session:contract-session".to_owned(),
            command_id: "command-pending".to_owned(),
            command_type: "sendText".to_owned(),
            payload_sha256: "1".repeat(64),
            status: CommandReceiptStatus::Admitted,
        };
        let command_query = json!({
            "results": [
                {
                    "key": {"sessionId": "contract-session", "commandId": "command-rejected"},
                    "result": command_receipt_query_result(&rejected_receipt),
                },
                {
                    "key": {"sessionId": "contract-session", "commandId": "command-pending"},
                    "result": command_receipt_query_result(&admitted_receipt),
                },
                {
                    "key": {"sessionId": null, "commandId": "command-not-found"},
                    "result": "unknown",
                },
            ],
        });
        let assistant_row = conversation["rows"]["window"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["kind"] == "assistantText"))
            .cloned()
            .expect("rich conversation fixture 应包含 assistant row");
        let assistant_row_id = assistant_row["rowId"]
            .as_u64()
            .expect("assistant row 应包含 numeric rowId");
        let mut streaming_row = assistant_row.clone();
        streaming_row["state"] = json!("streaming");
        let conversation_delta = json!({
            "op": "row.upserted",
            "row": streaming_row,
        });
        let conversation_text_delta = json!({
            "op": "row.delta",
            "rowId": assistant_row_id,
            "path": "text",
            "append": " stream",
        });
        // 首帧 snapshot.seq 是唯一的对话投影水位；online frame 的 fromSeq 必须
        // 精确承接它，不能在 fixture 中复制一个随着字段变化而失真的旧常量。
        let snapshot_seq = conversation
            .get("seq")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        // 物理 wire fixture 复用上面的真实 Rust snapshot，供 shared schema 与
        // TopicWireFrameAssembler 做跨语言验收；它不是第二套业务事实。
        let conversation_frame = json!({
            "topic": "conversation/contract-session",
            "subscriptionId": "contract-subscription",
            "fromSeq": 0,
            "toSeq": snapshot_seq,
            "sentAt": 123,
            "payload": {"kind": "snapshot", "snapshot": conversation.clone()},
        });
        let conversation_wire = json!({
            "wireVersion": 3,
            "kind": "complete",
            "deliveryKind": "initial",
            "logicalFrameId": "contract-subscription:1",
            "logicalFrameOrdinal": 1,
            "topic": "conversation/contract-session",
            "subscriptionId": "contract-subscription",
            "frame": conversation_frame,
        });
        let conversation_online_frame = json!({
            "topic": "conversation/contract-session",
            "subscriptionId": "contract-subscription",
            "fromSeq": snapshot_seq,
            "toSeq": snapshot_seq.saturating_add(1),
            "sentAt": 124,
            "payload": {
                "kind": "deltas",
                "deltas": [conversation_delta, conversation_text_delta],
            },
        });
        assert_eq!(
            conversation_frame["toSeq"], conversation["seq"],
            "initial wire 必须携带 snapshot 的真实水位"
        );
        assert_eq!(
            conversation_online_frame["fromSeq"], conversation_frame["toSeq"],
            "online wire 必须从 initial wire 的水位继续"
        );
        let conversation_online_wire = json!({
            "wireVersion": 3,
            "kind": "complete",
            "deliveryKind": "online",
            "logicalFrameId": "contract-subscription:2",
            "logicalFrameOrdinal": 2,
            "topic": "conversation/contract-session",
            "subscriptionId": "contract-subscription",
            "frame": conversation_online_frame,
        });
        let progress = serde_json::to_value(progress_from_workflow_event(&WorkflowJournalEvent {
            run_id: "run-contract".to_owned(),
            tool_call_id: "tool-contract".to_owned(),
            sequence: 1,
            event_type: "node-progress".to_owned(),
            payload: json!({"kind": "ask", "instance": {"siteId": "reader", "ordinal": 0}}),
            artifacts: Vec::new(),
            actor_session_id: Some("actor-contract".to_owned()),
            launch_input_id: Some("input-contract".to_owned()),
        }))
        .expect("progress fixture 应可序列化");

        for (name, value) in [
            ("conversation_snapshot.json", conversation),
            ("conversation_child_snapshot.json", child_conversation),
            ("api_retry.json", api_retry),
            ("sessions_index.json", sessions_index),
            ("workspace_config.json", workspace_config),
            ("conversation_delta.json", conversation_delta),
            ("conversation_wire.json", conversation_wire),
            ("conversation_online_wire.json", conversation_online_wire),
            ("progress.json", progress),
            ("command_query.json", command_query),
        ] {
            let bytes = serde_json::to_vec_pretty(&value).expect("fixture 应为 JSON");
            fs::write(directory.join(name), bytes).expect("应写入 RPC 契约 fixture");
        }
    }
}
