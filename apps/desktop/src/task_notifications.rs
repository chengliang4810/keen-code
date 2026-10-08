//! 根据 Runtime 权威根 Turn 终态发送 macOS 与 Windows 原生通知。

use keencode_resources::TurnStopReason;
use serde::Deserialize;

use crate::{agent_runtime::AgentRuntime, native_paths::NativePaths};

const MAX_NOTIFICATION_TEXT_BYTES: usize = 16 * 1024;

/// Native UI 请求系统通知时使用的严格载荷；状态与当前 shared contract 保持同名。
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskNotificationPayload {
    pub task_id: String,
    pub status: TaskNotificationStatus,
    pub request_id: Option<String>,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskNotificationStatus {
    Completed,
    Failed,
    PermissionRequest,
    ElicitationRequest,
    FeedbackUpdate,
}

/// 任务结束通知的当前分类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompletionKind {
    /// 正常结束。
    Completed,
    /// 执行失败或达到最大轮次。
    Failed,
    /// 用户主动取消，不发送完成通知。
    Cancelled,
}

/// 根据 Runtime 的结构化停止原因判断任务通知类型。
fn completion_kind(stop_reason: Option<TurnStopReason>) -> CompletionKind {
    match stop_reason {
        None => CompletionKind::Completed,
        Some(TurnStopReason::Cancelled) => CompletionKind::Cancelled,
        Some(
            TurnStopReason::Failed
            | TurnStopReason::LimitReached
            | TurnStopReason::ContextBlocked
            | TurnStopReason::ModelOutputLimit
            | TurnStopReason::ModelRefusal,
        ) => CompletionKind::Failed,
    }
}

/// 仅当应用没有任何获得焦点的窗口时发送桌面通知。
fn should_send_notification(has_focused_window: bool) -> bool {
    !has_focused_window
}

