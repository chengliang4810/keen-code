//! 根据 ACP 任务边界发送 macOS 与 Windows 原生通知。

use std::collections::HashSet;
use std::sync::Mutex;

use serde_json::Value;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

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

/// 结合显式失败事件和结束原因判断任务通知类型。
fn completion_kind(failed: bool, stop_reason: &str) -> CompletionKind {
    if stop_reason == "cancelled" {
        CompletionKind::Cancelled
    } else if failed || stop_reason == "max_turn_requests" {
        CompletionKind::Failed
    } else {
        CompletionKind::Completed
    }
}

/// 仅当应用没有任何获得焦点的窗口时发送桌面通知。
fn should_send_notification(has_focused_window: bool) -> bool {
    !has_focused_window
}

/// 判断 KeenCode 当前是否有获得焦点的窗口。
fn has_focused_window(app: &AppHandle) -> bool {
    app.webview_windows()
        .values()
        .any(|window| window.is_focused().unwrap_or(false))
}

/// 跟踪已经出现执行失败的精确 client turn，供完成边界准确分类。
#[derive(Default)]
pub struct TaskNotifications {
    /// 收到 `agent_execution_failed`、尚未收到对应 done 的 (Session, turn)。
    failed_turns: Mutex<HashSet<(String, String)>>,
    /// 已发送等待确认通知的请求，避免同一请求被重复投递。
    notified_confirmations: Mutex<HashSet<(String, i64)>>,
}

impl TaskNotifications {
    /// 从 ACP Agent 事件中记录任务失败状态。
    pub fn observe_agent_event(&self, session_id: &str, turn_id: &str, event_json: &str) {
        let Ok(event) = serde_json::from_str::<Value>(event_json) else {
            return;
        };
        if event.get("type").and_then(Value::as_str) == Some("agent_execution_failed") {
            self.failed_turns
                .lock()
                .expect("任务通知状态锁已损坏")
                .insert((session_id.to_owned(), turn_id.to_owned()));
        }
    }

    /// 只消费完全匹配的失败 turn；迟到旧事件不得污染当前 turn。
    fn take_failed_turn(&self, session_id: &str, turn_id: &str) -> bool {
        self.failed_turns
            .lock()
            .expect("任务通知状态锁已损坏")
            .remove(&(session_id.to_owned(), turn_id.to_owned()))
    }

    /// transport 断开时精确丢弃未完成 turn 的失败标记，不发送桌面通知。
    pub fn discard_turn(&self, session_id: &str, turn_id: &str) {
        let _ = self.take_failed_turn(session_id, turn_id);
    }

    /// 在任务结束时发送完成或失败通知，并清理本轮失败状态。
    pub fn notify_done(
        &self,
        app: &AppHandle,
        session_id: &str,
        turn_id: &str,
        task_title: Option<&str>,
        stop_reason: &str,
    ) {
        let failed = self.take_failed_turn(session_id, turn_id);
        self.notified_confirmations
            .lock()
            .expect("任务确认通知状态锁已损坏")
            .retain(|(stored_session_id, _)| stored_session_id != session_id);
        let completion = completion_kind(failed, stop_reason);
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
        self.send(app, title, body);
    }

    /// 在任务等待用户确认时发送通知。
    pub fn notify_needs_confirmation(
        &self,
        app: &AppHandle,
        session_id: &str,
        rpc_id: i64,
        task_title: Option<&str>,
    ) {
        if !self
            .notified_confirmations
            .lock()
            .expect("任务确认通知状态锁已损坏")
            .insert((session_id.to_owned(), rpc_id))
        {
            return;
        }
        let body = task_title
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("KeenCode 任务");
        self.send(app, "任务需要你的确认", body);
    }

    /// 根据当前设置发送一条系统通知。
    fn send(&self, app: &AppHandle, title: &str, body: &str) {
        let Ok(settings) = crate::app_settings::get(app) else {
            return;
        };
        if !settings.task_notifications {
            return;
        }
        if !should_send_notification(has_focused_window(app)) {
            return;
        }
        let mut builder = app.notification().builder().title(title).body(body);
        if settings.notification_sound {
            builder = builder.sound("default");
        }
        if let Err(error) = builder.show() {
            eprintln!("[keencode] 发送任务通知失败: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CompletionKind, TaskNotifications, completion_kind, should_send_notification};

    /// 执行失败事件必须按 Session 隔离记录。
    #[test]
    fn tracks_failed_session_from_agent_event() {
        let notifications = TaskNotifications::default();
        notifications.observe_agent_event(
            "session-a",
            "turn-a",
            r#"{"type":"agent_execution_failed","value":{"message":"failed"}}"#,
        );
        assert!(
            notifications
                .failed_turns
                .lock()
                .expect("读取测试通知状态")
                .contains(&("session-a".to_owned(), "turn-a".to_owned()))
        );
        assert!(!notifications.take_failed_turn("session-a", "turn-b"));
        notifications.discard_turn("session-a", "turn-a");
        assert!(!notifications.take_failed_turn("session-a", "turn-a"));
    }

    /// 完成、失败、轮次上限和主动取消必须使用不同通知语义。
    #[test]
    fn classifies_completion_boundaries() {
        assert_eq!(
            completion_kind(false, "end_turn"),
            CompletionKind::Completed
        );
        assert_eq!(completion_kind(true, "end_turn"), CompletionKind::Failed);
        assert_eq!(
            completion_kind(false, "max_turn_requests"),
            CompletionKind::Failed
        );
        assert_eq!(
            completion_kind(true, "cancelled"),
            CompletionKind::Cancelled
        );
    }

    /// 应用正在被使用时静默，失去焦点后才允许发送桌面通知。
    #[test]
    fn only_notifies_when_application_is_unfocused() {
        assert!(!should_send_notification(true));
        assert!(should_send_notification(false));
    }
}
