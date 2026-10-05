//! V4 附件、文件变更和后台 Bash 的真实桌面 RPC。
//!
//! 该模块只保存短生命周期的上传事务；已提交附件、文件快照和后台输出分别
//! 由 `ui_attachments`、Runtime Journal/ArtifactStore 与 BackgroundTaskManager
//! 持有权威状态。session gateway 只需把同一连接的 agent 方法转发到这里。

use super::dispatch::{GatewayContext, RpcError};
use super::session::projection::{self, ConversationQueryError};
use crate::agent_runtime::{AgentRuntime, BackgroundBashOutput};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use keencode_model::{ImageContent, InputReference};
use keencode_resources::{MessagePart, MessageRole, SessionState, ToolFileChange};
use keencode_runtime::RuntimeSession;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use tauri::{AppHandle, Manager};

const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 512 * 1024;
const MAX_UPLOAD_CHUNKS: usize = 64;
const MAX_STAGED_BYTES: usize = 64 * 1024 * 1024;
const MAX_PREVIEW_BYTES: u64 = 30 * 1024 * 1024;
/// stat 只读取元数据，沿用 shared schema 的 2 GiB 上限，不应误用预览 30 MiB 上限。
const MAX_STAT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SEND_ATTACHMENTS: usize = 16;
/// 文本附件会作为隐藏用户上下文送入模型，必须有独立总量上限。
const MAX_TEXT_CONTEXT_BYTES: usize = 8 * 1024 * 1024;
/// attachmentRead 两个方法共享的 strict 字段表；preview source 的 clientMode 不属于 read。
const ATTACHMENT_READ_ALLOWED_FIELDS: &[&str] = &[
    "connectionId",
    "sessionId",
    "target",
    "attachmentIndex",
    "offset",
    "limit",
    "ref",
];

static UPLOADS: OnceLock<Mutex<HashMap<UploadKey, UploadEntry>>> = OnceLock::new();

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct UploadKey {
    connection_id: String,
    session_id: String,
    upload_id: String,
}

#[derive(Clone, Debug)]
struct UploadEntry {
    file_name: String,
    mime: String,
    total_bytes: usize,
    total_chunks: usize,
    checksum: String,
    path: PathBuf,
    received_bytes: usize,
    next_chunk_index: usize,
    committed_ref: Option<String>,
}

#[derive(Clone, Debug)]
struct RowTarget {
    row_id: u64,
    entity_id: String,
}

#[derive(Clone, Debug)]
struct AttachmentFile {
    path: PathBuf,
    media_type: String,
    total_bytes: u64,
}

#[derive(Clone, Copy, Debug)]
struct AttachmentResolveOptions<'a> {
    target: Option<&'a RowTarget>,
    attachment_index: Option<usize>,
    media_only: bool,
    max_bytes: u64,
}

/// sendText 通过这里把 renderer 的 AttachmentRef 转成 Runtime 输入。
///
/// `references` 写入权威用户消息，图片和文本则分别进入 Image block 与隐藏
/// meta 消息；调用方不能直接把未经宿主读取和授权的路径交给 Runtime。
#[derive(Clone, Debug, Default)]
pub(crate) struct PreparedSendAttachments {
    pub(crate) references: Vec<InputReference>,
    pub(crate) images: Vec<ImageContent>,
    pub(crate) text_context: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SendAttachmentRef {
    #[serde(rename = "ref")]
    reference: String,
    file_name: String,
    mime: String,
    bytes: u64,
    #[serde(default)]
    preview_ref: Option<String>,
}

#[derive(Clone, Debug)]
struct TargetTurn {
    state: SessionState,
    turn_id: String,
}

#[derive(Clone, Debug)]
struct ChangeRecord {
    /// Journal Transcript 中首次出现该工具调用的顺序；缺失时才使用请求时间兜底。
    transcript_order: Option<usize>,
    requested_at_unix_ms: u64,
    request_id: String,
    tool_name: String,
    file_change: ToolFileChange,
}

/// session gateway 用于判断是否应把 agent 方法交给本模块。
pub(crate) fn is_v4_method(method: &str) -> bool {
    matches!(
        method,
        "backgroundBashOutputV4"
            | "conversationFileChangesV4"
            | "conversationFileRewindPreviewV4"
            | "attachmentBeginV4"
            | "attachmentChunkV4"
            | "attachmentCommitV4"
            | "attachmentAbortV4"
            | "attachmentPreviewSourceV4"
            | "attachmentReadV4"
            | "conversationAttachmentReadV4"
            | "conversationAttachmentStatV4"
    )
}

/// 连接销毁时释放未提交的上传文件；已提交资产属于 Session 事实，不随连接删除。
pub(crate) fn connection_closed(connection_id: &str) {
    let Some(store) = UPLOADS.get() else {
        return;
    };
    let Ok(mut uploads) = store.lock() else {
        return;
    };
    for path in remove_connection_uploads(&mut uploads, connection_id) {
        let _ = fs::remove_file(path);
    }
}

/// 执行 V4 只读/上传调用；参数仍在 Rust 边界严格校验，未知字段不被静默丢弃。
pub(crate) async fn call_v4(
    ctx: &GatewayContext,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    let args = normalize_v4_args(ctx, args)?;
    let args = if method == "attachmentPreviewSourceV4" {
        inject_desktop_preview_client_mode(args)?
    } else {
        args
    };
    match method {
        "backgroundBashOutputV4" => background_output(ctx, &args).await,
        "conversationFileChangesV4" => conversation_file_changes(ctx, &args),
        "conversationFileRewindPreviewV4" => conversation_file_rewind_preview(ctx, &args),
        "attachmentBeginV4" => attachment_begin(ctx, &args),
        "attachmentChunkV4" => attachment_chunk(ctx, &args),
        "attachmentCommitV4" => attachment_commit(ctx, &args),
        "attachmentAbortV4" => attachment_abort(ctx, &args),
        "attachmentPreviewSourceV4" => attachment_preview_source(ctx, &args),
        "attachmentReadV4" => attachment_read(ctx, &args, false),
        "conversationAttachmentReadV4" => attachment_read(ctx, &args, true),
        "conversationAttachmentStatV4" => attachment_stat(ctx, &args),
        _ => Err(invalid_params(format!("未知 V4 附件方法: {method}"))),
    }
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new("fault.invalidParams", message)
}

fn backend_error(message: impl Into<String>) -> RpcError {
    RpcError::new("fault.backend", message)
}

fn stale_revision(message: impl Into<String>) -> RpcError {
    RpcError::new("fault.conversation.staleRevision", message)
}

fn required_string(args: &Value, name: &str) -> Result<String, RpcError> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid_params(format!("{name} 必须为非空字符串")))
}

fn optional_target_string(args: &Value, name: &str) -> Result<Option<String>, RpcError> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| invalid_params(format!("{name} 必须为非空字符串")))?;
    Ok(Some(value.to_owned()))
}

/// 校验 Desktop service 的工作区路由外壳，再移除它交给 shared V4 method schema。
///
/// `ZCodeAgent*Params` 为连接路由保留 workspace target，但 shared V4 的 begin、
/// chunk、read 等严格载荷没有这些字段。路由外壳只能在确认已登记工作区和 Session
/// 权威根目录一致后剥离，不能用放宽方法级 unknown-field 校验来兼容。
fn normalize_v4_args(ctx: &GatewayContext, args: Value) -> Result<Value, RpcError> {
    let workspace_path = required_string(&args, "workspacePath")?;
    let _workspace_identity = optional_target_string(&args, "workspaceIdentity")?;
    let _remote_session_id = optional_target_string(&args, "remoteSessionId")?;
    let session_id = required_string(&args, "sessionId")?;
    let requested_root = crate::session_commands::authorize_stored_root(&ctx.app, &workspace_path)
        .map_err(backend_error)?;
    let runtime = runtime_arc(ctx)?;
    let session_root =
        crate::session_commands::authorize_stored_session_root(&runtime, &ctx.app, &session_id)
            .map_err(backend_error)?;
    if requested_root != session_root {
        return Err(RpcError::new(
            "fault.workspace.scope",
            "workspacePath 不属于当前 Session 的已授权工作区",
        ));
    }

    strip_v4_target_fields(args)
}

fn strip_v4_target_fields(args: Value) -> Result<Value, RpcError> {
    let mut normalized = args
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_params("V4 附件参数必须为对象"))?;
    for field in ["workspacePath", "workspaceIdentity", "remoteSessionId"] {
        normalized.remove(field);
    }
    Ok(Value::Object(normalized))
}

