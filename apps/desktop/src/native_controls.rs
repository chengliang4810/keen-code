//! GPUI NativeHost 的窗口、托盘、通知、更新和电源生命周期。
//!
//! 领域模块只定义 typed trait；这里把这些 trait 组合到一个拥有真实平台资源的
//! 对象上。GPUI 的 `App` 不能跨线程保存，因此 UI 线程操作通过启动器注入的回调
//! 投递，Windows 的窗口和 Shell 资源则由本模块直接维护。

use crate::{
    agent_runtime::AgentRuntime,
    app_exit::{self, ExitRequestedPayload, ExitState, NativeHostExit},
    app_settings,
    app_updates::{AppUpdateStatus, NativeUpdateHost, PendingUpdate},
    native_paths::NativePaths,
    native_ui::settings::{NativeKeybindingsState, NativeSettingsUpdatePort},
    native_update_installer::NativeUpdateInstaller,
    power_management::PowerManagement,
    task_notifications::{
        NativeNotificationHost, TaskNotificationPayload, TaskNotificationStatus, TaskNotifications,
        task_notification_show,
    },
    tray::{self, NativeTrayHost, NativeTrayNavigation, TrayAction, TrayMenuPayload},
};
use std::{
    collections::HashSet,
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio::{runtime::Runtime, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use keencode_resources::TurnStopReason;

pub(crate) use crate::native_update_installer::WindowFocusProbe;

/// 原生控制事件不能无限堆积；UI 线程只投递状态变化，消费端负责按顺序投影。
pub const NATIVE_CONTROL_EVENT_CAPACITY: usize = 64;

/// NativeHost 到 GPUI root 的有界事件总线消息。
#[derive(Clone, Debug)]
pub enum NativeControlEvent {
    Show,
    Hide,
    Activate(bool),
    NewChat,
    OpenSession(String),
    /// 后台 Session 终态后重新读取侧栏和未读水位；事件只携带刷新意图。
    RefreshWorkspace,
    ExitRequested(ExitRequestedPayload),
    UpdateStatus(AppUpdateStatus),
    Quit,
    #[cfg(windows)]
    Badge(Option<u32>),
}

pub type NativeControlEventSender = mpsc::Sender<NativeControlEvent>;
pub type NativeControlEventReceiver = mpsc::Receiver<NativeControlEvent>;

/// 安装回调的路径、已验证字节、摘要和版本必须在同一次交接中保持一致。
type VerifiedUpdateInstaller =
    Arc<dyn Fn(&Path, &[u8], &[u8; 32], &str) -> Result<(), String> + Send + Sync>;

/// 创建一次性 NativeHost UI 事件总线；容量固定，避免原生回调拖垮宿主内存。
pub fn native_control_event_channel() -> (NativeControlEventSender, NativeControlEventReceiver) {
    mpsc::channel(NATIVE_CONTROL_EVENT_CAPACITY)
}

/// NativeControls 只保留需要在后台执行的真实平台/Runtime 回调；窗口动作统一走事件总线。
pub struct NativeControlsBackendCallbacks {
    /// 非 Windows 平台的系统通知实现；Windows 使用 NativeControls 自己的 Shell balloon。
    #[cfg(not(windows))]
    pub show_notification: Arc<dyn Fn(&str, &str, bool) -> Result<(), String> + Send + Sync>,
    /// Runtime/工作台退出前的同步收尾。实现方应保证可重复调用。
    pub prepare_for_exit: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    /// 更新安装前停止活动工作和写入。
    pub prepare_for_update: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    /// 将已验证的安装包交给平台安装流程。
    pub install_verified_update: VerifiedUpdateInstaller,
    /// 安装包交接后请求平台重启。
    pub restart_after_update: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    /// 当前窗口是否获得焦点，用于抑制任务完成通知。
    pub has_focused_window: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl NativeControlsBackendCallbacks {
    /// 用 NativeHost 的真实 Runtime、更新交接器和窗口焦点探针装配后台回调。
    ///
    /// `trace_flush` 由启动器注入，因为 Provider trace 的生命周期属于启动器；
    /// 更新缓存仍必须经过 `app_updates` 验签后才能进入 `NativeUpdateInstaller`。
    pub(crate) fn from_host(
        host: Arc<crate::native_host::NativeHost>,
        executor: Arc<tokio::runtime::Runtime>,
        trace_flush: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
        focus_probe: Arc<WindowFocusProbe>,
    ) -> Self {
        let weak_host = Arc::downgrade(&host);
        let installer = NativeUpdateInstaller::new(host.native_paths());
        let prepare_for_exit = shutdown_callback(
            Arc::downgrade(&host),
            Arc::clone(&executor),
            Arc::clone(&trace_flush),
        );
        let prepare_for_update = shutdown_callback(weak_host, executor, trace_flush);
        let install_verified_update = {
            let installer = Arc::clone(&installer);
            Arc::new(
                move |package_path: &Path, bytes: &[u8], sha256: &[u8; 32], signature: &str| {
                    installer.install_verified_update(package_path, bytes, sha256, signature)
                },
            )
        };
        let restart_after_update = {
            let installer = Arc::clone(&installer);
            Arc::new(move || installer.restart_after_update())
        };
        let has_focused_window = Arc::new(move || focus_probe.is_focused());
        Self {
            #[cfg(not(windows))]
            show_notification: Arc::new(show_system_notification),
            prepare_for_exit,
            prepare_for_update,
            install_verified_update,
            restart_after_update,
            has_focused_window,
        }
    }
}

#[cfg(all(unix, not(windows)))]
fn show_system_notification(title: &str, body: &str, sound: bool) -> Result<(), String> {
    let mut notification = notify_rust::Notification::new();
    notification.summary(title).body(body).appname("KeenCode");
    #[cfg(not(target_os = "macos"))]
    {
        notification.hint(notify_rust::Hint::SuppressSound(!sound));
        if sound {
            notification.sound_name("message-new-instant");
        }
    }
    #[cfg(target_os = "macos")]
    if sound {
        notification.sound_name("default");
    }
    notification
        .show()
        .map(|_| ())
        .map_err(|error| format!("发送系统通知失败：{error}"))
}

#[cfg(not(any(windows, unix)))]
fn show_system_notification(_title: &str, _body: &str, _sound: bool) -> Result<(), String> {
    Err("当前平台未装配系统通知实现".to_owned())
}

fn shutdown_callback(
    host: Weak<crate::native_host::NativeHost>,
    executor: Arc<tokio::runtime::Runtime>,
    trace_flush: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Arc<dyn Fn() -> Result<(), String> + Send + Sync> {
    Arc::new(move || {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("keencode-native-shutdown".to_owned())
            .spawn({
                let host = host.clone();
                let executor = Arc::clone(&executor);
                let trace_flush = Arc::clone(&trace_flush);
                move || {
                    let mut errors = Vec::new();
                    if let Some(host) = host.upgrade() {
                        if let Err(error) = executor.block_on(host.shutdown()) {
                            errors.push(error.to_string());
                        }
                    } else {
                        errors.push("NativeHost 已释放，无法执行原生收尾".to_owned());
                    }
                    if let Err(error) = trace_flush() {
                        errors.push(error);
                    }
                    let _ = sender.send(join_errors(errors));
                }
            })
            .map_err(|error| format!("无法启动原生收尾线程：{error}"))?;
        let result = receiver
            .recv()
            .map_err(|_| "原生收尾线程未返回结果".to_owned())?;
        let _ = worker.join();
        result
    })
}

/// 传给 GPUI root 的窗口和后台 lifecycle 回调集合。
///
/// 窗口动作不再由启动器逐个创建闭包，统一通过 `event_sender` 发送到 GPUI root；
/// 这里只保留需要访问 Runtime 或安装器的后台回调。
pub struct NativeControlsCallbacks {
    pub event_sender: NativeControlEventSender,
    #[cfg(not(windows))]
    pub show_notification: Arc<dyn Fn(&str, &str, bool) -> Result<(), String> + Send + Sync>,
    pub prepare_for_exit: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub prepare_for_update: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub install_verified_update: VerifiedUpdateInstaller,
    pub restart_after_update: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    pub has_focused_window: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl NativeControlsCallbacks {
    /// 通过一个 sender 装配后台回调，避免 launch 为 Show/Hide/Session 等动作复制闭包。
    pub fn from_event_sender(
        event_sender: NativeControlEventSender,
        backend: NativeControlsBackendCallbacks,
    ) -> Self {
        Self {
            event_sender,
            #[cfg(not(windows))]
            show_notification: backend.show_notification,
            prepare_for_exit: backend.prepare_for_exit,
            prepare_for_update: backend.prepare_for_update,
            install_verified_update: backend.install_verified_update,
            restart_after_update: backend.restart_after_update,
            has_focused_window: backend.has_focused_window,
        }
    }
}

/// 可选的原生窗口句柄。Windows 由 GPUI 启动器在窗口创建后传入；其他平台忽略。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeWindowHandle(Option<isize>);

impl NativeWindowHandle {
    #[cfg(not(windows))]
    pub const fn none() -> Self {
        Self(None)
    }

    /// 从平台窗口的稳定整数句柄构造；Windows 的 HWND 可安全转换为该表示。
    pub const fn from_raw(value: isize) -> Self {
        Self(Some(value))
    }

    pub const fn raw(self) -> Option<isize> {
        self.0
    }
}

/// NativeHost 原生控制总对象。
#[derive(Clone)]
pub struct NativeControls {
    paths: Arc<NativePaths>,
    runtime: Arc<AgentRuntime>,
    callbacks: Arc<NativeControlsCallbacks>,
    keybindings: NativeKeybindingsState,
    #[cfg(windows)]
    platform: Arc<PlatformTray>,
    power: Arc<PowerManagement>,
    exit_state: Arc<ExitState>,
    pending_update: PendingUpdate,
    started: Arc<AtomicBool>,
    exit_prepared: Arc<AtomicBool>,
}

impl NativeControls {
    /// 只装配依赖，不触碰窗口或托盘；`start` 负责建立平台资源。
    pub fn new(
        paths: Arc<NativePaths>,
        runtime: Arc<AgentRuntime>,
        callbacks: NativeControlsCallbacks,
        window: NativeWindowHandle,
        keybindings: NativeKeybindingsState,
    ) -> Arc<Self> {
        #[cfg(not(windows))]
        let _ = window;
        Arc::new(Self {
            paths,
            runtime,
            callbacks: Arc::new(callbacks),
            keybindings,
            #[cfg(windows)]
            platform: Arc::new(PlatformTray::new(window)),
            power: Arc::new(PowerManagement::new()),
            exit_state: Arc::new(ExitState::default()),
            pending_update: PendingUpdate::default(),
            started: Arc::new(AtomicBool::new(false)),
            exit_prepared: Arc::new(AtomicBool::new(false)),
        })
    }

    /// 启动 Windows 应用身份、系统托盘和所有平台的空闲睡眠抑制。
    #[cfg(windows)]
    pub fn start(&self, menu: TrayMenuPayload) -> Result<(), String> {
        if self.started.swap(true, Ordering::AcqRel) {
            return self.update_menu(menu);
        }
        self.exit_prepared.store(false, Ordering::Release);
        self.platform.set_app_identity()?;
        if let Err(error) = self.platform.install(&menu) {
            self.started.store(false, Ordering::Release);
            return Err(error);
        }
        let keep_awake = self
            .settings()
            .map(|settings| settings.keep_computer_awake)
            .unwrap_or(false);
        if let Err(error) = self
            .power
            .set_keep_awake(keep_awake)
            .map_err(|error| error.to_string())
        {
            let _ = self.platform.remove();
            self.started.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    /// 启动非 Windows 平台的电源策略；托盘菜单仅在具备真实 Shell 后端的平台编译。
    #[cfg(not(windows))]
    pub fn start(&self) -> Result<(), String> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.exit_prepared.store(false, Ordering::Release);
        let keep_awake = self
            .settings()
            .map(|settings| settings.keep_computer_awake)
            .unwrap_or(false);
        if let Err(error) = self
            .power
            .set_keep_awake(keep_awake)
            .map_err(|error| error.to_string())
        {
            self.started.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    /// 更新原生托盘菜单；菜单内容来自当前权威 Session 投影。
    #[cfg(windows)]
    pub fn update_menu(&self, menu: TrayMenuPayload) -> Result<(), String> {
        if !self.started.load(Ordering::Acquire) {
            return Err("NativeControls 尚未启动".to_owned());
        }
        self.platform.update_menu(&menu)
    }

    /// 移除托盘并释放电源抑制；重复调用保持成功。
    pub fn remove(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        if self.started.swap(false, Ordering::AcqRel) {
            #[cfg(windows)]
            if let Err(error) = self.platform.remove() {
                errors.push(error);
            }
        }
        if let Err(error) = self.power.set_keep_awake(false) {
            errors.push(error.to_string());
        }
        join_errors(errors)
    }

    pub fn pending_update(&self) -> &PendingUpdate {
        &self.pending_update
    }

    pub fn keybindings_state(&self) -> NativeKeybindingsState {
        self.keybindings.clone()
    }

    pub fn exit_state(&self) -> Arc<ExitState> {
        Arc::clone(&self.exit_state)
    }

    /// 请求 GPUI 根实体从 Runtime 重新读取工作区投影。
    ///
    /// 后台完成事件不能直接借用 GPUI；通过同一条有界控制总线投递，避免只刷新
    /// 当前会话而遗漏后台 Session 的侧栏和未读状态。
    pub fn refresh_workspace(&self) {
        self.emit_event_lossy(NativeControlEvent::RefreshWorkspace, "RefreshWorkspace");
    }

    /// 将 Windows Shell 的回调消息转换为 GPUI/托盘动作。
    ///
    /// 启动器收到 `tray_callback_message()` 后把 `lparam` 低 32 位传入；菜单右键
    /// 会在平台层取当前鼠标位置并返回稳定的 `TrayAction`。
    #[cfg(windows)]
    pub fn handle_tray_event(
        &self,
        event: u32,
        x: i32,
        y: i32,
    ) -> Result<Option<TrayAction>, String> {
        if event == TRAY_EVENT_LEFT_CLICK || event == TRAY_EVENT_DOUBLE_CLICK {
            tray::handle_action(self, TrayAction::Show)?;
            return Ok(Some(TrayAction::Show));
        }
        if event == TRAY_EVENT_RIGHT_CLICK {
            let Some(command_id) = self.platform.track_menu(x, y)? else {
                return Ok(None);
            };
            return self.handle_menu_command(command_id).map(Some);
        }
        Ok(None)
    }

    /// 供宿主从 `WM_COMMAND` 或测试直接驱动托盘命令。
    #[cfg(windows)]
    pub fn handle_menu_command(&self, command_id: u32) -> Result<TrayAction, String> {
        let id = self
            .platform
            .menu_command(command_id)
            .ok_or_else(|| "未知的 Native 托盘菜单命令".to_owned())?;
        let action =
            tray::classify_menu_id(&id).ok_or_else(|| "Native 托盘菜单项标识无效".to_owned())?;
        tray::handle_action(self, action.clone())?;
        Ok(action)
    }

    /// Windows Shell_NotifyIconW 使用的消息号。
    #[cfg(windows)]
    pub const fn tray_callback_message(&self) -> u32 {
        TRAY_CALLBACK_MESSAGE
    }

    fn settings(&self) -> Result<app_settings::AppSettings, String> {
        app_settings::get(&self.paths).map_err(|error| error.to_string())
    }

    fn prepare_platform_shutdown(&self) -> Result<(), String> {
        self.remove()
    }

    fn emit_event(&self, event: NativeControlEvent) -> Result<(), String> {
        self.callbacks
            .event_sender
            .try_send(event)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => "NativeControl 事件队列已满".to_owned(),
                mpsc::error::TrySendError::Closed(_) => "NativeControl 事件队列已关闭".to_owned(),
            })
    }

    fn emit_event_lossy(&self, event: NativeControlEvent, label: &str) {
        if let Err(error) = self.emit_event(event) {
            tracing::error!(%error, event = label, "投递 NativeControl 事件失败");
        }
    }

    fn show_notification(&self, title: &str, body: &str, sound: bool) -> Result<(), String> {
        #[cfg(windows)]
        {
            let _ = sound;
            self.platform.show_notification(title, body, sound)
        }
        #[cfg(not(windows))]
        {
            (self.callbacks.show_notification)(title, body, sound)
        }
    }
}

/// 原生托盘和角标所需的最小投影；来源必须是当前 NativeHost 的权威工作区读取。
#[derive(Clone, Debug)]
pub(crate) struct NativeProjection {
    pub menu: TrayMenuPayload,
    pub unread_count: u32,
}

/// 由启动器注入的原生投影读取器。闭包实现便于复用启动时已经授权的工作区根路径。
pub(crate) trait NativeProjectionProvider: Send + Sync {
    fn current_projection(&self) -> Result<NativeProjection, String>;
}

impl<F> NativeProjectionProvider for F
where
    F: Fn() -> Result<NativeProjection, String> + Send + Sync,
{
    fn current_projection(&self) -> Result<NativeProjection, String> {
        self()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum PendingNotificationKind {
    Permission,
    Elicitation,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct PendingNotificationKey {
    session_id: String,
    request_id: String,
    kind: PendingNotificationKind,
}

/// Runtime pending/终态到原生通知和托盘投影的唯一中继。
pub(crate) struct NativeNotificationPump {
    cancellation: CancellationToken,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl NativeNotificationPump {
    /// 在启动器提供的 Tokio Runtime 上启动通知订阅；返回值必须由宿主持有到退出。
    pub(crate) fn start(
        runtime: Arc<AgentRuntime>,
        executor: Arc<Runtime>,
        controls: Arc<NativeControls>,
        projection: Arc<dyn NativeProjectionProvider>,
    ) -> Arc<Self> {
        let cancellation = CancellationToken::new();
        let pump = Arc::new(Self {
            cancellation: cancellation.clone(),
            task: Mutex::new(None),
        });
        let task = executor.spawn(run_notification_pump(
            runtime,
            controls,
            projection,
            cancellation,
        ));
        match pump.task.lock() {
            Ok(mut slot) => *slot = Some(task),
            Err(_) => task.abort(),
        }
        pump
    }

    /// 取消订阅并立即停止通知泵；重复调用保持幂等。
    pub(crate) fn stop(&self) {
        self.cancellation.cancel();
        if let Ok(mut task) = self.task.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

impl Drop for NativeNotificationPump {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn run_notification_pump(
    runtime: Arc<AgentRuntime>,
    controls: Arc<NativeControls>,
    projection: Arc<dyn NativeProjectionProvider>,
    cancellation: CancellationToken,
) {
    let mut completions = runtime.subscribe_task_completions();
    let mut permissions = runtime.permissions().subscribe_pending();
    let mut elicitations = runtime.elicitation_coordinator().subscribe_pending();
    let mut retry = tokio::time::interval(Duration::from_secs(1));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut known_sessions = HashSet::new();
    let mut known_pending = HashSet::new();
    let mut notified_pending = HashSet::new();
    if rebuild_pending_sessions(
        runtime.as_ref(),
        controls.as_ref(),
        &mut known_sessions,
        &mut known_pending,
        &mut notified_pending,
    ) {
        refresh_native_projection(controls.as_ref(), projection.as_ref());
    }

    loop {
        // 没有真实 pending 时不轮询；广播或取消信号仍可立即唤醒 select。
        let retry_pending = !known_pending.is_empty();
        tokio::select! {
            _ = cancellation.cancelled() => break,
            result = completions.recv() => match result {
                Ok(notice) => {
                    forget_session_tracking(
                        &notice.session_id,
                        &mut known_sessions,
                        &mut known_pending,
                        &mut notified_pending,
                    );
                    let stop_reason = match notice.status_text {
                        "succeeded" => None,
                        "failed" => Some(TurnStopReason::Failed),
                        "cancelled" => Some(TurnStopReason::Cancelled),
                        status => {
                            tracing::warn!(status, task_id = %notice.task_id, "收到未知任务终态，按失败通知处理");
                            Some(TurnStopReason::Failed)
                        }
                    };
                    TaskNotifications::default().notify_terminal(
                        controls.as_ref(),
                        notice.summary.as_deref(),
                        stop_reason,
                    );
                    refresh_native_projection(controls.as_ref(), projection.as_ref());
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "原生任务通知泵发生广播丢帧");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            result = permissions.recv() => match result {
                Ok(change) => {
                    known_sessions.insert(change.display_session_id.clone());
                    refresh_pending_session(
                        runtime.as_ref(),
                        controls.as_ref(),
                        &change.display_session_id,
                        &mut known_pending,
                        &mut notified_pending,
                    );
                    retain_pending_sessions(&mut known_sessions, &known_pending);
                    refresh_native_projection(controls.as_ref(), projection.as_ref());
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "原生权限通知泵发生广播丢帧");
                    let changed = rebuild_pending_sessions(
                        runtime.as_ref(),
                        controls.as_ref(),
                        &mut known_sessions,
                        &mut known_pending,
                        &mut notified_pending,
                    );
                    if changed {
                        refresh_native_projection(controls.as_ref(), projection.as_ref());
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            result = elicitations.recv() => match result {
                Ok(change) => {
                    known_sessions.insert(change.display_session_id.clone());
                    refresh_pending_session(
                        runtime.as_ref(),
                        controls.as_ref(),
                        &change.display_session_id,
                        &mut known_pending,
                        &mut notified_pending,
                    );
                    retain_pending_sessions(&mut known_sessions, &known_pending);
                    refresh_native_projection(controls.as_ref(), projection.as_ref());
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "原生问答通知泵发生广播丢帧");
                    let changed = rebuild_pending_sessions(
                        runtime.as_ref(),
                        controls.as_ref(),
                        &mut known_sessions,
                        &mut known_pending,
                        &mut notified_pending,
                    );
                    if changed {
                        refresh_native_projection(controls.as_ref(), projection.as_ref());
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            _ = retry.tick(), if retry_pending => {
                let sessions = known_sessions.iter().cloned().collect::<Vec<_>>();
                let mut changed = false;
                for session_id in sessions {
                    changed |= refresh_pending_session(
                        runtime.as_ref(),
                        controls.as_ref(),
                        &session_id,
                        &mut known_pending,
                        &mut notified_pending,
                    );
                }
                retain_pending_sessions(&mut known_sessions, &known_pending);
                if changed {
                    refresh_native_projection(controls.as_ref(), projection.as_ref());
                }
            }
        }
    }
}

fn rebuild_pending_sessions(
    runtime: &AgentRuntime,
    controls: &NativeControls,
    known_sessions: &mut HashSet<String>,
    known_pending: &mut HashSet<PendingNotificationKey>,
    notified_pending: &mut HashSet<PendingNotificationKey>,
) -> bool {
    let mut sessions = runtime.permissions().pending_display_session_ids();
    sessions.extend(
        runtime
            .elicitation_coordinator()
            .pending_display_session_ids(),
    );
    sessions.sort_unstable();
    sessions.dedup();
    let candidate_sessions = sessions.iter().cloned().collect::<HashSet<_>>();
    let mut changed = false;
    for session_id in sessions {
        known_sessions.insert(session_id.clone());
        changed |= refresh_pending_session(
            runtime,
            controls,
            &session_id,
            known_pending,
            notified_pending,
        );
    }
    known_pending.retain(|key| candidate_sessions.contains(&key.session_id));
    notified_pending.retain(|key| candidate_sessions.contains(&key.session_id));
    retain_pending_sessions(known_sessions, known_pending);
    changed
}

fn forget_session_tracking(
    session_id: &str,
    known_sessions: &mut HashSet<String>,
    known_pending: &mut HashSet<PendingNotificationKey>,
    notified_pending: &mut HashSet<PendingNotificationKey>,
) {
    known_sessions.remove(session_id);
    known_pending.retain(|key| key.session_id != session_id);
    notified_pending.retain(|key| key.session_id != session_id);
}

fn retain_pending_sessions(
    known_sessions: &mut HashSet<String>,
    known_pending: &HashSet<PendingNotificationKey>,
) {
    let pending_sessions = known_pending
        .iter()
        .map(|key| key.session_id.as_str())
        .collect::<HashSet<_>>();
    known_sessions.retain(|session_id| pending_sessions.contains(session_id.as_str()));
}

fn refresh_pending_session(
    runtime: &AgentRuntime,
    controls: &NativeControls,
    session_id: &str,
    known_pending: &mut HashSet<PendingNotificationKey>,
    notified_pending: &mut HashSet<PendingNotificationKey>,
) -> bool {
    let Some(connection) = runtime
        .elicitation_coordinator()
        .session_connection(session_id)
    else {
        return false;
    };
    let permissions = match runtime.pending_permission_views(session_id, &connection) {
        Ok(views) => views,
        Err(error) => {
            tracing::debug!(session_id, %error, "读取权限 pending 失败");
            return false;
        }
    };
    let elicitations = match runtime.pending_elicitation_views(session_id, &connection) {
        Ok(views) => views,
        Err(error) => {
            tracing::debug!(session_id, %error, "读取问答 pending 失败");
            return false;
        }
    };
    let mut active = HashSet::new();
    for view in &permissions {
        active.insert(PendingNotificationKey {
            session_id: session_id.to_owned(),
            request_id: view.interaction_id.clone(),
            kind: PendingNotificationKind::Permission,
        });
    }
    for view in &elicitations {
        active.insert(PendingNotificationKey {
            session_id: session_id.to_owned(),
            request_id: view.request_id.clone(),
            kind: PendingNotificationKind::Elicitation,
        });
    }
    let previous = known_pending
        .iter()
        .filter(|key| key.session_id == session_id)
        .cloned()
        .collect::<HashSet<_>>();
    known_pending.retain(|key| key.session_id != session_id || active.contains(key));
    known_pending.extend(active.iter().cloned());
    notified_pending.retain(|key| key.session_id != session_id || active.contains(key));

    for view in permissions {
        let body = if view.summary.trim().is_empty() {
            format!("工具 {} 等待授权", view.tool_name)
        } else {
            view.summary
        };
        notify_pending(
            controls,
            PendingNotificationKey {
                session_id: session_id.to_owned(),
                request_id: view.interaction_id,
                kind: PendingNotificationKind::Permission,
            },
            TaskNotificationStatus::PermissionRequest,
            "需要批准工具操作",
            body,
            notified_pending,
        );
    }
    for view in elicitations {
        let body = match view.questions.len() {
            1 => "有一个问题等待回答".to_owned(),
            count => format!("有 {count} 个问题等待回答"),
        };
        notify_pending(
            controls,
            PendingNotificationKey {
                session_id: session_id.to_owned(),
                request_id: view.request_id,
                kind: PendingNotificationKind::Elicitation,
            },
            TaskNotificationStatus::ElicitationRequest,
            "需要回答问题",
            body,
            notified_pending,
        );
    }
    previous != active
}

fn notify_pending(
    controls: &NativeControls,
    key: PendingNotificationKey,
    status: TaskNotificationStatus,
    title: &str,
    body: String,
    notified_pending: &mut HashSet<PendingNotificationKey>,
) {
    if notified_pending.contains(&key)
        || !controls.task_notifications_enabled()
        || controls.has_focused_window()
    {
        return;
    }
    let payload = TaskNotificationPayload {
        task_id: key.session_id.clone(),
        status,
        request_id: Some(key.request_id.clone()),
        title: title.to_owned(),
        body,
    };
    match task_notification_show(controls, payload) {
        Ok(()) => {
            notified_pending.insert(key);
        }
        Err(error) => {
            tracing::debug!(%error, "发送 pending 原生通知失败");
        }
    }
}

fn refresh_native_projection(
    controls: &NativeControls,
    projection_provider: &dyn NativeProjectionProvider,
) {
    controls.refresh_workspace();
    #[cfg(not(windows))]
    let _ = projection_provider;
    #[cfg(windows)]
    {
        let projection = match projection_provider.current_projection() {
            Ok(projection) => projection,
            Err(error) => {
                tracing::debug!(%error, "读取 Native 工作区投影失败");
                return;
            }
        };
        if let Err(error) = controls.update_menu(projection.menu) {
            tracing::debug!(%error, "更新 Native 托盘菜单失败");
        }
        if let Err(error) = tray::set_badge(controls, projection.unread_count) {
            tracing::debug!(%error, "更新 Native 托盘角标失败");
        }
    }
}

fn join_errors(errors: Vec<String>) -> Result<(), String> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

impl NativeHostExit for NativeControls {
    fn active_session_ids(&self) -> Result<Vec<String>, String> {
        self.runtime
            .active_session_ids()
            .map_err(|error| error.to_string())
    }

    fn prepare_for_exit(&self) -> Result<(), String> {
        self.prepare_platform_shutdown()?;
        if !self.exit_prepared.swap(true, Ordering::AcqRel) {
            (self.callbacks.prepare_for_exit)()?;
        }
        Ok(())
    }

    fn run_approved_shutdown(&self) {
        self.emit_event_lossy(NativeControlEvent::Quit, "Quit");
    }

    fn emit_exit_requested(&self, payload: ExitRequestedPayload) -> Result<(), String> {
        self.emit_event(NativeControlEvent::ExitRequested(payload))
    }
}

impl NativeTrayHost for NativeControls {
    fn show_main_window(&self) {
        self.emit_event_lossy(NativeControlEvent::Show, "Show");
        #[cfg(windows)]
        if let Err(error) = self.platform.show_window() {
            tracing::error!(%error, "显示 Native 主窗口失败");
        }
        self.emit_event_lossy(NativeControlEvent::Activate(true), "Activate");
    }

    #[cfg(windows)]
    fn hide_main_window(&self) -> Result<(), String> {
        let event = self.emit_event(NativeControlEvent::Hide);
        let platform = self.platform.hide_window();
        join_errors(
            [event.err(), platform.err()]
                .into_iter()
                .flatten()
                .collect(),
        )
    }

    #[cfg(windows)]
    fn set_dock_badge(&self, count: Option<u32>) -> Result<(), String> {
        // 平台失败时立即返回，不能继续投递“已成功”的 UI 事件。
        self.platform.set_badge(count)?;
        self.emit_event(NativeControlEvent::Badge(count))
    }

    #[cfg(windows)]
    fn close_to_tray_enabled(&self) -> bool {
        self.settings()
            .map(|settings| settings.close_to_tray)
            .unwrap_or(false)
    }

    fn request_exit(&self) -> Result<(), String> {
        let host = Arc::new(self.clone());
        let state = Arc::clone(&self.exit_state);
        app_exit::request_exit(host, state)?;
        Ok(())
    }
}

impl NativeTrayNavigation for NativeControls {
    fn open_new_chat(&self) {
        self.emit_event_lossy(NativeControlEvent::NewChat, "NewChat");
    }

    fn open_session(&self, session_id: &str) {
        self.emit_event_lossy(
            NativeControlEvent::OpenSession(session_id.to_owned()),
            "OpenSession",
        );
    }
}

impl NativeNotificationHost for NativeControls {
    fn paths(&self) -> &NativePaths {
        &self.paths
    }

    fn runtime(&self) -> &AgentRuntime {
        &self.runtime
    }

    fn task_notifications_enabled(&self) -> bool {
        self.settings()
            .map(|settings| settings.task_notifications)
            .unwrap_or(false)
    }

    fn notification_sound_enabled(&self) -> bool {
        self.settings()
            .map(|settings| settings.notification_sound)
            .unwrap_or(false)
    }

    fn has_focused_window(&self) -> bool {
        (self.callbacks.has_focused_window)()
    }

    fn show_notification(&self, title: &str, body: &str, sound: bool) -> Result<(), String> {
        self.show_notification(title, body, sound)
    }
}

impl NativeUpdateHost for NativeControls {
    fn paths(&self) -> &NativePaths {
        &self.paths
    }

    fn current_version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn download_source(&self) -> app_settings::AppUpdateDownloadSource {
        self.settings()
            .map(|settings| settings.app_update_download_source)
            .unwrap_or_default()
    }

    fn emit_update_status(&self, status: AppUpdateStatus) {
        self.emit_event_lossy(NativeControlEvent::UpdateStatus(status), "UpdateStatus");
    }

    fn prepare_for_update(&self) -> Result<(), String> {
        self.prepare_platform_shutdown()?;
        (self.callbacks.prepare_for_update)()
    }

    fn install_verified_update(
        &self,
        package_path: &Path,
        bytes: &[u8],
        sha256: &[u8; 32],
        signature: &str,
    ) -> Result<(), String> {
        (self.callbacks.install_verified_update)(package_path, bytes, sha256, signature)
    }

    fn restart_after_update(&self) -> Result<(), String> {
        (self.callbacks.restart_after_update)()
    }
}

impl NativeSettingsUpdatePort for NativeControls {
    fn info(&self) -> Result<AppUpdateStatus, String> {
        crate::app_updates::app_update_info(self, self.pending_update())
    }

    fn check(&self) -> Result<AppUpdateStatus, String> {
        crate::app_updates::app_update_check(self, self.pending_update())
    }

    fn download(&self) -> Result<AppUpdateStatus, String> {
        crate::app_updates::app_update_download(self, self.pending_update())
    }

    fn install(&self) -> Result<AppUpdateStatus, String> {
        // app_update_install 负责验签、平台收尾、安装器交接和重启；此处只在
        // 平台回调返回后读取同一 PendingUpdate，绝不以按钮点击伪造安装成功。
        crate::app_updates::app_update_install(self, self.pending_update())?;
        crate::app_updates::app_update_info(self, self.pending_update())
    }
}

#[cfg(windows)]
const TRAY_CALLBACK_MESSAGE: u32 = 0x8000 + 0x4b43;
#[cfg(windows)]
const TRAY_EVENT_LEFT_CLICK: u32 = 0x0202;
#[cfg(windows)]
const TRAY_EVENT_DOUBLE_CLICK: u32 = 0x0203;
#[cfg(windows)]
const TRAY_EVENT_RIGHT_CLICK: u32 = 0x0205;

#[cfg(windows)]
type PlatformMenuHandle = isize;

#[cfg(windows)]
struct PlatformTray {
    hwnd: Option<isize>,
    menu: Mutex<Option<PlatformMenuHandle>>,
    commands: Mutex<std::collections::BTreeMap<u32, String>>,
    installed: AtomicBool,
}

#[cfg(windows)]
impl PlatformTray {
    #[cfg(windows)]
    fn new(window: NativeWindowHandle) -> Self {
        Self {
            hwnd: window.raw(),
            menu: Mutex::new(None),
            commands: Mutex::new(std::collections::BTreeMap::new()),
            installed: AtomicBool::new(false),
        }
    }

    #[cfg(windows)]
    fn set_app_identity(&self) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        const APP_USER_MODEL_ID: &str = "com.keencode.desktop";
        let app_id = APP_USER_MODEL_ID
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: `app_id` is UTF-16、以 NUL 结尾，并在系统调用返回前保持存活。
        let result = unsafe { SetCurrentProcessExplicitAppUserModelID(app_id.as_ptr()) };
        if result < 0 {
            Err(format!(
                "设置 Windows AppUserModelID 失败，HRESULT 0x{:08X}",
                result as u32
            ))
        } else {
            Ok(())
        }
    }

    #[cfg(windows)]
    fn install(&self, menu: &TrayMenuPayload) -> Result<(), String> {
        use std::ptr::null_mut;
        use windows_sys::Win32::UI::{
            Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_SETVERSION, Shell_NotifyIconW},
            WindowsAndMessaging::{IDI_APPLICATION, LoadIconW},
        };
        let hwnd = self.window_handle()?;
        let mut data = notify_data(hwnd);
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.hIcon = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
        copy_wide(&mut data.szTip, "KeenCode");
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } == 0 {
            return Err(last_error("安装 Windows 系统托盘失败"));
        }
        let mut version = data;
        version.Anonymous.uVersion = 4;
        let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &version) };
        self.installed.store(true, Ordering::Release);
        if let Err(error) = self.update_menu(menu) {
            let rollback = self.remove().err();
            return Err(
                join_errors([Some(error), rollback].into_iter().flatten().collect())
                    .err()
                    .unwrap_or_else(|| "安装 Windows 系统托盘失败".to_owned()),
            );
        }
        Ok(())
    }

    #[cfg(windows)]
    fn update_menu(&self, menu: &TrayMenuPayload) -> Result<(), String> {
        use std::ptr::null;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, DestroyMenu, MF_SEPARATOR,
        };
        if !self.installed.load(Ordering::Acquire) {
            return Err("Windows 系统托盘尚未安装".to_owned());
        }
        let new_menu = unsafe { CreatePopupMenu() };
        if new_menu.is_null() {
            return Err(last_error("创建 Windows 托盘菜单失败"));
        }
        let build = (|| -> Result<std::collections::BTreeMap<u32, String>, String> {
            let mut commands = std::collections::BTreeMap::new();
            let entries = [
                (1001_u32, tray::MENU_SHOW, menu.labels.show.as_str()),
                (1002_u32, tray::MENU_NEW_CHAT, menu.labels.new_chat.as_str()),
                (1003_u32, tray::MENU_QUIT, menu.labels.quit.as_str()),
            ];
            for (command, item, label) in entries {
                append_menu_item(new_menu, command, tray::menu_text(label))?;
                commands.insert(command, item.to_owned());
            }
            if !menu.sessions.is_empty()
                && unsafe { AppendMenuW(new_menu, MF_SEPARATOR, 0, null()) } == 0
            {
                return Err(last_error("创建 Windows 托盘菜单分隔符失败"));
            }
            for (offset, session) in menu.sessions.iter().enumerate() {
                let command = 2000_u32
                    .checked_add(u32::try_from(offset).map_err(|_| "托盘会话菜单过多".to_owned())?)
                    .ok_or_else(|| "托盘会话菜单编号溢出".to_owned())?;
                append_menu_item(new_menu, command, tray::menu_text(&session.title))?;
                commands.insert(command, tray::session_menu_id(&session.id));
            }
            Ok(commands)
        })();
        let commands = match build {
            Ok(commands) => commands,
            Err(error) => {
                unsafe { DestroyMenu(new_menu) };
                return Err(error);
            }
        };
        let old = {
            let mut slot = match self.menu.lock() {
                Ok(slot) => slot,
                Err(_) => {
                    unsafe { DestroyMenu(new_menu) };
                    return Err("托盘菜单锁已损坏".to_owned());
                }
            };
            let mut command_slot = match self.commands.lock() {
                Ok(slot) => slot,
                Err(_) => {
                    unsafe { DestroyMenu(new_menu) };
                    return Err("托盘命令锁已损坏".to_owned());
                }
            };
            let old = slot.replace(new_menu as isize);
            *command_slot = commands;
            old
        };
        if let Some(old) = old {
            unsafe { DestroyMenu(old as _) };
        }
        Ok(())
    }

    #[cfg(windows)]
    fn remove(&self) -> Result<(), String> {
        use windows_sys::Win32::UI::{
            Shell::{NIM_DELETE, Shell_NotifyIconW},
            WindowsAndMessaging::DestroyMenu,
        };
        let was_installed = self.installed.swap(false, Ordering::AcqRel);
        let mut errors = Vec::new();
        if was_installed {
            match self.window_handle() {
                Ok(hwnd) => {
                    let data = notify_data(hwnd);
                    if unsafe { Shell_NotifyIconW(NIM_DELETE, &data) } == 0 {
                        errors.push(last_error("移除 Windows 系统托盘失败"));
                    }
                }
                Err(error) => errors.push(error),
            }
        }
        if let Some(menu) = self
            .menu
            .lock()
            .map_err(|_| "托盘菜单锁已损坏".to_owned())?
            .take()
        {
            unsafe { DestroyMenu(menu as _) };
        }
        self.commands
            .lock()
            .map_err(|_| "托盘命令锁已损坏".to_owned())?
            .clear();
        join_errors(errors)
    }

    #[cfg(windows)]
    fn menu_command(&self, command: u32) -> Option<String> {
        self.commands.lock().ok()?.get(&command).cloned()
    }

    #[cfg(windows)]
    fn track_menu(&self, x: i32, y: i32) -> Result<Option<u32>, String> {
        use std::ptr::null;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
        };
        let hwnd = self.window_handle()?;
        let menu = self
            .menu
            .lock()
            .map_err(|_| "托盘菜单锁已损坏".to_owned())?
            .ok_or_else(|| "Windows 托盘菜单尚未创建".to_owned())?;
        unsafe {
            SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu as _,
                TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
                x,
                y,
                0,
                hwnd,
                null(),
            );
            Ok((command != 0).then_some(
                u32::try_from(command).map_err(|_| "Windows 托盘命令编号无效".to_owned())?,
            ))
        }
    }

    #[cfg(windows)]
    fn show_window(&self) -> Result<(), String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
        };
        let hwnd = self.window_handle()?;
        unsafe {
            ShowWindow(hwnd, SW_RESTORE);
            ShowWindow(hwnd, SW_SHOW);
            if SetForegroundWindow(hwnd) == 0 {
                return Err(last_error("激活 Windows 主窗口失败"));
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    fn hide_window(&self) -> Result<(), String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
        let hwnd = self.window_handle()?;
        if unsafe { ShowWindow(hwnd, SW_HIDE) } == 0 {
            // ShowWindow 返回值表示原可见状态，不表示调用失败；这里只需保证句柄有效。
        }
        Ok(())
    }

    #[cfg(windows)]
    fn show_notification(&self, title: &str, body: &str, sound: bool) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::{
            NIF_INFO, NIIF_INFO, NIIF_NOSOUND, NIM_MODIFY, Shell_NotifyIconW,
        };
        if !self.installed.load(Ordering::Acquire) {
            return Err("Windows 系统托盘尚未安装，无法发送通知".to_owned());
        }
        let hwnd = self.window_handle()?;
        let mut data = notify_data(hwnd);
        data.uFlags = NIF_INFO;
        copy_wide(&mut data.szInfo, body);
        copy_wide(&mut data.szInfoTitle, title);
        data.dwInfoFlags = if sound {
            NIIF_INFO
        } else {
            NIIF_INFO | NIIF_NOSOUND
        };
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } == 0 {
            Err(last_error("发送 Windows 托盘通知失败"))
        } else {
            Ok(())
        }
    }

    #[cfg(windows)]
    fn set_badge(&self, count: Option<u32>) -> Result<(), String> {
        use windows_sys::Win32::UI::Shell::{NIF_TIP, NIM_MODIFY, Shell_NotifyIconW};
        if !self.installed.load(Ordering::Acquire) {
            return Err("Windows 系统托盘尚未安装，无法设置角标提示".to_owned());
        }
        let hwnd = self.window_handle()?;
        let mut data = notify_data(hwnd);
        data.uFlags = NIF_TIP;
        copy_wide(
            &mut data.szTip,
            &count.map_or_else(
                || "KeenCode".to_owned(),
                |value| format!("KeenCode ({value})"),
            ),
        );
        if unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } == 0 {
            Err(last_error("更新 Windows 托盘角标提示失败"))
        } else {
            Ok(())
        }
    }

    #[cfg(windows)]
    fn window_handle(&self) -> Result<windows_sys::Win32::Foundation::HWND, String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;
        let value = self
            .hwnd
            .ok_or_else(|| "NativeControls 缺少 Windows 主窗口句柄".to_owned())?;
        let hwnd = value as windows_sys::Win32::Foundation::HWND;
        if unsafe { IsWindow(hwnd) } == 0 {
            return Err("Windows 主窗口句柄无效".to_owned());
        }
        Ok(hwnd)
    }
}

#[cfg(windows)]
fn notify_data(
    hwnd: windows_sys::Win32::Foundation::HWND,
) -> windows_sys::Win32::UI::Shell::NOTIFYICONDATAW {
    windows_sys::Win32::UI::Shell::NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<windows_sys::Win32::UI::Shell::NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 0x4b43,
        uCallbackMessage: TRAY_CALLBACK_MESSAGE,
        ..Default::default()
    }
}

#[cfg(windows)]
fn copy_wide<const N: usize>(destination: &mut [u16; N], text: &str) {
    for (slot, value) in destination
        .iter_mut()
        .zip(text.encode_utf16().take(N.saturating_sub(1)))
    {
        *slot = value;
    }
}

#[cfg(windows)]
fn append_menu_item(
    menu: windows_sys::Win32::UI::WindowsAndMessaging::HMENU,
    id: u32,
    text: String,
) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{AppendMenuW, MF_STRING};
    let wide = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe { AppendMenuW(menu, MF_STRING, id as usize, wide.as_ptr()) } == 0 {
        Err(last_error("添加 Windows 托盘菜单项失败"))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn last_error(action: &str) -> String {
    let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    format!("{action}，Win32 错误码 {code}")
}

#[cfg(test)]
mod tests {
    use super::NativeWindowHandle;

    #[test]
    fn native_window_handle_preserves_raw_value() {
        assert_eq!(NativeWindowHandle::from_raw(42).raw(), Some(42));
    }
}
