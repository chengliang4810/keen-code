//! 窗口级 Task Controller 的本地 Rust 投影。
//!
//! Controller 不建立第二份任务表：列表、workspace facts 和动态帧都从 Runtime
//! SessionStore/Journal 即时读取。连接级 listener、订阅序号和取消句柄只属于
//! 当前 RPC 连接，重启后不会作为业务事实恢复。

use super::dispatch::{EventCallback, GatewayContext, RpcError, Subscription};
use super::session::{projection, task_extras};
use crate::agent_runtime::{AgentRuntime, AgentRuntimeError};
use crate::elicitation::ElicitationChange;
use crate::path_utils::path_to_frontend;
use crate::permissions::PermissionMode;
use crate::session_commands::{authorize_stored_root, open_authorized_session};
use keencode_resources::{SessionStatus, TitleSource, TurnStatus};
use keencode_runtime::{RuntimeEventPayload, RuntimeEventReceiveError, RuntimeEventSubscription};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::AppHandle;
use tokio::task::AbortHandle;

const CHANNEL: &str = "window-controller";
const DYNAMIC_EVENT: &str = "onDynamicControllerFrame";
const WORKSPACES_TOPIC: &str = "controller/workspaces";
const TASKS_TOPIC: &str = "controller/tasks-index";
const SNAPSHOT_PROTOCOL_VERSION: u64 = 1;
const MAX_TASK_LIST_LIMIT: usize = 2_000;
const MAX_PENDING_FRAMES: usize = 8;

/// 根装配层使用的窗口级 Controller 调用入口。
pub async fn call(ctx: GatewayContext, method: &str, args: Value) -> Result<Value, RpcError> {
    gateway().call(ctx, method, args).await
}

/// 根装配层使用的窗口级 Controller 事件入口。
pub fn listen(
    ctx: &GatewayContext,
    event: &str,
    args: Value,
    callback: EventCallback,
) -> Result<Subscription, RpcError> {
    gateway().listen(ctx, event, args, callback)
}

/// Transport 关闭连接时清理窗口 owner、listener 和 Runtime 事件订阅。
pub fn connection_closed(ctx: &GatewayContext) {
    gateway().connection_closed(&ctx.connection_id);
}

/// Transport 成功写入 subscribe ACK 后打开首帧 barrier。
///
/// ACK 前收到的 Runtime 事件仍保留在连接级订阅中；只有底层确认 response 已经
/// 发给前端，才能排放初始快照和之后排队的动态帧。
pub fn response_sent(
    ctx: &GatewayContext,
    channel: &str,
    method: &str,
    args: Value,
    response: Value,
) {
    if channel != CHANNEL || !matches!(method, "subscribe" | "subscribeControllerV4") {
        return;
    }
    gateway().response_sent(ctx, method, &args, &response);
}

/// 任务组织状态写入后通知当前窗口级 Controller 订阅者。
///
/// `zcode-task` 与 Controller mutation 共用同一份 task-groups 事实源；由 session
/// 层统一调用这里，避免 legacy 入口只更新磁盘而漏掉 pinned/timeline 的投影刷新。
pub(crate) fn emit_task_snapshots_for_app(app: &AppHandle) {
    gateway().emit_task_snapshots(app);
}

fn gateway() -> Arc<ControllerGateway> {
    static INSTANCE: OnceLock<Arc<ControllerGateway>> = OnceLock::new();
    Arc::clone(INSTANCE.get_or_init(|| Arc::new(ControllerGateway::new())))
}

struct ControllerGateway {
    inner: Arc<ControllerGatewayInner>,
}

struct ControllerGatewayInner {
    listeners: Mutex<HashMap<u64, ListenerBinding>>,
    subscriptions: Mutex<HashMap<String, ActiveSubscription>>,
    next_listener: AtomicU64,
    next_subscription: AtomicU64,
}

#[derive(Clone)]
struct ListenerBinding {
    connection_id: String,
    window_label: String,
    callback: EventCallback,
}

#[derive(Clone)]
struct ActiveSubscription {
    connection_id: String,
    window_label: String,
    topic: String,
    log_epoch: String,
    seq: u64,
    subscription_id: String,
    /// 首个快照在订阅 ACK 前暂存；真实动态事件随后排队，避免首帧越过订阅边界。
    initial_frame: Option<Value>,
    pending_frames: Vec<Value>,
    activated: bool,
    aborts: Vec<AbortHandle>,
    /// 同一订阅内串行化“分配序号→读取快照→排放”，避免 resync 与 watcher 反序发送。
    frame_gate: Arc<Mutex<()>>,
}

/// resync 使用的订阅所有权快照；把序号 gate 一起取出，避免检查 owner 后
/// 在读取快照前被另一条动态事件插队。
struct OwnedSubscription {
    topic: String,
    log_epoch: String,
    seq: u64,
    frame_gate: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceScope {
    workspace_path: String,
    #[serde(default)]
    workspace_identity: Option<String>,
    #[serde(default)]
    workspace_purpose: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskListQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    workspace_scopes: Vec<WorkspaceScope>,
    #[serde(default)]
    sort_by: Option<String>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Clone, Debug)]
struct NormalizedScope {
    path: String,
    identity: Option<String>,
}

#[derive(Clone, Debug)]
struct NormalizedQuery {
    kind: String,
    scopes: Vec<NormalizedScope>,
    /// 只读列表遇到未登记 workspace 时返回空集合，不能误当成全局查询。
    deny_all_scopes: bool,
    sort_by: String,
    search: Option<String>,
    limit: Option<usize>,
}

#[derive(Clone, Debug)]
struct TaskAddress {
    remote_session_id: Option<String>,
    workspace_path: String,
    workspace_identity: Option<String>,
    task_id: String,
}

#[derive(Clone, Debug)]
struct TaskRow {
    address: Value,
    meta: Value,
    membership: Value,
    source_availability: &'static str,
    live_status: &'static str,
    activity: Value,
    search_snippets: Option<Vec<String>>,
}

impl ControllerGateway {
    fn new() -> Self {
        Self {
            inner: Arc::new(ControllerGatewayInner {
                listeners: Mutex::new(HashMap::new()),
                subscriptions: Mutex::new(HashMap::new()),
                next_listener: AtomicU64::new(1),
                next_subscription: AtomicU64::new(1),
            }),
        }
    }

    async fn call(
        &self,
        ctx: GatewayContext,
        method: &str,
        args: Value,
    ) -> Result<Value, RpcError> {
        match method {
            "getState" => self.get_state(&ctx),
            "readGlobalPinned" => self.read_global_list(&ctx, args, "pinned"),
            "readGlobalTimeline" => self.read_global_list(&ctx, args, "timeline"),
            "listTaskList" | "listTasks" => self.list_task_list(&ctx, &args),
            "mutateTask" => self.mutate_task(&ctx, &args).await,
            "deleteArchivedTask" => self.delete_archived_task(&ctx, &args).await,
            "deleteArchivedTasks" => self.delete_archived_tasks(&ctx, &args).await,
            "subscribe" | "subscribeControllerV4" => self.subscribe(&ctx, &args),
            "resyncControllerV4" | "resync" => self.resync(&ctx, &args),
            "unsubscribeControllerV4" | "unsubscribe" => self.unsubscribe(&ctx, &args),
            _ => Err(unknown_method(method)),
        }
    }