fn inject_desktop_preview_client_mode(args: Value) -> Result<Value, RpcError> {
    let mut object = args
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_params("attachmentPreviewSourceV4 参数必须为对象"))?;
    // 桌面 RPC 的连接 authority 固定为 desktop-continuous；renderer 传入的值
    // 只能被覆盖，不能改变 local_path 是否可返回的宿主安全边界。
    object.insert(
        "clientMode".to_owned(),
        Value::String("desktop-continuous".to_owned()),
    );
    Ok(Value::Object(object))
}

fn required_usize(args: &Value, name: &str) -> Result<usize, RpcError> {
    let value = args
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_params(format!("{name} 必须为非负整数")))?;
    usize::try_from(value).map_err(|_| invalid_params(format!("{name} 超出本机范围")))
}

fn reject_unknown(args: &Value, allowed: &[&str], method: &str) -> Result<(), RpcError> {
    let object = args
        .as_object()
        .ok_or_else(|| invalid_params(format!("{method} 参数必须为对象")))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_params(format!("{method} 不支持字段: {key}")));
    }
    Ok(())
}

fn connection_id(args: &Value, ctx: &GatewayContext) -> Result<(), RpcError> {
    if args
        .get("connectionId")
        .is_some_and(|value| value.as_str() != Some(ctx.connection_id.as_str()))
    {
        return Err(RpcError::new(
            "fault.connection.scope",
            "connectionId 不属于当前连接",
        ));
    }
    Ok(())
}

fn runtime_arc(ctx: &GatewayContext) -> Result<std::sync::Arc<AgentRuntime>, RpcError> {
    crate::require_owned_runtime(&ctx.app).map_err(backend_error)
}

fn open_session(ctx: &GatewayContext, session_id: &str) -> Result<RuntimeSession, RpcError> {
    let runtime = runtime_arc(ctx)?;
    crate::session_commands::open_authorized_session(&runtime, &ctx.app, session_id)
        .map_err(backend_error)
}

fn text_attachment_mime(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/ld+json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/toml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/sql"
        )
}

fn validate_send_attachment_name(name: &str) -> Result<(), RpcError> {
    if name.trim().is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
        return Err(invalid_params("附件 fileName 无效"));
    }
    Ok(())
}

/// 将 sendText/队列中的附件引用重新绑定到宿主授权，并物化为 Runtime 输入。
///
/// Queue 只保存原始引用以便恢复；真正消费前必须再次执行本函数，避免冷恢复或
/// 跨连接消费绕过当前 Session/workspace 的资产归属检查。
pub(crate) fn prepare_send_attachments(
    ctx: &GatewayContext,
    session_id: &str,
    values: &[Value],
) -> Result<PreparedSendAttachments, RpcError> {
    if values.len() > MAX_SEND_ATTACHMENTS {
        return Err(invalid_params(format!(
            "附件数量不能超过 {MAX_SEND_ATTACHMENTS}"
        )));
    }
    if values.is_empty() {
        return Ok(PreparedSendAttachments::default());
    }
    let session = open_session(ctx, session_id)?;
    let mut prepared = PreparedSendAttachments::default();
    let mut text_context_bytes = 0usize;
    for raw in values {
        let item: SendAttachmentRef = serde_json::from_value(raw.clone())
            .map_err(|error| invalid_params(format!("附件引用无效: {error}")))?;
        validate_send_attachment_name(&item.file_name)?;
        if !valid_mime(&item.mime) {
            return Err(invalid_params("附件 mime 无效"));
        }
        if item.reference.trim().is_empty() || item.reference.trim() != item.reference {
            return Err(invalid_params("附件 ref 无效"));
        }
        // previewRef 只负责 UI 展示；发送时仍以 ref 对应的真实资产为准，不能把
        // renderer 提供的第二条路径当作读取权限来源。
        let _ = item.preview_ref.as_deref();
        let file = resolve_attachment(
            ctx,
            &session,
            session_id,
            &item.reference,
            AttachmentResolveOptions {
                target: None,
                attachment_index: None,
                media_only: false,
                max_bytes: MAX_UPLOAD_BYTES as u64,
            },
        )?;
        if item.bytes != file.total_bytes {
            return Err(RpcError::new(
                "fault.attachment.sizeMismatch",
                format!(
                    "附件 {} 声明大小 {}，实际大小 {}",
                    item.file_name, item.bytes, file.total_bytes
                ),
            ));
        }
        let reference = InputReference {
            name: item.file_name.clone(),
            path: item.reference.clone(),
        };
        prepared.references.push(reference);

        let is_image = item.mime.to_ascii_lowercase().starts_with("image/");
        if is_image {
            let bytes = fs::read(&file.path).map_err(|error| {
                RpcError::new(
                    "fault.attachment.readFailed",
                    format!("无法读取图片附件 {}: {error}", item.file_name),
                )
            })?;
            prepared
                .images
                .push(ImageContent::from_base64(item.mime, STANDARD.encode(bytes)));
        } else if text_attachment_mime(&item.mime) {
            let bytes = fs::read(&file.path).map_err(|error| {
                RpcError::new(
                    "fault.attachment.readFailed",
                    format!("无法读取文本附件 {}: {error}", item.file_name),
                )
            })?;
            let text = String::from_utf8(bytes).map_err(|_| {
                RpcError::new(
                    "fault.attachment.invalidText",
                    format!("文本附件 {} 不是有效 UTF-8", item.file_name),
                )
            })?;
            text_context_bytes = text_context_bytes.saturating_add(text.len());
            if text_context_bytes > MAX_TEXT_CONTEXT_BYTES {
                return Err(RpcError::new(
                    "fault.attachment.textTooLarge",
                    "文本附件合计超过模型上下文上限",
                ));
            }
            prepared.text_context.push(format!(
                "用户附件文件 `{}` 的授权 UTF-8 内容如下：\n{}",
                item.file_name, text
            ));
        }
    }
    InputReference::validate_all(&prepared.references)
        .map_err(|error| invalid_params(format!("附件引用无效: {error}")))?;
    Ok(prepared)
}

fn parse_target(args: &Value, required: bool) -> Result<Option<RowTarget>, RpcError> {
    let Some(value) = args.get("target") else {
        if required {
            return Err(invalid_params("target 必须存在"));
        }
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| invalid_params("target 必须为对象"))?;
    if object
        .keys()
        .any(|key| !["rowId", "entityId"].contains(&key.as_str()))
    {
        return Err(invalid_params("target 包含未知字段"));
    }
    let row_id = object
        .get("rowId")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_params("target.rowId 必须为非负整数"))?;
    let entity_id = object
        .get("entityId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| invalid_params("target.entityId 必须为非空字符串"))?
        .to_owned();
    Ok(Some(RowTarget { row_id, entity_id }))
}

fn validate_connection_session(
    args: &Value,
    ctx: &GatewayContext,
    method: &str,
    require_target: bool,
) -> Result<(String, Option<RowTarget>), RpcError> {
    reject_unknown(args, ATTACHMENT_READ_ALLOWED_FIELDS, method)?;
    connection_id(args, ctx)?;
    let session_id = required_string(args, "sessionId")?;
    let target = parse_target(args, require_target)?;
    let attachment_index = args.get("attachmentIndex");
    if method == "conversationAttachmentReadV4" && attachment_index.is_none() {
        return Err(invalid_params(
            "conversationAttachmentReadV4 必须提供 attachmentIndex",
        ));
    }
    if target.is_some() != attachment_index.is_some() && method != "conversationAttachmentReadV4" {
        return Err(invalid_params(
            "target 和 attachmentIndex 必须同时存在或同时省略",
        ));
    }
    Ok((session_id, target))
}

async fn background_output(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    reject_unknown(args, &["sessionId", "workId"], "backgroundBashOutputV4")?;
    let session_id = required_string(args, "sessionId")?;
    let work_id = required_string(args, "workId")?;
    let runtime = runtime_arc(ctx)?;
    match runtime.background_bash_output(&session_id, &work_id).await {
        Ok(output) => Ok(background_output_value(&work_id, output)),
        Err(_) => Ok(background_unavailable_value(&work_id)),
    }
}

fn background_output_value(work_id: &str, output: BackgroundBashOutput) -> Value {
    json!({
        "kind": "output",
        "workId": work_id,
        "status": output.status,
        "output": output.output,
        "truncated": output.truncated,
        "outputPath": output.output_path,
    })
}

fn background_unavailable_value(work_id: &str) -> Value {
    json!({
        "kind": "unavailable",
        "workId": work_id,
        "code": "background_task_unavailable",
    })
}

