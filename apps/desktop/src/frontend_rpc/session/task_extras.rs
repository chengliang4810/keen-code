//! zcode-task 的本地任务扩展。
//!
//! 任务正文、生命周期和 pin/archive 状态只来自 Runtime/Journal。这里保存的
//! task-groups.json 只包含界面组织投影（分组、成员顺序、顶层顺序和未读标记），
//! 不复制 Session、Transcript 或模型内容。

use super::super::dispatch::{GatewayContext, RpcError};
use super::projection::{is_promoted_task_state, permission_mode_wire};
use crate::agent_runtime::AgentRuntime;
use crate::path_utils::path_to_frontend;
use crate::session_commands::{authorize_stored_root, open_authorized_session};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use keencode_resources::{
    MessageImageSource, MessagePart, MessageRole, SessionId, SessionMessage, SessionState,
    StoredSessionMetadata, ToolResultPart, TranscriptRecord, TranscriptSegment,
};
use keencode_runtime::RuntimeSession;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const GROUP_STORE_FILE: &str = "task-groups.json";
const GROUP_STORE_LOCK_FILE: &str = "task-groups.json.lock";
const GROUP_STORE_VERSION: u32 = 1;
const MAX_TASK_LIST_LIMIT: usize = 1_000;
const DEFAULT_TASK_LIST_LIMIT: usize = 200;
const GROUP_ORDER_STEP: i64 = 1_000;
const MAX_GROUP_TITLE_CHARS: usize = 256;
const MAX_MODEL_TRAJECTORY_LIMIT: usize = 1_000;
const MAX_GROUPED_TASK_REFS: usize = 10_000;

static GROUP_STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static NEXT_GROUP_ID: OnceLock<Mutex<u64>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceScope {
    workspace_path: String,
    #[serde(default)]
    workspace_identity: Option<String>,
    #[serde(default)]
    workspace_purpose: Option<String>,
}