    fn listen(
        &self,
        ctx: &GatewayContext,
        event: &str,
        _args: Value,
        callback: EventCallback,
    ) -> Result<Subscription, RpcError> {
        if event != DYNAMIC_EVENT {
            return Err(unknown_method(event));
        }
        let listener_id = self.inner.next_listener.fetch_add(1, Ordering::Relaxed);
        self.inner
            .listeners
            .lock()
            .map_err(|_| state_error())?
            .insert(
                listener_id,
                ListenerBinding {
                    connection_id: ctx.connection_id.clone(),
                    window_label: ctx.window_label.clone(),
                    callback,
                },
            );

        // 事件 listener 可能先于 subscribe 建立，也可能因为重连晚到；重新读取
        // 当前权威快照能让两种时序都得到冷恢复后的首帧。
        let subscriptions = self
            .inner
            .subscriptions
            .lock()
            .map_err(|_| state_error())?
            .values()
            .filter(|subscription| {
                subscription.connection_id == ctx.connection_id
                    && subscription.window_label == ctx.window_label
                    && subscription.activated
            })
            .map(|subscription| subscription.subscription_id.clone())
            .collect::<Vec<_>>();
        for subscription_id in subscriptions {
            self.emit_snapshot(&ctx.app, &subscription_id);
        }

        let inner = Arc::clone(&self.inner);
        Ok(Subscription::new(move || {
            if let Ok(mut listeners) = inner.listeners.lock() {
                listeners.remove(&listener_id);
            }
        }))
    }

