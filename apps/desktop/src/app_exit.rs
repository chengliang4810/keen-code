//! NativeHost 退出审批与清理。
//!
//! 退出状态由宿主持有的 Runtime、诊断和资源清理事实驱动。该模块只负责
//! 有界等待、重复请求串行化和退出确认事件，不读取窗口句柄，也不模拟旧 IPC。

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use serde::Serialize;

/// 活动会话查询允许宿主等待的最长时间；超时代表宿主可能已无响应。
const ACTIVE_QUERY_TIMEOUT: Duration = Duration::from_secs(2);
/// 退出清理允许的最长时间；超时后由宿主执行进程级兜底退出。
const EXIT_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
/// 清理放行后执行最终关闭的看门狗时长。
const EXIT_WATCHDOG_TIMEOUT: Duration = Duration::from_secs(2);
/// 退出清理未完成时使用非零状态码，避免把异常退出报告为成功。
const EXIT_FAILURE_CODE: i32 = 1;

/// NativeHost 为退出协调器提供的最小事实接口。
///
/// 具体宿主可把 `prepare_for_exit` 接到自己的 Tokio executor；业务模块因此不
/// 需要持有 executor、窗口对象或任何平台框架句柄。`Arc<NativeHost>` 实现该
/// trait 后即可直接传入本模块的函数。
pub trait NativeHostExit: Send + Sync + 'static {
    /// 返回 Runtime 当前仍有活动根 Turn 的 Session 数量来源。
    fn active_session_ids(&self) -> Result<Vec<String>, String>;
    /// 停止开发进程、Runtime、analytics、诊断和宿主 transport，并等待落盘。
    fn prepare_for_exit(&self) -> Result<(), String>;
    /// 已批准退出后执行最后的幂等宿主关闭。
    fn run_approved_shutdown(&self);
    /// 向 GPUI typed event bus 发布退出确认请求。
    fn emit_exit_requested(&self, payload: ExitRequestedPayload) -> Result<(), String>;
    /// 退出清理失败或超时后的宿主兜底；默认直接结束当前进程。
    fn force_exit(&self, code: i32) -> ! {
        std::process::exit(code)
    }
}

/// 应用退出审批与串行清理状态。
#[derive(Default)]
pub struct ExitState {
    /// 清理完成后允许 NativeHost 放行窗口关闭或更新重启。
    approved: AtomicBool,
    /// 确保退出确认、窗口关闭和更新交接只启动一个清理任务。
    cleanup_started: AtomicBool,
    /// 确保重复的关闭手势只发布一次最终 Quit，并只启动一个看门狗。
    shutdown_dispatched: AtomicBool,
    /// 防止退出确认、安装更新与窗口关闭同时重复清理。
    shutdown_lock: Mutex<()>,
}

impl ExitState {
    /// 标记 Runtime 与本地记录已经完成退出清理。
    pub fn approve(&self) {
        self.approved.store(true, Ordering::Release);
    }

    /// 返回下一次退出请求是否可以直接放行。
    pub fn is_approved(&self) -> bool {
        self.approved.load(Ordering::Acquire)
    }