fn file_changes_value(
    files: usize,
    additions: usize,
    deletions: usize,
    items: Vec<Value>,
) -> Value {
    json!({
        "files": files,
        "additions": additions,
        "deletions": deletions,
        "items": items,
    })
}

fn rewind_preview_value(
    can_apply: bool,
    ignored_files: Vec<Value>,
    safe_files: Vec<Value>,
    unsafe_files: Vec<Value>,
) -> Value {
    json!({
        "canApply": can_apply,
        "ignoredFiles": ignored_files,
        "safeFiles": safe_files,
        "unsafeFiles": unsafe_files,
    })
}

fn attachment_read_value(
    data: &[u8],
    media_type: &str,
    total_bytes: u64,
    next_offset: Option<usize>,
) -> Value {
    json!({
        "dataBase64": STANDARD.encode(data),
        "mediaType": media_type,
        "totalBytes": total_bytes,
        "nextOffset": next_offset,
    })
}

fn attachment_stat_value(media_type: &str, total_bytes: u64, mtime_ms: Option<f64>) -> Value {
    let mut result = json!({
        "mediaType": media_type,
        "totalBytes": total_bytes,
    });
    if let Some(mtime_ms) = mtime_ms {
        result["mtimeMs"] = json!(mtime_ms);
    }
    result
}

fn parse_file_query(
    ctx: &GatewayContext,
    args: &Value,
    method: &str,
) -> Result<(RuntimeSession, TargetTurn), RpcError> {
    reject_unknown(
        args,
        &["sessionId", "target", "baseRevision", "baseLogEpoch"],
        method,
    )?;
    let session_id = required_string(args, "sessionId")?;
    let target = parse_target(args, true)?.ok_or_else(|| invalid_params("target 必须存在"))?;
    let base_revision = required_usize(args, "baseRevision")? as u64;
    let base_epoch = required_string(args, "baseLogEpoch")?;
    if base_epoch.is_empty() {
        return Err(invalid_params("baseLogEpoch 无效"));
    }
    let session = open_session(ctx, &session_id)?;
    let snapshot = session
        .snapshot()
        .map_err(|error| backend_error(error.to_string()))?;
    let turn_id = match projection::validate_conversation_query_target(
        &snapshot.state,
        &session_id,
        base_revision,
        &base_epoch,
        target.row_id,
        &target.entity_id,
    ) {
        Ok(turn_id) => turn_id,
        Err(ConversationQueryError::StaleRevision) => {
            return Err(stale_revision(format!(
                "文件变更基于 revision {base_revision}，当前为 {}",
                snapshot.state.transcript_revision
            )));
        }
        Err(ConversationQueryError::StaleLogEpoch) => {
            return Err(stale_revision("文件变更基于过期的 conversation log epoch"));
        }
        Err(ConversationQueryError::TargetNotFound) => {
            return Err(RpcError::new(
                "fault.conversation.targetNotFound",
                "目标行不属于当前会话",
            ));
        }
    };
    Ok((
        session,
        TargetTurn {
            state: snapshot.state,
            turn_id,
        },
    ))
}

fn collect_changes(state: &SessionState, turn_id: &str) -> Vec<ChangeRecord> {
    let mut transcript_order = HashMap::new();
    let mut order = 0usize;
    for message in state.raw_transcript_messages() {
        for part in &message.content {
            let call_id = match part {
                MessagePart::ToolCall { tool_call_id, .. }
                | MessagePart::ToolResult { tool_call_id, .. } => tool_call_id,
                _ => continue,
            };
            transcript_order.entry(call_id.as_str()).or_insert(order);
            order = order.saturating_add(1);
        }
    }
    let mut changes = state
        .tools
        .values()
        .filter(|tool| tool.request.turn_id.as_str() == turn_id)
        .filter_map(|tool| {
            tool.file_change.clone().map(|file_change| ChangeRecord {
                transcript_order: transcript_order
                    .get(tool.request.model_tool_call_id.as_str())
                    .copied(),
                requested_at_unix_ms: tool.requested_at_unix_ms,
                request_id: tool.request.request_id.as_str().to_owned(),
                tool_name: tool.request.tool_name.clone(),
                file_change,
            })
        })
        .collect::<Vec<_>>();
    // 以 Transcript 的首次工具调用位置为主；尚未物化 Transcript 的恢复记录只能
    // 使用其 Journal 请求时间和稳定 requestId，绝不按 BTreeMap 的字典序假定先后。
    changes.sort_by(|left, right| {
        left.transcript_order
            .unwrap_or(usize::MAX)
            .cmp(&right.transcript_order.unwrap_or(usize::MAX))
            .then_with(|| left.requested_at_unix_ms.cmp(&right.requested_at_unix_ms))
            .then_with(|| left.request_id.cmp(&right.request_id))
    });
    changes
}

/// 为 turnHeader 生成同一份文件快照事实的轻量摘要。这里不读取工作区当前内容，
/// 只读取 Runtime 已提交的 before/after Artifact；因此外部修改不会伪造历史摘要。
pub(crate) fn file_change_summary_for_turn(
    session: &RuntimeSession,
    state: &SessionState,
    turn_id: &str,
    reverted: bool,
) -> Result<Option<Value>, RpcError> {
    let changes = collect_changes(state, turn_id)
        .into_iter()
        .filter(|change| change.file_change.applied)
        .collect::<Vec<_>>();
    if changes.is_empty() {
        return Ok(None);
    }

    let mut grouped: HashMap<String, Vec<ChangeRecord>> = HashMap::new();
    for change in changes {
        grouped
            .entry(change.file_change.path.clone())
            .or_default()
            .push(change);
    }
    let mut additions = 0usize;
    let mut deletions = 0usize;
    let mut files = 0usize;
    for records in grouped.values() {
        let first = records
            .first()
            .ok_or_else(|| backend_error("文件变更记录为空"))?;
        let last = records
            .last()
            .ok_or_else(|| backend_error("文件变更记录为空"))?;
        let before = first
            .file_change
            .before
            .as_ref()
            .map(|snapshot| session.read_file_snapshot(snapshot))
            .transpose()
            .map_err(|error| backend_error(error.to_string()))?
            .unwrap_or_default();
        let after = session
            .read_file_snapshot(&last.file_change.after)
            .map_err(|error| backend_error(error.to_string()))?;
        let (file_additions, file_deletions, _) = line_diff(&before, &after);
        files = files.saturating_add(1);
        additions = additions.saturating_add(file_additions);
        deletions = deletions.saturating_add(file_deletions);
    }
    let mut result = file_changes_value(files, additions, deletions, Vec::new());
    result["state"] = json!(if reverted { "reverted" } else { "active" });
    Ok(Some(result))
}

fn remove_connection_uploads(
    uploads: &mut HashMap<UploadKey, UploadEntry>,
    connection_id: &str,
) -> Vec<PathBuf> {
    let keys = uploads
        .keys()
        .filter(|key| key.connection_id == connection_id)
        .cloned()
        .collect::<Vec<_>>();
    keys.into_iter()
        .filter_map(|key| uploads.remove(&key).map(|entry| entry.path))
        .collect()
}

fn conversation_file_changes(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let session_id = required_string(args, "sessionId")?;
    let (session, target) = parse_file_query(ctx, args, "conversationFileChangesV4")?;
    let runtime = runtime_arc(ctx)?;
    let reverted_turn_ids =
        super::session::completed_rewind_turn_ids(runtime.storage_root(), &session_id)
            .map_err(backend_error)?;
    let reverted = reverted_turn_ids.contains(&target.turn_id);
    let changes = collect_changes(&target.state, &target.turn_id)
        .into_iter()
        .filter(|change| change.file_change.applied)
        .collect::<Vec<_>>();
    let root = PathBuf::from(&target.state.project_root);
    let mut items = Vec::new();
    let mut additions = 0usize;
    let mut deletions = 0usize;
    let mut grouped: HashMap<String, Vec<ChangeRecord>> = HashMap::new();
    for change in changes {
        let display_path = display_workspace_path(&root, Path::new(&change.file_change.path))?;
        grouped.entry(display_path).or_default().push(change);
    }
    let mut paths = grouped.into_iter().collect::<Vec<_>>();
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    for (path, records) in paths {
        let first = records
            .first()
            .ok_or_else(|| backend_error("文件变更记录为空"))?;
        let last = records
            .last()
            .ok_or_else(|| backend_error("文件变更记录为空"))?;
        let before = first
            .file_change
            .before
            .as_ref()
            .map(|snapshot| session.read_file_snapshot(snapshot))
            .transpose()
            .map_err(|error| backend_error(error.to_string()))?
            .unwrap_or_default();
        let after = session
            .read_file_snapshot(&last.file_change.after)
            .map_err(|error| backend_error(error.to_string()))?;
        let (file_additions, file_deletions, patches) = line_diff(&before, &after);
        additions = additions.saturating_add(file_additions);
        deletions = deletions.saturating_add(file_deletions);
        items.push(json!({
            "path": path,
            "additions": file_additions,
            "deletions": file_deletions,
            "writeCount": records.len(),
            "toolNames": records.into_iter().map(|record| record.tool_name).collect::<Vec<_>>(),
            "patches": patches,
        }));
    }
    let mut result = file_changes_value(items.len(), additions, deletions, items);
    if result["files"].as_u64().unwrap_or(0) > 0 {
        result["state"] = json!(if reverted { "reverted" } else { "active" });
    }
    Ok(result)
}