    fn connection_closed(&self, connection_id: &str) {
        if let Ok(mut listeners) = self.inner.listeners.lock() {
            listeners.retain(|_, listener| listener.connection_id != connection_id);
        }
        let ids = self
            .inner
            .subscriptions
            .lock()
            .ok()
            .map(|subscriptions| {
                subscriptions
                    .iter()
                    .filter(|(_, subscription)| subscription.connection_id == connection_id)
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for id in ids {
            self.remove_subscription(&id);
        }
    }

    fn get_state(&self, ctx: &GatewayContext) -> Result<Value, RpcError> {
        let runtime = owned_runtime(&ctx.app)?;
        let workspaces = self.workspace_snapshot(&runtime, &ctx.app)?;
        let tasks = self.tasks_snapshot(&runtime, &ctx.app, &ctx.connection_id)?;
        Ok(json!({
            "protocolVersion": SNAPSHOT_PROTOCOL_VERSION,
            "logEpoch": controller_log_epoch(&runtime),
            "workspaces": workspaces.get("workspaces").cloned().unwrap_or_else(|| json!([])),
            "tasks": tasks.get("tasks").cloned().unwrap_or_else(|| json!([])),
        }))
    }

    fn read_global_list(
        &self,
        ctx: &GatewayContext,
        args: Value,
        kind: &str,
    ) -> Result<Value, RpcError> {
        let mut object = args
            .as_object()
            .cloned()
            .ok_or_else(|| invalid_params(format!("{kind} 参数必须为对象")))?;
        object.insert("kind".to_owned(), Value::String(kind.to_owned()));
        object
            .entry("sortBy".to_owned())
            .or_insert_with(|| Value::String("updated".to_owned()));
        self.list_task_list(ctx, &Value::Object(object))
    }

    fn list_task_list(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let query = authorize_query_scopes(&ctx.app, parse_query(args)?)?;
        let runtime = owned_runtime(&ctx.app)?;
        let rows = self.collect_rows(&runtime, &ctx.app, &ctx.connection_id, &query)?;
        let total = rows.len();
        let limit = query.limit.unwrap_or(total).min(MAX_TASK_LIST_LIMIT);
        let has_more = total > limit;
        let items = rows
            .into_iter()
            .take(limit)
            .map(TaskRow::list_item)
            .collect::<Vec<_>>();
        Ok(json!({"items": items, "total": total, "hasMore": has_more}))
    }

    fn subscribe(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        let topic = parse_subscribe_params(args)?;
        if topic != WORKSPACES_TOPIC && topic != TASKS_TOPIC {
            return Err(invalid_params("Controller topic 不受支持"));
        }
        let runtime = owned_runtime(&ctx.app)?;
        let log_epoch = controller_log_epoch(&runtime);
        let subscription_id = format!(
            "controller-sub-{}-{}",
            ctx.connection_id,
            self.inner.next_subscription.fetch_add(1, Ordering::Relaxed)
        );
        // Controller 不持有第二份 delta 日志；即使客户端带有同一 epoch 的 base，
        // 也必须返回真实 Runtime/Journal 快照，不能把“resume”当作没有重放事实的假承诺。
        let mode = "snapshot";
        let active = ActiveSubscription {
            connection_id: ctx.connection_id.clone(),
            window_label: ctx.window_label.clone(),
            topic: topic.to_owned(),
            log_epoch: log_epoch.clone(),
            seq: 0,
            subscription_id: subscription_id.clone(),
            initial_frame: None,
            pending_frames: Vec::new(),
            activated: false,
            aborts: Vec::new(),
            frame_gate: Arc::new(Mutex::new(())),
        };
        self.inner
            .subscriptions
            .lock()
            .map_err(|_| state_error())?
            .insert(subscription_id.clone(), active);

        if let Err(error) = self.start_watchers(ctx, &runtime, &subscription_id) {
            self.remove_subscription(&subscription_id);
            return Err(error);
        }
        let initial = match self.build_frame_snapshot(ctx, &runtime, &subscription_id, 0) {
            Ok(initial) => initial,
            Err(error) => {
                self.remove_subscription(&subscription_id);
                return Err(error);
            }
        };
        if let Err(error) = self.set_initial_frame(&subscription_id, initial) {
            self.remove_subscription(&subscription_id);
            return Err(error);
        }
        Ok(json!({
            "ack": {"subscriptionId": subscription_id, "mode": mode, "logEpoch": log_epoch},
        }))
    }

    fn resync(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        validate_resync_params(args)?;
        let subscription_id = required_string(args, "subscriptionId")?;
        let OwnedSubscription {
            topic,
            log_epoch,
            seq: current_seq,
            frame_gate,
        } = self.owned_subscription(ctx, &subscription_id)?;
        let force = args
            .get("forceSnapshot")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let base_matches = args
            .get("base")
            .and_then(|base| base.get("logEpoch"))
            .and_then(Value::as_str)
            .is_some_and(|epoch| epoch == log_epoch)
            && args
                .pointer("/base/seq")
                .and_then(Value::as_u64)
                .is_some_and(|seq| seq == current_seq);
        if !force && base_matches {
            return Ok(json!({
                "ack": {"subscriptionId": subscription_id, "mode": "resume", "logEpoch": log_epoch},
            }));
        }
        let runtime = owned_runtime(&ctx.app)?;
        let _frame_guard = frame_gate.lock().map_err(|_| state_error())?;
        let next_seq = self.reserve_next_seq(&subscription_id)?;
        let frame = self.build_frame_snapshot_for_topic(
            &ctx.app,
            &ctx.connection_id,
            &runtime,
            &subscription_id,
            &topic,
            next_seq,
        )?;
        self.queue_or_emit_frame(&subscription_id, frame);
        Ok(json!({
            "ack": {"subscriptionId": subscription_id, "mode": "snapshot", "logEpoch": log_epoch},
        }))
    }

    fn unsubscribe(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        reject_unknown_keys(args, &["subscriptionId"], "unsubscribeControllerV4")?;
        let subscription_id = required_string(args, "subscriptionId")?;
        let _ = self.owned_subscription(ctx, &subscription_id)?;
        self.remove_subscription(&subscription_id);
        Ok(Value::Null)
    }

    async fn mutate_task(&self, ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
        reject_unknown_keys(
            args,
            &["address", "mutation", "operationId", "commandId"],
            "mutateTask",
        )?;
        let address = parse_address(args.get("address").unwrap_or(args))?;
        let mutation = args
            .get("mutation")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid_params("mutateTask.mutation 必须为对象"))?;
        let kind = required_string(&Value::Object(mutation.clone()), "kind")?;
        let allowed_mutation_fields: &[&str] = match kind.as_str() {
            "pin" => &["kind", "pinned"],
            "archive" => &["kind", "archived"],
            "mark-read" => &["kind", "expectedUnreadAt"],
            "delete" | "delete-archived" | "mark-unread" | "open" | "resume" => &["kind"],
            _ => return Err(invalid_params("未知 Controller task mutation")),
        };
        reject_unknown_keys(
            &Value::Object(mutation.clone()),
            allowed_mutation_fields,
            "mutateTask.mutation",
        )?;
        let runtime = owned_runtime(&ctx.app)?;
        let session = self.authorized_session_for_address(&runtime, &ctx.app, &address)?;
        let operation_id = args
            .get("operationId")
            .or_else(|| args.get("commandId"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "controller-{:016x}-{:016x}-{:016x}-{}",
                    stable_hash(&ctx.connection_id),
                    stable_hash(&address.task_id),
                    stable_hash(&kind),
                    mutation
                        .get("pinned")
                        .or_else(|| mutation.get("archived"))
                        .and_then(Value::as_bool)
                        .map(|value| if value { "1" } else { "0" })
                        .unwrap_or("0")
                )
            });
        match kind.as_str() {
            "pin" => {
                let pinned = mutation
                    .get("pinned")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid_params("pin mutation 缺少 pinned"))?;
                session
                    .set_preference(&operation_id, Some(pinned), None)
                    .map_err(backend_error)?;
            }
            "archive" => {
                let archived = mutation
                    .get("archived")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid_params("archive mutation 缺少 archived"))?;
                session
                    .set_preference(&operation_id, None, Some(archived))
                    .map_err(backend_error)?;
            }
            "open" => {}
            "resume" => {
                session
                    .set_preference(&operation_id, None, Some(false))
                    .map_err(backend_error)?;
            }
            "delete" | "delete-archived" => {
                if kind == "delete-archived"
                    && !session.snapshot().map_err(backend_error)?.state.archived
                {
                    return Err(RpcError::new(
                        "controller.taskNotArchived",
                        "只能删除已归档任务",
                    ));
                }
                let method = if kind == "delete-archived" {
                    "deleteArchivedTask"
                } else {
                    "deleteTask"
                };
                let mut request = json!({
                    "taskId": address.task_id,
                    "workspacePath": address.workspace_path,
                });
                if let Some(identity) = address.workspace_identity {
                    request["workspaceIdentity"] = Value::String(identity);
                }
                super::session::call_session(ctx.clone(), "zcode-task", method, request).await?;
                return Ok(Value::Null);
            }
            "mark-read" | "mark-unread" => {
                let mut request = json!({
                    "taskId": address.task_id,
                    "workspacePath": address.workspace_path,
                    "unread": kind == "mark-unread",
                });
                if let Some(identity) = address.workspace_identity {
                    request["workspaceIdentity"] = Value::String(identity);
                }
                if kind == "mark-read"
                    && let Some(expected_unread_at) = mutation.get("expectedUnreadAt")
                {
                    request["expectedUnreadAt"] = expected_unread_at.clone();
                }
                // Controller 与 zcode-task 共用 task-groups.json；不能在 Controller
                // 侧另建未读缓存，否则窗口间事件和重启恢复会产生不同事实。
                // set_task_unread 提交后统一广播 Controller snapshot 与 workspace event，
                // 这里不再重复发送，避免同一 CAS mutation 产生两帧。
                let meta = task_extras::set_task_unread(ctx, &request)?;
                return Ok(meta);
            }
            _ => return Err(invalid_params("未知 Controller task mutation")),
        }
        let unread_at = task_extras::unread_at_for_task(
            &ctx.app,
            &address.workspace_path,
            address.workspace_identity.as_deref(),
            address.task_id.as_str(),
        )?;
        let row = self.row_for_session(&runtime, &ctx.connection_id, &session, unread_at)?;
        Ok(row.meta)
    }

    async fn delete_archived_task(
        &self,
        ctx: &GatewayContext,
        args: &Value,
    ) -> Result<Value, RpcError> {
        if args.get("address").is_some() {
            reject_unknown_keys(args, &["address"], "deleteArchivedTask")?;
        }
        let address = parse_address(args.get("address").unwrap_or(args))?;
        let runtime = owned_runtime(&ctx.app)?;
        let metadata = match runtime
            .runtime_manager()
            .stored_session_metadata(&address.task_id)
        {
            Ok(metadata) => metadata,
            Err(
                keencode_runtime::RuntimeError::SessionNotCreated
                | keencode_runtime::RuntimeError::SessionNotRegistered
                | keencode_runtime::RuntimeError::SessionCorrupt,
            ) => return Ok(Value::Bool(false)),
            Err(error) => return Err(backend_error(error)),
        };
        if metadata.corrupt || !metadata.archived {
            return Ok(Value::Bool(false));
        }
        let session = self.authorized_session_for_address(&runtime, &ctx.app, &address)?;
        if !session.snapshot().map_err(backend_error)?.state.archived {
            return Ok(Value::Bool(false));
        }
        let mut request = json!({
            "taskId": address.task_id,
            "workspacePath": address.workspace_path,
        });
        if let Some(identity) = address.workspace_identity {
            request["workspaceIdentity"] = Value::String(identity);
        }
        super::session::call_session(ctx.clone(), "zcode-task", "deleteArchivedTask", request).await
    }

    async fn delete_archived_tasks(
        &self,
        ctx: &GatewayContext,
        args: &Value,
    ) -> Result<Value, RpcError> {
        reject_unknown_keys(args, &["address", "taskIds"], "deleteArchivedTasks")?;
        let address = parse_address(args.get("address").unwrap_or(args))?;
        if address.remote_session_id.is_some() {
            return Err(RpcError::new(
                "controller.remoteMutationUnsupported",
                "本地 Controller 不能删除远程任务",
            ));
        }
        let ids = args
            .get("taskIds")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_params("deleteArchivedTasks.taskIds 必须为数组"))?;
        let mut request = json!({
            "workspacePath": address.workspace_path,
            "taskIds": ids,
        });
        if let Some(identity) = address.workspace_identity {
            request["workspaceIdentity"] = Value::String(identity);
        }
        super::session::call_session(ctx.clone(), "zcode-task", "deleteArchivedTasks", request)
            .await
    }

    fn authorized_session_for_address(
        &self,
        runtime: &Arc<AgentRuntime>,
        app: &AppHandle,
        address: &TaskAddress,
    ) -> Result<keencode_runtime::RuntimeSession, RpcError> {
        if address.remote_session_id.is_some() {
            return Err(RpcError::new(
                "controller.remoteMutationUnsupported",
                "本地 Controller 不能修改远程任务",
            ));
        }
        let root = authorize_stored_root(app, &address.workspace_path).map_err(backend_error)?;
        let session =
            open_authorized_session(runtime, app, &address.task_id).map_err(backend_error)?;
        if session.is_workflow_actor().map_err(backend_error)? {
            return Err(RpcError::new(
                "controller.workflowActorUnsupported",
                "工作流 actor 不属于窗口任务 Controller",
            ));
        }
        let state = session.snapshot().map_err(backend_error)?.state;
        if state.project_root != root.to_string_lossy() {
            return Err(RpcError::new(
                "controller.taskAddressMismatch",
                "任务地址不属于请求的 workspace",
            ));
        }
        if address.workspace_identity.is_some() {
            return Err(RpcError::new(
                "controller.remoteTaskUnsupported",
                "本地 Session 没有 workspaceIdentity",
            ));
        }
        Ok(session)
    }

    fn collect_rows(
        &self,
        runtime: &Arc<AgentRuntime>,
        app: &AppHandle,
        connection_id: &str,
        query: &NormalizedQuery,
    ) -> Result<Vec<TaskRow>, RpcError> {
        if query.deny_all_scopes {
            return Ok(Vec::new());
        }
        let metadata = runtime.stored_sessions().map_err(backend_error)?;
        let unread_index = task_extras::unread_at_index(app)?;
        let connection_id = keencode_acp::ConnectionId::new(connection_id.to_owned())
            .map_err(|error| backend_error(error.to_string()))?;
        let mut rows = Vec::new();
        let mut deleted_by_root = HashMap::<String, HashSet<String>>::new();
        for item in metadata {
            if item.corrupt {
                continue;
            }
            let root = match authorize_stored_root(app, &item.project_root) {
                Ok(root) => root,
                Err(_) => continue,
            };
            let root_text = root.to_string_lossy().into_owned();
            if !deleted_by_root.contains_key(&root_text) {
                let deleted = keencode_resources::list_deleted_session_ids(
                    runtime.storage_root(),
                    &root_text,
                )
                .map_err(backend_error)?
                .into_iter()
                .map(|id| id.as_str().to_owned())
                .collect();
                deleted_by_root.insert(root_text.clone(), deleted);
            }
            if deleted_by_root
                .get(&root_text)
                .is_some_and(|deleted| deleted.contains(item.session_id.as_str()))
            {
                continue;
            }
            if !query.scopes.is_empty()
                && !query
                    .scopes
                    .iter()
                    .any(|scope| scope.identity.is_none() && scope.path == root_text)
            {
                continue;
            }
            let session = open_authorized_session(runtime, app, item.session_id.as_str())
                .map_err(backend_error)?;
            if session.is_workflow_actor().map_err(backend_error)? {
                continue;
            }
            let state = session.snapshot().map_err(backend_error)?.state;
            // Controller 的 tasks-index 是持久侧栏行集合；只有 Journal 已产生
            // 用户输入/Turn 等 promotion 事实才允许进入，纯 SessionCreated 预热
            // draft 必须留给 sessions-index 的 detail 投影，不能生成假任务行。
            if !projection::is_promoted_task_state(&state) {
                continue;
            }
            let unread_at = unread_index
                .get(&root_text)
                .and_then(|tasks| tasks.get(item.session_id.as_str()))
                .copied();
            let row =
                self.row_for_session(runtime, &connection_id.to_string(), &session, unread_at)?;
            if !matches_kind(&row.membership, &query.kind) {
                continue;
            }
            if let Some(search) = query.search.as_deref()
                && !row_matches_search(&row, search)
            {
                continue;
            }
            rows.push(row);
        }
        rows.sort_by(|left, right| {
            let left_meta = &left.meta;
            let right_meta = &right.meta;
            let field = if query.sort_by == "created" {
                "createdAt"
            } else {
                "updatedAt"
            };
            right_meta
                .get(field)
                .and_then(Value::as_u64)
                .cmp(&left_meta.get(field).and_then(Value::as_u64))
                .then_with(|| {
                    left_meta
                        .get("taskId")
                        .and_then(Value::as_str)
                        .cmp(&right_meta.get("taskId").and_then(Value::as_str))
                })
        });
        Ok(rows)
    }

    fn row_for_session(
        &self,
        runtime: &AgentRuntime,
        connection_id: &str,
        session: &keencode_runtime::RuntimeSession,
        unread_at: Option<u64>,
    ) -> Result<TaskRow, RpcError> {
        let snapshot = session.snapshot().map_err(backend_error)?;
        let state = snapshot.state;
        let active = runtime
            .session_has_active_work(session.session_id().as_str())
            .map_err(backend_error)?;
        let connection = keencode_acp::ConnectionId::new(connection_id.to_owned())
            .map_err(|error| backend_error(error.to_string()))?;
        let pending =
            match runtime.pending_elicitation_views(session.session_id().as_str(), &connection) {
                Ok(pending) => pending,
                // 其他连接的 pending 询问不可投影；这是权限过滤，不是状态读取失败。
                Err(AgentRuntimeError::ClientResponseRejected) => Vec::new(),
                Err(error) => return Err(backend_error(error)),
            };
        let pending_permissions =
            match runtime.pending_permission_views(session.session_id().as_str(), &connection) {
                Ok(pending) => pending.len(),
                // 其他连接的权限 pending 不可投影；不能把跨窗口审批状态泄露到列表。
                Err(AgentRuntimeError::ClientResponseRejected) => 0,
                Err(error) => return Err(backend_error(error)),
            };
        let live_status = live_status(
            &state,
            active,
            !pending.is_empty() || pending_permissions > 0,
        );
        let phase = phase_for_state(&state, active);
        let provider = state.provider.as_ref();
        let permission_mode = runtime
            .permission_mode(session.session_id().as_str())
            .map_err(backend_error)?;
        let mode = if state.plan.enabled {
            "plan"
        } else {
            match permission_mode {
                PermissionMode::Build => "build",
                PermissionMode::Edit => "edit",
                PermissionMode::Plan => "plan",
                PermissionMode::Yolo => "yolo",
            }
        };
        // Journal 保留 Windows extended-length 路径，前端地址必须与项目登记表
        // 的 C:/ 或 UNC 形式一致，否则 controllerStore 无法按 workspace join。
        let workspace_path = path_to_frontend(std::path::Path::new(&state.project_root));
        let mut meta = json!({
            "taskId": session.session_id().as_str(),
            "traceId": format!("session:{}", session.session_id()),
            "title": state.title,
            "workspacePath": workspace_path.clone(),
            "createdAt": state.created_at_unix_ms,
            "updatedAt": state.updated_at_unix_ms,
            "mode": mode,
            "provider": "glm",
        });
        if let Some(unread_at) = unread_at {
            meta["unreadAt"] = Value::Number(unread_at.into());
        }
        if state.title_source == TitleSource::Manual {
            meta["titleOverridden"] = Value::Bool(true);
        }
        if let Some(provider) = provider {
            meta["model"] = Value::String(provider.model.clone());
            if let Some(effort) = provider.reasoning_effort
                && let Ok(value) = serde_json::to_value(effort)
                && let Some(value) = value.as_str()
            {
                meta["thoughtLevel"] = Value::String(value.to_owned());
            }
        }
        if !state.turns.is_empty() {
            meta["status"] = Value::String(match live_status {
                "running" => "running".to_owned(),
                "error" => "error".to_owned(),
                _ => "completed".to_owned(),
            });
        }
        let address = json!({
            "workspacePath": workspace_path,
            "taskId": session.session_id().as_str(),
        });
        let activity = json!({
            "phase": phase,
            "lastActivityAt": state.updated_at_unix_ms,
            "hasBackgroundWork": active,
            "pendingInteractions": {
                "permissionCount": pending_permissions,
                "userInputCount": pending.len(),
            },
        });
        Ok(TaskRow {
            address,
            meta,
            membership: json!({
                "pinned": state.pinned,
                "archived": state.archived,
                "active": !state.archived,
            }),
            source_availability: "online",
            live_status,
            activity,
            search_snippets: None,
        })
    }

    fn workspace_snapshot(
        &self,
        runtime: &Arc<AgentRuntime>,
        app: &AppHandle,
    ) -> Result<Value, RpcError> {
        let metadata = runtime.stored_sessions().map_err(backend_error)?;
        let mut workspaces = BTreeMap::<String, Value>::new();
        for item in metadata {
            if item.corrupt {
                continue;
            }
            let Ok(root) = authorize_stored_root(app, &item.project_root) else {
                continue;
            };
            // Controller 对外统一使用前端路径格式；授权和索引 key 仍使用 root。
            let path = path_to_frontend(&root);
            workspaces.entry(path.clone()).or_insert_with(|| {
                json!({
                    "workspacePath": path,
                    "sourceAvailability": "online",
                    "connectionState": "online",
                })
            });
        }
        Ok(json!({
            "protocolVersion": SNAPSHOT_PROTOCOL_VERSION,
            "logEpoch": controller_log_epoch(runtime),
            "workspaces": workspaces.into_values().collect::<Vec<_>>(),
        }))
    }

    fn tasks_snapshot(
        &self,
        runtime: &Arc<AgentRuntime>,
        app: &AppHandle,
        connection_id: &str,
    ) -> Result<Value, RpcError> {
        let query = NormalizedQuery {
            // V4 任务索引缓存需要看到归档和置顶 membership，前端再按查询类型筛选。
            kind: "all".to_owned(),
            scopes: Vec::new(),
            deny_all_scopes: false,
            sort_by: "updated".to_owned(),
            search: None,
            limit: None,
        };
        let rows = self.collect_rows(runtime, app, connection_id, &query)?;
        Ok(json!({
            "protocolVersion": SNAPSHOT_PROTOCOL_VERSION,
            "logEpoch": controller_log_epoch(runtime),
            "tasks": rows.into_iter().map(TaskRow::into_value).collect::<Vec<_>>(),
        }))
    }

    fn build_frame_snapshot(
        &self,
        ctx: &GatewayContext,
        runtime: &Arc<AgentRuntime>,
        subscription_id: &str,
        seq: u64,
    ) -> Result<Value, RpcError> {
        let topic = self
            .inner
            .subscriptions
            .lock()
            .map_err(|_| state_error())?
            .get(subscription_id)
            .map(|subscription| subscription.topic.clone())
            .ok_or_else(|| RpcError::new("controller.subscriptionNotFound", "订阅不存在"))?;
        self.build_frame_snapshot_for_topic(
            &ctx.app,
            &ctx.connection_id,
            runtime,
            subscription_id,
            &topic,
            seq,
        )
    }

    fn build_frame_snapshot_for_topic(
        &self,
        app: &AppHandle,
        connection_id: &str,
        runtime: &Arc<AgentRuntime>,
        subscription_id: &str,
        topic: &str,
        seq: u64,
    ) -> Result<Value, RpcError> {
        let log_epoch = controller_log_epoch(runtime);
        let snapshot = if topic == WORKSPACES_TOPIC {
            self.workspace_snapshot(runtime, app)?
        } else {
            self.tasks_snapshot(runtime, app, connection_id)?
        };
        let payload = json!({"kind": "snapshot", "snapshot": snapshot});
        Ok(json!({
            "topic": topic,
            "subscriptionId": subscription_id,
            "logEpoch": log_epoch,
            "fromSeq": 0,
            "toSeq": seq,
            "sentAt": now_epoch_ms(),
            "payload": payload,
        }))
    }

    fn start_watchers(
        &self,
        ctx: &GatewayContext,
        runtime: &Arc<AgentRuntime>,
        subscription_id: &str,
    ) -> Result<(), RpcError> {
        let metadata = runtime.stored_sessions().map_err(backend_error)?;
        let mut aborts = Vec::new();
        for item in metadata {
            if item.corrupt || authorize_stored_root(&ctx.app, &item.project_root).is_err() {
                continue;
            }
            let session = match open_authorized_session(runtime, &ctx.app, item.session_id.as_str())
            {
                Ok(session) => session,
                Err(_) => continue,
            };
            if session.is_workflow_actor().map_err(backend_error)? {
                continue;
            }
            let events = session.subscribe().map_err(backend_error)?;
            aborts.push(self.spawn_runtime_watcher(
                ctx.app.clone(),
                Arc::clone(runtime),
                subscription_id.to_owned(),
                events,
            ));
        }
        aborts.push(self.spawn_elicitation_watcher(
            ctx.app.clone(),
            Arc::clone(runtime),
            subscription_id.to_owned(),
        ));
        aborts.push(self.spawn_permission_watcher(
            ctx.app.clone(),
            Arc::clone(runtime),
            subscription_id.to_owned(),
        ));
        aborts.push(self.spawn_completion_watcher(
            ctx.app.clone(),
            Arc::clone(runtime),
            subscription_id.to_owned(),
        ));
        let mut subscriptions = self.inner.subscriptions.lock().map_err(|_| state_error())?;
        let subscription = subscriptions
            .get_mut(subscription_id)
            .ok_or_else(|| RpcError::new("controller.subscriptionNotFound", "订阅不存在"))?;
        subscription.aborts.extend(aborts);
        Ok(())
    }

    fn spawn_runtime_watcher(
        &self,
        app: AppHandle,
        _runtime: Arc<AgentRuntime>,
        subscription_id: String,
        mut events: RuntimeEventSubscription,
    ) -> AbortHandle {
        let inner = Arc::clone(&self.inner);
        let task = tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(delivery) => {
                        if !matches!(delivery.payload, RuntimeEventPayload::Transient(_)) {
                            let gateway = ControllerGateway {
                                inner: Arc::clone(&inner),
                            };
                            gateway.emit_snapshot(&app, &subscription_id);
                        }
                    }
                    Err(RuntimeEventReceiveError::Lagged(_)) => {
                        let gateway = ControllerGateway {
                            inner: Arc::clone(&inner),
                        };
                        gateway.emit_snapshot(&app, &subscription_id);
                    }
                    Err(RuntimeEventReceiveError::Closed) => break,
                }
            }
        });
        task.abort_handle()
    }

    fn spawn_elicitation_watcher(
        &self,
        app: AppHandle,
        runtime: Arc<AgentRuntime>,
        subscription_id: String,
    ) -> AbortHandle {
        let inner = Arc::clone(&self.inner);
        let mut changes = runtime.elicitation_coordinator().subscribe_changes();
        let task = tokio::spawn(async move {
            while let Ok(ElicitationChange { .. }) = changes.recv().await {
                let gateway = ControllerGateway {
                    inner: Arc::clone(&inner),
                };
                gateway.emit_snapshot(&app, &subscription_id);
            }
        });
        task.abort_handle()
    }

    fn spawn_completion_watcher(
        &self,
        app: AppHandle,
        runtime: Arc<AgentRuntime>,
        subscription_id: String,
    ) -> AbortHandle {
        let inner = Arc::clone(&self.inner);
        let mut completions = runtime.subscribe_task_completions();
        let task = tokio::spawn(async move {
            while completions.recv().await.is_ok() {
                let gateway = ControllerGateway {
                    inner: Arc::clone(&inner),
                };
                gateway.emit_snapshot(&app, &subscription_id);
            }
        });
        task.abort_handle()
    }

    fn spawn_permission_watcher(
        &self,
        app: AppHandle,
        runtime: Arc<AgentRuntime>,
        subscription_id: String,
    ) -> AbortHandle {
        let inner = Arc::clone(&self.inner);
        let mut changes = runtime.subscribe_permission_changes();
        let task = tokio::spawn(async move {
            while changes.recv().await.is_ok() {
                let gateway = ControllerGateway {
                    inner: Arc::clone(&inner),
                };
                gateway.emit_snapshot(&app, &subscription_id);
            }
        });
        task.abort_handle()
    }

    fn emit_task_snapshots(&self, app: &AppHandle) {
        let subscription_ids = self
            .inner
            .subscriptions
            .lock()
            .ok()
            .map(|subscriptions| {
                subscriptions
                    .values()
                    .filter(|subscription| subscription.topic == TASKS_TOPIC)
                    .map(|subscription| subscription.subscription_id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for subscription_id in subscription_ids {
            self.emit_snapshot(app, &subscription_id);
        }
    }

    fn emit_snapshot(&self, app: &AppHandle, subscription_id: &str) {
        let Some(subscription) = self
            .inner
            .subscriptions
            .lock()
            .ok()
            .and_then(|subscriptions| subscriptions.get(subscription_id).cloned())
        else {
            return;
        };
        let Ok(_frame_guard) = subscription.frame_gate.lock() else {
            tracing::warn!(subscription_id, "Controller 动态帧 gate 不可用");
            return;
        };
        let runtime = match owned_runtime(app) {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(subscription_id, error = %error, "Controller 动态帧读取 Runtime 失败");
                return;
            }
        };
        let next_seq = match self.reserve_next_seq(subscription_id) {
            Ok(next_seq) => next_seq,
            Err(error) => {
                tracing::warn!(subscription_id, error = %error, "Controller 动态帧序号分配失败");
                return;
            }
        };
        let frame = match self.build_frame_snapshot_for_topic(
            app,
            &subscription.connection_id,
            &runtime,
            subscription_id,
            &subscription.topic,
            next_seq,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(subscription_id, error = %error, "Controller 动态帧投影失败");
                return;
            }
        };
        self.queue_or_emit_frame(subscription_id, frame);
    }

    fn response_sent(&self, ctx: &GatewayContext, method: &str, args: &Value, response: &Value) {
        let Some(subscription_id) = self.subscription_id_for_response(
            &ctx.connection_id,
            &ctx.window_label,
            method,
            args,
            response,
        ) else {
            return;
        };
        self.activate_subscription(&subscription_id);
    }

    fn set_initial_frame(&self, subscription_id: &str, initial: Value) -> Result<(), RpcError> {
        let mut subscriptions = self.inner.subscriptions.lock().map_err(|_| state_error())?;
        let subscription = subscriptions
            .get_mut(subscription_id)
            .ok_or_else(|| RpcError::new("controller.subscriptionNotFound", "订阅不存在"))?;
        subscription.initial_frame = Some(initial);
        Ok(())
    }

    fn subscription_id_for_response(
        &self,
        connection_id: &str,
        window_label: &str,
        method: &str,
        args: &Value,
        response: &Value,
    ) -> Option<String> {
        let topic = args.get("topic").and_then(Value::as_str)?;
        let subscription_id = response
            .pointer("/ack/subscriptionId")
            .and_then(Value::as_str)
            .or_else(|| response.get("subscriptionId").and_then(Value::as_str))?;
        let subscriptions = self.inner.subscriptions.lock().ok()?;
        subscriptions.get(subscription_id).and_then(|subscription| {
            (subscription.connection_id == connection_id
                && subscription.window_label == window_label
                && !subscription.activated
                && subscription.topic == topic
                && matches!(method, "subscribe" | "subscribeControllerV4"))
            .then(|| subscription_id.to_owned())
        })
    }

    fn activate_subscription(&self, subscription_id: &str) {
        let (connection_id, window_label, mut frames) = {
            let Ok(mut subscriptions) = self.inner.subscriptions.lock() else {
                return;
            };
            let Some(subscription) = subscriptions.get_mut(subscription_id) else {
                return;
            };
            if subscription.activated {
                return;
            }
            subscription.activated = true;
            let mut frames = Vec::with_capacity(1 + subscription.pending_frames.len());
            if let Some(frame) = subscription.initial_frame.take() {
                frames.push(frame);
            }
            frames.append(&mut subscription.pending_frames);
            (
                subscription.connection_id.clone(),
                subscription.window_label.clone(),
                frames,
            )
        };
        if frames.len() > MAX_PENDING_FRAMES + 1 {
            frames.drain(1..frames.len() - 1);
        }
        self.emit_frames(&connection_id, &window_label, &mut frames);
    }

    fn queue_or_emit_frame(&self, subscription_id: &str, frame: Value) {
        let Some((connection_id, window_label, frame)) = (|| {
            let mut subscriptions = self.inner.subscriptions.lock().ok()?;
            let subscription = subscriptions.get_mut(subscription_id)?;
            if !subscription.activated {
                subscription.pending_frames.clear();
                subscription.pending_frames.push(frame);
                return None;
            }
            Some((
                subscription.connection_id.clone(),
                subscription.window_label.clone(),
                frame,
            ))
        })() else {
            return;
        };
        let mut frames = vec![frame];
        self.emit_frames(&connection_id, &window_label, &mut frames);
    }

    fn emit_frames(&self, connection_id: &str, window_label: &str, frames: &mut Vec<Value>) {
        let listeners = self
            .inner
            .listeners
            .lock()
            .ok()
            .map(|listeners| listeners.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for frame in frames.drain(..) {
            for listener in &listeners {
                if listener.connection_id == connection_id && listener.window_label == window_label
                {
                    let _ = (listener.callback)(frame.clone());
                }
            }
        }
    }

    fn owned_subscription(
        &self,
        ctx: &GatewayContext,
        subscription_id: &str,
    ) -> Result<OwnedSubscription, RpcError> {
        let subscriptions = self.inner.subscriptions.lock().map_err(|_| state_error())?;
        let subscription = subscriptions
            .get(subscription_id)
            .ok_or_else(|| RpcError::new("controller.subscriptionNotFound", "订阅不存在"))?;
        if subscription.connection_id != ctx.connection_id
            || subscription.window_label != ctx.window_label
        {
            return Err(RpcError::new(
                "controller.subscriptionOwnerMismatch",
                "订阅不属于当前窗口连接",
            ));
        }
        Ok(OwnedSubscription {
            topic: subscription.topic.clone(),
            log_epoch: subscription.log_epoch.clone(),
            seq: subscription.seq,
            frame_gate: Arc::clone(&subscription.frame_gate),
        })
    }

    fn reserve_next_seq(&self, subscription_id: &str) -> Result<u64, RpcError> {
        let mut subscriptions = self.inner.subscriptions.lock().map_err(|_| state_error())?;
        let subscription = subscriptions
            .get_mut(subscription_id)
            .ok_or_else(|| RpcError::new("controller.subscriptionNotFound", "订阅不存在"))?;
        subscription.seq = subscription.seq.saturating_add(1);
        Ok(subscription.seq)
    }

    fn remove_subscription(&self, subscription_id: &str) {
        if let Ok(mut subscriptions) = self.inner.subscriptions.lock()
            && let Some(subscription) = subscriptions.remove(subscription_id)
        {
            for abort in subscription.aborts {
                abort.abort();
            }
        }
    }
}

fn parse_query(args: &Value) -> Result<NormalizedQuery, RpcError> {
    let raw: TaskListQuery = serde_json::from_value(args.clone())
        .map_err(|error| invalid_params(format!("listTaskList 参数无效: {error}")))?;
    let kind = raw.kind.unwrap_or_else(|| "timeline".to_owned());
    if !matches!(kind.as_str(), "pinned" | "archived" | "timeline" | "active") {
        return Err(invalid_params(
            "kind 必须为 pinned/archived/timeline/active",
        ));
    }
    let sort_by = raw.sort_by.unwrap_or_else(|| "updated".to_owned());
    if !matches!(sort_by.as_str(), "created" | "updated") {
        return Err(invalid_params("sortBy 必须为 created 或 updated"));
    }
    let search = raw
        .search
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_lowercase());
    if raw.limit == Some(0) {
        return Err(invalid_params("limit 必须为正整数"));
    }
    let limit = raw.limit.map(|limit| limit.min(MAX_TASK_LIST_LIMIT));
    // 这里仅解析结构，不接触 Runtime；workspace authorization 在 collect 前完成。
    let scopes = raw
        .workspace_scopes
        .into_iter()
        .map(|scope| {
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
            Ok(NormalizedScope {
                path: scope.workspace_path,
                identity: scope.workspace_identity,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(NormalizedQuery {
        kind,
        scopes,
        deny_all_scopes: false,
        sort_by,
        search,
        limit,
    })
}

fn authorize_query_scopes(
    app: &AppHandle,
    mut query: NormalizedQuery,
) -> Result<NormalizedQuery, RpcError> {
    let had_scopes = !query.scopes.is_empty();
    query.scopes.retain_mut(|scope| {
        let Ok(root) = authorize_stored_root(app, &scope.path) else {
            return false;
        };
        scope.path = root.to_string_lossy().into_owned();
        true
    });
    query.deny_all_scopes = had_scopes && query.scopes.is_empty();
    Ok(query)
}

fn reject_unknown_keys(value: &Value, allowed: &[&str], context: &str) -> Result<(), RpcError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_params(format!("{context} 参数必须为对象")))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_params(format!("{context} 不支持字段: {key}")));
    }
    Ok(())
}

