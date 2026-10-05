//! Desktop 工具权限审批协调器。
//!
//! 权限审批与 `elicitation.rs` 的 AskUserQuestion 问答分开维护：前者只接受
//! `ToolApprovalRequest`，并在真实工具执行前返回 `Approved`/`Denied`，后者只
//! 负责用户问题的标准 ACP Elicitation。Session owner 负责把 mode 的变化写入
//! Runtime Journal；本模块在冷恢复没有收到 mode 时始终退回 `build`。

use keencode_acp::ConnectionId;
use keencode_agent::{
    CollaborationIdGenerator, ToolApprovalDecision, ToolApprovalError, ToolApprovalFuture,
    ToolApprovalGate, ToolApprovalRequest, ToolEffect, TurnCancellation,
    UuidCollaborationIdGenerator,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot};

/// Workspace mode 对副作用工具的审批策略。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum PermissionMode {
    /// 文件、命令和其他状态变更均逐次确认。
    #[default]
    Build,
    /// 文件编辑自动允许，命令和未识别的状态变更仍逐次确认。
    Edit,
    /// 只读交互；PlanGuard 仍是更高优先级的只读硬门。
    Plan,
    /// 允许状态变更；仅在用户明确选择该模式后由 Session owner 注入。
    Yolo,
}

impl PermissionMode {
    /// 解析 V4 mode 字符串；未知或缺失值安全退回 build。
    pub(crate) fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("edit") => Self::Edit,
            Some("plan") => Self::Plan,
            Some("yolo") => Self::Yolo,
            _ => Self::Build,
        }
    }

    /// 返回本模式是否允许本次调用跳过交互。
    fn allows_without_prompt(self, tool_name: &str) -> bool {
        match self {
            Self::Yolo => true,
            Self::Edit => is_file_edit_tool(tool_name),
            Self::Build | Self::Plan => false,
        }
    }
}

/// 权限 pending 生命周期变化；正文始终由 `pending_views_for_connection` 读取。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PermissionChange {
    /// 应刷新的父 Session。
    pub(crate) display_session_id: String,
}

/// 可供 V4 projection 读取的权限 pending 只读视图。
#[derive(Clone, Debug)]
pub(crate) struct PendingPermissionView {
    /// 交互稳定 ID。
    pub(crate) interaction_id: String,
    /// 真实发起调用的 Session（actor 时不是父 Session）。
    pub(crate) session_id: String,
    /// 允许展示和回答的父 Session。
    pub(crate) display_session_id: String,
    /// 唯一允许回答的连接。
    pub(crate) connection_id: ConnectionId,
    /// 工具调用稳定 ID。
    pub(crate) tool_call_id: String,
    /// 工具名称。
    pub(crate) tool_name: String,
    /// 给用户展示的摘要。
    pub(crate) summary: String,
    /// 有界工具输入及 effect 详情。
    pub(crate) detail: Value,
    /// 登记时间。
    pub(crate) created_at_unix_ms: u64,
}

impl PendingPermissionView {
    /// 转为 shared V4 `pendingInteraction` 原始 permission 形状。
    pub(crate) fn to_v4_value(&self) -> Value {
        let allow_once = json!({
            "optionId": "allowOnce",
            "label": "允许一次",
            "kind": "allowOnce",
            "response": {"decision": "allow"},
        });
        let allow_always = json!({
            "optionId": "allowAlways",
            "label": "始终允许",
            "kind": "allowAlways",
            "response": {
                "decision": "allow",
                "permissionUpdates": [{
                    "type": "addRules",
                    "behavior": "allow",
                    "rules": [{"toolName": self.tool_name}],
                }],
            },
        });
        let deny = json!({
            "optionId": "deny",
            "label": "拒绝",
            "kind": "deny",
            "response": {"decision": "deny"},
        });
        json!({
            "interactionId": self.interaction_id,
            "kind": "permission",
            "anchorRowId": Value::Null,
            "createdAt": self.created_at_unix_ms,
            "payload": {
                "kind": "permission",
                "toolCallId": self.tool_call_id,
                "toolName": self.tool_name,
                "summary": self.summary,
                "detail": self.detail,
                "options": [allow_once, allow_always, deny],
            },
        })
    }
}

