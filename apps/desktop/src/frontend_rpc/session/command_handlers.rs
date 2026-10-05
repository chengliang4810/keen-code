//! V4 Session 写命令的独立业务实现。
//!
//! 父网关负责连接初始化、commandId 去重和 CAS 参数提取；本模块负责 side
//! session 派生与 workspace-only 文件撤销，避免副作用细节混入会话路由。

use super::super::attachments;
use super::super::dispatch::{GatewayContext, RpcError};
use super::{SessionGateway, backend_error, invalid_params, open_authorized_session};
use crate::agent_runtime::AgentRuntime;
use crate::path_utils::path_to_frontend;
use crate::session_commands::{close_session_for_mutation, restore_session_after_mutation};
use crate::workspace;
use keencode_resources::{FileSnapshot, MessagePart, SessionState};
use keencode_runtime::RuntimeSession;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const REWIND_RECORD_SCHEMA: &str = "keencode/frontend-file-rewind";
const REWIND_RECORD_VERSION: u32 = 1;
const REWIND_RECORD_LIMIT: usize = 64 * 1024;

/// side session 继承父 Session 已确认的模型选择，firstInput 的显式选择再覆盖它。
pub(super) async fn create_selection_side_session(
    gateway: &SessionGateway,
    ctx: &GatewayContext,
    parent_session_id: &str,
    workspace_path: &str,
    command_id: &str,
    payload: &Value,
) -> Result<Value, RpcError> {
    super::reject_unknown_fields(payload, &["firstInput"], "createSelectionSideSession")?;
    let runtime = super::authorized_runtime_session(&ctx.app, parent_session_id, workspace_path)?;
    let parent =
        open_authorized_session(&runtime, &ctx.app, parent_session_id).map_err(backend_error)?;
    let parent_state = parent.snapshot().map_err(backend_error)?.state;
    let root = super::authorize_workspace_scope(&ctx.app, workspace_path)?;
    let child = runtime
        .open_or_create_session_serialized(&root, None, command_id)
        .await
        .map_err(|error| backend_error(error.to_string()))?;
    let child_id = child.session_id().as_str().to_owned();
    runtime
        .claim_deferred_session(&child_id)
        .map_err(|error| backend_error(error.to_string()))?;
    super::prepare_session_reference_catalog(&ctx.app, &runtime, &child_id, &root).await?;

    if let Some(provider) = parent_state.provider.as_ref() {
        runtime
            .set_session_model(
                &child_id,
                &format!("{command_id}:inherit-model"),
                &provider.provider_id,
                &provider.model,
            )
            .map_err(|error| backend_error(error.to_string()))?;
        if let Some(effort) = provider.reasoning_effort.as_ref() {
            let effort =
                serde_json::to_value(effort).map_err(|error| backend_error(error.to_string()))?;
            let effort = effort
                .as_str()
                .ok_or_else(|| backend_error("父 Session 的 reasoning effort 无效"))?;
            runtime
                .set_session_effort(&child_id, &format!("{command_id}:inherit-effort"), effort)
                .map_err(|error| backend_error(error.to_string()))?;
        }
    }

    let input = if let Some(first_input) = payload.get("firstInput") {
        super::reject_unknown_fields(
            first_input,
            &["text", "modelSelection"],
            "createSelectionSideSession.firstInput",
        )?;
        let text = super::required_string(first_input, "text")?;
        if let Some(selection) = first_input.get("modelSelection") {
            super::apply_requested_config(
                &runtime,
                &ctx.app,
                &child_id,
                &format!("{command_id}:explicit-model"),
                &json!({"modelSelection": selection}),
            )?;
        }
        Some(
            gateway
                .send_text(
                    ctx,
                    &child_id,
                    workspace_path,
                    command_id,
                    &json!({"text": text}),
                )
                .await?,
        )
    } else {
        None
    };
    let mut result = json!({"type": "createSelectionSideSession", "sessionId": child_id});
    if let Some(input) = input {
        result["input"] = input;
    }
    Ok(result)
}

