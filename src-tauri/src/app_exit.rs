use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::peri_runtime::{PeriRuntime, SessionState};

#[derive(Default)]
pub struct ExitState {
    approved: AtomicBool,
}

impl ExitState {
    pub fn approve(&self) {
        self.approved.store(true, Ordering::Release);
    }

    pub fn reset(&self) {
        self.approved.store(false, Ordering::Release);
    }

    pub fn is_approved(&self) -> bool {
        self.approved.load(Ordering::Acquire)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExitRequestedPayload {
    active_count: usize,
}

pub fn request_exit(app: &AppHandle) -> Result<usize, String> {
    let runtime = app.state::<Arc<PeriRuntime>>();
    let active_count = runtime.active_session_ids().len();
    if active_count == 0 {
        app.state::<ExitState>().approve();
        app.exit(0);
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
pub fn app_request_exit(app: AppHandle) -> Result<usize, String> {
    request_exit(&app)
}

#[tauri::command]
pub async fn app_confirm_exit(app: AppHandle) -> Result<(), String> {
    prepare_for_exit(&app).await?;
    app.exit(0);
    Ok(())
}

/// 停止所有任务并放行下一次退出或重启事件。
pub async fn prepare_for_exit(app: &AppHandle) -> Result<(), String> {
    let runtime = app.state::<Arc<PeriRuntime>>().inner().clone();
    let session_ids = runtime.active_session_ids();
    for session_id in &session_ids {
        runtime.cancel_pending_for(session_id).await;
        runtime.cancel_session_for_exit(session_id);
        if let Err(error) = runtime
            .send_notification(
                "session/cancel-for-exit",
                json!({ "sessionId": session_id }),
            )
            .await
        {
            runtime.log(
                "error",
                "app.exit",
                format!("取消 Session 通知发送失败 session_id={session_id}: {error:#}"),
            );
        }
        let _ = runtime.set_session_state(session_id, SessionState::Ready);
    }
    app.state::<ExitState>().approve();
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
        state.reset();
        assert!(!state.is_approved());
    }
}