/// 权限桥接错误；错误文本不包含工具参数或连接细节。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PermissionBridgeError {
    /// Runtime 已关闭。
    RuntimeClosed,
    /// 请求不存在或已收口。
    UnknownRequest,
    /// 回答来自错误连接。
    ResponseConnectionMismatch,
    /// 回答 Session 不是允许展示的父 Session。
    ResponseSessionMismatch,
    /// 回答 JSON 或 option 不符合严格边界。
    InvalidResponse,
}

impl std::fmt::Display for PermissionBridgeError {
    /// 输出不带请求正文的稳定错误。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeClosed => formatter.write_str("权限 Runtime 已关闭"),
            Self::UnknownRequest => formatter.write_str("权限请求不存在或已经结束"),
            Self::ResponseConnectionMismatch => formatter.write_str("权限响应来自非目标连接"),
            Self::ResponseSessionMismatch => formatter.write_str("权限响应 Session 不匹配"),
            Self::InvalidResponse => formatter.write_str("权限响应无效"),
        }
    }
}

impl std::error::Error for PermissionBridgeError {}

struct PendingPermission {
    view: PendingPermissionView,
    waiter: oneshot::Sender<ToolApprovalDecision>,
}

struct PermissionState {
    closed: bool,
    modes: HashMap<String, PermissionMode>,
    /// 当前进程内由 allowAlways 建立的工具规则；冷恢复不从投影恢复，默认重新询问。
    always_allowed_tools: HashMap<String, HashSet<String>>,
    session_connections: HashMap<String, ConnectionId>,
    actor_parents: HashMap<String, String>,
    /// Actor 首次绑定时冻结父 Session mode；同一工作流节点的后续等待不随父窗口切换漂移。
    actor_modes: HashMap<String, PermissionMode>,
    pending: HashMap<String, PendingPermission>,
}

struct PermissionCoordinatorInner {
    state: Mutex<PermissionState>,
    changes: broadcast::Sender<PermissionChange>,
}

/// Session 权限审批的唯一进程内协调器。
#[derive(Clone)]
pub(crate) struct PermissionCoordinator {
    inner: Arc<PermissionCoordinatorInner>,
}