/// 应用 workspace-only 文件撤销。预览沿用附件查询的权威实现；写入阶段使用
/// Session mutation gate 和独立事务墓碑，避免进程在半写状态下重放用户文件。
pub(super) async fn apply_file_rewind(
    ctx: &GatewayContext,
    session_id: &str,
    workspace_path: &str,
    command_id: &str,
    payload: &Value,
    base_revision: u64,
    base_log_epoch: &str,
) -> Result<Value, RpcError> {
    super::reject_unknown_fields(payload, &["target"], "applyFileRewind")?;
    let target = payload
        .get("target")
        .ok_or_else(|| invalid_params("applyFileRewind 缺少 target"))?;
    let authorized_root = super::authorize_workspace_scope(&ctx.app, workspace_path)?;
    let authorized_root_string = authorized_root.to_string_lossy().into_owned();
    let preview = attachments::call_v4(
        ctx,
        "conversationFileRewindPreviewV4",
        json!({
            // 附件查询网关把 workspace target 作为外层路由校验；内部复用也必须
            // 显式传入同一已授权根，不能因绕过 renderer 而省略归属校验。
            "workspacePath": workspace_path,
            "sessionId": session_id,
            "target": target,
            "baseRevision": base_revision,
            "baseLogEpoch": base_log_epoch,
        }),
    )
    .await?;

    let runtime = super::authorized_runtime_session(&ctx.app, session_id, workspace_path)?;
    let record_path = rewind_record_path(&runtime, session_id, command_id);
    if let Some(record) = read_rewind_record(&record_path)? {
        validate_record(
            &record,
            session_id,
            command_id,
            &authorized_root_string,
            base_revision,
            base_log_epoch,
        )?;
        // prepared 可能停在第一笔 workspace 写入前，也可能停在多文件写入中间；
        // 进程重启后无法仅凭墓碑判断已写入哪些文件，因此禁止自动重放。
        if matches!(
            record.phase,
            RewindPhase::Prepared | RewindPhase::Indeterminate
        ) {
            return Err(indeterminate_error());
        }
        if record.phase == RewindPhase::Completed {
            return Ok(json!({
                "type": "applyFileRewind",
                "applied": true,
                "preview": preview,
                "response": "文件撤销已完成",
            }));
        }
    }
    if preview.get("canApply").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({
            "type": "applyFileRewind",
            "applied": false,
            "preview": preview,
            "response": "当前文件无法安全撤销",
        }));
    }

    let session = open_authorized_session(&runtime, &ctx.app, session_id).map_err(backend_error)?;
    let snapshot = session.snapshot().map_err(backend_error)?;
    let target = parse_target(target)?;
    let turn_id = super::projection::validate_conversation_query_target(
        &snapshot.state,
        session_id,
        base_revision,
        base_log_epoch,
        target.row_id,
        &target.entity_id,
    )
    .map_err(map_query_error)?;
    let plans = build_rewind_plans(ctx, &session, &snapshot.state, &turn_id)?;
    if plans.is_empty() {
        return Err(backend_error("预览声明可撤销，但没有可用的文件快照"));
    }

    let context = close_session_for_mutation(&runtime, &ctx.app, session_id)
        .await
        .map_err(backend_error)?;
    let mut record = RewindRecord {
        schema: REWIND_RECORD_SCHEMA.to_owned(),
        version: REWIND_RECORD_VERSION,
        session_id: session_id.to_owned(),
        operation_id: command_id.to_owned(),
        project_root: authorized_root_string.clone(),
        base_revision,
        base_log_epoch: base_log_epoch.to_owned(),
        turn_id: turn_id.clone(),
        phase: RewindPhase::Prepared,
        paths: plans
            .iter()
            .map(|plan| path_to_frontend(&plan.path))
            .collect(),
    };
    if let Err(error) = write_rewind_record(&record_path, &record) {
        let _ = restore_session_after_mutation(&runtime, session_id, &context);
        return Err(error);
    }

    match apply_rewind_plans(&ctx.app, &authorized_root_string, &plans) {
        Ok(()) => {
            record.phase = RewindPhase::Completed;
            if let Err(error) = write_rewind_record(&record_path, &record) {
                record.phase = RewindPhase::Indeterminate;
                let _ = write_rewind_record(&record_path, &record);
                let _ = restore_session_after_mutation(&runtime, session_id, &context);
                return Err(error);
            }
            restore_session_after_mutation(&runtime, session_id, &context)
                .map_err(backend_error)?;
            Ok(json!({
                "type": "applyFileRewind",
                "applied": true,
                "preview": preview,
                "response": "文件撤销已完成",
            }))
        }
        Err(error) => {
            record.phase = RewindPhase::Indeterminate;
            let _ = write_rewind_record(&record_path, &record);
            let restore_error =
                restore_session_after_mutation(&runtime, session_id, &context).err();
            if let Some(restore_error) = restore_error {
                return Err(backend_error(format!(
                    "文件撤销结果不确定，且 Session 恢复失败：{restore_error}"
                )));
            }
            Err(RpcError::new(
                "fault.conversation.fileRewindIndeterminate",
                format!("文件撤销结果不确定：{error}"),
            ))
        }
    }
}