fn parse_subscribe_params(args: &Value) -> Result<&str, RpcError> {
    reject_unknown_keys(
        args,
        &["topic", "base", "visibility"],
        "subscribeControllerV4",
    )?;
    let topic = args
        .get("topic")
        .and_then(Value::as_str)
        .filter(|topic| !topic.is_empty() && topic.trim() == *topic)
        .ok_or_else(|| invalid_params("subscribeControllerV4.topic 必须为字符串"))?;
    if topic != WORKSPACES_TOPIC && topic != TASKS_TOPIC {
        return Err(invalid_params("Controller topic 不受支持"));
    }
    if let Some(base) = args.get("base") {
        let object = base
            .as_object()
            .ok_or_else(|| invalid_params("subscribeControllerV4.base 必须为对象"))?;
        reject_unknown_keys(base, &["logEpoch", "seq"], "subscribeControllerV4.base")?;
        if object
            .get("logEpoch")
            .and_then(Value::as_str)
            .is_none_or(|value| value.is_empty() || value.trim() != value)
        {
            return Err(invalid_params("subscribeControllerV4.base.logEpoch 无效"));
        }
        if object.get("seq").and_then(Value::as_u64).is_none() {
            return Err(invalid_params("subscribeControllerV4.base.seq 无效"));
        }
    }
    if let Some(visibility) = args.get("visibility")
        && !matches!(visibility.as_str(), Some("foreground" | "background"))
    {
        return Err(invalid_params("subscribeControllerV4.visibility 无效"));
    }
    Ok(topic)
}