impl Default for PermissionCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionCoordinator {
    /// 创建默认安全模式为 build 的协调器。
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(PermissionCoordinatorInner {
                state: Mutex::new(PermissionState {
                    closed: false,
                    modes: HashMap::new(),
                    always_allowed_tools: HashMap::new(),
                    session_connections: HashMap::new(),
                    actor_parents: HashMap::new(),
                    actor_modes: HashMap::new(),
                    pending: HashMap::new(),
                }),
                changes: broadcast::channel(256).0,
            }),
        }
    }

    /// 绑定父 Session 当前唯一可回答权限的连接。
    pub(crate) fn bind_session_connection(
        &self,
        session_id: &str,
        connection_id: ConnectionId,
    ) -> Result<(), PermissionBridgeError> {
        let (cancelled, displays) = {
            let mut state = self.inner.state.lock();
            if state.closed {
                return Err(PermissionBridgeError::RuntimeClosed);
            }
            let previous = state
                .session_connections
                .insert(session_id.to_owned(), connection_id.clone());
            let stale_ids = previous
                .filter(|previous| *previous != connection_id)
                .map(|_| {
                    state
                        .pending
                        .iter()
                        .filter(|(_, pending)| {
                            pending.view.display_session_id == session_id
                                && pending.view.connection_id != connection_id
                        })
                        .map(|(interaction_id, _)| interaction_id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut cancelled = Vec::new();
            let mut displays = Vec::new();
            for interaction_id in stale_ids {
                if let Some(pending) = state.pending.remove(&interaction_id) {
                    displays.push(pending.view.display_session_id.clone());
                    cancelled.push(pending.waiter);
                }
            }
            (cancelled, displays)
        };
        for waiter in cancelled {
            let _ = waiter.send(ToolApprovalDecision::Denied {
                reason: "权限连接已切换".to_owned(),
            });
        }
        for display_session_id in displays {
            self.notify_change(display_session_id);
        }
        Ok(())
    }

    /// 注册单层 actor 到父 Session 的权限投影关系。
    pub(crate) fn bind_actor_parent(&self, actor_session_id: &str, parent_session_id: &str) {
        let mut state = self.inner.state.lock();
        let previous_parent = state
            .actor_parents
            .insert(actor_session_id.to_owned(), parent_session_id.to_owned());
        if previous_parent.as_deref() != Some(parent_session_id) {
            let mode = state
                .modes
                .get(parent_session_id)
                .copied()
                .unwrap_or_default();
            state.actor_modes.insert(actor_session_id.to_owned(), mode);
        }
    }

    /// 取消 actor 到父 Session 的路由；下次真实运行必须重新冻结父连接。
    pub(crate) fn unbind_actor_parent(&self, actor_session_id: &str) {
        let mut state = self.inner.state.lock();
        state.actor_parents.remove(actor_session_id);
        state.actor_modes.remove(actor_session_id);
    }

    /// 由 Session owner 在 Journal 提交后发布当前 mode；缺失调用不会改变 build 回退。
    pub(crate) fn set_mode(&self, session_id: &str, mode: PermissionMode) {
        self.inner
            .state
            .lock()
            .modes
            .insert(session_id.to_owned(), mode);
    }

    /// 返回当前 mode；冷恢复尚未重放配置时安全退回 build。
    pub(crate) fn mode(&self, session_id: &str) -> PermissionMode {
        self.inner
            .state
            .lock()
            .modes
            .get(session_id)
            .copied()
            .unwrap_or_default()
    }

    /// 订阅权限 pending 的轻量变化通知。
    pub(crate) fn subscribe_changes(&self) -> broadcast::Receiver<PermissionChange> {
        self.inner.changes.subscribe()
    }

    /// 按连接和父 Session 读取权限 pending；不会枚举其他 Session。
    pub(crate) fn pending_views_for_connection(
        &self,
        session_id: &str,
        connection_id: &ConnectionId,
    ) -> Vec<PendingPermissionView> {
        let state = self.inner.state.lock();
        if state.session_connections.get(session_id) != Some(connection_id) {
            return Vec::new();
        }
        let mut views = state
            .pending
            .values()
            .filter(|pending| {
                pending.view.display_session_id == session_id
                    && pending.view.connection_id == *connection_id
            })
            .map(|pending| pending.view.clone())
            .collect::<Vec<_>>();
        views.sort_by(|left, right| {
            left.created_at_unix_ms
                .cmp(&right.created_at_unix_ms)
                .then_with(|| left.interaction_id.cmp(&right.interaction_id))
        });
        views
    }

    /// 返回当前 Session 的权限 pending 数量。
    pub(crate) fn pending_count_for_session(&self, session_id: &str) -> usize {
        self.inner
            .state
            .lock()
            .pending
            .values()
            .filter(|pending| pending.view.display_session_id == session_id)
            .count()
    }

    /// 判断请求是否属于权限账本，供 V4 interaction 入口避免误交给 AskUser。
    pub(crate) fn contains_pending(&self, interaction_id: &str) -> bool {
        self.inner.state.lock().pending.contains_key(interaction_id)
    }

    /// 严格校验父 Session、连接和 permission option 后收口请求。
    pub(crate) fn resolve_from_connection(
        &self,
        session_id: &str,
        connection_id: &ConnectionId,
        interaction_id: &str,
        answer_json: &str,
    ) -> Result<(), PermissionBridgeError> {
        let answer: Value = serde_json::from_str(answer_json)
            .map_err(|_| PermissionBridgeError::InvalidResponse)?;
        let object = answer
            .as_object()
            .ok_or(PermissionBridgeError::InvalidResponse)?;
        let decision = parse_permission_decision(object)?;
        let remember = decision == ToolApprovalDecision::Approved
            && object.get("optionId").and_then(Value::as_str) == Some("allowAlways");
        let pending = {
            let state = self.inner.state.lock();
            if state.session_connections.get(session_id) != Some(connection_id) {
                return Err(PermissionBridgeError::ResponseConnectionMismatch);
            }
            let pending = state
                .pending
                .get(interaction_id)
                .ok_or(PermissionBridgeError::UnknownRequest)?;
            if pending.view.connection_id != *connection_id {
                return Err(PermissionBridgeError::ResponseConnectionMismatch);
            }
            if pending.view.display_session_id != session_id {
                return Err(PermissionBridgeError::ResponseSessionMismatch);
            }
            pending.view.clone()
        };
        if remember {
            self.inner
                .state
                .lock()
                .always_allowed_tools
                .entry(pending.display_session_id.clone())
                .or_default()
                .insert(pending.tool_name.clone());
        }
        self.resolve_pending(&pending.interaction_id, decision);
        Ok(())
    }

    /// 连接关闭时拒绝其全部 pending，并清理连接绑定。
    pub(crate) fn disconnect(&self, connection_id: &ConnectionId) {
        let (cancelled, displays) = {
            let mut state = self.inner.state.lock();
            let sessions = state
                .session_connections
                .iter()
                .filter(|(_, connection)| *connection == connection_id)
                .map(|(session_id, _)| session_id.clone())
                .collect::<Vec<_>>();
            for session_id in sessions {
                state.session_connections.remove(&session_id);
            }
            let ids = state
                .pending
                .values()
                .filter(|pending| pending.view.connection_id == *connection_id)
                .map(|pending| pending.view.interaction_id.clone())
                .collect::<Vec<_>>();
            let mut cancelled = Vec::new();
            let mut displays = Vec::new();
            for id in ids {
                if let Some(pending) = state.pending.remove(&id) {
                    displays.push(pending.view.display_session_id.clone());
                    cancelled.push(pending.waiter);
                }
            }
            (cancelled, displays)
        };
        for waiter in cancelled {
            let _ = waiter.send(ToolApprovalDecision::Denied {
                reason: "权限连接已关闭".to_owned(),
            });
        }
        for display_session_id in displays {
            self.notify_change(display_session_id);
        }
    }

    /// Runtime 关闭时拒绝并释放所有 pending，禁止 Runner 悬挂。
    pub(crate) fn close(&self) {
        let (pending, displays) = {
            let mut state = self.inner.state.lock();
            if state.closed && state.pending.is_empty() {
                return;
            }
            state.closed = true;
            let mut pending = Vec::new();
            let mut displays = Vec::new();
            for (_, item) in state.pending.drain() {
                displays.push(item.view.display_session_id.clone());
                pending.push(item.waiter);
            }
            (pending, displays)
        };
        for waiter in pending {
            let _ = waiter.send(ToolApprovalDecision::Denied {
                reason: "权限 Runtime 已关闭".to_owned(),
            });
        }
        for display_session_id in displays {
            self.notify_change(display_session_id);
        }
    }

    /// Session 关闭时收口其 root/actor 权限等待和连接身份。
    pub(crate) fn close_session(&self, session_id: &str) {
        let (pending, displays) = {
            let mut state = self.inner.state.lock();
            state.modes.remove(session_id);
            state.always_allowed_tools.remove(session_id);
            state.session_connections.remove(session_id);
            let actor_ids = state
                .actor_parents
                .iter()
                .filter(|(actor_id, parent_id)| {
                    actor_id.as_str() == session_id || parent_id.as_str() == session_id
                })
                .map(|(actor_id, _)| actor_id.clone())
                .collect::<Vec<_>>();
            for actor_id in actor_ids {
                state.actor_parents.remove(&actor_id);
                state.actor_modes.remove(&actor_id);
            }
            let interaction_ids = state
                .pending
                .iter()
                .filter(|(_, pending)| {
                    pending.view.session_id == session_id
                        || pending.view.display_session_id == session_id
                })
                .map(|(interaction_id, _)| interaction_id.clone())
                .collect::<Vec<_>>();
            let mut pending_waiters = Vec::new();
            let mut displays = Vec::new();
            for interaction_id in interaction_ids {
                if let Some(pending) = state.pending.remove(&interaction_id) {
                    displays.push(pending.view.display_session_id.clone());
                    pending_waiters.push(pending.waiter);
                }
            }
            (pending_waiters, displays)
        };
        for waiter in pending {
            let _ = waiter.send(ToolApprovalDecision::Denied {
                reason: "权限 Session 已关闭".to_owned(),
            });
        }
        for display_session_id in displays {
            self.notify_change(display_session_id);
        }
    }

    fn notify_change(&self, display_session_id: String) {
        let _ = self
            .inner
            .changes
            .send(PermissionChange { display_session_id });
    }

    fn resolve_pending(&self, interaction_id: &str, decision: ToolApprovalDecision) {
        let (pending, display_session_id) = {
            let mut state = self.inner.state.lock();
            let Some(pending) = state.pending.remove(interaction_id) else {
                return;
            };
            let display_session_id = pending.view.display_session_id.clone();
            (pending, display_session_id)
        };
        let _ = pending.waiter.send(decision);
        self.notify_change(display_session_id);
    }

    fn cancel_pending(&self, interaction_id: &str) {
        self.resolve_pending(
            interaction_id,
            ToolApprovalDecision::Denied {
                reason: "权限等待已取消".to_owned(),
            },
        );
    }
}

impl ToolApprovalGate for PermissionCoordinator {
    /// 按 Session mode 创建一次严格绑定的权限等待。
    fn request(
        &self,
        request: ToolApprovalRequest,
        cancellation: TurnCancellation,
    ) -> ToolApprovalFuture<'_> {
        if request.effect == ToolEffect::ReadOnly {
            return Box::pin(async { Ok(ToolApprovalDecision::Approved) });
        }
        if cancellation.is_cancelled() {
            return Box::pin(async { Ok(ToolApprovalDecision::Cancelled) });
        }

        let (closed, mode, remembered, display_session_id, connection_id) = {
            let state = self.inner.state.lock();
            let parent_session_id = state.actor_parents.get(request.session_id.as_str());
            let display_session_id = parent_session_id
                .cloned()
                .unwrap_or_else(|| request.session_id.as_str().to_owned());
            // actor 沿用首次绑定时冻结的父 mode；独立 actor 身份只用于等待与日志关联。
            let mode = parent_session_id
                .and_then(|_| state.actor_modes.get(request.session_id.as_str()).copied())
                .or_else(|| state.modes.get(&display_session_id).copied())
                .unwrap_or_default();
            let remembered = state
                .always_allowed_tools
                .get(&display_session_id)
                .is_some_and(|tools| tools.contains(&request.tool_name));
            let connection_id = state.session_connections.get(&display_session_id).cloned();
            (
                state.closed,
                mode,
                remembered,
                display_session_id,
                connection_id,
            )
        };
        if closed {
            return Box::pin(async { Err(ToolApprovalError::ConnectionClosed) });
        }
        if remembered
            || mode == PermissionMode::Yolo
            || mode.allows_without_prompt(&request.tool_name)
        {
            return Box::pin(async { Ok(ToolApprovalDecision::Approved) });
        }
        if mode == PermissionMode::Plan {
            return Box::pin(async {
                Ok(ToolApprovalDecision::Denied {
                    reason: "计划模式禁止执行会改变状态的工具".to_owned(),
                })
            });
        }
        let Some(connection_id) = connection_id else {
            return Box::pin(async { Err(ToolApprovalError::ConnectionClosed) });
        };

        let interaction_id = next_permission_request_id();
        let (sender, receiver) = oneshot::channel();
        let now = unix_time_ms();
        let view = PendingPermissionView {
            interaction_id: interaction_id.clone(),
            session_id: request.session_id.as_str().to_owned(),
            display_session_id: display_session_id.clone(),
            connection_id,
            tool_call_id: request.tool_call_id.as_str().to_owned(),
            tool_name: request.tool_name.clone(),
            summary: format!("允许工具 {} 修改工作区或外部状态？", request.tool_name),
            detail: json!({
                "toolCallId": request.tool_call_id.as_str(),
                "toolName": request.tool_name,
                "effect": "write",
                "input": bounded_json(request.input),
                "operationId": request.operation_id,
            }),
            created_at_unix_ms: now,
        };
        {
            let mut state = self.inner.state.lock();
            if state.closed {
                return Box::pin(async { Err(ToolApprovalError::ConnectionClosed) });
            }
            state.pending.insert(
                interaction_id.clone(),
                PendingPermission {
                    view,
                    waiter: sender,
                },
            );
        }
        self.notify_change(display_session_id);
        let guard = PendingPermissionGuard {
            coordinator: self.clone(),
            interaction_id,
            active: true,
        };
        Box::pin(async move {
            let mut guard = guard;
            let decision = receiver
                .await
                .map_err(|_| ToolApprovalError::ConnectionClosed);
            guard.active = false;
            decision
        })
    }
}

struct PendingPermissionGuard {
    coordinator: PermissionCoordinator,
    interaction_id: String,
    active: bool,
}

impl Drop for PendingPermissionGuard {
    fn drop(&mut self) {
        if self.active {
            self.coordinator.cancel_pending(&self.interaction_id);
        }
    }
}

fn parse_permission_decision(
    answer: &serde_json::Map<String, Value>,
) -> Result<ToolApprovalDecision, PermissionBridgeError> {
    let action = answer.get("action").and_then(Value::as_str);
    let option_id = answer.get("optionId").and_then(Value::as_str);
    let action_kind = action
        .map(|value| match value {
            "accept" => Ok("allow"),
            "decline" => Ok("deny"),
            "cancel" => Ok("cancel"),
            _ => Err(PermissionBridgeError::InvalidResponse),
        })
        .transpose()?;
    let option_kind = option_id
        .map(|value| match value {
            "allowOnce" | "allowAlways" => Ok("allow"),
            "deny" => Ok("deny"),
            _ => Err(PermissionBridgeError::InvalidResponse),
        })
        .transpose()?;
    if let (Some(action_kind), Some(option_kind)) = (action_kind, option_kind)
        && action_kind != option_kind
    {
        // action 与 optionId 同时存在时必须表示同一决定，禁止冲突字段走优先级旁路。
        return Err(PermissionBridgeError::InvalidResponse);
    }
    match action_kind.or(option_kind) {
        Some("allow") => Ok(ToolApprovalDecision::Approved),
        Some("cancel") => Ok(ToolApprovalDecision::Cancelled),
        Some("deny") => Ok(ToolApprovalDecision::Denied {
            reason: "用户拒绝工具权限".to_owned(),
        }),
        _ => Err(PermissionBridgeError::InvalidResponse),
    }
}

fn is_file_edit_tool(tool_name: &str) -> bool {
    let name = tool_name.trim().to_ascii_lowercase();
    [
        "edit",
        "write",
        "apply_patch",
        "write_file",
        "edit_file",
        "file_edit",
        "replace_in_file",
    ]
    .iter()
    .any(|prefix| name == *prefix || name.starts_with(&format!("{prefix}:")))
}

fn next_permission_request_id() -> String {
    let message_id = UuidCollaborationIdGenerator.next_message_id();
    let suffix = message_id
        .as_str()
        .strip_prefix("message-")
        .unwrap_or_else(|| message_id.as_str());
    format!("permission-{suffix}")
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn bounded_json(value: Value) -> Value {
    const MAX_BYTES: usize = 128 * 1024;
    if serde_json::to_vec(&value).map_or(true, |bytes| bytes.len() > MAX_BYTES) {
        json!({"truncated": true})
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_agent::{AgentId, SessionId, ToolCallId, TurnId};

    fn request(session: &str) -> ToolApprovalRequest {
        ToolApprovalRequest {
            session_id: SessionId::new(session).unwrap(),
            turn_id: TurnId::new("turn-1").unwrap(),
            source_agent_id: AgentId::new("agent-1").unwrap(),
            tool_call_id: ToolCallId::new("call-1").unwrap(),
            tool_name: "Shell".to_owned(),
            input: json!({"command": "echo test"}),
            effect: ToolEffect::ChangesState,
            operation_id: None,
        }
    }

    /// 权限交互标识必须由 UUID 生成，不能在冷恢复后回到 permission-0 并
    /// 让新请求误命中上一实例的自动审批记录。
    #[test]
    fn permission_request_ids_are_uuid_backed_and_not_reused() {
        let first = next_permission_request_id();
        let second = next_permission_request_id();
        assert_ne!(first, second);
        for id in [first, second] {
            let suffix = id
                .strip_prefix("permission-")
                .expect("权限 ID 应带类型前缀");
            assert_eq!(suffix.len(), 36);
            assert_eq!(suffix.as_bytes()[8], b'-');
            assert_eq!(suffix.as_bytes()[13], b'-');
            assert_eq!(suffix.as_bytes()[18], b'-');
            assert_eq!(suffix.as_bytes()[23], b'-');
        }
    }

    /// 两个独立 Coordinator 模拟冷实例恢复时，新权限请求不能复用旧 ID。
    #[tokio::test]
    async fn permission_request_ids_stay_unique_across_coordinator_instances() {
        let first_coordinator = PermissionCoordinator::new();
        let first_connection = ConnectionId::new("cold-permission-1").unwrap();
        first_coordinator
            .bind_session_connection("cold-session-1", first_connection.clone())
            .unwrap();
        let first_wait =
            first_coordinator.request(request("cold-session-1"), TurnCancellation::new());
        let first_id = first_coordinator
            .pending_views_for_connection("cold-session-1", &first_connection)
            .pop()
            .expect("首个实例应有权限 pending")
            .interaction_id;
        first_coordinator.close();
        assert!(matches!(
            first_wait.await,
            Ok(ToolApprovalDecision::Denied { .. })
        ));

        let second_coordinator = PermissionCoordinator::new();
        let second_connection = ConnectionId::new("cold-permission-2").unwrap();
        second_coordinator
            .bind_session_connection("cold-session-2", second_connection.clone())
            .unwrap();
        let second_wait =
            second_coordinator.request(request("cold-session-2"), TurnCancellation::new());
        let second_id = second_coordinator
            .pending_views_for_connection("cold-session-2", &second_connection)
            .pop()
            .expect("恢复实例应有新的权限 pending")
            .interaction_id;
        assert_ne!(first_id, second_id);
        second_coordinator.close();
        assert!(matches!(
            second_wait.await,
            Ok(ToolApprovalDecision::Denied { .. })
        ));
    }

    #[tokio::test]
    async fn build_requires_approval_and_answer_is_connection_bound() {
        let coordinator = PermissionCoordinator::new();
        let connection = ConnectionId::new("connection-1").unwrap();
        let wrong_connection = ConnectionId::new("connection-2").unwrap();
        coordinator
            .bind_session_connection("session-1", connection.clone())
            .unwrap();
        let future = coordinator.request(request("session-1"), TurnCancellation::new());
        assert_eq!(coordinator.pending_count_for_session("session-1"), 1);
        let views = coordinator.pending_views_for_connection("session-1", &connection);
        assert_eq!(views.len(), 1);
        let id = views[0].interaction_id.clone();
        assert_eq!(
            coordinator.resolve_from_connection(
                "session-1",
                &wrong_connection,
                &id,
                r#"{"optionId":"allowOnce"}"#,
            ),
            Err(PermissionBridgeError::ResponseConnectionMismatch)
        );
        assert_eq!(
            coordinator.resolve_from_connection(
                "session-1",
                &connection,
                &id,
                r#"{"action":"decline","optionId":"allowOnce"}"#,
            ),
            Err(PermissionBridgeError::InvalidResponse)
        );
        coordinator
            .resolve_from_connection("session-1", &connection, &id, r#"{"optionId":"allowOnce"}"#)
            .unwrap();
        assert_eq!(future.await.unwrap(), ToolApprovalDecision::Approved);
        assert_eq!(coordinator.pending_count_for_session("session-1"), 0);
    }

    #[tokio::test]
    async fn disconnect_releases_wait_and_edit_only_allows_file_tools() {
        let coordinator = PermissionCoordinator::new();
        let connection = ConnectionId::new("connection-1").unwrap();
        coordinator
            .bind_session_connection("session-1", connection.clone())
            .unwrap();
        let future = coordinator.request(request("session-1"), TurnCancellation::new());
        coordinator.disconnect(&connection);
        assert!(matches!(
            future.await,
            Ok(ToolApprovalDecision::Denied { .. })
        ));

        coordinator
            .bind_session_connection("session-2", connection)
            .unwrap();
        coordinator.set_mode("session-2", PermissionMode::Edit);
        let mut file_request = request("session-2");
        file_request.tool_name = "Edit".to_owned();
        assert_eq!(
            coordinator
                .request(file_request, TurnCancellation::new())
                .await
                .unwrap(),
            ToolApprovalDecision::Approved
        );
        let command_future = coordinator.request(request("session-2"), TurnCancellation::new());
        assert_eq!(coordinator.pending_count_for_session("session-2"), 1);
        coordinator.close();
        assert!(matches!(
            command_future.await,
            Ok(ToolApprovalDecision::Denied { .. })
        ));
    }

    #[tokio::test]
    async fn plan_is_hard_deny_and_actor_inherits_parent_mode_and_connection() {
        let coordinator = PermissionCoordinator::new();
        let connection = ConnectionId::new("connection-plan").unwrap();
        coordinator
            .bind_session_connection("parent", connection.clone())
            .unwrap();
        coordinator.set_mode("parent", PermissionMode::Plan);
        coordinator.bind_actor_parent("actor", "parent");

        let mut actor_request = request("actor");
        actor_request.source_agent_id = AgentId::new("actor-agent").unwrap();
        assert!(matches!(
            coordinator
                .request(actor_request, TurnCancellation::new())
                .await
                .unwrap(),
            ToolApprovalDecision::Denied { .. }
        ));
        assert_eq!(coordinator.pending_count_for_session("parent"), 0);
    }

    #[tokio::test]
    async fn actor_mode_stays_frozen_after_parent_switch() {
        let coordinator = PermissionCoordinator::new();
        let connection = ConnectionId::new("connection-frozen").unwrap();
        coordinator
            .bind_session_connection("parent", connection)
            .unwrap();
        coordinator.bind_actor_parent("actor", "parent");
        coordinator.set_mode("parent", PermissionMode::Yolo);

        let pending = coordinator.request(request("actor"), TurnCancellation::new());
        assert_eq!(coordinator.pending_count_for_session("parent"), 1);
        coordinator.close();
        assert!(matches!(
            pending.await,
            Ok(ToolApprovalDecision::Denied { .. })
        ));
    }

    #[tokio::test]
    async fn allow_always_remembers_only_the_approved_tool() {
        let coordinator = PermissionCoordinator::new();
        let connection = ConnectionId::new("connection-always").unwrap();
        coordinator
            .bind_session_connection("session-always", connection.clone())
            .unwrap();
        let first = coordinator.request(request("session-always"), TurnCancellation::new());
        let view = coordinator
            .pending_views_for_connection("session-always", &connection)
            .pop()
            .unwrap();
        coordinator
            .resolve_from_connection(
                "session-always",
                &connection,
                &view.interaction_id,
                r#"{"optionId":"allowAlways"}"#,
            )
            .unwrap();
        assert_eq!(first.await.unwrap(), ToolApprovalDecision::Approved);
        assert_eq!(
            coordinator
                .request(request("session-always"), TurnCancellation::new())
                .await
                .unwrap(),
            ToolApprovalDecision::Approved
        );

        let mut other_tool = request("session-always");
        other_tool.tool_name = "Write".to_owned();
        let other = coordinator.request(other_tool, TurnCancellation::new());
        assert_eq!(coordinator.pending_count_for_session("session-always"), 1);
        coordinator.close();
        assert!(matches!(
            other.await,
            Ok(ToolApprovalDecision::Denied { .. })
        ));
    }
}