fn conversation_file_rewind_preview(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let session_id = required_string(args, "sessionId")?;
    let (_session, target) = parse_file_query(ctx, args, "conversationFileRewindPreviewV4")?;
    let runtime = runtime_arc(ctx)?;
    let reverted_turn_ids =
        super::session::completed_rewind_turn_ids(runtime.storage_root(), &session_id)
            .map_err(backend_error)?;
    if reverted_turn_ids.contains(&target.turn_id) {
        // 已完成的 rewind 是持久事实；重复打开面板不能重新验证当前工作区并制造可重放入口。
        return Ok(rewind_preview_value(
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ));
    }
    let root = PathBuf::from(&target.state.project_root);
    let changes = collect_changes(&target.state, &target.turn_id)
        .into_iter()
        .filter(|change| change.file_change.applied)
        .collect::<Vec<_>>();
    let mut grouped: HashMap<String, Vec<ChangeRecord>> = HashMap::new();
    for change in changes {
        let path = display_workspace_path(&root, Path::new(&change.file_change.path))?;
        grouped.entry(path).or_default().push(change);
    }
    let mut safe_files = Vec::new();
    let mut unsafe_files = Vec::new();
    let ignored_files = Vec::<Value>::new();
    for (path, records) in grouped {
        let first = records.first().expect("grouped change cannot be empty");
        let last = records.last().expect("grouped change cannot be empty");
        let expected_hash = &last.file_change.after.sha256;
        let current = current_workspace_file(ctx, &root, &first.file_change.path)?;
        let tool_names = records
            .iter()
            .map(|record| record.tool_name.clone())
            .collect::<Vec<_>>();
        let operation_count = records.len();
        let Some(current) = current else {
            unsafe_files.push(json!({
                "expectedHash": expected_hash,
                "message": "当前文件不存在，无法证明仍是最后一次工具写入结果",
                "operationCount": operation_count,
                "path": path,
                "reason": "external_modified",
                "toolNames": tool_names,
            }));
            continue;
        };
        let bytes = fs::read(&current).map_err(|error| backend_error(error.to_string()))?;
        let current_hash = sha256_hex(&bytes);
        if &current_hash != expected_hash {
            unsafe_files.push(json!({
                "currentHash": current_hash,
                "expectedHash": expected_hash,
                "message": "工作区文件已被外部修改",
                "operationCount": operation_count,
                "path": path,
                "reason": "external_modified",
                "toolNames": tool_names,
            }));
            continue;
        }
        safe_files.push(json!({
            "action": if first.file_change.before.is_some() { "restore" } else { "delete" },
            "operationCount": operation_count,
            "path": path,
            "toolNames": tool_names,
        }));
    }
    Ok(rewind_preview_value(
        unsafe_files.is_empty() && !safe_files.is_empty(),
        ignored_files,
        safe_files,
        unsafe_files,
    ))
}

fn display_workspace_path(root: &Path, path: &Path) -> Result<String, RpcError> {
    if !path.is_absolute() {
        return Err(RpcError::new(
            "fault.conversation.fileChangePath",
            "文件变更路径必须是绝对路径",
        ));
    }
    let root = fs::canonicalize(root).map_err(|error| backend_error(error.to_string()))?;
    let candidate = if path.exists() {
        fs::canonicalize(path).map_err(|error| backend_error(error.to_string()))?
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| backend_error("文件变更路径缺少父目录"))?;
        let parent = fs::canonicalize(parent).map_err(|error| backend_error(error.to_string()))?;
        parent.join(
            path.file_name()
                .ok_or_else(|| backend_error("文件名为空"))?,
        )
    };
    if !candidate.starts_with(&root) {
        return Err(RpcError::new(
            "fault.conversation.fileChangePath",
            "文件变更路径越过 Session 工作区",
        ));
    }
    Ok(crate::path_utils::path_to_frontend(&candidate))
}

fn current_workspace_file(
    ctx: &GatewayContext,
    root: &Path,
    path: &str,
) -> Result<Option<PathBuf>, RpcError> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return Err(invalid_params("文件变更路径必须是绝对路径"));
    }
    if candidate.exists() {
        let canonical = crate::workspace::authorize_existing_absolute(&ctx.app, candidate)
            .map_err(backend_error)?;
        let canonical_root =
            fs::canonicalize(root).map_err(|error| backend_error(error.to_string()))?;
        if !canonical.starts_with(&canonical_root) || !canonical.is_file() {
            return Err(backend_error("文件变更目标不是工作区普通文件"));
        }
        return Ok(Some(canonical));
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| backend_error("文件变更路径缺少父目录"))?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| backend_error(error.to_string()))?;
    let canonical_root =
        fs::canonicalize(root).map_err(|error| backend_error(error.to_string()))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(backend_error("文件变更目标越过工作区"));
    }
    Ok(None)
}

fn line_diff(before: &[u8], after: &[u8]) -> (usize, usize, Vec<Value>) {
    let old = String::from_utf8(before.to_vec());
    let new = String::from_utf8(after.to_vec());
    let (Ok(old), Ok(new)) = (old, new) else {
        return (line_count(before), line_count(after), Vec::new());
    };
    let old_lines = split_lines(&old);
    let new_lines = split_lines(&new);
    if old_lines == new_lines {
        return (0, 0, Vec::new());
    }
    let area = old_lines.len().saturating_mul(new_lines.len());
    if area > 4_000_000 {
        let mut lines = old_lines
            .iter()
            .map(|line| format!("-{line}"))
            .collect::<Vec<_>>();
        lines.extend(new_lines.iter().map(|line| format!("+{line}")));
        return (
            new_lines.len(),
            old_lines.len(),
            vec![json!({
                "oldStart": 1,
                "oldLines": old_lines.len(),
                "newStart": 1,
                "newLines": new_lines.len(),
                "lines": lines,
            })],
        );
    }
    let mut table = vec![vec![0u32; new_lines.len() + 1]; old_lines.len() + 1];
    for old_index in (0..old_lines.len()).rev() {
        for new_index in (0..new_lines.len()).rev() {
            table[old_index][new_index] = if old_lines[old_index] == new_lines[new_index] {
                table[old_index + 1][new_index + 1] + 1
            } else {
                table[old_index + 1][new_index].max(table[old_index][new_index + 1])
            };
        }
    }
    let mut old_index = 0;
    let mut new_index = 0;
    let mut patch_lines = Vec::new();
    let mut additions = 0;
    let mut deletions = 0;
    while old_index < old_lines.len() || new_index < new_lines.len() {
        if old_index < old_lines.len()
            && new_index < new_lines.len()
            && old_lines[old_index] == new_lines[new_index]
        {
            patch_lines.push(format!(" {}", old_lines[old_index]));
            old_index += 1;
            new_index += 1;
        } else if new_index < new_lines.len()
            && (old_index == old_lines.len()
                || table[old_index][new_index + 1] >= table[old_index + 1][new_index])
        {
            patch_lines.push(format!("+{}", new_lines[new_index]));
            additions += 1;
            new_index += 1;
        } else {
            patch_lines.push(format!("-{}", old_lines[old_index]));
            deletions += 1;
            old_index += 1;
        }
    }
    (
        additions,
        deletions,
        vec![json!({
            "oldStart": 1,
            "oldLines": old_lines.len(),
            "newStart": 1,
            "newLines": new_lines.len(),
            "lines": patch_lines,
        })],
    )
}

fn split_lines(value: &str) -> Vec<String> {
    if value.is_empty() {
        Vec::new()
    } else {
        value.split_inclusive('\n').map(ToOwned::to_owned).collect()
    }
}

fn line_count(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    bytes.iter().filter(|byte| **byte == b'\n').count().max(1)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn upload_store() -> &'static Mutex<HashMap<UploadKey, UploadEntry>> {
    UPLOADS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn valid_upload_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'.' | b'_' | b':' | b'-'))
        })
}