/// 判断 KeenCode 当前是否有获得焦点的窗口。
fn validate_text(
    field: &str,
    value: &str,
    allow_newlines: bool,
    required: bool,
) -> Result<(), String> {
    if required && value.trim().is_empty() {
        return Err(format!("通知 {field} 不能为空"));
    }
    if value.len() > MAX_NOTIFICATION_TEXT_BYTES {
        return Err(format!("通知 {field} 超过长度限制"));
    }
    if value.chars().any(|character| {
        character.is_control() && (!allow_newlines || !matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(format!("通知 {field} 包含控制字符"));
    }
    Ok(())
}

fn validate_payload(payload: &TaskNotificationPayload) -> Result<(), String> {
    validate_text("taskId", &payload.task_id, false, true)?;
    validate_text("title", &payload.title, true, false)?;
    validate_text("body", &payload.body, true, false)?;
    if let Some(request_id) = payload.request_id.as_deref() {
        validate_text("requestId", request_id, false, true)?;
    }
    if matches!(
        payload.status,
        TaskNotificationStatus::PermissionRequest | TaskNotificationStatus::ElicitationRequest
    ) && payload.request_id.is_none()
    {
        return Err("交互通知缺少 requestId".to_owned());
    }
    Ok(())
}

fn session_connection(
    runtime: &crate::agent_runtime::AgentRuntime,
    task_id: &str,
) -> Result<keencode_acp::ConnectionId, String> {
    runtime
        .elicitation_coordinator()
        .session_connection(task_id)
        .ok_or_else(|| "通知任务没有当前 Native UI 连接".to_owned())
}

fn validate_pending_notification(
    runtime: &crate::agent_runtime::AgentRuntime,
    payload: &TaskNotificationPayload,
) -> Result<(), String> {
    let request_id = payload
        .request_id
        .as_deref()
        .ok_or_else(|| "交互通知缺少 requestId".to_owned())?;
    let connection = session_connection(runtime, &payload.task_id)?;
    match payload.status {
        TaskNotificationStatus::PermissionRequest => {
            let pending = runtime
                .pending_permission_views(&payload.task_id, &connection)
                .map_err(|error| error.to_string())?;
            if pending.iter().any(|view| view.interaction_id == request_id) {
                Ok(())
            } else {
                Err("权限请求不存在、已结束或不属于当前任务".to_owned())
            }
        }
        TaskNotificationStatus::ElicitationRequest => {
            let pending = runtime
                .pending_elicitation_views(&payload.task_id, &connection)
                .map_err(|error| error.to_string())?;
            if pending.iter().any(|view| view.request_id == request_id) {
                Ok(())
            } else {
                Err("问答请求不存在、已结束或不属于当前任务".to_owned())
            }
        }
        TaskNotificationStatus::Completed
        | TaskNotificationStatus::Failed
        | TaskNotificationStatus::FeedbackUpdate => Err("非交互通知不能校验 requestId".to_owned()),
    }
}

fn validate_terminal_notification(
    runtime: &crate::agent_runtime::AgentRuntime,
    payload: &TaskNotificationPayload,
) -> Result<(), String> {
    // 通知只需要根 Turn 的两个终态字段；使用小投影避免为一次校验复制完整
    // SessionState、Journal 计数和消息正文。
    let session = runtime
        .runtime_manager()
        .get(payload.task_id.clone())
        .map_err(|error| error.to_string())?;
    let latest_root_turn = session
        .read_state(|state| {
            state
                .turns
                .values()
                .filter(|turn| turn.source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID)
                .max_by_key(|turn| {
                    (
                        turn.completed_at_unix_ms.unwrap_or(0),
                        turn.started_at_unix_ms,
                    )
                })
                .map(|turn| (turn.status.clone(), turn.stop_reason))
        })
        .map_err(|error| error.to_string())?;
    let Some(turn) = latest_root_turn else {
        return Err("任务没有可验证的根 Turn 终态".to_owned());
    };
    let expected_status = match payload.status {
        TaskNotificationStatus::Completed => {
            matches!(turn.0, keencode_resources::TurnStatus::Completed) && turn.1.is_none()
        }
        TaskNotificationStatus::Failed => {
            matches!(turn.0, keencode_resources::TurnStatus::Failed)
                && matches!(
                    turn.1,
                    Some(
                        TurnStopReason::Failed
                            | TurnStopReason::LimitReached
                            | TurnStopReason::ContextBlocked
                            | TurnStopReason::ModelOutputLimit
                            | TurnStopReason::ModelRefusal
                    )
                )
        }
        TaskNotificationStatus::PermissionRequest
        | TaskNotificationStatus::ElicitationRequest
        | TaskNotificationStatus::FeedbackUpdate => false,
    };
    if expected_status {
        Ok(())
    } else {
        Err("任务终态通知与 Runtime 当前根 Turn 事实不匹配".to_owned())
    }
}

fn ensure_task_exists(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    task_id: &str,
) -> Result<(), String> {
    crate::session_commands::authorized_metadata(runtime, paths, task_id).map(|_| ())
}

/// NativeHost 通知适配器；Windows 实现应在这里调用 WinRT Toast，
/// macOS 实现应调用 UserNotifications，域逻辑不依赖任何桌面框架。
pub trait NativeNotificationHost: Send + Sync {
    fn paths(&self) -> &NativePaths;
    fn runtime(&self) -> &AgentRuntime;
    fn task_notifications_enabled(&self) -> bool;
    fn notification_sound_enabled(&self) -> bool;
    fn has_focused_window(&self) -> bool;
    fn show_notification(&self, title: &str, body: &str, sound: bool) -> Result<(), String>;
}

/// NativeHost 任务通知的唯一入口。
///
/// 交互通知必须命中当前连接上的真实 pending 账本，
/// 终态与反馈通知必须命中已登记的 Session。系统通知插件不提供统一点击回调，
/// 因而这里只发送原生通知，不伪造点击事件。
pub fn task_notification_show<H: NativeNotificationHost>(
    host: &H,
    payload: TaskNotificationPayload,
) -> Result<(), String> {
    validate_payload(&payload)?;
    let runtime = host.runtime();
    match payload.status {
        TaskNotificationStatus::PermissionRequest | TaskNotificationStatus::ElicitationRequest => {
            validate_pending_notification(runtime, &payload)?;
        }
        TaskNotificationStatus::Completed | TaskNotificationStatus::Failed => {
            validate_terminal_notification(runtime, &payload)?;
            // 根 Runtime 的 terminal pump 已经按 stop reason 发送原生通知；
            // Native UI 终态只作一致性校验，避免同一 Turn 弹出两次。
            return Ok(());
        }
        TaskNotificationStatus::FeedbackUpdate => {
            ensure_task_exists(host.paths(), runtime, &payload.task_id)?;
        }
    }

    if !host.task_notifications_enabled() || host.has_focused_window() {
        return Ok(());
    }
    host.show_notification(
        &payload.title,
        &payload.body,
        host.notification_sound_enabled(),
    )
}

/// 根据桌面设置发送根任务终态通知。
#[derive(Default)]
pub struct TaskNotifications {}

impl TaskNotifications {
    /// 在根任务形成唯一权威终态后发送完成或失败通知；主动取消保持静默。
    pub fn notify_terminal<H: NativeNotificationHost>(
        &self,
        host: &H,
        task_title: Option<&str>,
        stop_reason: Option<TurnStopReason>,
    ) {
        let completion = completion_kind(stop_reason);
        if completion == CompletionKind::Cancelled {
            return;
        }
        let title = if completion == CompletionKind::Failed {
            "任务执行失败"
        } else {
            "任务已完成"
        };
        let body = task_title
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("KeenCode 任务");
        self.send(host, title, body);
    }

    /// 根据当前设置发送一条系统通知。
    fn send<H: NativeNotificationHost>(&self, host: &H, title: &str, body: &str) {
        if !host.task_notifications_enabled() {
            return;
        }
        if !should_send_notification(host.has_focused_window()) {
            return;
        }
        if let Err(error) = host.show_notification(title, body, host.notification_sound_enabled()) {
            tracing::error!(%error, "发送任务通知失败");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CompletionKind, TaskNotificationPayload, TaskNotificationStatus, completion_kind,
        should_send_notification, validate_payload,
    };
    use keencode_resources::TurnStopReason;
    use serde_json::json;

    /// 完成、失败、轮次上限和主动取消必须使用不同通知语义。
    #[test]
    fn classifies_completion_boundaries() {
        assert_eq!(completion_kind(None), CompletionKind::Completed);
        assert_eq!(
            completion_kind(Some(TurnStopReason::Failed)),
            CompletionKind::Failed
        );
        assert_eq!(
            completion_kind(Some(TurnStopReason::LimitReached)),
            CompletionKind::Failed
        );
        assert_eq!(
            completion_kind(Some(TurnStopReason::ContextBlocked)),
            CompletionKind::Failed
        );
        assert_eq!(
            completion_kind(Some(TurnStopReason::Cancelled)),
            CompletionKind::Cancelled
        );
        for reason in [
            TurnStopReason::ModelOutputLimit,
            TurnStopReason::ModelRefusal,
        ] {
            assert_eq!(completion_kind(Some(reason)), CompletionKind::Failed);
        }
    }

    /// 应用正在被使用时静默，失去焦点后才允许发送桌面通知。
    #[test]
    fn only_notifies_when_application_is_unfocused() {
        assert!(!should_send_notification(true));
        assert!(should_send_notification(false));
    }

    #[test]
    fn validates_interaction_notification_identity() {
        let payload = serde_json::from_value::<TaskNotificationPayload>(json!({
            "taskId": "session-1",
            "status": "permission_request",
            "requestId": "permission-1",
            "title": "需要确认",
            "body": "允许执行工具？"
        }))
        .unwrap();
        assert_eq!(payload.status, TaskNotificationStatus::PermissionRequest);
        assert!(validate_payload(&payload).is_ok());

        let missing_request = TaskNotificationPayload {
            request_id: None,
            ..payload
        };
        assert!(validate_payload(&missing_request).is_err());
    }

    #[test]
    fn rejects_unknown_notification_fields() {
        let error = serde_json::from_value::<TaskNotificationPayload>(json!({
            "taskId": "session-1",
            "status": "completed",
            "title": "完成",
            "body": "已完成",
            "clickAction": "open"
        }))
        .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }
}