#[derive(Clone, Debug)]
struct TargetRow {
    row_id: u64,
    entity_id: String,
}

#[derive(Clone, Debug)]
struct RewindPlan {
    path: PathBuf,
    expected_hash: String,
    before: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum RewindPhase {
    Prepared,
    Completed,
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RewindRecord {
    schema: String,
    version: u32,
    session_id: String,
    operation_id: String,
    project_root: String,
    base_revision: u64,
    base_log_epoch: String,
    /// 事务目标的权威 Turn；用于冷恢复时把同一 turnHeader 标为 reverted。
    /// 这是 receipt 的必需字段；缺失记录按损坏/不确定处理，不能猜测归属。
    turn_id: String,
    phase: RewindPhase,
    paths: Vec<String>,
}

/// 读取已经完成的文件撤销事务归属。文件撤销记录是命令副作用的持久事实，
/// 这里只返回已完成且属于当前 Session 的 Turn，不把 pending/indeterminate 误投影为 reverted。
pub(super) fn completed_rewind_turn_ids(
    storage_root: &Path,
    session_id: &str,
) -> Result<BTreeSet<String>, String> {
    let directory = storage_root.join("file-rewinds");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(format!("读取文件撤销事务目录失败：{error}")),
    };
    let mut turn_ids = BTreeSet::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("读取文件撤销事务目录项失败：{error}"))?
            .path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(record) = read_rewind_record(&path).map_err(|error| error.to_string())? else {
            continue;
        };
        if record.schema != REWIND_RECORD_SCHEMA || record.version != REWIND_RECORD_VERSION {
            return Err("文件撤销事务记录版本不受支持".to_owned());
        }
        if record.session_id == session_id && record.phase == RewindPhase::Completed {
            turn_ids.insert(record.turn_id);
        }
    }
    Ok(turn_ids)
}

fn parse_target(value: &Value) -> Result<TargetRow, RpcError> {
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
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .ok_or_else(|| invalid_params("target.entityId 必须为非空字符串"))?;
    Ok(TargetRow {
        row_id,
        entity_id: entity_id.to_owned(),
    })
}

#[derive(Clone)]
struct ChangeEntry {
    path: String,
    before: Option<FileSnapshot>,
    after: FileSnapshot,
    transcript_order: Option<usize>,
    requested_at: u64,
    request_id: String,
}

fn build_rewind_plans(
    ctx: &GatewayContext,
    session: &RuntimeSession,
    state: &SessionState,
    turn_id: &str,
) -> Result<Vec<RewindPlan>, RpcError> {
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
    let mut grouped: HashMap<String, Vec<ChangeEntry>> = HashMap::new();
    for lifecycle in state.tools.values() {
        if lifecycle.request.turn_id.as_str() != turn_id {
            continue;
        }
        let Some(change) = lifecycle.file_change.as_ref() else {
            continue;
        };
        grouped
            .entry(change.path.clone())
            .or_default()
            .push(ChangeEntry {
                path: change.path.clone(),
                before: change.before.clone(),
                after: change.after.clone(),
                transcript_order: transcript_order
                    .get(lifecycle.request.model_tool_call_id.as_str())
                    .copied(),
                requested_at: lifecycle.requested_at_unix_ms,
                request_id: lifecycle.request.request_id.as_str().to_owned(),
            });
    }

    let root =
        fs::canonicalize(&state.project_root).map_err(|error| backend_error(error.to_string()))?;
    let mut plans = Vec::new();
    for (_, mut entries) in grouped {
        entries.sort_by(|left, right| {
            left.transcript_order
                .unwrap_or(usize::MAX)
                .cmp(&right.transcript_order.unwrap_or(usize::MAX))
                .then_with(|| left.requested_at.cmp(&right.requested_at))
                .then_with(|| left.request_id.cmp(&right.request_id))
        });
        let first = entries
            .first()
            .ok_or_else(|| backend_error("文件变更记录为空"))?;
        let last = entries
            .last()
            .ok_or_else(|| backend_error("文件变更记录为空"))?;
        let path = resolve_workspace_path(&ctx.app, &root, Path::new(&first.path))?;
        let current = read_current_file(&path)?
            .ok_or_else(|| backend_error("工作区文件在撤销前消失，拒绝继续"))?;
        if sha256_hex(&current) != last.after.sha256 {
            return Err(super::stale_revision("工作区文件在撤销前被外部修改"));
        }
        let before = first
            .before
            .as_ref()
            .map(|snapshot| session.read_file_snapshot(snapshot))
            .transpose()
            .map_err(|error| backend_error(error.to_string()))?;
        plans.push(RewindPlan {
            path,
            expected_hash: last.after.sha256.clone(),
            before,
        });
    }
    Ok(plans)
}

