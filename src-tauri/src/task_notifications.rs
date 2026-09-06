//! 根据 Runtime 权威根 Turn 终态发送 macOS 与 Windows 原生通知。

use keencode_resources::TurnStopReason;
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
fn has_focused_window(app: &AppHandle) -> bool {
    app.webview_windows()
        .values()
        .any(|window| window.is_focused().unwrap_or(false))
}

/// 根据桌面设置发送根任务终态通知。
#[derive(Default)]
pub struct TaskNotifications {}

impl TaskNotifications {
    /// 在根任务形成唯一权威终态后发送完成或失败通知；主动取消保持静默。
    pub fn notify_terminal(
        &self,
        app: &AppHandle,
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
        self.send(app, title, body);
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
    use super::{CompletionKind, completion_kind, should_send_notification};
    use keencode_resources::TurnStopReason;

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
}
