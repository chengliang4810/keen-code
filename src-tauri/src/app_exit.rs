use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::agent_runtime::AgentRuntime;

/// 应用退出审批与串行清理状态。
#[derive(Default)]
pub struct ExitState {
    /// 清理完成后放行下一次窗口退出事件。
    approved: AtomicBool,
    /// 防止退出确认、安装更新与窗口关闭同时重复清理。
    shutdown_lock: tokio::sync::Mutex<()>,
}

impl ExitState {
    /// 标记 Runtime 与本地记录已经完成退出清理。
    pub fn approve(&self) {
        self.approved.store(true, Ordering::Release);
    }

    /// 返回下一次退出事件是否可以直接放行。
    pub fn is_approved(&self) -> bool {
        self.approved.load(Ordering::Acquire)
    }
}

/// 主窗口退出请求事件只携带仍有工作的 Session 数量。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExitRequestedPayload {
    /// 当前至少存在一个运行中 Turn 的 Session 数量。
    active_count: usize,
}

/// 退出清理失败事件只携带供前端显示的原始错误信息。
#[derive(Clone, Serialize)]
struct ExitFailedPayload {
    /// 退出清理失败的原始错误语义。
    message: String,
}

/// 根据运行中 Session 决定直接清理退出还是请求用户确认。
pub fn request_exit(app: &AppHandle) -> Result<usize, String> {
    let runtime = app.state::<Arc<AgentRuntime>>();
    let active_count = runtime
        .active_session_ids()
        .map_err(|error| error.to_string())?
        .len();
    if active_count == 0 {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            match prepare_for_exit(&app).await {
                Ok(()) => app.exit(0),
                Err(error) => {
                    tracing::error!("应用退出清理失败");
                    if app
                        .emit("app://exit-failed", ExitFailedPayload { message: error })
                        .is_err()
                    {
                        tracing::error!("退出失败事件发送失败");
                    }
                }
            }
        });
    } else {
        app.emit(
            "app://exit-requested",
            ExitRequestedPayload { active_count },
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(active_count)
}

#[tauri::command]
/// 处理标题栏或菜单发起的退出请求。
pub fn app_request_exit(app: AppHandle) -> Result<usize, String> {
    request_exit(&app)
}

#[tauri::command]
/// 确认停止全部工作并退出应用。
pub async fn app_confirm_exit(app: AppHandle) -> Result<(), String> {
    prepare_for_exit(&app).await?;
    app.exit(0);
    Ok(())
}

/// 停止所有任务并放行下一次退出或重启事件。
pub async fn prepare_for_exit(app: &AppHandle) -> Result<(), String> {
    let exit_state = app.state::<ExitState>();
    let _shutdown_guard = exit_state.shutdown_lock.lock().await;
    if exit_state.is_approved() {
        return Ok(());
    }
    let runtime = app.state::<Arc<AgentRuntime>>().inner().clone();
    runtime
        .shutdown()
        .await
        .map_err(|error| error.to_string())?;
    app.state::<Arc<crate::analytics::AnalyticsRecorder>>()
        .flush()?;
    exit_state.approve();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_is_not_approved_by_default() {
        let state = ExitState::default();
        assert!(!state.is_approved());
        state.approve();
        assert!(state.is_approved());
    }

    /// 无任务退出失败时必须保留原始错误语义。
    #[test]
    fn exit_failure_payload_keeps_error_without_active_tasks() {
        let payload = ExitFailedPayload {
            message: "flush failed".to_owned(),
        };
        assert_eq!(payload.message, "flush failed");
    }

    /// 失败事件独立携带消息，不混用运行任务计数。
    #[test]
    fn exit_failure_payload_serializes_frontend_error_event() {
        let payload = ExitFailedPayload {
            message: "flush failed".to_owned(),
        };
        let value = serde_json::to_value(payload).expect("退出失败事件应可序列化");
        assert_eq!(value["message"], "flush failed");
        assert!(
            !value
                .as_object()
                .expect("退出失败事件应为对象")
                .contains_key("activeCount")
        );
    }

    /// 正常退出确认事件只携带真实的活动任务计数。
    #[test]
    fn exit_confirmation_payload_keeps_existing_event_shape() {
        let payload = ExitRequestedPayload { active_count: 2 };
        let value = serde_json::to_value(payload).expect("退出确认事件应可序列化");
        assert_eq!(value["activeCount"], 2);
        assert!(
            !value
                .as_object()
                .expect("退出确认事件应为对象")
                .contains_key("error")
        );
    }
}