fn resolve_workspace_path(
    app: &tauri::AppHandle,
    root: &Path,
    path: &Path,
) -> Result<PathBuf, RpcError> {
    if !path.is_absolute() {
        return Err(invalid_params("文件变更路径必须为绝对路径"));
    }
    let candidate = if path.exists() {
        workspace::authorize_existing_absolute(app, path).map_err(backend_error)?
    } else {
        let parent = path
            .parent()
            .ok_or_else(|| backend_error("文件变更路径缺少父目录"))?;
        fs::canonicalize(parent)
            .map_err(|error| backend_error(error.to_string()))?
            .join(
                path.file_name()
                    .ok_or_else(|| backend_error("文件变更文件名为空"))?,
            )
    };
    if !candidate.starts_with(root) {
        return Err(RpcError::new(
            "fault.conversation.fileChangePath",
            "文件变更路径越过 Session 工作区",
        ));
    }
    Ok(candidate)
}

fn read_current_file(path: &Path) -> Result<Option<Vec<u8>>, RpcError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(backend_error(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(backend_error("撤销目标不是普通文件"));
    }
    fs::read(path)
        .map(Some)
        .map_err(|error| backend_error(error.to_string()))
}

fn apply_rewind_plans(
    app: &tauri::AppHandle,
    root: &str,
    plans: &[RewindPlan],
) -> Result<(), String> {
    let root = fs::canonicalize(root).map_err(|error| error.to_string())?;
    for plan in plans {
        let current = read_current_file(&plan.path)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "工作区文件在写入前消失".to_owned())?;
        if sha256_hex(&current) != plan.expected_hash {
            return Err("工作区文件在写入前被外部修改".to_owned());
        }
        let canonical = workspace::authorize_existing_absolute(app, &plan.path)
            .map_err(|error| error.to_string())?;
        if !canonical.starts_with(&root) {
            return Err("撤销目标越过工作区".to_owned());
        }
        if let Some(before) = plan.before.as_deref() {
            atomic_write_workspace_file(&canonical, before)?;
        } else {
            fs::remove_file(&canonical).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn atomic_write_workspace_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "撤销目标缺少父目录".to_owned())?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("file");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".{name}.keencode-rewind-{stamp}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary, metadata.permissions())
                .map_err(|error| error.to_string())?;
        }
        fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn rewind_record_path(runtime: &AgentRuntime, session_id: &str, operation_id: &str) -> PathBuf {
    let digest = Sha256::digest(format!("{session_id}\0{operation_id}").as_bytes());
    runtime
        .storage_root()
        .join("file-rewinds")
        .join(format!("{digest:x}.json"))
}

fn read_rewind_record(path: &Path) -> Result<Option<RewindRecord>, RpcError> {
    let Some(bytes) = crate::storage::read_private_bytes_bounded(
        path,
        REWIND_RECORD_LIMIT as u64,
        "文件撤销事务记录",
    )
    .map_err(|error| backend_error(error.to_string()))?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        RpcError::new(
            "fault.conversation.fileRewindIndeterminate",
            format!("文件撤销事务记录损坏：{error}"),
        )
    })
}

fn write_rewind_record(path: &Path, record: &RewindRecord) -> Result<(), RpcError> {
    let bytes = serde_json::to_vec(record).map_err(|error| backend_error(error.to_string()))?;
    if bytes.len() > REWIND_RECORD_LIMIT {
        return Err(backend_error("文件撤销事务记录超过大小限制"));
    }
    crate::storage::atomic_write_private(path, &bytes)
        .map_err(|error| backend_error(error.to_string()))
}

fn validate_record(
    record: &RewindRecord,
    session_id: &str,
    operation_id: &str,
    workspace_path: &str,
    base_revision: u64,
    base_log_epoch: &str,
) -> Result<(), RpcError> {
    if record.schema != REWIND_RECORD_SCHEMA
        || record.version != REWIND_RECORD_VERSION
        || record.session_id != session_id
        || record.operation_id != operation_id
        || record.project_root != workspace_path
        || record.base_revision != base_revision
        || record.base_log_epoch != base_log_epoch
    {
        return Err(indeterminate_error());
    }
    Ok(())
}