fn valid_mime(value: &str) -> bool {
    let mut parts = value.split('/');
    let Some(major) = parts.next() else {
        return false;
    };
    let Some(minor) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && (3..=255).contains(&value.len())
        && major.bytes().all(|byte| byte.is_ascii_alphanumeric())
        && minor.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}

fn valid_checksum(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn same_upload_metadata(
    entry: &UploadEntry,
    file_name: &str,
    mime: &str,
    total_bytes: usize,
    total_chunks: usize,
    checksum: &str,
) -> bool {
    entry.file_name == file_name
        && entry.mime == mime
        && entry.total_bytes == total_bytes
        && entry.total_chunks == total_chunks
        && entry.checksum == checksum
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChunkValidationError {
    Committed,
    OutOfOrder,
    ExceedsDeclaredSize,
}

fn validate_chunk_progress(
    entry: &UploadEntry,
    chunk_index: usize,
    chunk_len: usize,
) -> Result<(), ChunkValidationError> {
    if entry.committed_ref.is_some() {
        return Err(ChunkValidationError::Committed);
    }
    if chunk_index != entry.next_chunk_index || chunk_index >= entry.total_chunks {
        return Err(ChunkValidationError::OutOfOrder);
    }
    if entry.received_bytes.saturating_add(chunk_len) > entry.total_bytes
        || entry.received_bytes.saturating_add(chunk_len) > MAX_STAGED_BYTES
    {
        return Err(ChunkValidationError::ExceedsDeclaredSize);
    }
    Ok(())
}

fn staging_path(app: &AppHandle, key: &UploadKey) -> Result<PathBuf, RpcError> {
    let root = crate::storage::root_dir(app)
        .map_err(|error| backend_error(error.to_string()))?
        .join("v4-attachment-upload");
    fs::create_dir_all(&root).map_err(|error| backend_error(error.to_string()))?;
    let digest = sha256_hex(
        format!(
            "{}\0{}\0{}",
            key.connection_id, key.session_id, key.upload_id
        )
        .as_bytes(),
    );
    Ok(root.join(format!("{digest}.part")))
}

fn canonical_base64(value: &str) -> Result<Vec<u8>, RpcError> {
    if !value.len().is_multiple_of(4) {
        return Err(invalid_params("dataBase64 不是规范 Base64"));
    }
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| invalid_params("dataBase64 不是规范 Base64"))?;
    if STANDARD.encode(&decoded) != value {
        return Err(invalid_params("dataBase64 不是规范 Base64"));
    }
    Ok(decoded)
}

fn attachment_begin(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    reject_unknown(
        args,
        &[
            "connectionId",
            "uploadId",
            "sessionId",
            "fileName",
            "mime",
            "totalBytes",
            "totalChunks",
            "checksum",
        ],
        "attachmentBeginV4",
    )?;
    // UI facade 不携带 connectionId；上传事务的可信连接身份来自 GatewayContext。
    connection_id(args, ctx)?;
    let upload_id = required_string(args, "uploadId")?;
    if !valid_upload_id(&upload_id) {
        return Err(invalid_params("uploadId 格式无效"));
    }
    let session_id = required_string(args, "sessionId")?;
    let file_name = required_string(args, "fileName")?;
    if file_name.len() > 255 || file_name.chars().any(char::is_control) {
        return Err(invalid_params("fileName 无效"));
    }
    let mime = required_string(args, "mime")?;
    if !valid_mime(&mime) {
        return Err(invalid_params("mime 无效"));
    }
    let total_bytes = required_usize(args, "totalBytes")?;
    let total_chunks = required_usize(args, "totalChunks")?;
    let checksum = required_string(args, "checksum")?;
    if total_bytes > MAX_UPLOAD_BYTES
        || total_chunks > MAX_UPLOAD_CHUNKS
        || (total_bytes == 0) != (total_chunks == 0)
        || (total_bytes > 0 && total_chunks == 0)
        || !valid_checksum(&checksum)
    {
        return Err(invalid_params("上传大小、分块数量或 checksum 无效"));
    }
    if total_bytes > 0 && total_chunks < total_bytes.div_ceil(MAX_CHUNK_BYTES) {
        return Err(invalid_params("totalChunks 小于声明的最小分块数量"));
    }
    let _ = open_session(ctx, &session_id)?;
    let key = UploadKey {
        connection_id: ctx.connection_id.clone(),
        session_id,
        upload_id: upload_id.clone(),
    };
    let mut uploads = upload_store()
        .lock()
        .map_err(|_| RpcError::new("fault.state", "附件上传状态不可用"))?;
    if let Some(entry) = uploads.get(&key) {
        if !same_upload_metadata(
            entry,
            &file_name,
            &mime,
            total_bytes,
            total_chunks,
            &checksum,
        ) {
            return Err(RpcError::new(
                "fault.attachment.conflict",
                "uploadId 已绑定其他上传",
            ));
        }
        if let Some(reference) = &entry.committed_ref {
            return Ok(json!({
                "uploadId": upload_id,
                "state": "committed",
                "nextChunkIndex": entry.next_chunk_index,
                "ref": reference,
            }));
        }
        return Ok(json!({
            "uploadId": upload_id,
            "state": "staging",
            "nextChunkIndex": entry.next_chunk_index,
        }));
    }
    let path = staging_path(&ctx.app, &key)?;
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| backend_error(error.to_string()))?;
    drop(file);
    uploads.insert(
        key,
        UploadEntry {
            file_name,
            mime,
            total_bytes,
            total_chunks,
            checksum,
            path,
            received_bytes: 0,
            next_chunk_index: 0,
            committed_ref: None,
        },
    );
    Ok(json!({
        "uploadId": upload_id,
        "state": "staging",
        "nextChunkIndex": 0,
    }))
}

fn upload_key(
    ctx: &GatewayContext,
    args: &Value,
    method: &str,
) -> Result<(UploadKey, String), RpcError> {
    reject_unknown(
        args,
        &[
            "connectionId",
            "uploadId",
            "sessionId",
            "chunkIndex",
            "dataBase64",
        ],
        method,
    )?;
    connection_id(args, ctx)?;
    let upload_id = required_string(args, "uploadId")?;
    if !valid_upload_id(&upload_id) {
        return Err(invalid_params("uploadId 格式无效"));
    }
    let session_id = required_string(args, "sessionId")?;
    Ok((
        UploadKey {
            connection_id: ctx.connection_id.clone(),
            session_id: session_id.clone(),
            upload_id,
        },
        session_id,
    ))
}

fn attachment_chunk(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let (key, session_id) = upload_key(ctx, args, "attachmentChunkV4")?;
    let chunk_index = required_usize(args, "chunkIndex")?;
    let data = args
        .get("dataBase64")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("dataBase64 必须为字符串"))?;
    let bytes = canonical_base64(data)?;
    if bytes.len() > MAX_CHUNK_BYTES {
        return Err(invalid_params("上传分块超过 512 KiB"));
    }
    let _ = open_session(ctx, &session_id)?;
    let mut uploads = upload_store()
        .lock()
        .map_err(|_| RpcError::new("fault.state", "附件上传状态不可用"))?;
    let entry = uploads
        .get_mut(&key)
        .ok_or_else(|| RpcError::new("fault.attachment.notFound", "上传事务不存在"))?;
    match validate_chunk_progress(entry, chunk_index, bytes.len()) {
        Ok(()) => {}
        Err(ChunkValidationError::Committed) => {
            return Err(RpcError::new(
                "fault.attachment.committed",
                "上传事务已经提交",
            ));
        }
        Err(ChunkValidationError::OutOfOrder) => {
            return Err(RpcError::new(
                "fault.attachment.chunkOrder",
                "上传分块顺序不正确",
            ));
        }
        Err(ChunkValidationError::ExceedsDeclaredSize) => {
            return Err(invalid_params("上传分块超出声明大小"));
        }
    }
    let mut file = OpenOptions::new()
        .append(true)
        .open(&entry.path)
        .map_err(|error| backend_error(error.to_string()))?;
    file.write_all(&bytes)
        .map_err(|error| backend_error(error.to_string()))?;
    entry.received_bytes += bytes.len();
    entry.next_chunk_index += 1;
    Ok(json!({
        "uploadId": key.upload_id,
        "nextChunkIndex": entry.next_chunk_index,
    }))
}