fn validate_resync_params(args: &Value) -> Result<(), RpcError> {
    reject_unknown_keys(
        args,
        &["subscriptionId", "base", "forceSnapshot"],
        "resyncControllerV4",
    )?;
    let _ = required_string(args, "subscriptionId")?;
    let base = args
        .get("base")
        .ok_or_else(|| invalid_params("resyncControllerV4.base 必须存在"))?;
    if !base.is_null() {
        let object = base
            .as_object()
            .ok_or_else(|| invalid_params("resyncControllerV4.base 必须为对象或 null"))?;
        reject_unknown_keys(base, &["logEpoch", "seq"], "resyncControllerV4.base")?;
        if object
            .get("logEpoch")
            .and_then(Value::as_str)
            .is_none_or(|value| value.is_empty() || value.trim() != value)
            || object.get("seq").and_then(Value::as_u64).is_none()
        {
            return Err(invalid_params("resyncControllerV4.base 无效"));
        }
    }
    if let Some(force_snapshot) = args.get("forceSnapshot")
        && !force_snapshot.is_boolean()
    {
        return Err(invalid_params(
            "resyncControllerV4.forceSnapshot 必须为布尔值",
        ));
    }
    Ok(())
}

fn parse_address(value: &Value) -> Result<TaskAddress, RpcError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_params("task address 必须为对象"))?;
    reject_unknown_keys(
        value,
        &[
            "remoteSessionId",
            "workspacePath",
            "workspaceIdentity",
            "taskId",
        ],
        "task address",
    )?;
    let remote_session_id = optional_string(object, "remoteSessionId")?;
    let workspace_path = required_string(value, "workspacePath")?;
    let workspace_identity = optional_string(object, "workspaceIdentity")?;
    let task_id = required_string(value, "taskId")?;
    if remote_session_id.is_some() && workspace_identity.is_none() {
        return Err(invalid_params(
            "remote task address requires workspaceIdentity",
        ));
    }
    Ok(TaskAddress {
        remote_session_id,
        workspace_path,
        workspace_identity,
        task_id,
    })
}