    fn try_start_cleanup(&self) -> bool {
        self.cleanup_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn try_dispatch_shutdown(&self) -> bool {
        self.shutdown_dispatched
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

/// 主窗口退出请求只携带仍有工作的 Session 数量。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExitRequestedPayload {
    /// 当前至少存在一个运行中根 Turn 的 Session 数量。
    pub active_count: usize,
}

/// 根据 NativeHost 当前 Runtime 事实决定直接清理还是请求 GPUI 确认。
pub fn request_exit<H: NativeHostExit>(
    host: Arc<H>,
    state: Arc<ExitState>,
) -> Result<usize, String> {
    let (sender, receiver) = mpsc::channel();
    let query_host = Arc::clone(&host);
    std::thread::Builder::new()
        .name("keencode-exit-query".to_owned())
        .spawn(move || {
            let _ = sender.send(query_host.active_session_ids());
        })
        .map_err(|error| format!("无法启动退出状态查询线程：{error}"))?;

    let active_count = match receiver.recv_timeout(ACTIVE_QUERY_TIMEOUT) {
        Ok(Ok(session_ids)) => session_ids.len(),
        Ok(Err(error)) => {
            tracing::error!(%error, "查询活动会话失败，将执行 NativeHost 退出清理");
            finalize_exit(host, state);
            return Ok(0);
        }
        Err(_) => {
            tracing::error!("查询活动会话超时，将执行 NativeHost 退出清理");
            finalize_exit(host, state);
            return Ok(0);
        }
    };

    if active_count == 0 {
        finalize_exit(host, state);
    } else {
        host.emit_exit_requested(ExitRequestedPayload { active_count })?;
    }
    Ok(active_count)
}

/// GPUI 用户确认后停止全部工作并放行关闭或更新重启。
pub fn confirm_exit<H: NativeHostExit>(host: Arc<H>, state: Arc<ExitState>) {
    finalize_exit(host, state);
}

/// 在独立线程中限时执行宿主清理，避免关闭路径被 Runtime/文件锁永久阻塞。
fn finalize_exit<H: NativeHostExit>(host: Arc<H>, state: Arc<ExitState>) {
    if !state.try_start_cleanup() {
        return;
    }
    std::thread::Builder::new()
        .name("keencode-exit".to_owned())
        .spawn(move || {
            let _shutdown_guard = state.shutdown_lock.lock().expect("退出清理状态锁已损坏");
            // 清理线程独立于等待线程；超时后不能因为 `scope` 的隐式 join
            // 再次阻塞退出路径。清理线程即使继续收尾，也不会重新取得退出审批。
            let (sender, receiver) = mpsc::channel();
            let cleanup_host = Arc::clone(&host);
            let cleanup = std::thread::Builder::new()
                .name("keencode-exit-cleanup".to_owned())
                .spawn(move || {
                    let _ = sender.send(cleanup_host.prepare_for_exit());
                });
            let result = match cleanup {
                Ok(_) => receiver.recv_timeout(EXIT_CLEANUP_TIMEOUT),
                Err(error) => {
                    tracing::error!(%error, "无法启动 NativeHost 退出清理线程");
                    Ok(Err("无法启动 NativeHost 退出清理线程".to_owned()))
                }
            };
            match result {
                Ok(result) => finish_cleanup(Arc::clone(&host), Arc::clone(&state), result),
                Err(_) => {
                    force_failure_exit(host.as_ref(), "NativeHost 退出清理超时，执行进程级兜底退出")
                }
            }
        })
        .expect("退出收尾线程应能启动");
}

fn finish_cleanup<H: NativeHostExit>(
    host: Arc<H>,
    state: Arc<ExitState>,
    result: Result<(), String>,
) {
    match result {
        Ok(()) => {
            state.approve();
            run_approved_shutdown(host, state);
        }
        Err(error) => {
            force_failure_exit(host.as_ref(), &format!("NativeHost 退出清理失败：{error}"))
        }
    }
}

fn force_failure_exit<H: NativeHostExit>(host: &H, reason: &str) -> ! {
    tracing::error!(reason, code = EXIT_FAILURE_CODE, "NativeHost 退出失败");
    // 进程级退出不会等待异步日志写入；先保留有限脱敏原因，确保原生验收能诊断收尾失败。
    crate::diagnostics::write_stderr_best_effort(format_args!(
        "KeenCode 退出失败：{}",
        keencode_model::redact_error_secrets_bounded(reason, 2048)
    ));
    host.force_exit(EXIT_FAILURE_CODE)
}

/// 已批准的退出事件中补充一次幂等宿主 shutdown；看门狗防止平台资源卡死。
pub fn run_approved_shutdown<H: NativeHostExit>(host: Arc<H>, state: Arc<ExitState>) {
    if !state.is_approved() || !state.try_dispatch_shutdown() {
        return;
    }
    let watchdog_host = Arc::clone(&host);
    std::thread::Builder::new()
        .name("keencode-exit-watchdog".to_owned())
        .spawn(move || {
            std::thread::sleep(EXIT_WATCHDOG_TIMEOUT);
            force_failure_exit(
                watchdog_host.as_ref(),
                "NativeHost 已批准退出但未完成最终关闭，执行进程级兜底退出",
            );
        })
        .expect("退出看门狗线程应能启动");
    host.run_approved_shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Host {
        active: Mutex<Vec<String>>,
        requested: Mutex<Vec<ExitRequestedPayload>>,
        prepared: AtomicBool,
        shutdown_completed: Mutex<Option<mpsc::Sender<()>>>,
    }

    impl Host {
        fn idle() -> Arc<Self> {
            Arc::new(Self {
                active: Mutex::new(Vec::new()),
                requested: Mutex::new(Vec::new()),
                prepared: AtomicBool::new(false),
                shutdown_completed: Mutex::new(None),
            })
        }
    }

    impl NativeHostExit for Host {
        fn active_session_ids(&self) -> Result<Vec<String>, String> {
            Ok(self.active.lock().unwrap().clone())
        }

        fn prepare_for_exit(&self) -> Result<(), String> {
            self.prepared.store(true, Ordering::Release);
            Ok(())
        }

        fn run_approved_shutdown(&self) {
            if let Some(sender) = self.shutdown_completed.lock().unwrap().take() {
                let _ = sender.send(());
            }
        }

        fn emit_exit_requested(&self, payload: ExitRequestedPayload) -> Result<(), String> {
            self.requested.lock().unwrap().push(payload);
            Ok(())
        }

        fn force_exit(&self, _code: i32) -> ! {
            // 测试宿主不能终止测试进程；看门狗线程停在这里即可模拟兜底路径。
            loop {
                std::thread::park();
            }
        }
    }

    struct PanicExitHost {
        forced_code: Mutex<Option<i32>>,
    }

    impl NativeHostExit for PanicExitHost {
        fn active_session_ids(&self) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }

        fn prepare_for_exit(&self) -> Result<(), String> {
            Ok(())
        }

        fn run_approved_shutdown(&self) {}

        fn emit_exit_requested(&self, _payload: ExitRequestedPayload) -> Result<(), String> {
            Ok(())
        }

        fn force_exit(&self, code: i32) -> ! {
            *self.forced_code.lock().unwrap() = Some(code);
            panic!("test force_exit({code})");
        }
    }

    #[test]
    fn exit_is_not_approved_by_default() {
        let state = ExitState::default();
        assert!(!state.is_approved());
        state.approve();
        assert!(state.is_approved());
    }

    #[test]
    fn idle_host_starts_cleanup_without_confirmation() {
        let host = Host::idle();
        let state = Arc::new(ExitState::default());
        let (sender, receiver) = mpsc::channel();
        *host.shutdown_completed.lock().unwrap() = Some(sender);
        assert_eq!(
            request_exit(Arc::clone(&host), Arc::clone(&state)).unwrap(),
            0
        );
        // 等待真实清理完成信号，不能用固定 20ms 假设繁忙 CI 的线程调度速度。
        receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("空闲宿主应完成退出清理");
        assert!(host.prepared.load(Ordering::Acquire));
        assert!(state.is_approved());
    }

    #[test]
    fn active_host_emits_only_active_count() {
        let host = Host::idle();
        host.active.lock().unwrap().extend(["a".into(), "b".into()]);
        let state = Arc::new(ExitState::default());
        assert_eq!(request_exit(Arc::clone(&host), state).unwrap(), 2);
        assert_eq!(
            host.requested.lock().unwrap().as_slice(),
            &[ExitRequestedPayload { active_count: 2 }]
        );
        assert!(!host.prepared.load(Ordering::Acquire));
    }

    #[test]
    fn cleanup_error_forces_nonzero_exit_without_approval() {
        let host = Arc::new(PanicExitHost {
            forced_code: Mutex::new(None),
        });
        let state = Arc::new(ExitState::default());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finish_cleanup(
                Arc::clone(&host),
                Arc::clone(&state),
                Err("test cleanup failure".to_owned()),
            );
        }));
        assert!(result.is_err());
        assert_eq!(*host.forced_code.lock().unwrap(), Some(EXIT_FAILURE_CODE));
        assert!(!state.is_approved());
    }

    #[test]
    fn forced_exit_paths_use_nonzero_failure_code() {
        let host = Arc::new(PanicExitHost {
            forced_code: Mutex::new(None),
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            force_failure_exit(host.as_ref(), "test timeout");
        }));
        assert!(result.is_err());
        assert_eq!(*host.forced_code.lock().unwrap(), Some(EXIT_FAILURE_CODE));
    }
}