fn attachment_commit(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let (key, session_id) = upload_key(ctx, args, "attachmentCommitV4")?;
    reject_unknown(
        args,
        &["connectionId", "uploadId", "sessionId"],
        "attachmentCommitV4",
    )?;
    let _ = open_session(ctx, &session_id)?;
    let mut uploads = upload_store()
        .lock()
        .map_err(|_| RpcError::new("fault.state", "附件上传状态不可用"))?;
    let entry = uploads
        .get_mut(&key)
        .ok_or_else(|| RpcError::new("fault.attachment.notFound", "上传事务不存在"))?;
    if let Some(reference) = &entry.committed_ref {
        return Ok(json!({"ref": reference}));
    }
    if entry.next_chunk_index != entry.total_chunks || entry.received_bytes != entry.total_bytes {
        return Err(RpcError::new(
            "fault.attachment.incomplete",
            "上传分块尚未完整提交",
        ));
    }
    let bytes = fs::read(&entry.path).map_err(|error| backend_error(error.to_string()))?;
    if bytes.len() != entry.total_bytes {
        return Err(RpcError::new(
            "fault.attachment.sizeMismatch",
            "上传文件大小不一致",
        ));
    }
    let actual = format!("sha256:{}", sha256_hex(&bytes));
    if actual != entry.checksum {
        return Err(RpcError::new(
            "fault.attachment.checksumMismatch",
            "附件 checksum 不匹配",
        ));
    }
    let kind = if entry.mime.starts_with("image/") {
        "image"
    } else {
        "file"
    };
    let asset = crate::ui_attachments::stage_v4(
        &crate::storage::root_dir(&ctx.app)
            .map_err(|error| backend_error(error.to_string()))?
            .join("ui-attachments"),
        session_id,
        kind.to_owned(),
        entry.file_name.clone(),
        entry.mime.clone(),
        &bytes,
    )
    .map_err(backend_error)?;
    ctx.app
        .asset_protocol_scope()
        .allow_file(&asset.path)
        .map_err(|error| backend_error(error.to_string()))?;
    let reference = asset.path;
    let path = entry.path.clone();
    entry.committed_ref = Some(reference.clone());
    let _ = fs::remove_file(path);
    Ok(json!({"ref": reference}))
}

fn attachment_abort(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let (key, _session_id) = upload_key(ctx, args, "attachmentAbortV4")?;
    reject_unknown(
        args,
        &["connectionId", "uploadId", "sessionId"],
        "attachmentAbortV4",
    )?;
    let mut uploads = upload_store()
        .lock()
        .map_err(|_| RpcError::new("fault.state", "附件上传状态不可用"))?;
    if let Some(entry) = uploads.remove(&key) {
        let _ = fs::remove_file(entry.path);
    }
    Ok(json!({}))
}

fn uploaded_asset_id(app: &AppHandle, reference: &str) -> Option<String> {
    let id = reference.strip_prefix("keencode-attachment://")?;
    if valid_upload_id(id) {
        return Some(id.to_owned());
    }
    let root = crate::storage::root_dir(app).ok()?.join("ui-attachments");
    let canonical_root = fs::canonicalize(root).ok()?;
    let candidate = fs::canonicalize(reference).ok()?;
    let folder = candidate.parent()?;
    if folder.parent()? != canonical_root || !candidate.is_file() {
        return None;
    }
    let id = folder.file_name()?.to_str()?;
    if valid_upload_id(id) {
        Some(id.to_owned())
    } else {
        None
    }
}

fn resolve_attachment(
    ctx: &GatewayContext,
    session: &RuntimeSession,
    session_id: &str,
    reference: &str,
    options: AttachmentResolveOptions<'_>,
) -> Result<AttachmentFile, RpcError> {
    if let (Some(target), Some(index)) = (options.target, options.attachment_index) {
        authorize_attachment_reference(session, target, index, reference)?;
    }
    if let Some(id) = uploaded_asset_id(&ctx.app, reference) {
        let asset =
            crate::ui_attachments::resolve_v4(&ctx.app, &id, session_id).map_err(backend_error)?;
        let path = PathBuf::from(&asset.path);
        let metadata = fs::metadata(&path).map_err(|error| backend_error(error.to_string()))?;
        if metadata.len() > options.max_bytes {
            return Err(RpcError::new(
                "fault.attachment.tooLarge",
                "附件超过预览大小上限",
            ));
        }
        if options.media_only && !is_media_preview(&asset.mime_type) {
            return Err(invalid_params("attachmentReadV4 只支持 image/video/pdf"));
        }
        enforce_media_preview_size(&asset.mime_type, metadata.len(), options.media_only)?;
        return Ok(AttachmentFile {
            path,
            media_type: asset.mime_type,
            total_bytes: metadata.len(),
        });
    }
    let path = crate::workspace::authorize_existing_absolute(&ctx.app, Path::new(reference))
        .map_err(backend_error)?;
    let metadata = fs::metadata(&path).map_err(|error| backend_error(error.to_string()))?;
    if !metadata.is_file() || metadata.len() > options.max_bytes {
        return Err(RpcError::new(
            "fault.attachment.tooLarge",
            "附件不是可读取的有界普通文件",
        ));
    }
    // Share 读取允许任意已授权 MIME；桌面 localPath 没有独立 MIME 元数据时，
    // 使用二进制兜底类型，媒体预览分支仍会在下一行严格拒绝它。
    let media_type = infer_media_type(&path).unwrap_or_else(|| "application/octet-stream".into());
    if options.media_only && !is_media_preview(&media_type) {
        return Err(invalid_params("attachmentReadV4 只支持 image/video/pdf"));
    }
    enforce_media_preview_size(&media_type, metadata.len(), options.media_only)?;
    Ok(AttachmentFile {
        path,
        media_type,
        total_bytes: metadata.len(),
    })
}

fn enforce_media_preview_size(
    media_type: &str,
    total_bytes: u64,
    media_only: bool,
) -> Result<(), RpcError> {
    // Source 的 attachmentReadResult 对 image 的 totalBytes 上限仍是上传上限，
    // workspace localPath 不能借由通用的 video/PDF 30 MiB 上限绕过它。
    if media_only && media_type.starts_with("image/") && total_bytes > MAX_UPLOAD_BYTES as u64 {
        return Err(RpcError::new(
            "fault.attachment.tooLarge",
            "图片附件超过 20 MiB 预览上限",
        ));
    }
    Ok(())
}

fn is_media_preview(media_type: &str) -> bool {
    media_type.starts_with("image/")
        || media_type.starts_with("video/")
        || media_type
            .split(';')
            .next()
            .map(str::trim)
            .map(|value| value.eq_ignore_ascii_case("application/pdf"))
            .unwrap_or(false)
}

fn infer_media_type(path: &Path) -> Option<String> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "pdf" => "application/pdf",
        _ => return None,
    };
    Some(media_type.to_owned())
}

fn authorize_attachment_reference(
    session: &RuntimeSession,
    target: &RowTarget,
    index: usize,
    reference: &str,
) -> Result<(), RpcError> {
    let snapshot = session
        .snapshot()
        .map_err(|error| backend_error(error.to_string()))?;
    if projection::conversation_row_turn(&snapshot.state, target.row_id, &target.entity_id)
        .is_none()
    {
        return Err(RpcError::new(
            "fault.conversation.targetNotFound",
            "附件目标行不存在或 rowId/entityId 不匹配",
        ));
    }
    let message = snapshot
        .state
        .raw_transcript_messages()
        .into_iter()
        .find(|message| message.message_id == target.entity_id)
        .ok_or_else(|| RpcError::new("fault.conversation.targetNotFound", "附件目标消息不存在"))?;
    if message.role != MessageRole::User {
        return Err(RpcError::new(
            "fault.attachment.unauthorized",
            "附件目标不是用户消息",
        ));
    }
    let input = message
        .references
        .get(index)
        .ok_or_else(|| RpcError::new("fault.attachment.notFound", "附件序号不存在"))?;
    if input.path != reference {
        return Err(RpcError::new(
            "fault.attachment.unauthorized",
            "附件引用与目标消息不一致",
        ));
    }
    Ok(())
}