fn optional_string(object: &Map<String, Value>, name: &str) -> Result<Option<String>, RpcError> {
    let Some(value) = object.get(name) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .ok_or_else(|| invalid_params(format!("{name} 必须为非空字符串")))?;
    Ok(Some(value.to_owned()))
}

fn required_string(value: &Value, name: &str) -> Result<String, RpcError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.trim() == *value)
        .map(ToOwned::to_owned)
        .ok_or_else(|| invalid_params(format!("{name} 必须为非空字符串")))
}

fn matches_kind(membership: &Value, kind: &str) -> bool {
    let pinned = membership
        .get("pinned")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let archived = membership
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match kind {
        "pinned" => pinned && !archived,
        "archived" => archived,
        "timeline" => !pinned && !archived,
        "active" => !archived,
        "all" => true,
        _ => false,
    }
}

fn row_matches_search(row: &TaskRow, search: &str) -> bool {
    let title = row
        .meta
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let task_id = row
        .meta
        .get("taskId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    title.contains(search) || task_id.contains(search)
}

fn live_status(
    state: &keencode_resources::SessionState,
    active: bool,
    pending: bool,
) -> &'static str {
    if active || matches!(state.status, SessionStatus::Running) {
        return "running";
    }
    if pending || matches!(state.status, SessionStatus::Waiting) {
        return "waiting";
    }
    let latest = state
        .turns
        .values()
        .max_by_key(|turn| turn.started_at_unix_ms);
    match latest.map(|turn| &turn.status) {
        Some(TurnStatus::Failed) => "error",
        Some(TurnStatus::Completed | TurnStatus::Cancelled) | None
            if matches!(state.status, SessionStatus::Closed) =>
        {
            "completed"
        }
        Some(TurnStatus::Completed | TurnStatus::Cancelled) => "completed",
        _ => "idle",
    }
}

