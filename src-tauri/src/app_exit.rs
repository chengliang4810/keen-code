use std::{
    sync::{
        Arc,
        mpsc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::agent_runtime::AgentRuntime;

/// 活动会话查询允许主线程等待的最长时间；超时说明 Runtime 已无响应，直接强制退出。
const ACTIVE_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// 退出清理允许的最长时间；超时后跳过剩余清理直接结束进程。
const EXIT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
/// 清理放行后 Tauri 事件循环内补充 shutdown 的兜底时长，防止再次卡死阻塞退出。
const EXIT_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(2);

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

/// 根据运行中 Session 决定直接清理退出还是请求用户确认。
///
/// 查询在专用线程上限时执行：Runtime 卡死时既不能永远阻塞主线程，也不能再依赖
/// Runtime 自身回答，此时按用户已明确发起退出处理，直接走强制退出。
pub fn request_exit(app: &AppHandle) -> Result<usize, String> {
    let runtime = app.state::<Arc<AgentRuntime>>().inner().clone();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(runtime.active_session_ids());
    });
    let active_count = match receiver.recv_timeout(ACTIVE_QUERY_TIMEOUT) {
        Ok(Ok(session_ids)) => session_ids.len(),
        Ok(Err(error)) => {
            tracing::error!(%error, "查询活动会话失败，将直接强制退出");
            finalize_exit(app);
            return Ok(0);
        }
        Err(_) => {
            tracing::error!("查询活动会话超时，Runtime 可能已卡死，将直接强制退出");
            finalize_exit(app);
            return Ok(0);
        }
    };
    if active_count == 0 {
        finalize_exit(app);
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
/// 确认停止全部工作并退出应用；同步返回后由后台收尾线程保证进程结束。
///
/// 保持同步命令以在主线程执行：Runtime 的异步线程池被阻塞时（如文件锁列车），
/// 异步命令永远不会被调度，确认按钮将失效。
pub fn app_confirm_exit(app: AppHandle) -> Result<(), String> {
    finalize_exit(&app);
    Ok(())
}

/// 在专用线程中限时执行退出清理，随后无论如何都结束进程。
///
/// 清理超时必须直接结束进程而不是 `app.exit`：未放行的退出事件会被自身的
/// `prevent_exit` 拦截并重新进入 `request_exit`，形成退出循环。
fn finalize_exit(app: &AppHandle) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("keencode-exit".to_owned())
        .spawn(move || {
            let (sender, receiver) = mpsc::channel();
            let cleanup_app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = sender.send(prepare_for_exit(&cleanup_app).await);
            });
            match receiver.recv_timeout(EXIT_CLEANUP_TIMEOUT) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(%error, "退出清理失败，仍继续退出");
                    // 放行退出事件，避免 app.exit 被 prevent_exit 拦截成循环。
                    app.state::<ExitState>().approve();
                }
                Err(_) => {
                    tracing::error!("退出清理超时，强制结束进程");
                    std::process::exit(0);
                }
            }
            let _ = app.exit(0);
        })
        .expect("退出收尾线程应能启动");
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

/// 在已放行的退出事件中补充一次幂等 shutdown；看门狗保证任何卡死都无法阻止退出。
pub fn run_approved_shutdown(runtime: &Arc<AgentRuntime>) {
    std::thread::Builder::new()
        .name("keencode-exit-watchdog".to_owned())
        .spawn(|| {
            std::thread::sleep(EXIT_WATCHDOG_TIMEOUT);
            std::process::exit(0);
        })
        .expect("退出看门狗线程应能启动");
    let _ = tauri::async_runtime::block_on(runtime.shutdown());
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