fn attachment_read(
    ctx: &GatewayContext,
    args: &Value,
    conversation_read: bool,
) -> Result<Value, RpcError> {
    let method = if conversation_read {
        "conversationAttachmentReadV4"
    } else {
        "attachmentReadV4"
    };
    let (session_id, target) = validate_connection_session(args, ctx, method, conversation_read)?;
    let attachment_index = args
        .get("attachmentIndex")
        .map(|_| required_usize(args, "attachmentIndex"))
        .transpose()?;
    let reference = required_string(args, "ref")?;
    let offset = required_usize(args, "offset")?;
    let limit = required_usize(args, "limit")?;
    if limit == 0 || limit > MAX_CHUNK_BYTES {
        return Err(invalid_params("limit 必须在 1..=512 KiB"));
    }
    let session = open_session(ctx, &session_id)?;
    let file = resolve_attachment(
        ctx,
        &session,
        &session_id,
        &reference,
        AttachmentResolveOptions {
            target: target.as_ref(),
            attachment_index,
            media_only: !conversation_read,
            max_bytes: MAX_PREVIEW_BYTES,
        },
    )?;
    if offset > file.total_bytes as usize {
        return Err(invalid_params("offset 超出附件大小"));
    }
    let mut handle =
        fs::File::open(&file.path).map_err(|error| backend_error(error.to_string()))?;
    let mut bytes = Vec::new();
    handle
        .read_to_end(&mut bytes)
        .map_err(|error| backend_error(error.to_string()))?;
    let end = offset.saturating_add(limit).min(bytes.len());
    let data = &bytes[offset..end];
    Ok(attachment_read_value(
        data,
        &file.media_type,
        file.total_bytes,
        (end < bytes.len()).then_some(end),
    ))
}

fn attachment_stat(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    reject_unknown(
        args,
        &["sessionId", "ref", "target", "attachmentIndex"],
        "conversationAttachmentStatV4",
    )?;
    let session_id = required_string(args, "sessionId")?;
    let target = parse_target(args, true)?.ok_or_else(|| invalid_params("target 必须存在"))?;
    let index = required_usize(args, "attachmentIndex")?;
    let reference = required_string(args, "ref")?;
    let session = open_session(ctx, &session_id)?;
    let file = resolve_attachment(
        ctx,
        &session,
        &session_id,
        &reference,
        AttachmentResolveOptions {
            target: Some(&target),
            attachment_index: Some(index),
            media_only: false,
            max_bytes: MAX_STAT_BYTES,
        },
    )?;
    let metadata = fs::metadata(&file.path).map_err(|error| backend_error(error.to_string()))?;
    Ok(attachment_stat_value(
        &file.media_type,
        metadata.len(),
        metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as f64),
    ))
}