#[derive(Clone, Debug)]
struct AuthorizedScope {
    path: String,
    key: String,
    identity: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskListQuery {
    kind: String,
    workspace_scopes: Vec<WorkspaceScope>,
    sort_by: String,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupedTaskRef {
    workspace_path: String,
    #[serde(default)]
    workspace_identity: Option<String>,
    task_id: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, tag = "type")]
enum TopLevelNodeRef {
    #[serde(rename = "group")]
    Group {
        // Source order DTO 固定使用 groupId；旧 snake_case 不能进入 wire 契约。
        #[serde(rename = "groupId")]
        group_id: String,
    },
    #[serde(rename = "task")]
    Task { task: GroupedTaskRef },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupOrderInput {
    // Source useGroupedTaskView.viewToOrderInput() 发出 camelCase，不能依赖调用方改名。
    #[serde(rename = "groupId")]
    group_id: String,
    task_refs: Vec<GroupedTaskRef>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupedViewOrderInput {
    workspace_scopes: Vec<WorkspaceScope>,
    top_level_nodes: Vec<TopLevelNodeRef>,
    groups: Vec<GroupOrderInput>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskGroupRecord {
    id: String,
    title: String,
    color: String,
    created_at: u64,
    updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupMemberRecord {
    group_id: String,
    workspace_key: String,
    workspace_path: String,
    #[serde(default)]
    workspace_identity: Option<String>,
    task_id: String,
    #[serde(default)]
    sort_order: Option<i64>,
    added_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, tag = "type")]
enum TopLevelOrderRecord {
    #[serde(rename = "group")]
    Group {
        #[serde(rename = "groupId")]
        group_id: String,
        #[serde(rename = "sortOrder")]
        sort_order: i64,
    },
    #[serde(rename = "task")]
    Task {
        #[serde(rename = "workspaceKey")]
        workspace_key: String,
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "sortOrder")]
        sort_order: i64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnreadRecord {
    workspace_key: String,
    task_id: String,
    unread_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupStoreFile {
    version: u32,
    #[serde(default)]
    groups: Vec<TaskGroupRecord>,
    #[serde(default)]
    members: Vec<GroupMemberRecord>,
    #[serde(default)]
    top_level_orders: Vec<TopLevelOrderRecord>,
    #[serde(default)]
    unread: Vec<UnreadRecord>,
}

impl Default for GroupStoreFile {
    fn default() -> Self {
        Self {
            version: GROUP_STORE_VERSION,
            groups: Vec::new(),
            members: Vec::new(),
            top_level_orders: Vec::new(),
            unread: Vec::new(),
        }
    }
}

impl GroupStoreFile {
    fn unread_at(&self, workspace_key: &str, task_id: &str) -> Option<u64> {
        self.unread
            .iter()
            .find(|item| item.workspace_key == workspace_key && item.task_id == task_id)
            .map(|item| item.unread_at)
    }

    fn set_unread(&mut self, workspace_key: &str, task_id: &str, unread_at: Option<u64>) {
        self.unread
            .retain(|item| item.workspace_key != workspace_key || item.task_id != task_id);
        if let Some(unread_at) = unread_at {
            self.unread.push(UnreadRecord {
                workspace_key: workspace_key.to_owned(),
                task_id: task_id.to_owned(),
                unread_at,
            });
        }
        self.unread.sort_by(|left, right| {
            left.workspace_key
                .cmp(&right.workspace_key)
                .then_with(|| left.task_id.cmp(&right.task_id))
        });
    }
}

/// 返回 Some 表示方法已由本模块处理；None 让 session gateway 继续匹配旧方法。
pub(crate) async fn call(
    ctx: &GatewayContext,
    method: &str,
    args: &Value,
) -> Result<Option<Value>, RpcError> {
    let result = match method {
        "listTaskList" => Some(list_task_list(ctx, args)?),
        "createTaskGroup" => Some(create_task_group(ctx, args)?),
        "renameTaskGroup" => Some(rename_task_group(ctx, args)?),
        "updateTaskGroupColor" => Some(update_task_group_color(ctx, args)?),
        "deleteTaskGroup" => Some(delete_task_group(ctx, args)?),
        "listGroupedTaskViewStructure" => Some(list_grouped_structure(ctx, args)?),
        "applyGroupedTaskViewOrder" => Some(apply_grouped_order(ctx, args).await?),
        "getModelTrajectory" => Some(get_model_trajectory(ctx, args)?),
        "getTaskNativeSessionLogFile" => Some(get_native_session_log_file(ctx, args)?),
        "getTaskSessionFilePath" => Some(get_task_session_file_path(ctx, args)?),
        "deleteTask" => Some(delete_task(ctx, args)?),
        "deleteArchivedTask" => Some(delete_archived_task(ctx, args).await?),
        "deleteArchivedTasks" => Some(delete_archived_tasks(ctx, args).await?),
        "setTaskPinned" => Some(set_task_pinned(ctx, args)?),
        "setTaskUnread" => Some(set_task_unread(ctx, args)?),
        "archiveTask" => Some(set_task_archived(ctx, args, true)?),
        "archiveTaskWithReceipt" => Some(archive_task_with_receipt(ctx, args)?),
        "unarchiveTask" => Some(set_task_archived(ctx, args, false)?),
        _ => None,
    };
    Ok(result)
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new("fault.invalidParams", message)
}

fn backend_error(error: impl Display) -> RpcError {
    RpcError::new("fault.backend", error.to_string())
}

fn not_found(message: impl Into<String>) -> RpcError {
    RpcError::new("task.notFound", message)
}

fn required_string(args: &Value, name: &str) -> Result<String, RpcError> {
    args.get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid_params(format!("{name} 必须为非空字符串")))
}

fn optional_string(args: &Value, name: &str) -> Result<Option<String>, RpcError> {
    let Some(value) = args.get(name) else {
        return Ok(None);
    };
    value
        .as_str()
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| invalid_params(format!("{name} 必须为非空字符串")))
}

fn required_bool(args: &Value, name: &str) -> Result<bool, RpcError> {
    args.get(name)
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid_params(format!("{name} 必须为布尔值")))
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn owned_runtime(app: &tauri::AppHandle) -> Result<Arc<AgentRuntime>, RpcError> {
    crate::require_owned_runtime(app).map_err(backend_error)
}

fn group_store_lock() -> Result<std::sync::MutexGuard<'static, ()>, RpcError> {
    GROUP_STORE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| RpcError::new("fault.state", "任务分组存储锁已损坏"))
}

fn group_store_path(app: &tauri::AppHandle) -> Result<PathBuf, RpcError> {
    Ok(crate::storage::root_dir(app)
        .map_err(backend_error)?
        .join(GROUP_STORE_FILE))
}

/// 在进程内 Mutex 之外锁住同一份磁盘事实源，覆盖冷重启时旧宿主尚未退出的窗口。
///
/// Tauri 的重启可能让两个宿主短暂共享 data 根目录；进程内 `OnceLock<Mutex>` 无法
/// 互斥这两个进程，后启动的旧快照写入会把刚删除的分组恢复。`fs2` 文件锁由操作系统
/// 在宿主崩溃时自动释放，锁文件本身只是一份私有协调元数据，不进入 wire schema。
fn group_store_process_lock(app: &tauri::AppHandle) -> Result<File, RpcError> {
    let path = group_store_path(app)?.with_file_name(GROUP_STORE_LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // 锁文件只负责跨进程协调，必须保留既有 inode，不能因打开而截断并发宿主的锁文件。
        .truncate(false)
        .open(&path)
        .map_err(|error| backend_error(format!("打开任务分组跨进程锁失败：{error}")))?;
    file.lock_exclusive()
        .map_err(|error| backend_error(format!("获取任务分组跨进程锁失败：{error}")))?;
    Ok(file)
}

fn read_group_store(app: &tauri::AppHandle) -> Result<GroupStoreFile, RpcError> {
    let path = group_store_path(app)?;
    let Some(bytes) =
        crate::storage::read_private_bytes_bounded(&path, 16 * 1024 * 1024, "任务分组存储")
            .map_err(backend_error)?
    else {
        return Ok(GroupStoreFile::default());
    };
    let store: GroupStoreFile = serde_json::from_slice(&bytes)
        .map_err(|error| backend_error(format!("任务分组存储无效：{error}")))?;
    if store.version != GROUP_STORE_VERSION {
        return Err(backend_error(format!(
            "任务分组存储版本 {} 不受支持",
            store.version
        )));
    }
    Ok(store)
}

fn write_group_store(app: &tauri::AppHandle, store: &GroupStoreFile) -> Result<(), RpcError> {
    let path = group_store_path(app)?;
    let bytes = serde_json::to_vec_pretty(store).map_err(backend_error)?;
    crate::storage::atomic_write_private(&path, &bytes).map_err(backend_error)
}

fn with_group_store<T>(
    app: &tauri::AppHandle,
    update: impl FnOnce(&mut GroupStoreFile) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    let _guard = group_store_lock()?;
    let _process_lock = group_store_process_lock(app)?;
    let mut store = read_group_store(app)?;
    let result = update(&mut store)?;
    write_group_store(app, &store)?;
    Ok(result)
}

fn parse_group_color(value: Option<&Value>) -> Result<String, RpcError> {
    let color = value.and_then(Value::as_str).unwrap_or("gray").to_owned();
    if matches!(
        color.as_str(),
        "gray" | "red" | "orange" | "yellow" | "green" | "blue" | "purple"
    ) {
        Ok(color)
    } else {
        Err(invalid_params(
            "color 必须是 gray/red/orange/yellow/green/blue/purple",
        ))
    }
}

fn normalize_group_title(value: Option<&Value>) -> Result<String, RpcError> {
    let title = value
        .and_then(Value::as_str)
        .unwrap_or("新建分组")
        .trim()
        .to_owned();
    if title.is_empty() {
        return Err(invalid_params("分组标题不能为空"));
    }
    if title.chars().count() > MAX_GROUP_TITLE_CHARS {
        return Err(invalid_params("分组标题过长"));
    }
    Ok(title)
}

fn next_group_id() -> String {
    let counter = NEXT_GROUP_ID.get_or_init(|| Mutex::new(0));
    let mut counter = counter.lock().expect("任务分组 ID 锁不应中毒");
    *counter = counter.saturating_add(1);
    format!("group-{:x}-{:x}", now_epoch_ms(), *counter)
}

fn parse_scope_values(
    app: &tauri::AppHandle,
    raw: &[WorkspaceScope],
) -> Result<Vec<AuthorizedScope>, RpcError> {
    let mut scopes = Vec::with_capacity(raw.len());
    let mut seen = HashSet::new();
    for scope in raw {
        if scope.workspace_path.trim().is_empty()
            || scope.workspace_path.trim() != scope.workspace_path
        {
            return Err(invalid_params("workspacePath 必须为非空规范字符串"));
        }
        if let Some(identity) = &scope.workspace_identity
            && (identity.trim().is_empty() || identity.trim() != identity)
        {
            return Err(invalid_params("workspaceIdentity 必须为非空规范字符串"));
        }
        if let Some(purpose) = &scope.workspace_purpose
            && !matches!(purpose.as_str(), "project" | "conversation")
        {
            return Err(invalid_params("workspacePurpose 无效"));
        }
        let path = authorize_stored_root(app, &scope.workspace_path)
            .map_err(backend_error)?
            .to_string_lossy()
            .into_owned();
        let key = scope
            .workspace_identity
            .clone()
            .unwrap_or_else(|| path.clone());
        if seen.insert(key.clone()) {
            scopes.push(AuthorizedScope {
                path,
                key,
                identity: scope.workspace_identity.clone(),
            });
        }
    }
    Ok(scopes)
}

fn parse_scopes(
    app: &tauri::AppHandle,
    args: &Value,
    field: &str,
) -> Result<Vec<AuthorizedScope>, RpcError> {
    let raw = args
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_params(format!("{field} 必须为数组")))?;
    let scopes = raw
        .iter()
        .map(|value| {
            serde_json::from_value::<WorkspaceScope>(value.clone())
                .map_err(|error| invalid_params(format!("{field} 参数无效：{error}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    parse_scope_values(app, &scopes)
}

fn task_workspace_key(path: &str, identity: Option<&str>) -> String {
    identity.unwrap_or(path).to_owned()
}

/// 将本地路径型 workspace key 转成 Source 使用的 wire 表示；远程 identity 本身
/// 不是路径，通常只含协议标识，path_to_frontend 对其保持原文本。
fn workspace_key_for_wire(key: &str) -> String {
    path_to_frontend(Path::new(key))
}

fn scope_matches(scopes: &[AuthorizedScope], path: &str, identity: Option<&str>) -> bool {
    let key = task_workspace_key(path, identity);
    scopes.iter().any(|scope| {
        scope.key == key || (identity.is_none() && scope.identity.is_none() && scope.path == path)
    })
}

fn task_id(args: &Value) -> Result<String, RpcError> {
    required_string(args, "taskId").or_else(|_| required_string(args, "sessionId"))
}

fn operation_id(
    args: &Value,
    method: &str,
    task_id: &str,
    value: impl Display,
    sequence: u64,
) -> Result<String, RpcError> {
    if let Some(value) = optional_string(args, "operationId")? {
        return Ok(value);
    }
    Ok(format!("task-extras:{method}:{task_id}:{value}:{sequence}"))
}

fn task_meta_workspace_key(meta: &Value) -> Option<String> {
    let path = meta.get("workspacePath").and_then(Value::as_str)?;
    let identity = meta.get("workspaceIdentity").and_then(Value::as_str);
    Some(task_workspace_key(path, identity))
}

struct AuthorizedTask {
    runtime: Arc<AgentRuntime>,
    session: RuntimeSession,
    metadata: StoredSessionMetadata,
    workspace_path: String,
    workspace_identity: Option<String>,
}

fn authorized_task(ctx: &GatewayContext, args: &Value) -> Result<AuthorizedTask, RpcError> {
    let id = task_id(args)?;
    let requested_path = required_string(args, "workspacePath")?;
    let root = authorize_stored_root(&ctx.app, &requested_path).map_err(backend_error)?;
    let runtime = owned_runtime(&ctx.app)?;
    let session = open_authorized_session(&runtime, &ctx.app, &id).map_err(backend_error)?;
    if session.is_workflow_actor().map_err(backend_error)? {
        return Err(not_found("工作流 actor 不属于用户任务"));
    }
    let state = session.snapshot().map_err(backend_error)?.state;
    let root_text = root.to_string_lossy().into_owned();
    if state.project_root != root_text {
        return Err(RpcError::new(
            "task.workspaceMismatch",
            "任务不属于请求的 workspace",
        ));
    }
    let metadata = runtime
        .runtime_manager()
        .stored_session_metadata(&id)
        .map_err(backend_error)?;
    Ok(AuthorizedTask {
        runtime,
        session,
        metadata,
        workspace_path: root_text,
        workspace_identity: optional_string(args, "workspaceIdentity")?,
    })
}

fn task_status_wire(
    active: bool,
    session_status: &keencode_resources::SessionStatus,
    latest_turn_status: Option<&keencode_resources::TurnStatus>,
) -> &'static str {
    if active
        || matches!(
            session_status,
            keencode_resources::SessionStatus::Running | keencode_resources::SessionStatus::Waiting
        )
    {
        // ZCodeTaskPersistStatus 没有 waiting；等待用户输入时，最近一次
        // prompt 仍未完成，必须按 running 传输，避免输出未知枚举值。
        "running"
    } else if matches!(
        latest_turn_status,
        Some(keencode_resources::TurnStatus::Failed)
    ) {
        "error"
    } else {
        "completed"
    }
}

/// 生成旧 task service 以及 mutation ACK 共用的完整 task meta。
///
/// 该 DTO 仍从 Runtime snapshot 和 task-groups 未读索引读取，不能在 legacy
/// gateway 内临时拼出标题、pin 或状态，避免 rename/setTaskUnread 返回值与列表投影分叉。
pub(crate) fn task_meta_value(
    runtime: &AgentRuntime,
    metadata: &StoredSessionMetadata,
    state: &SessionState,
    workspace_identity: Option<&str>,
    unread_at: Option<u64>,
) -> Result<Value, RpcError> {
    let active = runtime
        .session_has_active_work(metadata.session_id.as_str())
        .map_err(backend_error)?;
    let mode = if state.plan.enabled {
        "plan".to_owned()
    } else {
        runtime
            .permission_mode(metadata.session_id.as_str())
            .ok()
            .map(|value| permission_mode_wire(value).to_owned())
            .unwrap_or_else(|| "build".to_owned())
    };
    let latest_turn = state
        .turns
        .values()
        .max_by_key(|turn| turn.started_at_unix_ms);
    let status = task_status_wire(active, &state.status, latest_turn.map(|turn| &turn.status));
    // task-groups 与 Journal 只保存 canonical 路径；列表返回值要遵循 Source 的
    // 普通盘符/UNC 斜杠格式，避免 UI 按 workspacePath 丢弃任务行。
    let workspace_path = path_to_frontend(Path::new(&state.project_root));
    let mut task = json!({
        "taskId": metadata.session_id.as_str(),
        "traceId": format!("session:{}", metadata.session_id.as_str()),
        "title": state.title,
        "workspacePath": workspace_path,
        "createdAt": state.created_at_unix_ms,
        "updatedAt": state.updated_at_unix_ms,
        "mode": mode,
        "provider": "glm",
        "pinned": state.pinned,
        "archived": state.archived,
        "status": status,
    });
    if let Some(identity) = workspace_identity.filter(|value| !value.is_empty()) {
        task["workspaceIdentity"] = Value::String(identity.to_owned());
    }
    if state.title_source == keencode_resources::TitleSource::Manual {
        task["titleOverridden"] = Value::Bool(true);
    }
    if let Some(provider) = &state.provider {
        task["model"] = Value::String(provider.model.clone());
        if let Some(effort) = provider.reasoning_effort
            && let Ok(value) = serde_json::to_value(effort)
            && let Some(value) = value.as_str()
        {
            task["thoughtLevel"] = Value::String(value.to_owned());
        }
    }
    if let Some(unread_at) = unread_at {
        task["unreadAt"] = Value::Number(unread_at.into());
    }
    Ok(task)
}

fn collect_task_items(
    ctx: &GatewayContext,
    scopes: &[AuthorizedScope],
    kind: &str,
    sort_by: &str,
    search: Option<&str>,
) -> Result<Vec<Value>, RpcError> {
    if !matches!(kind, "pinned" | "archived" | "timeline" | "active") {
        return Err(invalid_params(
            "kind 必须为 pinned/archived/timeline/active",
        ));
    }
    if !matches!(sort_by, "createdAt" | "updatedAt") {
        return Err(invalid_params("sortBy 必须为 created 或 updated"));
    }
    if scopes.is_empty() {
        return Ok(Vec::new());
    }
    let runtime = owned_runtime(&ctx.app)?;
    let store = read_group_store(&ctx.app)?;
    let metadata = runtime.stored_sessions().map_err(backend_error)?;
    let mut deleted_by_root = HashMap::<String, HashSet<String>>::new();
    let mut rows = Vec::new();
    for item in metadata {
        if item.corrupt {
            continue;
        }
        let root = match authorize_stored_root(&ctx.app, &item.project_root) {
            Ok(root) => root,
            Err(_) => continue,
        };
        let root_text = root.to_string_lossy().into_owned();
        if !deleted_by_root.contains_key(&root_text) {
            let deleted = deleted_task_ids_for_workspace(&runtime, &root_text)?;
            deleted_by_root.insert(root_text.clone(), deleted);
        }
        if deleted_by_root
            .get(&root_text)
            .is_some_and(|deleted| deleted.contains(item.session_id.as_str()))
        {
            continue;
        }
        let Some(scope) = scopes.iter().find(|scope| {
            scope.path == root_text || (scope.identity.is_none() && scope.key == root_text)
        }) else {
            continue;
        };
        let matches_kind = match kind {
            "pinned" => item.pinned && !item.archived,
            "archived" => item.archived,
            "timeline" => !item.pinned && !item.archived,
            "active" => !item.archived,
            _ => false,
        };
        if !matches_kind {
            continue;
        }
        let session = open_authorized_session(&runtime, &ctx.app, item.session_id.as_str())
            .map_err(backend_error)?;
        if session.is_workflow_actor().map_err(backend_error)? {
            continue;
        }
        let state = session.snapshot().map_err(backend_error)?.state;
        // tasks-index 只能列出已经产生 Journal promotion 事实的 Session；workspace
        // 预热留下的空 draft 不属于侧栏任务，避免与真实任务争夺菜单/排序位置。
        if !is_promoted_task_state(&state) {
            continue;
        }
        let workspace_identity = scope.identity.as_deref();
        let workspace_key = task_workspace_key(&root_text, workspace_identity);
        let unread_at = store.unread_at(&workspace_key, item.session_id.as_str());
        let task = task_meta_value(&runtime, &item, &state, workspace_identity, unread_at)?;
        if let Some(search) = search
            && !task
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase()
                .contains(search)
            && !item.session_id.as_str().to_lowercase().contains(search)
        {
            continue;
        }
        rows.push(task);
    }
    rows.sort_by(|left, right| {
        let field = if sort_by == "createdAt" {
            "createdAt"
        } else {
            "updatedAt"
        };
        right
            .get(field)
            .and_then(Value::as_u64)
            .cmp(&left.get(field).and_then(Value::as_u64))
            .then_with(|| {
                left.get("taskId")
                    .and_then(Value::as_str)
                    .cmp(&right.get("taskId").and_then(Value::as_str))
            })
    });
    Ok(rows)
}

fn list_task_list(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let query: TaskListQuery = serde_json::from_value(args.clone())
        .map_err(|error| invalid_params(format!("listTaskList 参数无效：{error}")))?;
    let kind = query.kind.as_str();
    let sort_by = match query.sort_by.as_str() {
        "created" => "createdAt",
        "updated" => "updatedAt",
        _ => return Err(invalid_params("sortBy 必须为 created 或 updated")),
    };
    if query.limit == Some(0) {
        return Err(invalid_params("limit 必须为正整数"));
    }
    let limit = query
        .limit
        .unwrap_or(DEFAULT_TASK_LIST_LIMIT)
        .min(MAX_TASK_LIST_LIMIT);
    let search = query
        .search
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_lowercase());
    let scopes = parse_scope_values(&ctx.app, &query.workspace_scopes)?;
    let rows = collect_task_items(ctx, &scopes, kind, sort_by, search.as_deref())?;
    let total = rows.len();
    let has_more = total > limit;
    Ok(json!({
        "items": rows.into_iter().take(limit).collect::<Vec<_>>(),
        "total": total,
        "hasMore": has_more,
    }))
}

fn group_to_value(group: &TaskGroupRecord) -> Value {
    json!({
        "id": group.id,
        "title": group.title,
        "color": group.color,
        "createdAt": group.created_at,
        "updatedAt": group.updated_at,
    })
}

fn create_task_group(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let title = normalize_group_title(args.get("title"));
    let color = parse_group_color(args.get("color"));
    let title = title?;
    let color = color?;
    let now = now_epoch_ms();
    let group = TaskGroupRecord {
        id: next_group_id(),
        title,
        color,
        created_at: now,
        updated_at: now,
    };
    let value = group_to_value(&group);
    with_group_store(&ctx.app, |store| {
        let min_order = store
            .top_level_orders
            .iter()
            .map(|order| match order {
                TopLevelOrderRecord::Group { sort_order, .. }
                | TopLevelOrderRecord::Task { sort_order, .. } => *sort_order,
            })
            .min()
            .unwrap_or(0);
        store.groups.push(group);
        store.top_level_orders.push(TopLevelOrderRecord::Group {
            group_id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            sort_order: min_order.saturating_sub(GROUP_ORDER_STEP),
        });
        Ok(())
    })?;
    Ok(value)
}

fn rename_task_group(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let group_id = required_string(args, "groupId")?;
    let title = normalize_group_title(args.get("title"))?;
    with_group_store(&ctx.app, |store| {
        let group = store
            .groups
            .iter_mut()
            .find(|group| group.id == group_id)
            .ok_or_else(|| not_found("任务分组不存在"))?;
        group.title = title;
        group.updated_at = now_epoch_ms();
        Ok(group_to_value(group))
    })
}

fn update_task_group_color(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let group_id = required_string(args, "groupId")?;
    let color = parse_group_color(args.get("color"))?;
    with_group_store(&ctx.app, |store| {
        let group = store
            .groups
            .iter_mut()
            .find(|group| group.id == group_id)
            .ok_or_else(|| not_found("任务分组不存在"))?;
        group.color = color;
        group.updated_at = now_epoch_ms();
        Ok(group_to_value(group))
    })
}

fn delete_task_group(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let group_id = required_string(args, "groupId")?;
    with_group_store(&ctx.app, |store| {
        remove_task_group_records(store, &group_id)?;
        Ok(())
    })?;
    Ok(Value::Null)
}

fn remove_task_group_records(store: &mut GroupStoreFile, group_id: &str) -> Result<(), RpcError> {
    let original = store.groups.len();
    store.groups.retain(|group| group.id != group_id);
    if store.groups.len() == original {
        return Err(not_found("任务分组不存在"));
    }
    store.members.retain(|member| member.group_id != group_id);
    store.top_level_orders.retain(|order| {
        !matches!(
            order,
            TopLevelOrderRecord::Group {
                group_id: id,
                ..
            } if id == group_id
        )
    });
    Ok(())
}

struct NormalizedTaskRef {
    workspace_key: String,
    workspace_path: String,
    workspace_identity: Option<String>,
    task_id: String,
}

fn normalize_task_ref(
    ctx: &GatewayContext,
    scopes: &[AuthorizedScope],
    reference: &GroupedTaskRef,
    metadata: &[StoredSessionMetadata],
    runtime: &AgentRuntime,
) -> Result<NormalizedTaskRef, RpcError> {
    if reference.task_id.is_empty() || reference.task_id.trim() != reference.task_id {
        return Err(invalid_params("taskId 必须为非空规范字符串"));
    }
    let root = authorize_stored_root(&ctx.app, &reference.workspace_path).map_err(backend_error)?;
    let workspace_path = root.to_string_lossy().into_owned();
    let workspace_key =
        task_workspace_key(&workspace_path, reference.workspace_identity.as_deref());
    if !scope_matches(
        scopes,
        &workspace_path,
        reference.workspace_identity.as_deref(),
    ) {
        return Err(RpcError::new(
            "task.workspaceScopeMismatch",
            "分组任务引用不属于当前 workspace scope",
        ));
    }
    if deleted_task_ids_for_workspace(runtime, &workspace_path)?.contains(&reference.task_id) {
        return Err(not_found("任务已删除"));
    }
    let item = metadata
        .iter()
        .find(|item| {
            item.session_id.as_str() == reference.task_id
                && !item.corrupt
                && authorize_stored_root(&ctx.app, &item.project_root)
                    .ok()
                    .is_some_and(|path| path.to_string_lossy() == workspace_path)
        })
        .ok_or_else(|| not_found("任务不存在"))?;
    if item.archived || item.pinned {
        return Err(RpcError::new(
            "task.groupMembershipInvalid",
            "已置顶或已归档任务不能加入 grouped 视图",
        ));
    }
    let session = open_authorized_session(runtime, &ctx.app, item.session_id.as_str())
        .map_err(backend_error)?;
    if session.is_workflow_actor().map_err(backend_error)? {
        return Err(not_found("工作流 actor 不属于用户任务"));
    }
    Ok(NormalizedTaskRef {
        workspace_key,
        workspace_path,
        workspace_identity: reference.workspace_identity.clone(),
        task_id: reference.task_id.clone(),
    })
}

fn list_grouped_structure(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let scopes = parse_scopes(&ctx.app, args, "workspaceScopes")?;
    if scopes.is_empty() {
        return Ok(json!({
            "groups": [],
            "members": [],
            "topLevelOrders": [],
        }));
    }
    let store = read_group_store(&ctx.app)?;
    let scope_keys = scopes
        .iter()
        .map(|scope| scope.key.as_str())
        .collect::<HashSet<_>>();
    let visible_group_ids = store
        .groups
        .iter()
        .filter(|group| {
            let has_members = store
                .members
                .iter()
                .any(|member| member.group_id == group.id);
            !has_members
                || store.members.iter().any(|member| {
                    member.group_id == group.id
                        && scope_keys.contains(member.workspace_key.as_str())
                })
        })
        .map(|group| group.id.clone())
        .collect::<HashSet<_>>();
    let groups = store
        .groups
        .iter()
        .filter(|group| visible_group_ids.contains(&group.id))
        .map(group_to_value)
        .collect::<Vec<_>>();
    let members = store
        .members
        .iter()
        .filter(|member| member_belongs_to_scopes(member, &scope_keys))
        .map(|member| {
            json!({
                "groupId": member.group_id,
                "workspaceKey": workspace_key_for_wire(&member.workspace_key),
                "workspacePath": path_to_frontend(Path::new(&member.workspace_path)),
                "workspaceIdentity": member.workspace_identity,
                "taskId": member.task_id,
                "sortOrder": member.sort_order,
                "addedAt": member.added_at,
            })
        })
        .collect::<Vec<_>>();
    let top_level_orders = store
        .top_level_orders
        .iter()
        .filter(|order| match order {
            TopLevelOrderRecord::Group { group_id, .. } => visible_group_ids.contains(group_id),
            TopLevelOrderRecord::Task { workspace_key, .. } => {
                scope_keys.contains(workspace_key.as_str())
            }
        })
        .map(|order| -> Result<Value, RpcError> {
            let mut value = serde_json::to_value(order).map_err(backend_error)?;
            if let TopLevelOrderRecord::Task { workspace_key, .. } = order {
                value["workspaceKey"] = Value::String(workspace_key_for_wire(workspace_key));
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!({
        "groups": groups,
        "members": members,
        "topLevelOrders": top_level_orders,
    }))
}

fn member_belongs_to_scopes(member: &GroupMemberRecord, scope_keys: &HashSet<&str>) -> bool {
    scope_keys.contains(member.workspace_key.as_str())
}

fn order_value_for_group(store: &GroupStoreFile, group_id: &str) -> Option<i64> {
    store.top_level_orders.iter().find_map(|order| match order {
        TopLevelOrderRecord::Group {
            group_id: value,
            sort_order,
        } if value == group_id => Some(*sort_order),
        _ => None,
    })
}

fn order_value_for_task(store: &GroupStoreFile, workspace_key: &str, task_id: &str) -> Option<i64> {
    store.top_level_orders.iter().find_map(|order| match order {
        TopLevelOrderRecord::Task {
            workspace_key: value_workspace,
            task_id: value_task,
            sort_order,
        } if workspace_key_for_wire(value_workspace) == workspace_key && value_task == task_id => {
            Some(*sort_order)
        }
        _ => None,
    })
}

fn task_ref_key(reference: &NormalizedTaskRef) -> String {
    format!("{}\u{0}{}", reference.workspace_key, reference.task_id)
}

/// 分组成员持久化使用 canonical workspace key，而 task row 使用 Source wire key。
/// 冷恢复时两者必须经同一转换后再 join，不能把路径格式差异当成成员缺失。
fn grouped_task_join_key(workspace_key: &str, task_id: &str) -> String {
    format!("{}\u{0}{}", workspace_key_for_wire(workspace_key), task_id)
}

fn apply_grouped_order_sync(
    ctx: &GatewayContext,
    input: &GroupedViewOrderInput,
    scopes: &[AuthorizedScope],
) -> Result<(), RpcError> {
    if input.top_level_nodes.len() > MAX_GROUPED_TASK_REFS
        || input.groups.len() > MAX_GROUPED_TASK_REFS
        || input
            .groups
            .iter()
            .map(|group| group.task_refs.len())
            .sum::<usize>()
            > MAX_GROUPED_TASK_REFS
    {
        return Err(invalid_params("grouped 任务引用超出上限"));
    }
    let runtime = owned_runtime(&ctx.app)?;
    let metadata = runtime.stored_sessions().map_err(backend_error)?;
    let store_before = read_group_store(&ctx.app)?;
    let group_ids = store_before
        .groups
        .iter()
        .map(|group| group.id.clone())
        .collect::<HashSet<_>>();
    let mut group_inputs = HashSet::new();
    let mut group_task_refs: HashMap<String, Vec<NormalizedTaskRef>> = HashMap::new();
    let mut all_member_keys = HashSet::new();
    for group in &input.groups {
        if !group_ids.contains(&group.group_id) {
            return Err(not_found("任务分组不存在"));
        }
        if !group_inputs.insert(group.group_id.clone()) {
            return Err(invalid_params("groups 不能重复包含同一分组"));
        }
        let mut refs = Vec::with_capacity(group.task_refs.len());
        for reference in &group.task_refs {
            let normalized = normalize_task_ref(ctx, scopes, reference, &metadata, &runtime)?;
            let key = task_ref_key(&normalized);
            if !all_member_keys.insert(key) {
                return Err(invalid_params("同一任务不能加入多个分组"));
            }
            refs.push(normalized);
        }
        group_task_refs.insert(group.group_id.clone(), refs);
    }

    let mut top_nodes = Vec::with_capacity(input.top_level_nodes.len());
    let mut top_keys = HashSet::new();
    for node in &input.top_level_nodes {
        match node {
            TopLevelNodeRef::Group { group_id } => {
                if !group_ids.contains(group_id) {
                    return Err(not_found("顶层节点分组不存在"));
                }
                let key = format!("group:{group_id}");
                if !top_keys.insert(key) {
                    return Err(invalid_params("topLevelNodes 不能重复包含节点"));
                }
                top_nodes.push(TopLevelOrderRecord::Group {
                    group_id: group_id.clone(),
                    sort_order: 0,
                });
            }
            TopLevelNodeRef::Task { task } => {
                let normalized = normalize_task_ref(ctx, scopes, task, &metadata, &runtime)?;
                let key = format!("task:{}", task_ref_key(&normalized));
                if !top_keys.insert(key) {
                    return Err(invalid_params("topLevelNodes 不能重复包含节点"));
                }
                top_nodes.push(TopLevelOrderRecord::Task {
                    workspace_key: normalized.workspace_key,
                    task_id: normalized.task_id,
                    sort_order: 0,
                });
            }
        }
    }

    let scope_keys = scopes
        .iter()
        .map(|scope| scope.key.clone())
        .collect::<HashSet<_>>();
    with_group_store(&ctx.app, |store| {
        let existing_added_at = store
            .members
            .iter()
            .map(|member| {
                (
                    format!(
                        "{}:{}:{}",
                        member.group_id, member.workspace_key, member.task_id
                    ),
                    member.added_at,
                )
            })
            .collect::<HashMap<_, _>>();
        store
            .members
            .retain(|member| !scope_keys.contains(&member.workspace_key));
        for (group_id, refs) in &group_task_refs {
            for (index, reference) in refs.iter().enumerate() {
                let key = format!(
                    "{}:{}:{}",
                    group_id, reference.workspace_key, reference.task_id
                );
                store.members.push(GroupMemberRecord {
                    group_id: group_id.clone(),
                    workspace_key: reference.workspace_key.clone(),
                    workspace_path: reference.workspace_path.clone(),
                    workspace_identity: reference.workspace_identity.clone(),
                    task_id: reference.task_id.clone(),
                    sort_order: Some((index as i64 + 1) * GROUP_ORDER_STEP),
                    added_at: existing_added_at
                        .get(&key)
                        .copied()
                        .unwrap_or_else(now_epoch_ms),
                });
            }
        }
        store.top_level_orders.retain(|order| match order {
            TopLevelOrderRecord::Group { group_id, .. } => !group_inputs.contains(group_id),
            TopLevelOrderRecord::Task { workspace_key, .. } => !scope_keys.contains(workspace_key),
        });
        for (index, mut node) in top_nodes.into_iter().enumerate() {
            let sort_order = (index as i64 + 1) * GROUP_ORDER_STEP;
            match &mut node {
                TopLevelOrderRecord::Group {
                    sort_order: value, ..
                }
                | TopLevelOrderRecord::Task {
                    sort_order: value, ..
                } => *value = sort_order,
            }
            store.top_level_orders.push(node);
        }
        Ok(())
    })
}

async fn apply_grouped_order(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let input: GroupedViewOrderInput = serde_json::from_value(args.clone())
        .map_err(|error| invalid_params(format!("applyGroupedTaskViewOrder 参数无效：{error}")))?;
    let scopes = parse_scope_values(&ctx.app, &input.workspace_scopes)?;
    apply_grouped_order_sync(ctx, &input, &scopes)?;
    build_grouped_view(ctx, &scopes)
}

fn build_grouped_view(ctx: &GatewayContext, scopes: &[AuthorizedScope]) -> Result<Value, RpcError> {
    if scopes.is_empty() {
        return Ok(json!({"nodes": []}));
    }
    let rows = collect_task_items(ctx, scopes, "timeline", "createdAt", None)?;
    let store = read_group_store(&ctx.app)?;
    let scope_keys = scopes
        .iter()
        .map(|scope| scope.key.as_str())
        .collect::<HashSet<_>>();
    let visible_groups = store
        .groups
        .iter()
        .filter(|group| {
            !store
                .members
                .iter()
                .any(|member| member.group_id == group.id)
                || store.members.iter().any(|member| {
                    member.group_id == group.id
                        && scope_keys.contains(member.workspace_key.as_str())
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut rows_by_key = HashMap::new();
    for row in rows {
        if let Some(key) = task_meta_workspace_key(&row)
            && let Some(task_id) = row.get("taskId").and_then(Value::as_str)
        {
            // rows 已是发给 Source 的 wire meta；分组持久化 key 仍是 canonical，join
            // 必须把 member key 投影到同一 wire 表示，不能让 Windows `\\?\` 前缀造成漏行。
            rows_by_key.insert(grouped_task_join_key(&key, task_id), row);
        }
    }
    let members_by_group = store
        .members
        .iter()
        .filter(|member| scope_keys.contains(member.workspace_key.as_str()))
        .fold(
            HashMap::<String, Vec<&GroupMemberRecord>>::new(),
            |mut map, member| {
                map.entry(member.group_id.clone()).or_default().push(member);
                map
            },
        );
    let all_member_keys = store
        .members
        .iter()
        .filter(|member| scope_keys.contains(member.workspace_key.as_str()))
        .map(|member| grouped_task_join_key(&member.workspace_key, &member.task_id))
        .collect::<HashSet<_>>();
    let mut nodes = Vec::new();
    for group in &visible_groups {
        let mut tasks = members_by_group
            .get(&group.id)
            .into_iter()
            .flatten()
            .filter_map(|member| {
                rows_by_key
                    .get(&grouped_task_join_key(
                        &member.workspace_key,
                        &member.task_id,
                    ))
                    .cloned()
                    .map(|task| (member.sort_order, member.added_at, task))
            })
            .collect::<Vec<_>>();
        tasks.sort_by(|left, right| {
            left.0
                .unwrap_or(i64::MAX)
                .cmp(&right.0.unwrap_or(i64::MAX))
                .then_with(|| right.1.cmp(&left.1))
        });
        let mut node = json!({
            "type": "group",
            "group": group_to_value(group),
            "tasks": tasks.into_iter().map(|(_, _, task)| task).collect::<Vec<_>>(),
        });
        if let Some(order) = order_value_for_group(&store, &group.id) {
            node["sortOrder"] = Value::Number(order.into());
        }
        nodes.push(node);
    }
    for (key, task) in rows_by_key {
        if all_member_keys.contains(&key) {
            continue;
        }
        let Some((workspace_key, task_id)) = key.split_once('\u{0}') else {
            continue;
        };
        let mut node = json!({"type": "task", "task": task});
        if let Some(order) = order_value_for_task(&store, workspace_key, task_id) {
            node["sortOrder"] = Value::Number(order.into());
        }
        nodes.push(node);
    }
    let mut next_order = store
        .top_level_orders
        .iter()
        .map(|order| match order {
            TopLevelOrderRecord::Group { sort_order, .. }
            | TopLevelOrderRecord::Task { sort_order, .. } => *sort_order,
        })
        .max()
        .unwrap_or(0);
    for node in nodes
        .iter_mut()
        .filter(|node| node.get("sortOrder").is_none())
    {
        next_order = next_order.saturating_add(GROUP_ORDER_STEP);
        node["sortOrder"] = Value::Number(next_order.into());
    }
    nodes.sort_by(|left, right| {
        left.get("sortOrder")
            .and_then(Value::as_i64)
            .cmp(&right.get("sortOrder").and_then(Value::as_i64))
            .then_with(|| {
                serde_json::to_string(left)
                    .unwrap_or_default()
                    .cmp(&serde_json::to_string(right).unwrap_or_default())
            })
    });
    Ok(json!({"nodes": nodes}))
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn session_storage_paths(
    runtime: &AgentRuntime,
    task_id: &str,
) -> Result<(PathBuf, PathBuf, PathBuf), RpcError> {
    let session_id = SessionId::new(task_id.to_owned()).map_err(backend_error)?;
    let directory =
        keencode_resources::session_storage_directory(runtime.storage_root(), &session_id)
            .map_err(backend_error)?;
    Ok((
        directory.clone(),
        directory.join("events.jsonl"),
        directory.join("snapshot.json"),
    ))
}

fn get_native_session_log_file(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let task = authorized_task(ctx, args)?;
    let (_, event_path, _) =
        session_storage_paths(&task.runtime, &task.metadata.session_id.to_string())?;
    let provider = task
        .session
        .snapshot()
        .map_err(backend_error)?
        .state
        .provider
        .as_ref()
        .map(|_| "glm");
    Ok(json!({
        "provider": provider,
        "path": event_path.to_string_lossy(),
        "exists": is_regular_file(&event_path),
    }))
}

fn get_task_session_file_path(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let task = authorized_task(ctx, args)?;
    let (_, _, snapshot_path) =
        session_storage_paths(&task.runtime, &task.metadata.session_id.to_string())?;
    Ok(json!({
        "path": snapshot_path.to_string_lossy(),
        "exists": is_regular_file(&snapshot_path),
    }))
}

fn iso_time(unix_ms: u64) -> String {
    DateTime::<Utc>::from_timestamp_millis(unix_ms as i64)
        .map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| unix_ms.to_string())
}

fn trajectory_role(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Developer => "developer",
        MessageRole::Tool => "tool",
    }
}

fn image_media_type(source: &MessageImageSource) -> Option<String> {
    match source {
        MessageImageSource::Url { .. } => None,
        MessageImageSource::Artifact { artifact } => artifact.media_type.clone(),
    }
}

fn trajectory_image_part(source: &MessageImageSource) -> Value {
    let mut value = json!({"kind": "image"});
    if let Some(media_type) = image_media_type(source) {
        // URL 图片的媒体类型由远端响应决定；缺省时省略字段，不能把 TypeScript
        // optional 字段编码成 null。
        value["mediaType"] = Value::String(media_type);
    }
    value
}

fn tool_result_part_value(part: &ToolResultPart) -> Value {
    match part {
        ToolResultPart::Text { text } => json!({"kind": "text", "text": text}),
        ToolResultPart::Image { source } => trajectory_image_part(source),
        ToolResultPart::Artifact {
            artifact,
            materialization,
        } => json!({
            "kind": "artifact",
            "artifactId": artifact.artifact_id,
            "mediaType": artifact.media_type,
            "materialization": materialization,
        }),
    }
}

fn trajectory_part(part: &MessagePart) -> Value {
    match part {
        MessagePart::Text { text } => json!({"kind": "text", "text": text}),
        MessagePart::Reasoning { text, .. } => json!({"kind": "reasoning", "text": text}),
        MessagePart::Image { source } => trajectory_image_part(source),
        MessagePart::ToolCall {
            tool_call_id,
            tool_name,
            arguments,
        } => json!({
            "kind": "tool-call",
            "toolCallId": tool_call_id,
            "toolName": tool_name,
            "input": arguments,
        }),
        MessagePart::ToolResult {
            tool_call_id,
            content,
            is_error,
        } => json!({
            "kind": "tool-result",
            "toolCallId": tool_call_id,
            "output": {
                "content": content.iter().map(tool_result_part_value).collect::<Vec<_>>(),
                "isError": is_error,
            },
        }),
        MessagePart::Artifact {
            artifact,
            materialization,
        } => json!({
            "kind": "unknown",
            "raw": {
                "artifactId": artifact.artifact_id,
                "mediaType": artifact.media_type,
                "materialization": materialization,
            },
        }),
    }
}

fn trajectory_message(message: &SessionMessage) -> Value {
    json!({
        "role": trajectory_role(&message.role),
        "parts": message.content.iter().map(trajectory_part).collect::<Vec<_>>(),
    })
}

fn finish_reason(reason: &keencode_model::StopReason) -> String {
    match reason {
        keencode_model::StopReason::Completed => "completed".to_owned(),
        keencode_model::StopReason::ToolUse => "tool_use".to_owned(),
        keencode_model::StopReason::MaxOutputTokens => "max_output_tokens".to_owned(),
        keencode_model::StopReason::ContentFilter => "content_filter".to_owned(),
        keencode_model::StopReason::Cancelled => "cancelled".to_owned(),
        keencode_model::StopReason::Other { reason } => reason.clone(),
    }
}

fn trajectory_messages<'a>(messages: impl IntoIterator<Item = &'a SessionMessage>) -> Vec<Value> {
    messages.into_iter().map(trajectory_message).collect()
}

fn standalone_message_belongs_to_agent(
    state: &SessionState,
    message: &SessionMessage,
    source_agent_id: &keencode_resources::AgentId,
) -> bool {
    message.turn_id.as_ref().is_none_or(|turn_id| {
        state
            .turns
            .get(turn_id)
            .is_some_and(|turn| turn.source_agent_id == *source_agent_id)
    })
}

/// 从 Journal 重建某次模型请求已经拥有的持久上下文。
///
/// 模型 Round 只保存完成元数据，Provider 请求本身没有作为第二份事实落盘，
/// 因此这里按 Transcript 顺序截到该 Round 首个输出段之前。这样能保留真实
/// 的历史消息，同时不会把后续 Round 的助手输出误报为当前请求输入。
fn request_messages_for_round<'a>(
    state: &'a SessionState,
    round: &keencode_resources::ModelRoundState,
) -> Vec<&'a SessionMessage> {
    let target_turn_started_at = state
        .turns
        .get(&round.turn_id)
        .map(|turn| turn.started_at_unix_ms);
    let mut messages = Vec::new();
    for record in &state.transcript {
        match record {
            TranscriptRecord::MessageAdded(message) => {
                let before_target_turn = message.turn_id.as_ref().is_none_or(|turn_id| {
                    turn_id == &round.turn_id
                        || state
                            .turns
                            .get(turn_id)
                            .zip(target_turn_started_at)
                            .is_none_or(|(turn, target_started_at)| {
                                turn.started_at_unix_ms <= target_started_at
                            })
                });
                if before_target_turn
                    && standalone_message_belongs_to_agent(state, message, &round.source_agent_id)
                {
                    messages.push(message);
                }
            }
            TranscriptRecord::SegmentCommitted(segment) => {
                let same_round_or_later = segment.turn_id == round.turn_id
                    && segment.source_agent_id == round.source_agent_id
                    && segment.model_round >= round.model_round;
                let later_turn = target_turn_started_at
                    .zip(state.turns.get(&segment.turn_id))
                    .is_some_and(|(target_started_at, turn)| {
                        turn.started_at_unix_ms > target_started_at
                    });
                if same_round_or_later || later_turn {
                    break;
                }
                if segment.source_agent_id == round.source_agent_id {
                    messages.extend(segment.messages.iter());
                }
            }
            // 压缩记录只改变有效上下文的投影；不把未来记录带入当前请求。
            // 轨迹仍然来自 Journal，且不会因此伪造一份新的模型请求事实。
            TranscriptRecord::CompactionApplied(_) => {}
        }
    }
    messages
}

fn model_round_tool_names(
    state: &SessionState,
    round: &keencode_resources::ModelRoundState,
) -> Vec<String> {
    let mut names = state
        .tools
        .values()
        .filter(|tool| {
            let request = &tool.request;
            request.turn_id == round.turn_id
                && request.agent_id == round.source_agent_id
                && request.model_round == round.model_round
        })
        .map(|tool| tool.request.tool_name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn model_round_record(
    task_id: &str,
    state: &SessionState,
    round: &keencode_resources::ModelRoundState,
) -> Value {
    let turn = state.turns.get(&round.turn_id);
    let started_at = turn
        .map(|turn| turn.started_at_unix_ms)
        .unwrap_or(round.completed_at_unix_ms);
    let segments = state
        .transcript_segments()
        .filter(|segment| {
            segment.turn_id == round.turn_id
                && segment.source_agent_id == round.source_agent_id
                && segment.model_round == round.model_round
        })
        .collect::<Vec<&TranscriptSegment>>();
    let mut response_text = Vec::new();
    let mut reasoning_text = Vec::new();
    let mut tool_calls = Vec::new();
    let mut tool_names = HashSet::new();
    for segment in &segments {
        for message in &segment.messages {
            for part in &message.content {
                match part {
                    MessagePart::Text { text } if message.role == MessageRole::Assistant => {
                        response_text.push(text.as_str());
                    }
                    MessagePart::Reasoning { text, .. }
                        if message.role == MessageRole::Assistant =>
                    {
                        reasoning_text.push(text.as_str());
                    }
                    MessagePart::ToolCall {
                        tool_call_id,
                        tool_name,
                        arguments,
                    } => {
                        tool_names.insert(tool_name.clone());
                        tool_calls.push(json!({
                            "kind": "tool-call",
                            "toolCallId": tool_call_id,
                            "toolName": tool_name,
                            "input": arguments,
                        }));
                    }
                    _ => {}
                }
            }
        }
    }
    let request_messages = request_messages_for_round(state, round);
    let mut request_tool_names = model_round_tool_names(state, round);
    request_tool_names.extend(tool_names);
    request_tool_names.sort();
    request_tool_names.dedup();
    let usage = &round.usage;
    let mut usage_value = serde_json::Map::new();
    for (name, value) in [
        ("inputTokens", usage.input_tokens),
        ("outputTokens", usage.output_tokens),
        ("totalTokens", usage.total_tokens),
        ("cacheReadTokens", usage.cache_read_tokens),
        ("reasoningTokens", usage.reasoning_tokens),
    ] {
        if let Some(value) = value {
            usage_value.insert(name.to_owned(), Value::Number(value.into()));
        }
    }
    let mut response = json!({
        "finishReason": finish_reason(&round.stop_reason),
        "toolCalls": tool_calls,
    });
    if !usage_value.is_empty() {
        response["usage"] = Value::Object(usage_value);
    }
    if !response_text.is_empty() {
        response["text"] = Value::String(response_text.join(""));
    }
    if !reasoning_text.is_empty() {
        response["reasoningText"] = Value::String(reasoning_text.join(""));
    }
    if let Some(response_id) = &round.metadata.response_id {
        response["responseId"] = Value::String(response_id.clone());
    }
    if let Some(model) = &round.metadata.model {
        response["modelId"] = Value::String(model.clone());
    }
    let model = state.provider.as_ref();
    let mut record = json!({
        "requestId": format!(
            "{task_id}:model-round:{}:{}",
            round.turn_id.as_str(),
            round.model_round
        ),
        "attempt": 1,
        "startedAt": iso_time(started_at),
        "completedAt": iso_time(round.completed_at_unix_ms),
        "durationMs": round.completed_at_unix_ms.saturating_sub(started_at),
        "turnId": round.turn_id.as_str(),
        "traceId": format!("session:{task_id}"),
        "callSource": {
            "kind": if round.source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID {
                "main"
            } else {
                "subagent"
            },
        },
        "model": {
            "modelId": round.metadata.model.clone().or_else(|| Some(round.requested_model.clone())),
            "providerId": model.map(|provider| provider.provider_id.clone()),
            "role": round.source_agent_id.as_str(),
            "source": "keencode-journal",
        },
        "request": {
            "messages": trajectory_messages(request_messages),
            "toolNames": request_tool_names,
        },
        "response": response,
    });
    if let Some(turn) = turn
        && matches!(
            turn.status,
            keencode_resources::TurnStatus::Failed | keencode_resources::TurnStatus::Cancelled
        )
        && let Some(message) = &turn.outcome_message
    {
        record["error"] = json!({
            "name": "turn_failed",
            "message": message,
        });
    }
    record
}

fn get_model_trajectory(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let id = task_id(args)?;
    let requested_limit = args.get("limit").and_then(Value::as_u64).unwrap_or(200);
    if requested_limit == 0 {
        return Err(invalid_params("limit 必须为正整数"));
    }
    let limit = usize::try_from(requested_limit)
        .unwrap_or(MAX_MODEL_TRAJECTORY_LIMIT)
        .min(MAX_MODEL_TRAJECTORY_LIMIT);
    let mut task_args = args.clone();
    if task_args.get("taskId").is_none() {
        task_args["taskId"] = Value::String(id.clone());
    }
    let task = if task_args.get("workspacePath").is_some() {
        authorized_task(ctx, &task_args)?
    } else {
        let runtime = owned_runtime(&ctx.app)?;
        let metadata = runtime
            .runtime_manager()
            .stored_session_metadata(&id)
            .map_err(backend_error)?;
        let session = open_authorized_session(&runtime, &ctx.app, &id).map_err(backend_error)?;
        if session.is_workflow_actor().map_err(backend_error)? {
            return Err(not_found("工作流 actor 不属于用户任务"));
        }
        let state = session.snapshot().map_err(backend_error)?.state;
        let path = state.project_root.clone();
        let root = authorize_stored_root(&ctx.app, &path).map_err(backend_error)?;
        if root.to_string_lossy() != state.project_root {
            return Err(RpcError::new(
                "task.workspaceMismatch",
                "任务 workspace 未通过授权规范化",
            ));
        }
        AuthorizedTask {
            runtime,
            session,
            metadata,
            workspace_path: state.project_root,
            workspace_identity: None,
        }
    };
    let snapshot = task.session.snapshot().map_err(backend_error)?;
    let mut rounds = snapshot.state.model_rounds.iter().collect::<Vec<_>>();
    rounds.sort_by(|left, right| {
        left.completed_at_unix_ms
            .cmp(&right.completed_at_unix_ms)
            .then_with(|| left.turn_id.cmp(&right.turn_id))
            .then_with(|| left.model_round.cmp(&right.model_round))
    });
    let truncated = rounds.len() > limit;
    let start = rounds.len().saturating_sub(limit);
    let records = rounds[start..]
        .iter()
        .map(|round| model_round_record(&id, &snapshot.state, round))
        .collect::<Vec<_>>();
    let (_, event_path, snapshot_path) =
        session_storage_paths(&task.runtime, &task.metadata.session_id.to_string())?;
    let source_files = [event_path, snapshot_path]
        .into_iter()
        .filter(|path| is_regular_file(path))
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    Ok(json!({
        "taskId": id,
        "available": !snapshot.state.model_rounds.is_empty(),
        "records": records,
        "sourceFiles": source_files,
        "truncated": truncated,
    }))
}

fn task_meta_after_mutation(
    task: &AuthorizedTask,
    unread_at: Option<u64>,
) -> Result<Value, RpcError> {
    let state = task.session.snapshot().map_err(backend_error)?.state;
    task_meta_value(
        &task.runtime,
        &task.metadata,
        &state,
        task.workspace_identity.as_deref(),
        unread_at,
    )
}

fn set_task_pinned(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let pinned = required_bool(args, "pinned")?;
    let task = authorized_task(ctx, args)?;
    let state = task.session.snapshot().map_err(backend_error)?.state;
    let operation_id = operation_id(
        args,
        "set-pinned",
        task.metadata.session_id.as_str(),
        pinned,
        state.last_sequence,
    )?;
    task.session
        .set_preference(&operation_id, Some(pinned), None)
        .map_err(backend_error)?;
    let workspace_key =
        task_workspace_key(&task.workspace_path, task.workspace_identity.as_deref());
    let unread_at =
        read_group_store(&ctx.app)?.unread_at(&workspace_key, task.metadata.session_id.as_str());
    task_meta_after_mutation(&task, unread_at)
}

fn set_task_archived(
    ctx: &GatewayContext,
    args: &Value,
    archived: bool,
) -> Result<Value, RpcError> {
    let task = authorized_task(ctx, args)?;
    let state = task.session.snapshot().map_err(backend_error)?.state;
    let operation_id = operation_id(
        args,
        if archived { "archive" } else { "unarchive" },
        task.metadata.session_id.as_str(),
        archived,
        state.last_sequence,
    )?;
    task.session
        .set_preference(&operation_id, None, Some(archived))
        .map_err(backend_error)?;
    let workspace_key =
        task_workspace_key(&task.workspace_path, task.workspace_identity.as_deref());
    let unread_at =
        read_group_store(&ctx.app)?.unread_at(&workspace_key, task.metadata.session_id.as_str());
    task_meta_after_mutation(&task, unread_at)
}

/// 归档工作树前返回同一次 Journal mutation 的 operationId/sequence。
///
/// 清理命令必须校验精确的 SessionPreferenceSet，UI 不能从 task meta 的更新时间
/// 或本地计数推导 sequence；这里复用 archiveTask 的授权和写入路径，只扩展一个
/// 专用回执 DTO，避免工作树清理另存事实或重复提交归档事件。
fn archive_task_with_receipt(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let task = authorized_task(ctx, args)?;
    let state = task.session.snapshot().map_err(backend_error)?.state;
    let operation_id = operation_id(
        args,
        "archive",
        task.metadata.session_id.as_str(),
        true,
        state.last_sequence,
    )?;
    let committed = task
        .session
        .set_preference(&operation_id, None, Some(true))
        .map_err(backend_error)?;
    let workspace_key =
        task_workspace_key(&task.workspace_path, task.workspace_identity.as_deref());
    let unread_at =
        read_group_store(&ctx.app)?.unread_at(&workspace_key, task.metadata.session_id.as_str());
    let meta = task_meta_after_mutation(&task, unread_at)?;
    Ok(json!({
        "meta": meta,
        "operationId": operation_id,
        "journalSequence": committed.last_sequence,
    }))
}

/// Controller 与 legacy task 入口共用的未读写入事实源。
///
/// `expectedUnreadAt` 只约束已读清除；不带期望值时保留旧入口的无条件写入语义。
pub(crate) fn set_task_unread(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let unread = required_bool(args, "unread")?;
    let expected_unread_at: Option<u64> = match args.get("expectedUnreadAt") {
        None | Some(Value::Null) => None,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| invalid_params("expectedUnreadAt 必须为非负整数"))?
            .into(),
    };
    let task = authorized_task(ctx, args)?;
    let workspace_key =
        task_workspace_key(&task.workspace_path, task.workspace_identity.as_deref());
    let task_id = task.metadata.session_id.as_str().to_owned();
    // 先在锁内提交唯一持久事实源，再释放锁；通知会回读 task-groups 生成
    // Controller/workspace 投影，不能在同一锁作用域内重入读取。
    let next = {
        let _guard = group_store_lock()?;
        let _process_lock = group_store_process_lock(&ctx.app)?;
        let mut store = read_group_store(&ctx.app)?;
        let (next, changed) = apply_unread_mutation(
            &mut store,
            &workspace_key,
            &task_id,
            unread,
            expected_unread_at,
            now_epoch_ms(),
        );
        if changed {
            write_group_store(&ctx.app, &store)?;
        }
        next
    };
    let meta = task_meta_value(
        &task.runtime,
        &task.metadata,
        &task.session.snapshot().map_err(backend_error)?.state,
        task.workspace_identity.as_deref(),
        next,
    );
    // legacy zcode-task 和 Controller mutation 都经过这里；CAS 未命中时也广播
    // 当前权威值，让晚到的前端响应立即收敛到新一轮未读状态。
    super::notify_task_unread_mutation(ctx, &task.runtime, &task.workspace_path, &task_id);
    meta
}

/// Controller 列表/事件与 legacy Session 快照读取同一份持久化未读状态。
pub(crate) fn unread_at_for_task(
    app: &tauri::AppHandle,
    workspace_path: &str,
    workspace_identity: Option<&str>,
    task_id: &str,
) -> Result<Option<u64>, RpcError> {
    let _guard = group_store_lock()?;
    let store = read_group_store(app)?;
    let workspace_key = task_workspace_key(workspace_path, workspace_identity);
    Ok(store.unread_at(&workspace_key, task_id))
}

/// 一次读取 Controller 列表所需的全部未读索引，避免每行重复读取存储文件。
pub(crate) fn unread_at_index(
    app: &tauri::AppHandle,
) -> Result<HashMap<String, HashMap<String, u64>>, RpcError> {
    let _guard = group_store_lock()?;
    let store = read_group_store(app)?;
    let mut index = HashMap::<String, HashMap<String, u64>>::new();
    for item in store.unread {
        index
            .entry(item.workspace_key)
            .or_default()
            .insert(item.task_id, item.unread_at);
    }
    Ok(index)
}

/// 在已持有任务分组存储锁的调用路径中执行未读 CAS；返回投影值和是否实际写入。
fn apply_unread_mutation(
    store: &mut GroupStoreFile,
    workspace_key: &str,
    task_id: &str,
    unread: bool,
    expected_unread_at: Option<u64>,
    next_unread_at: u64,
) -> (Option<u64>, bool) {
    let current = store.unread_at(workspace_key, task_id);
    // 已读 compare-and-clear 只在客户端仍观察到同一个时间戳时生效，避免晚到的
    // “已读”响应清掉新一轮后台输出产生的未读标记。
    if expected_unread_at.is_some() && expected_unread_at != current {
        return (current, false);
    }
    let next = unread.then_some(next_unread_at);
    if next == current {
        return (current, false);
    }
    store.set_unread(workspace_key, task_id, next);
    (next, true)
}

fn parse_task_ids(args: &Value) -> Result<Vec<String>, RpcError> {
    let values = args
        .get("taskIds")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_params("taskIds 必须为数组"))?;
    let mut ids = Vec::with_capacity(values.len());
    let mut seen = HashSet::new();
    for value in values {
        let id = value
            .as_str()
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .ok_or_else(|| invalid_params("taskIds 必须为非空字符串数组"))?;
        if seen.insert(id.to_owned()) {
            ids.push(id.to_owned());
        }
    }
    Ok(ids)
}

fn deleted_task_ids_for_workspace(
    runtime: &AgentRuntime,
    workspace_path: &str,
) -> Result<HashSet<String>, RpcError> {
    keencode_resources::list_deleted_session_ids(runtime.storage_root(), workspace_path)
        .map_err(backend_error)
        .map(|ids| ids.into_iter().map(|id| id.as_str().to_owned()).collect())
}

fn remove_deleted_organization_records(
    ctx: &GatewayContext,
    workspace_key: &str,
    task_ids: &HashSet<String>,
) -> Result<(), RpcError> {
    if task_ids.is_empty() {
        return Ok(());
    }
    with_group_store(&ctx.app, |store| {
        store.members.retain(|member| {
            !(member.workspace_key == workspace_key && task_ids.contains(&member.task_id))
        });
        store.top_level_orders.retain(|order| {
            !matches!(
                order,
                TopLevelOrderRecord::Task {
                    workspace_key: key,
                    task_id,
                    ..
                } if key == workspace_key && task_ids.contains(task_id)
            )
        });
        store.unread.retain(|item| {
            !(item.workspace_key == workspace_key && task_ids.contains(&item.task_id))
        });
        Ok(())
    })
}

fn delete_task(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let task = authorized_task(ctx, args)?;
    if task
        .runtime
        .session_has_active_work(task.metadata.session_id.as_str())
        .map_err(backend_error)?
    {
        return Err(RpcError::new(
            "task.active",
            "活动任务不能删除，请先停止运行",
        ));
    }
    let deleted = deleted_task_ids_for_workspace(&task.runtime, &task.workspace_path)?;
    if deleted.contains(task.metadata.session_id.as_str()) {
        return Ok(Value::Null);
    }
    // 普通删除只写负向 membership，保留 Journal、快照和当前 Runtime 句柄，
    // 这样 CLI/恢复读取仍能按 taskId 访问完整会话事实。
    keencode_resources::record_deleted_session(
        task.runtime.storage_root(),
        &task.metadata.session_id,
        &task.workspace_path,
    )
    .map_err(backend_error)?;
    let mut deleted_ids = HashSet::new();
    deleted_ids.insert(task.metadata.session_id.as_str().to_owned());
    let workspace_key =
        task_workspace_key(&task.workspace_path, task.workspace_identity.as_deref());
    remove_deleted_organization_records(ctx, &workspace_key, &deleted_ids)?;
    Ok(Value::Null)
}

async fn delete_archived_tasks(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let workspace_path = required_string(args, "workspacePath")?;
    let root = authorize_stored_root(&ctx.app, &workspace_path)
        .map_err(backend_error)?
        .to_string_lossy()
        .into_owned();
    let workspace_identity = optional_string(args, "workspaceIdentity")?;
    let workspace_key = task_workspace_key(&root, workspace_identity.as_deref());
    let ids = parse_task_ids(args)?;
    let runtime = owned_runtime(&ctx.app)?;
    let already_deleted = deleted_task_ids_for_workspace(&runtime, &root)?;
    let mut deleted = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();
    let mut deleted_set = HashSet::new();
    for id in ids {
        if already_deleted.contains(&id) {
            skipped.push(id);
            continue;
        }
        let metadata = match runtime.runtime_manager().stored_session_metadata(&id) {
            Ok(metadata) => metadata,
            Err(
                keencode_runtime::RuntimeError::SessionNotCreated
                | keencode_runtime::RuntimeError::SessionNotRegistered
                | keencode_runtime::RuntimeError::SessionCorrupt,
            ) => {
                skipped.push(id);
                continue;
            }
            Err(_) => {
                failed.push(id);
                continue;
            }
        };
        let item_root = match authorize_stored_root(&ctx.app, &metadata.project_root) {
            Ok(root) => root.to_string_lossy().into_owned(),
            Err(_) => {
                skipped.push(id);
                continue;
            }
        };
        if metadata.corrupt || item_root != root || !metadata.archived {
            skipped.push(id);
            continue;
        }
        let session = match open_authorized_session(&runtime, &ctx.app, &id) {
            Ok(session) => session,
            Err(_) => {
                failed.push(id);
                continue;
            }
        };
        if session.is_workflow_actor().map_err(backend_error)? {
            skipped.push(id);
            continue;
        }
        if !session.snapshot().map_err(backend_error)?.state.archived {
            skipped.push(id);
            continue;
        }
        match runtime.session_has_active_work(&id) {
            Ok(true) => {
                failed.push(id);
                continue;
            }
            Ok(false) => {}
            Err(_) => {
                failed.push(id);
                continue;
            }
        }
        // 归档删除只提交现有负向 membership，保留 Session Journal 与快照，
        // 这样 CLI/恢复链仍能按 taskId 读取事实，且重复删除会稳定返回 skipped。
        match runtime.close_session(&id).await {
            Ok(()) => match keencode_resources::record_deleted_session(
                runtime.storage_root(),
                &metadata.session_id,
                &item_root,
            ) {
                Ok(()) => {
                    deleted_set.insert(id.clone());
                    deleted.push(id);
                }
                Err(_) => failed.push(id),
            },
            Err(_) => failed.push(id),
        }
    }
    remove_deleted_organization_records(ctx, &workspace_key, &deleted_set)?;
    Ok(json!({
        "deletedTaskIds": deleted,
        "skippedTaskIds": skipped,
        "failedTaskIds": failed,
    }))
}

async fn delete_archived_task(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let task_id = task_id(args)?;
    let mut object = args
        .as_object()
        .cloned()
        .ok_or_else(|| invalid_params("deleteArchivedTask 参数必须为对象"))?;
    object.remove("taskId");
    object.remove("sessionId");
    object.insert("taskIds".to_owned(), json!([task_id]));
    let result = delete_archived_tasks(ctx, &Value::Object(object)).await?;
    let deleted = result
        .get("deletedTaskIds")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    Ok(Value::Bool(deleted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_store_unread_is_idempotent_and_compareable() {
        let mut store = GroupStoreFile::default();
        assert_eq!(store.unread_at("workspace", "task"), None);

        store.set_unread("workspace", "task", Some(42));
        assert_eq!(store.unread_at("workspace", "task"), Some(42));
        store.set_unread("workspace", "task", Some(42));
        assert_eq!(store.unread.len(), 1);

        store.set_unread("workspace", "task", None);
        assert_eq!(store.unread_at("workspace", "task"), None);
    }

    #[test]
    fn unread_mutation_preserves_newer_value_when_read_cas_is_stale() {
        let mut store = GroupStoreFile::default();
        store.set_unread("workspace", "task", Some(42));

        let (value, changed) =
            apply_unread_mutation(&mut store, "workspace", "task", false, Some(41), 99);

        assert_eq!(value, Some(42));
        assert!(!changed);
        assert_eq!(store.unread_at("workspace", "task"), Some(42));
    }

    #[test]
    fn unread_mutation_writes_mark_unread_and_clears_matching_read_cas() {
        let mut store = GroupStoreFile::default();
        let (value, changed) =
            apply_unread_mutation(&mut store, "workspace", "task", true, None, 99);
        assert_eq!(value, Some(99));
        assert!(changed);

        let (value, changed) =
            apply_unread_mutation(&mut store, "workspace", "task", false, Some(99), 100);
        assert_eq!(value, None);
        assert!(changed);
        assert_eq!(store.unread_at("workspace", "task"), None);
    }

    #[test]
    fn group_input_validation_rejects_unknown_color_and_accepts_default() {
        assert_eq!(parse_group_color(None).unwrap(), "gray");
        assert_eq!(parse_group_color(Some(&json!("blue"))).unwrap(), "blue");
        assert!(parse_group_color(Some(&json!("transparent"))).is_err());
        assert_eq!(normalize_group_title(None).unwrap(), "新建分组");
        assert!(normalize_group_title(Some(&json!(" "))).is_err());
    }

    #[test]
    fn deleting_group_removes_members_and_cold_restore_order_record() {
        let mut store = GroupStoreFile {
            version: GROUP_STORE_VERSION,
            groups: vec![TaskGroupRecord {
                id: "group-1".to_owned(),
                title: "Native Group".to_owned(),
                color: "gray".to_owned(),
                created_at: 1,
                updated_at: 1,
            }],
            members: vec![GroupMemberRecord {
                group_id: "group-1".to_owned(),
                workspace_key: "workspace".to_owned(),
                workspace_path: "workspace".to_owned(),
                workspace_identity: None,
                task_id: "task-1".to_owned(),
                sort_order: Some(1_000),
                added_at: 1,
            }],
            top_level_orders: vec![TopLevelOrderRecord::Group {
                group_id: "group-1".to_owned(),
                sort_order: 1_000,
            }],
            unread: Vec::new(),
        };

        remove_task_group_records(&mut store, "group-1").unwrap();

        assert!(store.groups.is_empty());
        assert!(store.members.is_empty());
        assert!(store.top_level_orders.is_empty());
        assert!(remove_task_group_records(&mut store, "group-1").is_err());
    }

    #[test]
    fn top_level_order_serializes_to_source_discriminated_shape() {
        let value = serde_json::to_value(TopLevelOrderRecord::Task {
            workspace_key: "workspace".to_owned(),
            task_id: "task".to_owned(),
            sort_order: 1000,
        })
        .unwrap();
        assert_eq!(value["type"], "task");
        assert_eq!(value["workspaceKey"], "workspace");
        assert_eq!(value["taskId"], "task");
        assert_eq!(value["sortOrder"], 1000);
    }

    #[test]
    fn grouped_join_uses_frontend_workspace_key_for_local_paths() {
        let mut store = GroupStoreFile::default();
        store.top_level_orders.push(TopLevelOrderRecord::Task {
            workspace_key: r"\\?\C:\repo".to_owned(),
            task_id: "task".to_owned(),
            sort_order: 1000,
        });
        let meta = json!({"workspacePath": "C:/repo", "taskId": "task"});

        assert_eq!(task_meta_workspace_key(&meta).as_deref(), Some("C:/repo"));
        assert_eq!(order_value_for_task(&store, "C:/repo", "task"), Some(1000));
        assert_eq!(workspace_key_for_wire("ssh:repo"), "ssh:repo");
    }

    #[test]
    fn grouped_order_accepts_source_camel_case_and_preserves_cold_member_join_key() {
        let input: GroupedViewOrderInput = serde_json::from_value(json!({
            "workspaceScopes": [{"workspacePath": "C:/repo"}],
            "topLevelNodes": [{"type": "group", "groupId": "group-1"}],
            "groups": [{
                "groupId": "group-1",
                "taskRefs": [{"workspacePath": "C:/repo", "taskId": "task-1"}]
            }]
        }))
        .unwrap();
        assert_eq!(input.groups[0].group_id, "group-1");
        assert_eq!(input.groups[0].task_refs[0].task_id, "task-1");
        assert!(
            serde_json::from_value::<GroupedViewOrderInput>(json!({
                "workspaceScopes": [{"workspacePath": "C:/repo"}],
                "topLevelNodes": [{"type": "group", "group_id": "group-1"}],
                "groups": [{"group_id": "group-1", "taskRefs": []}]
            }))
            .is_err()
        );

        // task-groups.json 保留 canonical key；冷恢复的 task row 使用 wire key，
        // 两次转换后仍须指向同一成员，不能因 Windows extended path 产生空分组。
        let member = GroupMemberRecord {
            group_id: "group-1".to_owned(),
            workspace_key: r"\\?\C:\repo".to_owned(),
            workspace_path: r"\\?\C:\repo".to_owned(),
            workspace_identity: None,
            task_id: "task-1".to_owned(),
            sort_order: Some(1_000),
            added_at: 1,
        };
        let store = GroupStoreFile {
            version: GROUP_STORE_VERSION,
            groups: vec![TaskGroupRecord {
                id: "group-1".to_owned(),
                title: "Group".to_owned(),
                color: "gray".to_owned(),
                created_at: 1,
                updated_at: 1,
            }],
            members: vec![member.clone()],
            top_level_orders: Vec::new(),
            unread: Vec::new(),
        };
        let restored: GroupStoreFile =
            serde_json::from_value(serde_json::to_value(store).unwrap()).unwrap();
        let restored_member = &restored.members[0];
        assert_eq!(
            grouped_task_join_key(&restored_member.workspace_key, &restored_member.task_id),
            grouped_task_join_key("C:/repo", "task-1")
        );
    }

    #[test]
    fn grouped_structure_members_are_filtered_to_requested_workspace_scopes() {
        let member = |workspace_key: &str, task_id: &str| GroupMemberRecord {
            group_id: "group-1".to_owned(),
            workspace_key: workspace_key.to_owned(),
            workspace_path: workspace_key.to_owned(),
            workspace_identity: None,
            task_id: task_id.to_owned(),
            sort_order: None,
            added_at: 1,
        };
        let members = [
            member("workspace-visible", "task-visible"),
            member("workspace-hidden", "task-hidden"),
        ];
        let scope_keys = ["workspace-visible"].into_iter().collect::<HashSet<_>>();

        let visible = members
            .iter()
            .filter(|member| member_belongs_to_scopes(member, &scope_keys))
            .collect::<Vec<_>>();

        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].task_id, "task-visible");
        assert!(
            members
                .iter()
                .filter(|member| !member_belongs_to_scopes(member, &scope_keys))
                .all(|member| member.task_id == "task-hidden")
        );
    }

    #[test]
    fn trajectory_projection_preserves_tool_call_and_result_parts() {
        let message = SessionMessage {
            is_meta: false,
            references: Vec::new(),
            message_id: "message-1".to_owned(),
            turn_id: None,
            agent_id: None,
            role: MessageRole::Assistant,
            content: vec![
                MessagePart::Text {
                    text: "先读文件".to_owned(),
                },
                MessagePart::ToolCall {
                    tool_call_id: "call-1".to_owned(),
                    tool_name: "read_file".to_owned(),
                    arguments: json!({"path": "src/lib.rs"}),
                },
                MessagePart::ToolResult {
                    tool_call_id: "call-1".to_owned(),
                    content: vec![ToolResultPart::Text {
                        text: "文件内容".to_owned(),
                    }],
                    is_error: false,
                },
            ],
        };
        let value = trajectory_message(&message);
        assert_eq!(value["role"], "assistant");
        assert_eq!(
            value["parts"][0],
            json!({"kind": "text", "text": "先读文件"})
        );
        assert_eq!(value["parts"][1]["kind"], "tool-call");
        assert_eq!(value["parts"][1]["toolCallId"], "call-1");
        assert_eq!(value["parts"][2]["kind"], "tool-result");
        assert_eq!(
            value["parts"][2]["output"]["content"][0]["text"],
            "文件内容"
        );
        assert_eq!(value["parts"][2]["output"]["isError"], false);

        let remote_image = trajectory_image_part(&MessageImageSource::Url {
            url: "https://example.invalid/image.png".to_owned(),
        });
        assert_eq!(remote_image["kind"], "image");
        assert!(remote_image.get("mediaType").is_none());
    }

    #[test]
    fn task_meta_status_uses_only_source_persisted_values() {
        let waiting = keencode_resources::SessionStatus::Waiting;
        assert_eq!(task_status_wire(false, &waiting, None), "running");

        let idle = keencode_resources::SessionStatus::Idle;
        assert_eq!(
            task_status_wire(false, &idle, Some(&keencode_resources::TurnStatus::Failed)),
            "error"
        );
        assert_eq!(task_status_wire(false, &idle, None), "completed");
    }

    #[test]
    fn list_task_query_requires_source_fields() {
        let result = serde_json::from_value::<TaskListQuery>(json!({
            "workspaceScopes": []
        }));
        assert!(result.is_err());
    }

    #[test]
    fn trajectory_request_stops_before_the_current_round_output() {
        let turn_id = keencode_resources::TurnId::new("turn-1").unwrap();
        let source_agent_id = keencode_resources::AgentId::new("root").unwrap();
        let message = |message_id: &str, role: MessageRole, text: &str| SessionMessage {
            is_meta: false,
            references: Vec::new(),
            message_id: message_id.to_owned(),
            turn_id: None,
            agent_id: None,
            role,
            content: vec![MessagePart::Text {
                text: text.to_owned(),
            }],
        };
        let segment = |model_round: u32, message_id: &str, text: &str| {
            TranscriptRecord::SegmentCommitted(TranscriptSegment {
                turn_id: turn_id.clone(),
                source_agent_id: source_agent_id.clone(),
                model_round,
                segment_index: 0,
                expected_transcript_revision: u64::from(model_round),
                messages: vec![SessionMessage {
                    is_meta: false,
                    references: Vec::new(),
                    message_id: message_id.to_owned(),
                    turn_id: Some(turn_id.clone()),
                    agent_id: Some(source_agent_id.clone()),
                    role: MessageRole::Assistant,
                    content: vec![MessagePart::Text {
                        text: text.to_owned(),
                    }],
                }],
            })
        };
        let mut state = SessionState::empty(SessionId::new("task-trajectory").unwrap());
        state.transcript = vec![
            TranscriptRecord::MessageAdded(message("user-1", MessageRole::User, "第一轮输入")),
            segment(1, "assistant-1", "第一轮输出"),
            segment(2, "assistant-2", "第二轮输出"),
        ];
        let round = keencode_resources::ModelRoundState {
            turn_id,
            source_agent_id,
            model_round: 2,
            requested_model: "test-model".to_owned(),
            metadata: Default::default(),
            usage: Default::default(),
            stop_reason: keencode_model::StopReason::Completed,
            completed_at_unix_ms: 3,
        };

        let messages = request_messages_for_round(&state, &round);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message_id, "user-1");
        assert_eq!(messages[1].message_id, "assistant-1");
    }
}