fn map_query_error(error: super::projection::ConversationQueryError) -> RpcError {
    match error {
        super::projection::ConversationQueryError::StaleRevision => {
            super::stale_revision("文件变更基于过期的 conversation revision")
        }
        super::projection::ConversationQueryError::StaleLogEpoch => {
            super::stale_revision("文件变更基于过期的 conversation log epoch")
        }
        super::projection::ConversationQueryError::TargetNotFound => {
            RpcError::new("fault.conversation.targetNotFound", "目标行不属于当前会话")
        }
    }
}

fn indeterminate_error() -> RpcError {
    RpcError::new(
        "fault.conversation.fileRewindIndeterminate",
        "文件撤销结果不确定，禁止自动重放，请人工核对工作区",
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        REWIND_RECORD_SCHEMA, REWIND_RECORD_VERSION, RewindPhase, RewindRecord,
        completed_rewind_turn_ids,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn rewind_record_round_trips() {
        let record = RewindRecord {
            schema: REWIND_RECORD_SCHEMA.to_owned(),
            version: REWIND_RECORD_VERSION,
            session_id: "session-1".to_owned(),
            operation_id: "command-1".to_owned(),
            project_root: "C:/project".to_owned(),
            base_revision: 3,
            base_log_epoch: "conversation-epoch".to_owned(),
            turn_id: "turn-1".to_owned(),
            phase: RewindPhase::Prepared,
            paths: vec!["C:/project/a.txt".to_owned()],
        };
        let value = serde_json::to_value(&record).expect("encode");
        let decoded: RewindRecord = serde_json::from_value(value).expect("decode");
        assert_eq!(decoded.schema, REWIND_RECORD_SCHEMA);
        assert_eq!(decoded.phase, RewindPhase::Prepared);
    }

    #[test]
    fn rewind_record_rejects_unknown_fields() {
        let value = serde_json::json!({
            "schema": REWIND_RECORD_SCHEMA,
            "version": REWIND_RECORD_VERSION,
            "sessionId": "session-1",
            "operationId": "command-1",
            "projectRoot": "C:/project",
            "baseRevision": 3,
            "baseLogEpoch": "conversation-epoch",
            "turnId": "turn-1",
            "phase": "prepared",
            "paths": [],
            "unexpected": true,
        });
        assert!(serde_json::from_value::<RewindRecord>(value).is_err());
    }

    #[test]
    fn completed_rewind_turn_ids_only_returns_completed_records() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "keencode-file-rewind-test-{}-{suffix}",
            std::process::id()
        ));
        let directory = root.join("file-rewinds");
        fs::create_dir_all(&directory).expect("create fixture directory");
        let completed = RewindRecord {
            schema: REWIND_RECORD_SCHEMA.to_owned(),
            version: REWIND_RECORD_VERSION,
            session_id: "session-1".to_owned(),
            operation_id: "completed".to_owned(),
            project_root: "C:/project".to_owned(),
            base_revision: 1,
            base_log_epoch: "epoch".to_owned(),
            turn_id: "turn-completed".to_owned(),
            phase: RewindPhase::Completed,
            paths: vec![],
        };
        let prepared = RewindRecord {
            turn_id: "turn-prepared".to_owned(),
            operation_id: "prepared".to_owned(),
            phase: RewindPhase::Prepared,
            ..completed.clone()
        };
        for (name, record) in [("completed.json", completed), ("prepared.json", prepared)] {
            let bytes = serde_json::to_vec(&record).expect("encode fixture");
            fs::write(directory.join(name), bytes).expect("write fixture");
        }

        let result = completed_rewind_turn_ids(&root, "session-1").expect("read fixture");
        assert_eq!(
            result.into_iter().collect::<Vec<_>>(),
            vec!["turn-completed".to_owned()]
        );
        fs::remove_dir_all(root).expect("remove fixture directory");
    }

    #[test]
    fn rewind_record_rejects_missing_required_turn_id() {
        let value = serde_json::json!({
            "schema": REWIND_RECORD_SCHEMA,
            "version": REWIND_RECORD_VERSION,
            "sessionId": "session-1",
            "operationId": "command-1",
            "projectRoot": "C:/project",
            "baseRevision": 3,
            "baseLogEpoch": "conversation-epoch",
            "phase": "completed",
            "paths": [],
        });
        assert!(serde_json::from_value::<RewindRecord>(value).is_err());
    }
}