fn attachment_preview_source(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    reject_unknown(
        args,
        &[
            "sessionId",
            "ref",
            "target",
            "attachmentIndex",
            "clientMode",
        ],
        "attachmentPreviewSourceV4",
    )?;
    let session_id = required_string(args, "sessionId")?;
    let reference = required_string(args, "ref")?;
    let client_mode = required_string(args, "clientMode")?;
    if client_mode != "desktop-continuous" && client_mode != "web-remote-replayable" {
        return Err(invalid_params("clientMode 无效"));
    }
    let target = parse_target(args, false)?;
    let attachment_index = args
        .get("attachmentIndex")
        .map(|_| required_usize(args, "attachmentIndex"))
        .transpose()?;
    if target.is_some() != args.get("attachmentIndex").is_some() {
        return Err(invalid_params("target 和 attachmentIndex 必须成对出现"));
    }
    let session = open_session(ctx, &session_id)?;
    let file = resolve_attachment(
        ctx,
        &session,
        &session_id,
        &reference,
        AttachmentResolveOptions {
            target: target.as_ref(),
            attachment_index,
            media_only: true,
            max_bytes: MAX_PREVIEW_BYTES,
        },
    )?;
    if client_mode == "desktop-continuous" && file.media_type.starts_with("video/") {
        return Ok(json!({
            "kind": "local_path",
            "path": crate::path_utils::path_to_frontend(&file.path),
            "mediaType": file.media_type,
        }));
    }
    Ok(json!({"kind": "chunked"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_exact_keys(value: &Value, expected: &[&str]) {
        let object = value.as_object().expect("fixture must be an object");
        let mut actual = object.keys().map(String::as_str).collect::<Vec<_>>();
        let mut expected = expected.to_vec();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
    }

    fn test_upload_entry(total_bytes: usize, total_chunks: usize) -> UploadEntry {
        UploadEntry {
            file_name: "note.txt".to_owned(),
            mime: "text/plain".to_owned(),
            total_bytes,
            total_chunks,
            checksum: format!("sha256:{}", "a".repeat(64)),
            path: PathBuf::from("test.part"),
            received_bytes: 0,
            next_chunk_index: 0,
            committed_ref: None,
        }
    }

    #[test]
    fn checksum_and_upload_ids_are_strict() {
        assert!(valid_upload_id("upload-1:part"));
        assert!(!valid_upload_id("../escape"));
        assert!(valid_checksum(&format!("sha256:{}", "a".repeat(64))));
        assert!(!valid_checksum(&format!("sha256:{}", "A".repeat(64))));
    }

    #[test]
    fn canonical_base64_rejects_noncanonical_padding() {
        assert_eq!(canonical_base64("aGk=").unwrap(), b"hi");
        assert!(canonical_base64("aGk").is_err());
        assert!(canonical_base64("aGk=").is_ok());
    }

    #[test]
    fn workspace_target_is_removed_before_strict_v4_method_validation() {
        let args = json!({
            "workspacePath": "C:/workspace",
            "workspaceIdentity": "workspace-key",
            "remoteSessionId": "remote-session",
            "connectionId": "connection-1",
            "sessionId": "session-1",
            "uploadId": "upload-1",
            "fileName": "note.txt",
            "mime": "text/plain",
            "totalBytes": 2,
            "totalChunks": 1,
            "checksum": format!("sha256:{}", "a".repeat(64)),
        });
        let normalized = strip_v4_target_fields(args).unwrap();
        assert_exact_keys(
            &normalized,
            &[
                "connectionId",
                "sessionId",
                "uploadId",
                "fileName",
                "mime",
                "totalBytes",
                "totalChunks",
                "checksum",
            ],
        );
        reject_unknown(
            &normalized,
            &[
                "connectionId",
                "uploadId",
                "sessionId",
                "fileName",
                "mime",
                "totalBytes",
                "totalChunks",
                "checksum",
            ],
            "attachmentBeginV4",
        )
        .unwrap();
    }

    #[test]
    fn workspace_target_validation_does_not_accept_non_string_fields() {
        let args = json!({"workspaceIdentity": null});
        assert!(optional_target_string(&args, "workspaceIdentity").is_err());
        let args = json!({"remoteSessionId": " remote "});
        assert!(optional_target_string(&args, "remoteSessionId").is_err());
    }

    #[test]
    fn attachment_read_rejects_preview_client_mode() {
        let args = json!({
            "sessionId": "session-1",
            "ref": "C:/workspace/image.png",
            "offset": 0,
            "limit": 1,
            "clientMode": "desktop-continuous",
        });
        for method in ["attachmentReadV4", "conversationAttachmentReadV4"] {
            assert!(reject_unknown(&args, ATTACHMENT_READ_ALLOWED_FIELDS, method).is_err());
        }
    }

    #[test]
    fn desktop_preview_authority_overwrites_caller_mode() {
        let args = inject_desktop_preview_client_mode(json!({
            "sessionId": "session-1",
            "ref": "keencode-attachment://asset-1",
            "clientMode": "web-remote-replayable",
        }))
        .unwrap();
        assert_eq!(args["clientMode"], "desktop-continuous");
    }

    #[test]
    fn image_preview_uses_source_20_mib_limit_but_video_keeps_preview_limit() {
        assert!(enforce_media_preview_size("image/png", MAX_UPLOAD_BYTES as u64, true,).is_ok());
        assert!(
            enforce_media_preview_size("image/png", MAX_UPLOAD_BYTES as u64 + 1, true,).is_err()
        );
        assert!(enforce_media_preview_size("video/mp4", MAX_PREVIEW_BYTES, true,).is_ok());
        assert!(
            enforce_media_preview_size("image/png", MAX_UPLOAD_BYTES as u64 + 1, false,).is_ok()
        );
    }

    #[test]
    fn line_diff_reports_real_additions_and_deletions() {
        let (additions, deletions, patches) = line_diff(b"a\nb\n", b"a\nc\n");
        assert_eq!((additions, deletions), (1, 1));
        assert!(
            patches[0]["lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|line| line == "+c\n")
        );
    }

    #[test]
    fn upload_transactions_reject_reordering_and_metadata_conflicts() {
        let entry = test_upload_entry(3, 2);
        assert_eq!(
            validate_chunk_progress(&entry, 1, 1),
            Err(ChunkValidationError::OutOfOrder)
        );
        assert!(validate_chunk_progress(&entry, 0, 2).is_ok());

        assert!(same_upload_metadata(
            &entry,
            "note.txt",
            "text/plain",
            3,
            2,
            &entry.checksum,
        ));
        assert!(!same_upload_metadata(
            &entry,
            "note.txt",
            "text/plain",
            3,
            2,
            &format!("sha256:{}", "b".repeat(64)),
        ));
    }

    #[test]
    fn upload_transactions_are_isolated_and_disconnect_only_cleans_one_connection() {
        let mut uploads = HashMap::new();
        let entry = test_upload_entry(1, 1);
        uploads.insert(
            UploadKey {
                connection_id: "conn-a".to_owned(),
                session_id: "session-a".to_owned(),
                upload_id: "upload-a".to_owned(),
            },
            entry.clone(),
        );
        uploads.insert(
            UploadKey {
                connection_id: "conn-a".to_owned(),
                session_id: "session-b".to_owned(),
                upload_id: "upload-b".to_owned(),
            },
            entry.clone(),
        );
        uploads.insert(
            UploadKey {
                connection_id: "conn-b".to_owned(),
                session_id: "session-a".to_owned(),
                upload_id: "upload-a".to_owned(),
            },
            entry,
        );

        let removed = remove_connection_uploads(&mut uploads, "conn-a");
        assert_eq!(removed.len(), 2);
        assert_eq!(uploads.len(), 1);
        assert!(uploads.keys().all(|key| key.connection_id == "conn-b"));
    }

    #[test]
    fn file_changes_follow_transcript_order_instead_of_request_id_order() {
        use keencode_resources::{
            AgentId, FileSnapshot, MessagePart, MessageRole, RequestId, SessionId, SessionMessage,
            ToolEffect, ToolLifecycle, ToolRequest, TranscriptRecord, TurnId,
        };

        let session_id = SessionId::new("session-1").unwrap();
        let turn_id = TurnId::new("turn-1").unwrap();
        let agent_id = AgentId::new("root").unwrap();
        let first_call = "z-call";
        let second_call = "a-call";
        let message = |message_id: &str, call_id: &str| SessionMessage {
            is_meta: false,
            references: Vec::new(),
            message_id: message_id.to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(agent_id.clone()),
            role: MessageRole::Assistant,
            content: vec![MessagePart::ToolCall {
                tool_call_id: call_id.to_owned(),
                tool_name: "write_file".to_owned(),
                arguments: json!({"path": "file.txt"}),
            }],
        };
        let snapshot = |hash: &str| FileSnapshot {
            size_bytes: 0,
            sha256: hash.to_owned(),
            chunks: Vec::new(),
        };
        let change = |hash: &str| ToolFileChange {
            path: "C:\\workspace\\file.txt".to_owned(),
            before: None,
            before_readonly: None,
            after: snapshot(hash),
            after_readonly: None,
            applied: true,
        };
        let request = |request_id: RequestId, call_id: &str| ToolRequest {
            request_id,
            turn_id: turn_id.clone(),
            agent_id: agent_id.clone(),
            model_round: 1,
            request_index: 0,
            model_tool_call_id: call_id.to_owned(),
            tool_name: "write_file".to_owned(),
            arguments: json!({}),
            effect: ToolEffect::ChangesState,
        };
        let mut state = SessionState::empty(session_id);
        state.transcript = vec![
            TranscriptRecord::MessageAdded(message("message-z", first_call)),
            TranscriptRecord::MessageAdded(message("message-a", second_call)),
        ];
        state.tools.insert(
            RequestId::new("a".repeat(64)).unwrap(),
            ToolLifecycle {
                request: request(RequestId::new("a".repeat(64)).unwrap(), second_call),
                requested_at_unix_ms: 2,
                execution_started: true,
                execution_started_at_unix_ms: Some(2),
                outcome: None,
                completed_at_unix_ms: None,
                file_change: Some(change("second")),
                transcript_segment: None,
            },
        );
        state.tools.insert(
            RequestId::new("b".repeat(64)).unwrap(),
            ToolLifecycle {
                request: request(RequestId::new("b".repeat(64)).unwrap(), first_call),
                requested_at_unix_ms: 1,
                execution_started: true,
                execution_started_at_unix_ms: Some(1),
                outcome: None,
                completed_at_unix_ms: None,
                file_change: Some(change("first")),
                transcript_segment: None,
            },
        );

        let changes = collect_changes(&state, "turn-1");
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].file_change.after.sha256, "first");
        assert_eq!(changes[1].file_change.after.sha256, "second");
    }

    #[test]
    fn strict_v4_result_fixtures_have_source_schema_keys() {
        let item = json!({
            "path": "file.txt",
            "additions": 1,
            "deletions": 1,
            "writeCount": 1,
            "toolNames": ["write_file"],
            "patches": [{
                "oldStart": 1,
                "oldLines": 1,
                "newStart": 1,
                "newLines": 1,
                "lines": ["-old\n", "+new\n"]
            }]
        });
        let file_changes = file_changes_value(1, 1, 1, vec![item.clone()]);
        assert_exact_keys(&file_changes, &["files", "additions", "deletions", "items"]);
        assert_exact_keys(
            &item,
            &[
                "path",
                "additions",
                "deletions",
                "writeCount",
                "toolNames",
                "patches",
            ],
        );
        let rewind = rewind_preview_value(
            true,
            vec![json!({
                "operationCount": 1,
                "path": "file.txt",
                "reason": "bash_ignored",
                "toolNames": ["bash"],
            })],
            vec![json!({
                "action": "restore",
                "operationCount": 1,
                "path": "file.txt",
                "toolNames": ["write_file"],
            })],
            Vec::new(),
        );
        assert_exact_keys(
            &rewind,
            &["canApply", "ignoredFiles", "safeFiles", "unsafeFiles"],
        );
        let background = background_output_value(
            "work-1",
            BackgroundBashOutput {
                status: "completed",
                output: "ok".to_owned(),
                truncated: false,
                output_path: "C:/work/stdout.log".to_owned(),
            },
        );
        assert_exact_keys(
            &background,
            &[
                "kind",
                "workId",
                "status",
                "output",
                "truncated",
                "outputPath",
            ],
        );
        let unavailable = background_unavailable_value("work-1");
        assert_exact_keys(&unavailable, &["kind", "workId", "code"]);
        let attachment = attachment_read_value(b"hi", "image/png", 2, None);
        assert_exact_keys(
            &attachment,
            &["dataBase64", "mediaType", "totalBytes", "nextOffset"],
        );
        let stat = attachment_stat_value("image/png", 2, Some(1.0));
        assert_exact_keys(&stat, &["mediaType", "totalBytes", "mtimeMs"]);
    }

    /// 由同一批生产构造器导出 JSON，供 packages/shared 的原始 strict schema 做跨语言验收。
    /// 未设置环境变量时不写工作区，避免普通单测产生未追踪文件。
    #[test]
    fn export_attachment_contract_fixtures_when_requested() {
        let Ok(directory) = std::env::var("KEENCODE_RPC_CONTRACT_FIXTURES") else {
            return;
        };
        let directory = Path::new(&directory);
        fs::create_dir_all(directory).expect("应创建附件契约 fixture 目录");
        let files = [
            (
                "conversation_file_changes.json",
                file_changes_value(0, 0, 0, Vec::new()),
            ),
            (
                "conversation_file_rewind_preview.json",
                rewind_preview_value(true, Vec::new(), Vec::new(), Vec::new()),
            ),
            (
                "background_bash_output.json",
                background_output_value(
                    "work-contract",
                    BackgroundBashOutput {
                        status: "completed",
                        output: "fixture".to_owned(),
                        truncated: false,
                        output_path: "C:/contract/stdout.log".to_owned(),
                    },
                ),
            ),
            (
                "background_bash_unavailable.json",
                background_unavailable_value("work-contract"),
            ),
            (
                "attachment_begin.json",
                json!({
                    "uploadId": "upload-contract",
                    "state": "staging",
                    "nextChunkIndex": 0,
                }),
            ),
            (
                "attachment_chunk.json",
                json!({
                    "uploadId": "upload-contract",
                    "nextChunkIndex": 1,
                }),
            ),
            (
                "attachment_commit.json",
                json!({"ref": "keencode-attachment://asset-contract"}),
            ),
            ("attachment_abort.json", json!({})),
            ("attachment_preview_source.json", json!({"kind": "chunked"})),
            (
                "attachment_read.json",
                attachment_read_value(b"fixture", "image/png", 7, None),
            ),
            (
                "conversation_attachment_read.json",
                attachment_read_value(b"fixture", "text/plain", 7, None),
            ),
            (
                "conversation_attachment_stat.json",
                attachment_stat_value("text/plain", 7, None),
            ),
        ];
        for (name, value) in files {
            let bytes = serde_json::to_vec_pretty(&value).expect("fixture 应为 JSON");
            fs::write(directory.join(name), bytes).expect("应写入附件契约 fixture");
        }
    }
}