fn phase_for_state(state: &keencode_resources::SessionState, active: bool) -> &'static str {
    if active
        || matches!(
            state.status,
            SessionStatus::Running | SessionStatus::Waiting
        )
    {
        return "running";
    }
    match state
        .turns
        .values()
        .max_by_key(|turn| turn.started_at_unix_ms)
        .map(|turn| &turn.status)
    {
        Some(TurnStatus::Failed) => "error",
        Some(TurnStatus::Cancelled) => "completedInterrupted",
        Some(TurnStatus::Completed) => "completedSuccess",
        Some(TurnStatus::Running) => "running",
        None if matches!(state.status, SessionStatus::Closed) => "completedSuccess",
        _ => "draft",
    }
}

fn controller_log_epoch(runtime: &AgentRuntime) -> String {
    format!(
        "controller-{:016x}",
        stable_hash(&runtime.storage_root().to_string_lossy())
    )
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn owned_runtime(app: &AppHandle) -> Result<Arc<AgentRuntime>, RpcError> {
    crate::require_owned_runtime(app).map_err(backend_error)
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new("rpc.invalidParams", message)
}

fn backend_error(message: impl std::fmt::Display) -> RpcError {
    RpcError::new("controller.backendError", message.to_string())
}

fn state_error() -> RpcError {
    RpcError::new("controller.stateUnavailable", "Controller 状态不可用")
}

fn unknown_method(method: &str) -> RpcError {
    RpcError::new("rpc.unknownMethod", format!("未知方法: {CHANNEL}.{method}"))
}

impl TaskRow {
    fn into_value(self) -> Value {
        let mut row = Map::new();
        row.insert("address".to_owned(), self.address);
        row.insert("meta".to_owned(), self.meta);
        row.insert("membership".to_owned(), self.membership);
        row.insert(
            "sourceAvailability".to_owned(),
            Value::String(self.source_availability.to_owned()),
        );
        row.insert(
            "liveStatus".to_owned(),
            Value::String(self.live_status.to_owned()),
        );
        row.insert("activity".to_owned(), self.activity);
        if let Some(snippets) = self.search_snippets {
            row.insert("searchSnippets".to_owned(), json!(snippets));
        }
        Value::Object(row)
    }

    fn list_item(self) -> Value {
        let mut item = self.meta.as_object().cloned().unwrap_or_default();
        item.insert(
            "sourceAvailability".to_owned(),
            Value::String(self.source_availability.to_owned()),
        );
        item.insert(
            "liveStatus".to_owned(),
            Value::String(self.live_status.to_owned()),
        );
        item.insert("activity".to_owned(), self.activity);
        if let Some(snippets) = self.search_snippets {
            item.insert("searchSnippets".to_owned(), json!(snippets));
        }
        Value::Object(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_resources::{PlanState, SessionId, SessionState};

    #[test]
    fn controller_frame_is_a_schema_compatible_snapshot() {
        let frame = json!({
            "topic": TASKS_TOPIC,
            "subscriptionId": "controller-sub-test",
            "logEpoch": "controller-test",
            "fromSeq": 0,
            "toSeq": 3,
            "sentAt": 1,
            "payload": {"kind": "snapshot", "snapshot": {
                "protocolVersion": 1,
                "logEpoch": "controller-test",
                "tasks": [],
            }},
        });
        assert_eq!(frame["payload"]["kind"], "snapshot");
        assert_eq!(frame["fromSeq"], 0);
        assert_eq!(frame["toSeq"], 3);
    }

    #[test]
    fn controller_membership_and_status_do_not_invent_running_work() {
        let mut state = SessionState::empty(SessionId::new("session-controller-test").unwrap());
        state.plan = PlanState::default();
        assert!(matches_kind(
            &json!({"pinned": true, "archived": false, "active": true}),
            "pinned"
        ));
        assert!(!matches_kind(
            &json!({"pinned": true, "archived": true, "active": false}),
            "pinned"
        ));
        assert_eq!(live_status(&state, false, false), "idle");
        assert_eq!(phase_for_state(&state, false), "draft");
    }

    #[test]
    fn controller_address_requires_remote_identity() {
        assert!(
            parse_address(&json!({
                "remoteSessionId": "remote-1",
                "workspacePath": "C:/repo",
                "taskId": "task-1"
            }))
            .is_err()
        );
        assert!(
            parse_address(&json!({
                "workspacePath": "C:/repo",
                "taskId": "task-1"
            }))
            .is_ok()
        );
    }

    #[test]
    fn controller_ack_activates_only_the_matching_subscription() {
        let gateway = ControllerGateway::new();
        let make_subscription = |subscription_id: &str| ActiveSubscription {
            connection_id: "connection".to_owned(),
            window_label: "main".to_owned(),
            topic: TASKS_TOPIC.to_owned(),
            log_epoch: "epoch".to_owned(),
            seq: 0,
            subscription_id: subscription_id.to_owned(),
            initial_frame: Some(json!({"subscriptionId": subscription_id})),
            pending_frames: Vec::new(),
            activated: false,
            aborts: Vec::new(),
            frame_gate: Arc::new(Mutex::new(())),
        };
        gateway
            .inner
            .subscriptions
            .lock()
            .unwrap()
            .insert("sub-1".to_owned(), make_subscription("sub-1"));
        gateway
            .inner
            .subscriptions
            .lock()
            .unwrap()
            .insert("sub-2".to_owned(), make_subscription("sub-2"));

        let args = json!({"topic": TASKS_TOPIC});
        let subscription_id = gateway.subscription_id_for_response(
            "connection",
            "main",
            "subscribeControllerV4",
            &args,
            &json!({"ack": {"subscriptionId": "sub-2"}}),
        );
        assert_eq!(subscription_id.as_deref(), Some("sub-2"));
        gateway.activate_subscription("sub-2");
        let subscriptions = gateway.inner.subscriptions.lock().unwrap();
        assert!(!subscriptions["sub-1"].activated);
        assert!(subscriptions["sub-2"].activated);
    }

    #[test]
    fn controller_task_row_omits_absent_optional_fields() {
        let row = TaskRow {
            address: json!({"workspacePath": "C:/repo", "taskId": "task-1"}),
            meta: json!({
                "taskId": "task-1",
                "traceId": "session:task-1",
                "title": "Task",
                "workspacePath": "C:/repo",
                "createdAt": 1,
                "updatedAt": 2,
                "mode": "build",
            }),
            membership: json!({"pinned": false, "archived": false, "active": true}),
            source_availability: "online",
            live_status: "idle",
            activity: json!({
                "phase": "draft",
                "lastActivityAt": 2,
                "hasBackgroundWork": false,
            }),
            search_snippets: None,
        };
        let value = row.into_value();
        assert!(value.get("searchSnippets").is_none());
        assert_eq!(value["address"]["taskId"], value["meta"]["taskId"]);
    }

    #[test]
    fn controller_task_row_projection_keeps_persisted_unread_marker() {
        let row = TaskRow {
            address: json!({"workspacePath": "C:/repo", "taskId": "task-1"}),
            meta: json!({
                "taskId": "task-1",
                "workspacePath": "C:/repo",
                "unreadAt": 42,
            }),
            membership: json!({"pinned": true, "archived": false, "active": true}),
            source_availability: "online",
            live_status: "idle",
            activity: json!({
                "phase": "completed",
                "lastActivityAt": 42,
                "hasBackgroundWork": false,
            }),
            search_snippets: None,
        };
        assert_eq!(row.clone().list_item()["unreadAt"], 42);
        assert_eq!(row.into_value()["meta"]["unreadAt"], 42);
    }

    /// 在显式目录中导出 Controller 生产帧，供源仓库 Zod strict schema 验收。
    /// 默认不写文件，避免普通单测污染仓库；fixture 不含用户正文或凭据。
    #[test]
    fn export_controller_source_contract_fixtures_when_requested() {
        let Ok(directory) = std::env::var("KEENCODE_RPC_CONTRACT_FIXTURES") else {
            return;
        };
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).expect("应创建 Controller RPC fixture 目录");
        let task_row = TaskRow {
            address: json!({
                "workspacePath": "C:/contract-workspace",
                "taskId": "contract-task",
            }),
            meta: json!({
                "taskId": "contract-task",
                "traceId": "session:contract-task",
                "title": "Controller contract fixture",
                "workspacePath": "C:/contract-workspace",
                "createdAt": 1,
                "updatedAt": 2,
                "mode": "build",
                "provider": "glm",
                "model": "contract-model",
            }),
            membership: json!({"pinned": true, "archived": false, "active": true}),
            source_availability: "online",
            live_status: "idle",
            activity: json!({
                "phase": "draft",
                "lastActivityAt": 2,
                "hasBackgroundWork": false,
                "pendingInteractions": {
                    "permissionCount": 0,
                    "userInputCount": 0,
                },
            }),
            search_snippets: None,
        };
        let task = task_row.clone().into_value();
        let list_item = task_row.list_item();
        let log_epoch = "controller-contract-epoch";
        let frames = json!({
            "acks": {
                "subscribe": {
                    "ack": {
                        "subscriptionId": "controller-sub-tasks",
                        "mode": "snapshot",
                        "logEpoch": log_epoch,
                    },
                },
                "resync": {
                    "ack": {
                        "subscriptionId": "controller-sub-tasks",
                        "mode": "snapshot",
                        "logEpoch": log_epoch,
                    },
                },
            },
            "list": {
                "items": [list_item],
                "total": 1,
                "hasMore": false,
            },
            "workspace": {
                "topic": WORKSPACES_TOPIC,
                "subscriptionId": "controller-sub-workspace",
                "logEpoch": log_epoch,
                "fromSeq": 0,
                "toSeq": 1,
                "sentAt": 1,
                "payload": {"kind": "snapshot", "snapshot": {
                    "protocolVersion": 1,
                    "logEpoch": log_epoch,
                    "workspaces": [{
                        "workspacePath": "C:/contract-workspace",
                        "sourceAvailability": "online",
                        "connectionState": "online",
                    }],
                }},
            },
            "tasks": {
                "topic": TASKS_TOPIC,
                "subscriptionId": "controller-sub-tasks",
                "logEpoch": log_epoch,
                "fromSeq": 0,
                "toSeq": 1,
                "sentAt": 1,
                "payload": {"kind": "snapshot", "snapshot": {
                    "protocolVersion": 1,
                    "logEpoch": log_epoch,
                    "tasks": [task],
                }},
            },
        });
        std::fs::write(
            directory.join("window-controller.json"),
            serde_json::to_vec_pretty(&frames).expect("Controller fixture 应可序列化"),
        )
        .expect("应写入 Controller RPC fixture");
    }
}
