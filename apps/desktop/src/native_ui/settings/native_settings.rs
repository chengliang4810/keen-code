//! NativeHost 设置服务适配器。
//!
//! 这里是 GPUI 设置面板与既有 Rust domain 的唯一装配点：常规设置和 Provider
//! 配置直接调用现有本地服务，资源、自动化、工作流和 Agent 则通过宿主注入的
//! `NativeSettingsDomain` 调用。这样 UI 不会为了方便而复制 providers、Journal
//! 或 scheduler 的事实源。

use super::contracts::*;
use super::native_keybindings::{
    NativeKeybindingsState, read as read_keybindings, write as write_keybindings,
};
use crate::agent_runtime::AgentRuntime;
use crate::analytics::RequestRecordsQuery;
use crate::app_settings;
use crate::model_metadata::{self, ModelReasoningControl};
use crate::native_insights::{DiagnosticsSnapshot, NativeInsights, UsageSnapshot};
use crate::native_memory::NativeMemoryCoordinator;
use crate::native_paths::NativePaths;
use crate::personalization;
use crate::providers::{self, ProviderUpsert};
use crate::storage;
use keencode_provider::ChatOutputTokenField;
use keencode_resources::SessionEvent;
use keencode_runtime::{RuntimeEventDelivery, RuntimeEventPayload, RuntimeEventReceiveError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use tokio::sync::{Mutex as AsyncMutex, broadcast};

const GENERAL_FILE_SCHEMA: &str = "keencode/native-general-settings";
const GENERAL_FILE_VERSION: u32 = 2;
const MAX_GENERAL_FILE_BYTES: u64 = 64 * 1024;
const SETTINGS_EVENT_CAPACITY: usize = 64;

fn publish_invalidation(events: &broadcast::Sender<SettingsEvent>, page: SettingsPage) {
    match page {
        SettingsPage::General | SettingsPage::Appearance => {
            let _ = events.send(SettingsEvent::Invalidate(SettingsPage::General));
            let _ = events.send(SettingsEvent::Invalidate(SettingsPage::Appearance));
        }
        SettingsPage::Keyboard => {
            let _ = events.send(SettingsEvent::Invalidate(SettingsPage::Keyboard));
        }
        page => {
            let _ = events.send(SettingsEvent::Invalidate(page));
        }
    }
}

/// 原生外观偏好保存在 NativeHost 的严格小文件中；Provider、AppSettings 和
/// Workflow 使用各自领域的权威文件格式。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct GeneralFile {
    schema: String,
    version: u32,
    appearance: AppearanceMode,
    font_family: String,
    font_size_px: f32,
    density: UiDensity,
    reduced_motion: bool,
    code: CodeAppearanceSettings,
}

impl Default for GeneralFile {
    fn default() -> Self {
        let defaults = GeneralSettings::default();
        Self {
            schema: GENERAL_FILE_SCHEMA.to_owned(),
            version: GENERAL_FILE_VERSION,
            appearance: defaults.appearance,
            font_family: defaults.font_family,
            font_size_px: defaults.font_size_px,
            density: defaults.density,
            reduced_motion: defaults.reduced_motion,
            code: defaults.code,
        }
    }
}

impl GeneralFile {
    fn validate(&self) -> SettingsResult<()> {
        if self.schema != GENERAL_FILE_SCHEMA || self.version != GENERAL_FILE_VERSION {
            return Err(error(
                "settings_general_schema",
                "常规设置格式版本不受支持",
                false,
            ));
        }
        if !self.font_size_px.is_finite() || !(11.0..=20.0).contains(&self.font_size_px) {
            return Err(error(
                "settings_general_font_size",
                "字体大小必须在 11 到 20 像素之间",
                false,
            ));
        }
        if self.font_family.trim().is_empty()
            || self.font_family.len() > 256
            || self.font_family.chars().any(char::is_control)
        {
            return Err(error("settings_general_font", "字体族无效", false));
        }
        if !self.code.font_size_px.is_finite() || !(12.0..=20.0).contains(&self.code.font_size_px) {
            return Err(error(
                "settings_general_code_font_size",
                "代码字号必须在 12 到 20 像素之间",
                false,
            ));
        }
        if self.code.font_family.trim().is_empty()
            || self.code.font_family.len() > 256
            || self.code.font_family.chars().any(char::is_control)
        {
            return Err(error("settings_general_code_font", "代码字体族无效", false));
        }
        Ok(())
    }
}

/// 由 NativeHost 注入的资源和运行时 domain。
///
/// 这些操作可能涉及 scheduler、WorkflowHost、插件管理器或 Agent Journal，不能
/// 在设置 UI 中重新读取目录来模拟成功。实现应在返回快照前完成真实提交，并把
/// 错误转换为不含凭据和用户正文的 [`NativeSettingsError`]。
pub trait NativeSettingsDomain: Send + Sync {
    fn load(&self, page: SettingsPage) -> SettingsResult<SettingsSnapshot>;
    fn execute(&self, command: SettingsCommand) -> SettingsResult<SettingsSnapshot>;
}

/// NativeHost 为设置页注入的真实运行时端口。
///
/// Automation、Workflow 和子智能体的事实分别由 scheduler、WorkflowHost/Journal
/// 与 AgentRuntime 持有。设置域只负责把 typed command 转交给端口，避免把文件台账
/// 或静态投影误当成一次已经执行的运行。
pub trait NativeSettingsRuntimePort: Send + Sync {
    /// NativeHost 完成 Runtime 装配后调用一次；端口应在这里启动 scheduler 等后台任务。
    fn start(&self) -> SettingsResult<()>;

    /// NativeHost 关闭前调用一次；实现必须幂等并等待后台任务完成或取消。
    fn shutdown(&self);

    /// 加载由运行时持有事实的设置页。
    fn load(&self, page: SettingsPage) -> SettingsResult<SettingsSnapshot>;

    /// 执行由运行时持有事实的设置命令，并返回新的确认投影。
    fn execute(&self, command: SettingsCommand) -> SettingsResult<SettingsSnapshot>;

    /// 恢复指定的 Workflow run；实现必须从 Journal 的冻结事实恢复，不能接受页面参数替换。
    fn resume_workflow(&self, run_id: &str) -> SettingsResult<SettingsSnapshot>;

    /// 把设置页 typed 问答答案转交给当前父 Session 的 WorkflowHost。
    fn resolve_workflow_question(
        &self,
        question_id: &str,
        answer: &str,
    ) -> SettingsResult<SettingsSnapshot>;
}

/// NativeHost 注入的更新控制端口。
///
/// 端口只暴露已验证的更新领域操作；设置 UI 不持有 `PendingUpdate`，也不能绕过
/// `app_updates` 的签名校验、安装前收尾或平台重启流程。宿主尚未注入时，更新命令
/// 必须返回明确错误，不能以本地状态伪造成功。
pub trait NativeSettingsUpdatePort: Send + Sync {
    fn info(&self) -> Result<crate::app_updates::AppUpdateStatus, String>;
    fn check(&self) -> Result<crate::app_updates::AppUpdateStatus, String>;
    fn download(&self) -> Result<crate::app_updates::AppUpdateStatus, String>;
    fn install(&self) -> Result<crate::app_updates::AppUpdateStatus, String>;
}

/// NativeInsights 的最小设置端口；适配器只保存弱引用，避免把诊断和分析服务
/// 的生命周期绑定到设置面板或窗口实体。
pub trait NativeSettingsInsightsPort: Send + Sync {
    fn usage(&self, query: RequestRecordsQuery) -> Result<UsageSnapshot, String>;
    fn diagnostics(&self) -> DiagnosticsSnapshot;
    fn export_diagnostics(&self) -> Result<String, String>;
}

impl NativeSettingsInsightsPort for NativeInsights {
    fn usage(&self, query: RequestRecordsQuery) -> Result<UsageSnapshot, String> {
        NativeInsights::usage(self, query)
    }

    fn diagnostics(&self) -> DiagnosticsSnapshot {
        NativeInsights::diagnostics(self)
    }

    fn export_diagnostics(&self) -> Result<String, String> {
        NativeInsights::export_diagnostics(self)
    }
}

/// 生产设置服务。`paths` 和 `runtime` 与 NativeHost 共用同一实例，避免在后台
/// 线程重新发现 HOME 或创建第二个 ProviderRegistry。
pub struct NativeSettingsAdapter {
    paths: Arc<NativePaths>,
    runtime: Arc<AgentRuntime>,
    domain: Arc<dyn NativeSettingsDomain>,
    runtime_handle: tokio::runtime::Handle,
    // 设置操作与 NativeHost 的 Session 打开/订阅/空闲回收共用准入锁，避免
    // WorkflowHost 在旧 RuntimeSession 即将释放时捕获同一 Session 的悬空快照。
    operation_gate: Arc<tokio::sync::Mutex<()>>,
    revisions: Arc<AtomicU64>,
    events: broadcast::Sender<SettingsEvent>,
    keybindings: NativeKeybindingsState,
    update_port: Arc<OnceLock<Weak<dyn NativeSettingsUpdatePort>>>,
    insights: Arc<OnceLock<Arc<dyn NativeSettingsInsightsPort>>>,
    memory: Arc<OnceLock<Weak<NativeMemoryCoordinator>>>,
}

impl NativeSettingsAdapter {
    /// 使用 NativeHost 的共享准入锁构造设置适配器。
    ///
    /// 启动器和测试必须传入与对应生命周期共享的准入锁。
    pub fn new_with_operation_gate(
        paths: Arc<NativePaths>,
        runtime: Arc<AgentRuntime>,
        domain: Arc<dyn NativeSettingsDomain>,
        runtime_handle: tokio::runtime::Handle,
        operation_gate: Arc<tokio::sync::Mutex<()>>,
    ) -> Arc<Self> {
        let (events, _) = broadcast::channel(SETTINGS_EVENT_CAPACITY);
        Arc::new(Self {
            paths,
            runtime,
            domain,
            runtime_handle,
            operation_gate,
            revisions: Arc::new(AtomicU64::new(0)),
            events,
            keybindings: NativeKeybindingsState::new(KeyboardSettings::default()),
            update_port: Arc::new(OnceLock::new()),
            insights: Arc::new(OnceLock::new()),
            memory: Arc::new(OnceLock::new()),
        })
    }

    pub fn paths(&self) -> &Arc<NativePaths> {
        &self.paths
    }

    pub fn runtime(&self) -> &Arc<AgentRuntime> {
        &self.runtime
    }

    /// 返回窗口控制和 Composer 共用的快捷键快照；消费者只持有这个轻量句柄，
    /// 不会把设置适配器或页面实体引用进全局 keydown 回调。
    pub fn keybindings_state(&self) -> NativeKeybindingsState {
        self.keybindings.clone()
    }

    /// NativeHost 创建 GPUI 窗口前加载快捷键。读取失败时启动失败，避免窗口以
    /// 部分旧配置运行；缺少文件则由 `read_keybindings` 返回默认值。
    pub fn load_keybindings(&self) -> SettingsResult<KeyboardSettings> {
        let settings = read_keybindings(&self.paths)
            .map_err(|message| error("settings_keybindings_read", message, false))?;
        self.keybindings.replace(settings.clone());
        Ok(settings)
    }

    /// 在 NativeControls 创建完成后装配更新端口。一次性装配可以避免窗口重建时
    /// 把更新状态机切换到另一份 PendingUpdate；装配成功后让已打开的设置页重新读取。
    pub fn attach_update_port(
        &self,
        port: Arc<dyn NativeSettingsUpdatePort>,
    ) -> SettingsResult<()> {
        self.update_port
            .set(Arc::downgrade(&port))
            .map_err(|_| error("settings_update_port_exists", "更新控制端口已经装配", false))?;
        publish_invalidation(&self.events, SettingsPage::General);
        Ok(())
    }

    /// 装配 NativeInsights；未装配时 Usage/Diagnostics 必须返回明确错误。
    pub fn attach_insights(&self, insights: Arc<NativeInsights>) -> SettingsResult<()> {
        let insights: Arc<dyn NativeSettingsInsightsPort> = insights;
        self.insights
            .set(insights)
            .map_err(|_| error("settings_insights_exists", "NativeInsights 已经装配", false))?;
        let _ = self
            .events
            .send(SettingsEvent::Invalidate(SettingsPage::Usage));
        let _ = self
            .events
            .send(SettingsEvent::Invalidate(SettingsPage::Diagnostics));
        Ok(())
    }

    /// 装配本地记忆协调器；常规页切换记忆开关时必须同时更新 Runtime 与调度器。
    pub(crate) fn attach_memory(
        &self,
        memory: &Arc<NativeMemoryCoordinator>,
    ) -> SettingsResult<()> {
        self.memory
            .set(Arc::downgrade(memory))
            .map_err(|_| error("settings_memory_exists", "本地记忆协调器已经装配", false))?;
        publish_invalidation(&self.events, SettingsPage::General);
        Ok(())
    }

    /// NativeHost 创建首个 GPUI 窗口前读取已持久化的常规设置。
    ///
    /// 启动阶段没有设置面板实体可通过异步 service 加载，因此提供同一适配器
    /// 使用的同步投影入口；它只读取常规设置，不会启动或复制任何运行时状态。
    pub fn load_general_settings(&self) -> SettingsResult<GeneralSettings> {
        let snapshot = load_general(&self.paths, SettingsPage::General, &self.revisions, None)?;
        snapshot
            .general
            .ok_or_else(|| error("settings_general_missing", "常规设置投影为空", false))
    }

    /// 由 NativeHost 在 scheduler、Journal 或扩展状态实际变化后发布事件。
    pub fn publish(&self, event: SettingsEvent) {
        match event {
            SettingsEvent::Invalidate(page) => publish_invalidation(&self.events, page),
            SettingsEvent::Snapshot(snapshot)
                if matches!(
                    snapshot.page,
                    SettingsPage::General | SettingsPage::Appearance
                ) =>
            {
                publish_invalidation(&self.events, snapshot.page);
                let _ = self.events.send(SettingsEvent::Snapshot(snapshot));
            }
            event => {
                let _ = self.events.send(event);
            }
        }
    }

    pub fn invalidate(&self, page: SettingsPage) {
        self.publish(SettingsEvent::Invalidate(page));
    }

    /// 观察当前聚焦 Session 的 Runtime 事实，并让 Agents 页重新读取 Goal 与
    /// root Turn 屏障。Goal 本身存放在独立文件中，因此同时消费 Runtime 的
    /// Session 事件和 AgentRuntime 的 Goal 变化通知。
    pub fn start_runtime_event_bridge(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let runtime = Arc::downgrade(&self.runtime);
        let runtime_handle = self.runtime_handle.clone();
        runtime_handle.spawn(async move {
            let Some(runtime_ref) = runtime.upgrade() else {
                return;
            };
            let mut focus_changes = runtime_ref.subscribe_focus_changes();
            let mut focused_session_id = focus_changes.borrow().clone();
            let mut session_events = focused_session_id
                .as_deref()
                .and_then(|session_id| runtime_ref.subscribe_session_events(session_id).ok());
            let mut goal_changes = runtime_ref.subscribe_goal_changes();
            drop(runtime_ref);

            if focused_session_id.is_some() {
                if let Some(adapter) = weak.upgrade() {
                    adapter.invalidate(SettingsPage::Agents);
                } else {
                    return;
                }
            }

            enum BridgeWake {
                FocusChanged(Option<String>),
                SessionEvent(
                    Option<Result<bool, RuntimeEventReceiveError>>,
                ),
                Continue,
                Closed,
            }

            loop {
                let Some(runtime_ref) = runtime.upgrade() else {
                    break;
                };
                if runtime_ref.is_closed() || weak.upgrade().is_none() {
                    break;
                }
                drop(runtime_ref);

                let wake = {
                    // 将借用订阅句柄的 future 限定在 select 块内，关闭或切换 Session
                    // 后即可在外层安全丢弃旧订阅并重新绑定。
                    let session_event = async {
                        if let Some(source) = session_events.as_mut() {
                            Some(source.recv().await)
                        } else {
                            std::future::pending::<
                                Option<
                                    Result<RuntimeEventDelivery, RuntimeEventReceiveError>,
                                >,
                            >()
                            .await
                        }
                    };
                    tokio::select! {
                        focus_change = focus_changes.changed() => {
                            match focus_change {
                                Ok(()) => BridgeWake::FocusChanged(
                                    focus_changes.borrow_and_update().clone(),
                                ),
                                Err(_) => BridgeWake::Closed,
                            }
                        }
                        goal_change = goal_changes.recv() => {
                            match goal_change {
                                Ok(change)
                                    if focused_session_id.as_deref() == Some(change.session_id.as_str()) =>
                                {
                                    if let Some(adapter) = weak.upgrade() {
                                        adapter.invalidate(SettingsPage::Agents);
                                        BridgeWake::Continue
                                    } else {
                                        BridgeWake::Closed
                                    }
                                }
                                Ok(_) => BridgeWake::Continue,
                                Err(broadcast::error::RecvError::Lagged(_)) => {
                                    if focused_session_id.is_some()
                                        && let Some(adapter) = weak.upgrade()
                                    {
                                        adapter.invalidate(SettingsPage::Agents);
                                    }
                                    BridgeWake::Continue
                                }
                                Err(broadcast::error::RecvError::Closed) => BridgeWake::Closed,
                            }
                        }
                        runtime_event = session_event => BridgeWake::SessionEvent(
                            runtime_event.map(|event| event.map(|delivery| {
                                runtime_event_affects_agents(&delivery.payload)
                            })),
                        ),
                    }
                };
                match wake {
                    BridgeWake::FocusChanged(current_focus) => {
                        let Some(runtime_ref) = runtime.upgrade() else {
                            break;
                        };
                        if runtime_ref.is_closed() {
                            break;
                        }
                        focused_session_id = current_focus;
                        session_events = focused_session_id
                            .as_deref()
                            .and_then(|session_id| {
                                runtime_ref.subscribe_session_events(session_id).ok()
                            });
                        drop(runtime_ref);
                        if let Some(adapter) = weak.upgrade() {
                            adapter.invalidate(SettingsPage::Agents);
                        } else {
                            break;
                        }
                    }
                    BridgeWake::SessionEvent(runtime_event) => match runtime_event {
                        Some(Ok(true)) => {
                            let Some(runtime_ref) = runtime.upgrade() else {
                                break;
                            };
                            let is_focused = !runtime_ref.is_closed()
                                && runtime_ref
                                    .focused_session_id()
                                    .ok()
                                    .flatten()
                                    .as_deref()
                                    == focused_session_id.as_deref();
                            drop(runtime_ref);
                            if is_focused {
                                if let Some(adapter) = weak.upgrade() {
                                    adapter.invalidate(SettingsPage::Agents);
                                } else {
                                    break;
                                }
                            }
                        }
                        Some(Ok(false)) | None => {}
                        Some(Err(RuntimeEventReceiveError::Lagged(_))) => {
                            // 丢批意味着可能漏掉 TurnCompleted/TurnStopped；重新读取 Agents
                            // 快照才能恢复 root Turn 屏障，而不是继续显示旧的运行状态。
                            if focused_session_id.is_some()
                                && let Some(adapter) = weak.upgrade()
                            {
                                adapter.invalidate(SettingsPage::Agents);
                            }
                        }
                        Some(Err(RuntimeEventReceiveError::Closed)) => {
                            session_events = None;
                        }
                    },
                    BridgeWake::Continue => {}
                    BridgeWake::Closed => break,
                }
            }
        });
    }

    fn next_revision(revisions: &AtomicU64) -> u64 {
        revisions.fetch_add(1, Ordering::AcqRel).saturating_add(1)
    }
}

fn runtime_event_affects_agents(payload: &RuntimeEventPayload) -> bool {
    let RuntimeEventPayload::Authoritative(record) = payload else {
        return false;
    };
    session_event_affects_agents(&record.event)
}

fn session_event_affects_agents(event: &SessionEvent) -> bool {
    match event {
        SessionEvent::AtomicBatch { events } => events.iter().any(session_event_affects_agents),
        SessionEvent::TurnStarted { .. }
        | SessionEvent::TurnCompleted { .. }
        | SessionEvent::TurnStopped { .. } => true,
        _ => false,
    }
}

/// 订阅句柄在关闭后不再等待 domain 事件；Panel 销毁时可安全重复 close。
pub struct NativeSettingsSubscriptionHandle {
    receiver: Arc<AsyncMutex<broadcast::Receiver<SettingsEvent>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    wake: broadcast::Sender<SettingsEvent>,
}

impl NativeSettingsSubscription for NativeSettingsSubscriptionHandle {
    fn next(&self) -> SettingsFuture<Option<SettingsEvent>> {
        let receiver = Arc::clone(&self.receiver);
        let closed = Arc::clone(&self.closed);
        Box::pin(async move {
            if closed.load(Ordering::Acquire) {
                return None;
            }
            let mut receiver = receiver.lock().await;
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if closed.load(Ordering::Acquire) {
                            return None;
                        }
                        return Some(event);
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        })
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.wake.send(SettingsEvent::Notice(SettingsNotice {
            level: NoticeLevel::Info,
            code: "settings_subscription_closed".to_owned(),
            message: "设置窗口已关闭".to_owned(),
        }));
    }
}

impl NativeSettingsService for NativeSettingsAdapter {
    fn load(&self, page: SettingsPage) -> SettingsFuture<SettingsResult<SettingsSnapshot>> {
        let paths = Arc::clone(&self.paths);
        let runtime = Arc::clone(&self.runtime);
        let domain = Arc::clone(&self.domain);
        let revision = Arc::clone(&self.revisions);
        let update_port = Arc::clone(&self.update_port);
        let insights = Arc::clone(&self.insights);
        let keybindings = self.keybindings.clone();
        let operation_gate = Arc::clone(&self.operation_gate);
        let runtime_handle = self.runtime_handle.clone();
        Box::pin(async move {
            // 供应商、常规和更新页面不绑定 RuntimeSession；网络操作不能占住
            // NativeHost 的 Session admission，保证 Stop/审批仍能及时执行。
            let _operation = if settings_page_requires_operation_gate(page) {
                Some(operation_gate.lock_owned().await)
            } else {
                None
            };
            runtime_handle
                .spawn_blocking(move || {
                    load_page(
                        &paths,
                        &runtime,
                        domain.as_ref(),
                        page,
                        &revision,
                        &update_port,
                        &insights,
                        &keybindings,
                    )
                })
                .await
                .map_err(|error| {
                    NativeSettingsError::new(
                        "settings_worker_join",
                        format!("设置 domain 工作线程异常退出：{error}"),
                    )
                    .retryable(true)
                })?
        })
    }

    fn execute(
        &self,
        command: SettingsCommand,
    ) -> SettingsFuture<SettingsResult<SettingsSnapshot>> {
        let paths = Arc::clone(&self.paths);
        let runtime = Arc::clone(&self.runtime);
        let domain = Arc::clone(&self.domain);
        let revision = Arc::clone(&self.revisions);
        let events = self.events.clone();
        let update_port = Arc::clone(&self.update_port);
        let insights = Arc::clone(&self.insights);
        let keybindings = self.keybindings.clone();
        let memory = Arc::clone(&self.memory);
        let operation_gate = Arc::clone(&self.operation_gate);
        let runtime_handle = self.runtime_handle.clone();
        let requires_operation_gate = settings_command_requires_operation_gate(&command);
        let invalidates_shared_general = matches!(
            &command,
            SettingsCommand::SaveGeneral(_)
                | SettingsCommand::SaveAppearance(_)
                | SettingsCommand::SaveCustomInstructions { .. }
                | SettingsCommand::CheckForUpdates
                | SettingsCommand::DownloadUpdate
                | SettingsCommand::InstallUpdate
        );
        Box::pin(async move {
            // 只为会读取/持有当前 Session、WorkflowHost 或 Agent 状态的命令
            // 排队；Provider catalog、模型连通性和更新网络请求保持可并行。
            let _operation = if requires_operation_gate {
                Some(operation_gate.lock_owned().await)
            } else {
                None
            };
            let result = runtime_handle
                .spawn_blocking(move || {
                    execute_command(
                        &paths,
                        &runtime,
                        domain.as_ref(),
                        command,
                        &revision,
                        &update_port,
                        &insights,
                        &keybindings,
                        &memory,
                    )
                })
                .await
                .map_err(|error| {
                    NativeSettingsError::new(
                        "settings_worker_join",
                        format!("设置 domain 工作线程异常退出：{error}"),
                    )
                    .retryable(true)
                })?;
            if result.is_ok() && invalidates_shared_general {
                publish_invalidation(&events, SettingsPage::General);
            }
            if let Ok(snapshot) = &result {
                let _ = events.send(SettingsEvent::Snapshot(Box::new(snapshot.clone())));
            }
            result
        })
    }

    fn subscribe(&self) -> SettingsResult<Arc<dyn NativeSettingsSubscription>> {
        Ok(Arc::new(NativeSettingsSubscriptionHandle {
            receiver: Arc::new(AsyncMutex::new(self.events.subscribe())),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            wake: self.events.clone(),
        }))
    }
}

fn settings_page_requires_operation_gate(page: SettingsPage) -> bool {
    matches!(
        page,
        SettingsPage::Hooks
            | SettingsPage::Resources
            | SettingsPage::Workflows
            | SettingsPage::Agents
            | SettingsPage::Usage
            | SettingsPage::Diagnostics
    )
}

fn settings_command_requires_operation_gate(command: &SettingsCommand) -> bool {
    matches!(
        command,
        SettingsCommand::CreateHook(..)
            | SettingsCommand::UpdateHook { .. }
            | SettingsCommand::DeleteHook { .. }
            | SettingsCommand::SetHookEnabled { .. }
            | SettingsCommand::GrantHookTrust { .. }
            | SettingsCommand::RevokeHookTrust { .. }
            | SettingsCommand::SetPluginEnabled { .. }
            | SettingsCommand::InstallPlugin { .. }
            | SettingsCommand::UpdatePlugin { .. }
            | SettingsCommand::UninstallPlugin { .. }
            | SettingsCommand::ConfigurePlugin { .. }
            | SettingsCommand::SetMcpEnabled { .. }
            | SettingsCommand::AddMcp { .. }
            | SettingsCommand::UpdateMcp { .. }
            | SettingsCommand::RemoveMcp { .. }
            | SettingsCommand::InspectMcp { .. }
            | SettingsCommand::SetSkillEnabled { .. }
            | SettingsCommand::CopySkillToCommon { .. }
            | SettingsCommand::RemoveSkillFromCommon { .. }
            | SettingsCommand::DeleteSkill { .. }
            | SettingsCommand::CreateSkill { .. }
            | SettingsCommand::UpdateSkill { .. }
            | SettingsCommand::ReadMemory { .. }
            | SettingsCommand::DeleteMemory { .. }
            | SettingsCommand::CreateMemory { .. }
            | SettingsCommand::UpdateMemory { .. }
            | SettingsCommand::CreateAgentTemplate(..)
            | SettingsCommand::UpdateAgentTemplate(..)
            | SettingsCommand::DeleteAgentTemplate { .. }
            | SettingsCommand::SetAgentTemplateEnabled { .. }
            | SettingsCommand::SaveWorkflow(..)
            | SettingsCommand::MoveWorkflow { .. }
            | SettingsCommand::DeleteWorkflow { .. }
            | SettingsCommand::RunWorkflow { .. }
            | SettingsCommand::CancelWorkflow { .. }
            | SettingsCommand::AmendWorkflow { .. }
            | SettingsCommand::ResumeWorkflow { .. }
            | SettingsCommand::ResolveWorkflowQuestion { .. }
            | SettingsCommand::ListWorkflowRuns { .. }
            | SettingsCommand::ReadWorkflowEvents { .. }
            | SettingsCommand::ReadWorkflowGraph { .. }
            | SettingsCommand::ReadWorkflowWorkspace { .. }
            | SettingsCommand::ReadWorkflowNodeResult { .. }
            | SettingsCommand::ListWorkflowArtifacts { .. }
            | SettingsCommand::ListWorkflowArtifactItems { .. }
            | SettingsCommand::ReadWorkflowArtifact { .. }
            | SettingsCommand::SetGoal { .. }
            | SettingsCommand::ClearGoal { .. }
            | SettingsCommand::ResumeGoal { .. }
            | SettingsCommand::PauseGoal { .. }
            | SettingsCommand::CompleteGoal { .. }
            | SettingsCommand::BlockGoal { .. }
            | SettingsCommand::CancelGoalRun { .. }
            | SettingsCommand::CreateSubagent(..)
            | SettingsCommand::UpdateSubagent(..)
            | SettingsCommand::DeleteSubagent { .. }
            | SettingsCommand::SetSubagentEnabled { .. }
            | SettingsCommand::SetSubagentModel { .. }
            | SettingsCommand::QueryUsage(..)
            | SettingsCommand::RefreshDiagnostics
            | SettingsCommand::ExportDiagnostics
    )
}

/// 更新端口由 GPUI 控件实体持有；适配器只在一次操作期间临时升级弱引用，避免
/// Host -> Settings -> Controls -> callback -> Host 的强引用环。已装配但已释放时
/// 必须报错，不能把端口丢失解释为“没有更新”。
fn upgrade_update_port(
    update_port: &OnceLock<Weak<dyn NativeSettingsUpdatePort>>,
) -> SettingsResult<Option<Arc<dyn NativeSettingsUpdatePort>>> {
    let Some(update_port) = update_port.get() else {
        return Ok(None);
    };
    update_port.upgrade().map(Some).ok_or_else(|| {
        error(
            "settings_update_unavailable",
            "NativeControls 已释放，更新控制端口不可用",
            true,
        )
    })
}

fn upgrade_insights(
    insights: &OnceLock<Arc<dyn NativeSettingsInsightsPort>>,
) -> SettingsResult<Arc<dyn NativeSettingsInsightsPort>> {
    let Some(insights) = insights.get() else {
        return Err(error(
            "settings_insights_unavailable",
            "NativeInsights 未装配",
            true,
        ));
    };
    Ok(Arc::clone(insights))
}

fn upgrade_memory(
    memory: &OnceLock<Weak<NativeMemoryCoordinator>>,
) -> SettingsResult<Arc<NativeMemoryCoordinator>> {
    let Some(memory) = memory.get() else {
        return Err(error(
            "settings_memory_unavailable",
            "本地记忆协调器尚未装配",
            true,
        ));
    };
    memory
        .upgrade()
        .ok_or_else(|| error("settings_memory_unavailable", "本地记忆协调器已释放", true))
}

#[expect(
    clippy::too_many_arguments,
    reason = "适配器按各权威服务的独立生命周期装配页面"
)]
fn load_page(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    domain: &dyn NativeSettingsDomain,
    page: SettingsPage,
    revisions: &AtomicU64,
    update_port: &OnceLock<Weak<dyn NativeSettingsUpdatePort>>,
    insights: &OnceLock<Arc<dyn NativeSettingsInsightsPort>>,
    keybindings: &NativeKeybindingsState,
) -> SettingsResult<SettingsSnapshot> {
    match page {
        SettingsPage::General | SettingsPage::Appearance => {
            let update_port = upgrade_update_port(update_port)?;
            load_general(paths, page, revisions, update_port.as_deref())
        }
        SettingsPage::Keyboard => {
            let settings = read_keybindings(paths)
                .map_err(|message| error("settings_keybindings_read", message, false))?;
            keybindings.replace(settings.clone());
            Ok(SettingsSnapshot {
                page,
                revision: NativeSettingsAdapter::next_revision(revisions),
                keyboard: Some(settings),
                ..SettingsSnapshot::default()
            })
        }
        SettingsPage::Providers => load_providers(paths, runtime, revisions),
        SettingsPage::Hooks => domain.load(page),
        SettingsPage::Resources
        | SettingsPage::Automations
        | SettingsPage::Workflows
        | SettingsPage::Agents => domain.load(page),
        SettingsPage::Usage => {
            let snapshot = upgrade_insights(insights)?
                .usage(RequestRecordsQuery::default())
                .map_err(|message| error("settings_usage_read", message, true))?;
            Ok(SettingsSnapshot {
                page,
                revision: NativeSettingsAdapter::next_revision(revisions),
                usage: Some(UsageSettings {
                    query: RequestRecordsQuery::default(),
                    snapshot,
                }),
                ..SettingsSnapshot::default()
            })
        }
        SettingsPage::Diagnostics => {
            let snapshot = upgrade_insights(insights)?.diagnostics();
            Ok(SettingsSnapshot {
                page,
                revision: NativeSettingsAdapter::next_revision(revisions),
                diagnostics: Some(DiagnosticsSettings {
                    snapshot,
                    exported_json: None,
                }),
                ..SettingsSnapshot::default()
            })
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "复用读取路径的真实服务端口，不为命令另造包装层"
)]
fn execute_command(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    domain: &dyn NativeSettingsDomain,
    command: SettingsCommand,
    revisions: &AtomicU64,
    update_port: &OnceLock<Weak<dyn NativeSettingsUpdatePort>>,
    insights: &OnceLock<Arc<dyn NativeSettingsInsightsPort>>,
    keybindings: &NativeKeybindingsState,
    memory: &OnceLock<Weak<NativeMemoryCoordinator>>,
) -> SettingsResult<SettingsSnapshot> {
    match command {
        SettingsCommand::SaveGeneral(patch) => {
            let update_port = upgrade_update_port(update_port)?;
            let memory = if patch
                .preferences
                .as_ref()
                .is_some_and(|preferences| preferences.local_memories.is_some())
            {
                Some(upgrade_memory(memory)?)
            } else {
                None
            };
            save_general(
                paths,
                runtime,
                patch,
                SettingsPage::General,
                true,
                revisions,
                update_port.as_deref(),
                memory,
            )
        }
        SettingsCommand::SaveAppearance(patch) => {
            let update_port = upgrade_update_port(update_port)?;
            save_general(
                paths,
                runtime,
                patch,
                SettingsPage::Appearance,
                false,
                revisions,
                update_port.as_deref(),
                None,
            )
        }
        SettingsCommand::SaveKeyboard(settings) => {
            write_keybindings(paths, &settings)
                .map_err(|message| error("settings_keybindings_write", message, false))?;
            keybindings.replace(settings.clone());
            Ok(SettingsSnapshot {
                page: SettingsPage::Keyboard,
                revision: NativeSettingsAdapter::next_revision(revisions),
                keyboard: Some(settings),
                ..SettingsSnapshot::default()
            })
        }
        SettingsCommand::SaveCustomInstructions { instructions } => {
            let update_port = upgrade_update_port(update_port)?;
            personalization::set(paths, instructions)
                .map_err(|e| map_error("settings_custom_instructions_write", e))?;
            load_general(
                paths,
                SettingsPage::General,
                revisions,
                update_port.as_deref(),
            )
        }
        SettingsCommand::CheckForUpdates => {
            let update_port = upgrade_update_port(update_port)?;
            update_snapshot(
                paths,
                update_port.as_deref(),
                |port| port.check(),
                revisions,
            )
        }
        SettingsCommand::DownloadUpdate => {
            let update_port = upgrade_update_port(update_port)?;
            update_snapshot(
                paths,
                update_port.as_deref(),
                |port| port.download(),
                revisions,
            )
        }
        SettingsCommand::InstallUpdate => {
            let update_port = upgrade_update_port(update_port)?;
            update_snapshot(
                paths,
                update_port.as_deref(),
                |port| port.install(),
                revisions,
            )
        }
        SettingsCommand::QueryUsage(query) => {
            let snapshot = upgrade_insights(insights)?
                .usage(query.clone())
                .map_err(|message| error("settings_usage_read", message, true))?;
            Ok(SettingsSnapshot {
                page: SettingsPage::Usage,
                revision: NativeSettingsAdapter::next_revision(revisions),
                usage: Some(UsageSettings { query, snapshot }),
                ..SettingsSnapshot::default()
            })
        }
        SettingsCommand::RefreshDiagnostics => {
            let snapshot = upgrade_insights(insights)?.diagnostics();
            Ok(SettingsSnapshot {
                page: SettingsPage::Diagnostics,
                revision: NativeSettingsAdapter::next_revision(revisions),
                diagnostics: Some(DiagnosticsSettings {
                    snapshot,
                    exported_json: None,
                }),
                ..SettingsSnapshot::default()
            })
        }
        SettingsCommand::ExportDiagnostics => {
            let insights = upgrade_insights(insights)?;
            let snapshot = insights.diagnostics();
            let exported_json = insights
                .export_diagnostics()
                .map_err(|message| error("settings_diagnostics_export", message, true))?;
            Ok(SettingsSnapshot {
                page: SettingsPage::Diagnostics,
                revision: NativeSettingsAdapter::next_revision(revisions),
                diagnostics: Some(DiagnosticsSettings {
                    snapshot,
                    exported_json: Some(exported_json),
                }),
                ..SettingsSnapshot::default()
            })
        }
        SettingsCommand::CreateProvider {
            id,
            name,
            base_url,
            api_backend,
            api_key,
            models,
        } => {
            // providers 域要求每个模型都显式保存视觉能力；新建表单没有能力
            // 覆盖值时按“未启用”初始化，避免把不完整配置交给严格校验。
            let supports_vision = models.iter().cloned().map(|model| (model, false)).collect();
            let list = providers::upsert(
                paths,
                ProviderUpsert {
                    id,
                    models,
                    base_url,
                    name: Some(name),
                    api_backend,
                    api_key,
                    context_windows: BTreeMap::new(),
                    max_output_tokens: BTreeMap::new(),
                    chat_output_token_field: ChatOutputTokenField::default(),
                    supports_vision,
                    reasoning_efforts: BTreeMap::new(),
                    create_only: true,
                },
            )
            .map_err(|e| map_error("provider_create", e))?;
            sync_runtime(runtime, &list)?;
            provider_snapshot(paths, runtime, list, revisions)
        }
        SettingsCommand::UpdateProvider { provider_id, patch } => {
            let current = providers::list(paths).map_err(|e| map_error("provider_list", e))?;
            let provider = current
                .providers
                .iter()
                .find(|item| item.id == provider_id)
                .ok_or_else(|| error("provider_not_found", "找不到指定供应商", false))?;
            let models = provider.models.clone();
            let api_key = match patch.api_key {
                Some(value) => value,
                None => provider.api_key.clone(),
            };
            let updated = providers::upsert(
                paths,
                ProviderUpsert {
                    id: provider.id.clone(),
                    models,
                    base_url: patch.base_url.unwrap_or_else(|| provider.base_url.clone()),
                    name: Some(patch.name.unwrap_or_else(|| provider.name.clone())),
                    api_backend: patch
                        .api_backend
                        .unwrap_or_else(|| provider.api_backend.clone()),
                    api_key,
                    context_windows: provider.context_windows.clone(),
                    max_output_tokens: provider.max_output_tokens.clone(),
                    chat_output_token_field: provider.chat_output_token_field,
                    supports_vision: provider.supports_vision.clone(),
                    reasoning_efforts: provider.reasoning_efforts.clone(),
                    create_only: false,
                },
            )
            .map_err(|e| map_error("provider_update", e))?;
            sync_runtime(runtime, &updated)?;
            provider_snapshot(paths, runtime, updated, revisions)
        }
        SettingsCommand::DeleteProvider { provider_id } => {
            let list = providers::remove(paths, &provider_id)
                .map_err(|e| map_error("provider_delete", e))?;
            sync_runtime(runtime, &list)?;
            provider_snapshot(paths, runtime, list, revisions)
        }
        SettingsCommand::RefreshProviderCatalog { provider_id } => {
            refresh_provider_catalog(paths, runtime, &provider_id, revisions)
        }
        SettingsCommand::SetActiveModel(selection) => {
            let list = providers::select_model(paths, &selection.provider_id, &selection.model_id)
                .map_err(|e| map_error("provider_select", e))?;
            sync_runtime(runtime, &list)?;
            runtime
                .set_default_provider_selection(&selection.provider_id, &selection.model_id)
                .map_err(|e| map_error("provider_runtime_select", e))?;
            provider_snapshot(paths, runtime, list, revisions)
        }
        SettingsCommand::CreateModel {
            provider_id,
            model_id,
            config,
        } => patch_model(
            paths,
            runtime,
            &provider_id,
            &model_id,
            Some(config),
            true,
            revisions,
        ),
        SettingsCommand::PatchModel {
            provider_id,
            model_id,
            config,
        } => patch_model(
            paths,
            runtime,
            &provider_id,
            &model_id,
            Some(config),
            false,
            revisions,
        ),
        SettingsCommand::DeleteModel {
            provider_id,
            model_id,
        } => patch_model(
            paths,
            runtime,
            &provider_id,
            &model_id,
            None,
            false,
            revisions,
        ),
        SettingsCommand::TestModel { selection } => {
            verify_model(paths, runtime, &selection, revisions)
        }
        SettingsCommand::SetModelEnabled {
            provider_id,
            model_id,
            enabled,
        } => set_model_enabled(paths, runtime, &provider_id, &model_id, enabled, revisions),
        _ => domain.execute(command),
    }
}

fn load_general(
    paths: &NativePaths,
    page: SettingsPage,
    revisions: &AtomicU64,
    update_port: Option<&dyn NativeSettingsUpdatePort>,
) -> SettingsResult<SettingsSnapshot> {
    let extras = read_general_file(paths)?;
    let app_settings =
        app_settings::get(paths).map_err(|error| map_error("settings_app_read", error))?;
    let app_update = update_port
        .map(|port| {
            port.info()
                .map_err(|e| map_error("settings_update_info", e))
        })
        .transpose()?;
    let custom_instructions = personalization::get(paths)
        .map_err(|e| map_error("settings_custom_instructions_read", e))?;
    let preferences = preferences_snapshot(&app_settings);
    Ok(SettingsSnapshot {
        page,
        revision: NativeSettingsAdapter::next_revision(revisions),
        general: Some(GeneralSettings {
            appearance: extras.appearance,
            font_family: extras.font_family,
            font_size_px: extras.font_size_px,
            density: extras.density,
            reduced_motion: extras.reduced_motion,
            code: extras.code,
            terminal_shell: app_settings.terminal_shell,
            terminal_font_family: app_settings.terminal_font_family,
            terminal_inherit_system_profile: app_settings.terminal_inherit_system_profile,
            preferences,
            custom_instructions,
            app_update,
        }),
        ..SettingsSnapshot::default()
    })
}

fn preferences_snapshot(settings: &app_settings::AppSettings) -> PreferencesSnapshot {
    PreferencesSnapshot {
        app_update_download_source: settings.app_update_download_source,
        project_directory: settings.project_directory.clone(),
        task_notifications: settings.task_notifications,
        notification_sound: settings.notification_sound,
        keep_computer_awake: settings.keep_computer_awake,
        close_to_tray: settings.close_to_tray,
        background_agent_limit: settings.background_agent_limit,
        http_proxy: settings.http_proxy.clone(),
        http_proxy_no_proxy: settings.http_proxy_no_proxy.clone(),
        local_memories: settings.local_memories,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "常规设置保存同时协调页面投影、应用设置、运行时和本地记忆生命周期"
)]
fn save_general(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    patch: GeneralPatch,
    page: SettingsPage,
    persist_terminal: bool,
    revisions: &AtomicU64,
    update_port: Option<&dyn NativeSettingsUpdatePort>,
    memory: Option<Arc<NativeMemoryCoordinator>>,
) -> SettingsResult<SettingsSnapshot> {
    let GeneralPatch {
        appearance,
        font_family,
        font_size_px,
        density,
        reduced_motion,
        code,
        terminal_shell,
        terminal_font_family,
        terminal_inherit_system_profile,
        preferences,
    } = patch;
    let has_terminal_patch = terminal_shell.is_some()
        || terminal_font_family.is_some()
        || terminal_inherit_system_profile.is_some();
    if !persist_terminal && has_terminal_patch {
        return Err(error(
            "settings_appearance_terminal",
            "外观设置不能修改终端选项",
            false,
        ));
    }
    if !persist_terminal && preferences.is_some() {
        return Err(error(
            "settings_appearance_preferences",
            "外观设置不能修改应用偏好",
            false,
        ));
    }

    let current = read_general_file(paths)?;
    let mut next = current.clone();
    if let Some(value) = appearance {
        next.appearance = value;
    }
    if let Some(value) = font_size_px {
        next.font_size_px = value;
    }
    if let Some(value) = density {
        next.density = value;
    }
    if let Some(value) = font_family {
        next.font_family = value;
    }
    if let Some(value) = reduced_motion {
        next.reduced_motion = value;
    }
    if let Some(value) = code {
        next.code = value;
    }
    next.validate()?;

    // 两份设置文件分别拥有各自的 schema；应用设置补丁先在其文件锁内校验并提交。
    let mut app_patch = preferences.unwrap_or_default();
    app_patch.terminal_shell = terminal_shell;
    app_patch.terminal_font_family = terminal_font_family;
    app_patch.terminal_inherit_system_profile = terminal_inherit_system_profile;
    let has_app_patch = !app_patch.is_empty();
    let background_agent_limit = app_patch.background_agent_limit;
    let local_memories = app_patch.local_memories;
    if local_memories.is_some() && memory.is_none() {
        return Err(error(
            "settings_memory_unavailable",
            "本地记忆协调器尚未装配",
            true,
        ));
    }

    if has_app_patch {
        app_settings::update_preferences(paths, app_patch)
            .map_err(|error| map_error("settings_app_write", error))?;
        if let Some(limit) = background_agent_limit {
            runtime
                .set_background_agent_limit(usize::from(limit))
                .map_err(|error| map_error("settings_background_agent_limit", error))?;
        }
        if let Some(enabled) = local_memories {
            memory
                .expect("本地记忆补丁已在上方检查")
                .start(enabled)
                .map_err(|error| map_error("settings_local_memories", error))?;
        }
    }
    write_general_file(paths, &next)?;
    load_general(paths, page, revisions, update_port)
}

fn update_snapshot(
    paths: &NativePaths,
    update_port: Option<&dyn NativeSettingsUpdatePort>,
    action: impl FnOnce(
        &dyn NativeSettingsUpdatePort,
    ) -> Result<crate::app_updates::AppUpdateStatus, String>,
    revisions: &AtomicU64,
) -> SettingsResult<SettingsSnapshot> {
    let port = update_port
        .ok_or_else(|| error("settings_update_unavailable", "更新功能尚未可用", true))?;
    let app_update = action(port).map_err(|e| map_error("settings_update", e))?;
    let extras = read_general_file(paths)?;
    let app_settings =
        app_settings::get(paths).map_err(|error| map_error("settings_app_read", error))?;
    let custom_instructions = personalization::get(paths)
        .map_err(|e| map_error("settings_custom_instructions_read", e))?;
    let preferences = preferences_snapshot(&app_settings);
    Ok(SettingsSnapshot {
        page: SettingsPage::General,
        revision: NativeSettingsAdapter::next_revision(revisions),
        general: Some(GeneralSettings {
            appearance: extras.appearance,
            font_family: extras.font_family,
            font_size_px: extras.font_size_px,
            density: extras.density,
            reduced_motion: extras.reduced_motion,
            code: extras.code,
            terminal_shell: app_settings.terminal_shell,
            terminal_font_family: app_settings.terminal_font_family,
            terminal_inherit_system_profile: app_settings.terminal_inherit_system_profile,
            preferences,
            custom_instructions,
            app_update: Some(app_update),
        }),
        ..SettingsSnapshot::default()
    })
}

fn read_general_file(paths: &NativePaths) -> SettingsResult<GeneralFile> {
    let path = general_file_path(paths);
    let Some(bytes) =
        storage::read_private_bytes_bounded(&path, MAX_GENERAL_FILE_BYTES, "Native 常规设置")
            .map_err(|e| map_error("settings_general_read", e))?
    else {
        return Ok(GeneralFile::default());
    };
    let file: GeneralFile =
        serde_json::from_slice(&bytes).map_err(|e| map_error("settings_general_parse", e))?;
    file.validate()?;
    Ok(file)
}

fn write_general_file(paths: &NativePaths, file: &GeneralFile) -> SettingsResult<()> {
    let bytes =
        serde_json::to_vec_pretty(file).map_err(|e| map_error("settings_general_encode", e))?;
    if bytes.len() as u64 > MAX_GENERAL_FILE_BYTES {
        return Err(error(
            "settings_general_size",
            "常规设置超过大小限制",
            false,
        ));
    }
    storage::atomic_write_private(&general_file_path(paths), &bytes)
        .map_err(|e| map_error("settings_general_write", e))
}

fn general_file_path(paths: &NativePaths) -> PathBuf {
    paths.data_root.join("native-general-settings.json")
}

fn load_providers(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    revisions: &AtomicU64,
) -> SettingsResult<SettingsSnapshot> {
    let list = providers::list(paths).map_err(|e| map_error("provider_list", e))?;
    provider_snapshot(paths, runtime, list, revisions)
}

fn provider_snapshot(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    list: providers::ProvidersListResult,
    revisions: &AtomicU64,
) -> SettingsResult<SettingsSnapshot> {
    let registry = runtime
        .provider_registry()
        .snapshot()
        .map_err(|e| map_error("provider_registry_snapshot", e))?;
    let registered = registry
        .providers
        .iter()
        .map(|provider| provider.provider_id.as_str())
        .collect::<HashSet<_>>();
    let model_ids = list
        .providers
        .iter()
        .flat_map(|provider| provider.models.iter().cloned())
        .collect::<BTreeSet<_>>();
    let catalog_metadata = model_ids
        .iter()
        .collect::<Vec<_>>()
        .chunks(256)
        .filter_map(|chunk| {
            let ids = chunk
                .iter()
                .map(|model| (*model).clone())
                .collect::<Vec<_>>();
            model_metadata::get_many(paths, &ids).ok()
        })
        .flatten()
        .map(|metadata| (metadata.model_id.clone(), metadata))
        .collect::<BTreeMap<_, _>>();
    let providers = list
        .providers
        .iter()
        .map(|provider| {
            let models = provider
                .models
                .iter()
                .map(|model_id| {
                    let catalog_reasoning_efforts = catalog_metadata
                        .get(model_id)
                        .and_then(|metadata| metadata.reasoning.as_ref())
                        .and_then(|reasoning| {
                            reasoning.controls.iter().find_map(|control| match control {
                                ModelReasoningControl::Effort { values } => Some(values.clone()),
                                // Toggle 只有开关语义，`none` 是唯一可表达的关闭档位。
                                ModelReasoningControl::Toggle => Some(vec!["none".to_owned()]),
                                ModelReasoningControl::BudgetTokens { .. } => None,
                            })
                        })
                        .unwrap_or_default();
                    let reasoning_efforts_configured =
                        provider.reasoning_efforts.contains_key(model_id);
                    let reasoning_efforts = provider
                        .reasoning_efforts
                        .get(model_id)
                        .cloned()
                        .unwrap_or_else(|| catalog_reasoning_efforts.clone());
                    let config = ModelConfig {
                        supports_vision: provider
                            .supports_vision
                            .get(model_id)
                            .copied()
                            .unwrap_or(false),
                        reasoning_efforts_configured,
                        reasoning_efforts: reasoning_efforts.clone(),
                        use_recommended_config: false,
                        context_window: provider.context_windows.get(model_id).copied(),
                        max_output_tokens: provider.max_output_tokens.get(model_id).copied(),
                        output_token_field: Some(match provider.chat_output_token_field {
                            ChatOutputTokenField::MaxTokens => "max_tokens".to_owned(),
                            ChatOutputTokenField::MaxCompletionTokens => {
                                "max_completion_tokens".to_owned()
                            }
                        }),
                    };
                    ModelSummary {
                        id: model_id.clone(),
                        display_name: model_id.clone(),
                        enabled: !provider.disabled_models.contains(model_id),
                        executable: !provider.disabled_models.contains(model_id)
                            && registered.contains(provider.id.as_str()),
                        issues: Vec::new(),
                        supports_vision: config.supports_vision,
                        reasoning_efforts,
                        context_window: config.context_window,
                        max_output_tokens: config.max_output_tokens,
                        config,
                    }
                })
                .collect::<Vec<_>>();
            ProviderSummary {
                id: provider.id.clone(),
                name: provider.name.clone(),
                base_url: provider.base_url.clone(),
                api_backend: provider.api_backend.clone(),
                enabled: true,
                executable: models.iter().any(|model| model.executable),
                api_key_configured: provider.api_key.is_some(),
                models,
            }
        })
        .collect::<Vec<_>>();
    let catalog = providers
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(|model| ModelCatalogEntry {
                provider_id: provider.id.clone(),
                model_id: model.id.clone(),
                label: model.display_name.clone(),
                protocol: provider.api_backend.clone(),
                supports_vision: model.supports_vision,
                reasoning_efforts: catalog_metadata
                    .get(&model.id)
                    .and_then(|metadata| metadata.reasoning.as_ref())
                    .and_then(|reasoning| {
                        reasoning.controls.iter().find_map(|control| match control {
                            ModelReasoningControl::Effort { values } => Some(values.clone()),
                            ModelReasoningControl::Toggle => Some(vec!["none".to_owned()]),
                            ModelReasoningControl::BudgetTokens { .. } => None,
                        })
                    })
                    .unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let active = list
        .active_provider_id
        .zip(list.default_model)
        .map(|(provider_id, model_id)| ModelSelection {
            provider_id,
            model_id,
        });
    Ok(SettingsSnapshot {
        page: SettingsPage::Providers,
        revision: NativeSettingsAdapter::next_revision(revisions),
        providers: Some(ProviderSettings {
            revision: registry.generation,
            providers,
            active,
            catalog,
        }),
        ..SettingsSnapshot::default()
    })
}

fn refresh_provider_catalog(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    provider_id: &str,
    revisions: &AtomicU64,
) -> SettingsResult<SettingsSnapshot> {
    let list = providers::list(paths).map_err(|e| map_error("provider_list", e))?;
    let provider = list
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| error("provider_not_found", "找不到指定供应商", false))?;
    providers::validate_model_catalog_scope(
        paths,
        &provider.id,
        &provider.base_url,
        &provider.api_backend,
    )
    .map_err(|e| map_error("provider_catalog_scope", e))?;
    let catalog = providers::list_models(
        &provider.base_url,
        provider.api_key.as_deref(),
        &provider.api_backend,
    )
    .map_err(|e| map_error("provider_catalog_refresh", e))?;
    let expected = list
        .providers
        .iter()
        .flat_map(|item| {
            item.models
                .iter()
                .map(move |model| format!("{}::{model}", item.id))
        })
        .collect::<Vec<_>>();
    let current_provider_models = expected
        .iter()
        .filter(|qualified| qualified.starts_with(&format!("{}::", provider.id)))
        .cloned()
        .collect::<Vec<_>>();
    let known = current_provider_models
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut refreshed_provider_models = current_provider_models;
    for model in catalog.models {
        let qualified = format!("{}::{}", provider.id, model.id);
        if known.contains(&qualified) {
            continue;
        }
        refreshed_provider_models.push(qualified);
    }
    // update_models 只允许完整列表；把其他 Provider 的当前模型原样附回。
    let others = expected
        .into_iter()
        .filter(|qualified| !qualified.starts_with(&format!("{}::", provider.id)));
    let mut models = refreshed_provider_models;
    models.extend(others);
    let expected = providers::list(paths)
        .map_err(|e| map_error("provider_list", e))?
        .providers
        .into_iter()
        .flat_map(|item| {
            item.models
                .into_iter()
                .map(move |model| format!("{}::{model}", item.id))
        })
        .collect::<Vec<_>>();
    let updated = providers::update_models(paths, expected, models)
        .map_err(|e| map_error("provider_catalog_save", e))?;
    sync_runtime(runtime, &updated)?;
    provider_snapshot(paths, runtime, updated, revisions)
}

fn patch_model(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    provider_id: &str,
    model_id: &str,
    config: Option<ModelConfig>,
    create: bool,
    revisions: &AtomicU64,
) -> SettingsResult<SettingsSnapshot> {
    let list = providers::list(paths).map_err(|e| map_error("provider_list", e))?;
    let provider = list
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .ok_or_else(|| error("provider_not_found", "找不到指定供应商", false))?;
    let exists = provider.models.iter().any(|model| model == model_id);
    if create && exists {
        return Err(error("provider_model_exists", "模型已经存在", false));
    }
    if !create && !exists {
        return Err(error("provider_model_not_found", "找不到指定模型", false));
    }
    let mut models = provider.models.clone();
    if config.is_some() && create {
        models.push(model_id.to_owned());
    } else if config.is_none() {
        models.retain(|model| model != model_id);
    }
    if models.is_empty() {
        return Err(error(
            "provider_model_last",
            "供应商至少需要保留一个模型",
            false,
        ));
    }
    let mut supports_vision = provider.supports_vision.clone();
    let mut reasoning_efforts = provider.reasoning_efforts.clone();
    let mut context_windows = provider.context_windows.clone();
    let mut max_output_tokens = provider.max_output_tokens.clone();
    if let Some(config) = config {
        supports_vision.insert(model_id.to_owned(), config.supports_vision);
        if config.reasoning_efforts_configured {
            // 显式空列表表示用户关闭该模型的推理档位覆盖，必须保留 map key，
            // 这样读取时不会把它误解释为“沿用公共目录”。
            reasoning_efforts.insert(model_id.to_owned(), config.reasoning_efforts);
        } else {
            reasoning_efforts.remove(model_id);
        }
        match config.context_window {
            Some(value) => {
                context_windows.insert(model_id.to_owned(), value);
            }
            None => {
                context_windows.remove(model_id);
            }
        }
        match config.max_output_tokens {
            Some(value) => {
                max_output_tokens.insert(model_id.to_owned(), value);
            }
            None => {
                max_output_tokens.remove(model_id);
            }
        }
    } else {
        supports_vision.remove(model_id);
        reasoning_efforts.remove(model_id);
        context_windows.remove(model_id);
        max_output_tokens.remove(model_id);
    }
    supports_vision.retain(|model, _| models.contains(model));
    reasoning_efforts.retain(|model, _| models.contains(model));
    context_windows.retain(|model, _| models.contains(model));
    max_output_tokens.retain(|model, _| models.contains(model));
    for model in &models {
        supports_vision.entry(model.clone()).or_insert(false);
    }
    let updated = providers::upsert(
        paths,
        ProviderUpsert {
            id: provider.id.clone(),
            models,
            base_url: provider.base_url.clone(),
            name: Some(provider.name.clone()),
            api_backend: provider.api_backend.clone(),
            api_key: provider.api_key.clone(),
            context_windows,
            max_output_tokens,
            chat_output_token_field: provider.chat_output_token_field,
            supports_vision,
            reasoning_efforts,
            create_only: false,
        },
    )
    .map_err(|e| map_error("provider_model_save", e))?;
    sync_runtime(runtime, &updated)?;
    provider_snapshot(paths, runtime, updated, revisions)
}

/// 通过 Provider facade 原子保存启用状态，再刷新 Runtime 注册表和设置投影。
fn set_model_enabled(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    provider_id: &str,
    model_id: &str,
    enabled: bool,
    revisions: &AtomicU64,
) -> SettingsResult<SettingsSnapshot> {
    let updated = providers::set_model_enabled(paths, provider_id, model_id, enabled)
        .map_err(|e| map_error("provider_model_enable", e))?;
    sync_runtime(runtime, &updated)?;
    provider_snapshot(paths, runtime, updated, revisions)
}

fn verify_model(
    paths: &NativePaths,
    runtime: &AgentRuntime,
    selection: &ModelSelection,
    revisions: &AtomicU64,
) -> SettingsResult<SettingsSnapshot> {
    use keencode_model::{
        ContentBlock, Message, MessageRole, ModelProvider, ModelRequest, ToolChoice,
    };

    let list = providers::list(paths).map_err(|e| map_error("provider_list", e))?;
    let provider = list
        .providers
        .iter()
        .find(|provider| provider.id == selection.provider_id)
        .ok_or_else(|| error("provider_not_found", "找不到指定供应商", false))?;
    if !provider
        .models
        .iter()
        .any(|model| model == &selection.model_id)
        || provider.disabled_models.contains(&selection.model_id)
    {
        return Err(error(
            "provider_model_not_found",
            "模型未在当前目录中启用",
            false,
        ));
    }
    providers::validate_model_catalog_scope(
        paths,
        &provider.id,
        &provider.base_url,
        &provider.api_backend,
    )
    .map_err(|e| map_error("provider_verify_scope", e))?;
    let resolved = runtime
        .provider_registry()
        .resolve(&selection.provider_id, &selection.model_id)
        .map_err(|e| map_error("provider_verify_selection", e))?;
    let mut request = ModelRequest::new(
        resolved.model(),
        vec![Message::text(MessageRole::User, "Reply with OK only.")],
    );
    // 模型测试必须走实际生成协议；目录可访问不代表生成端点、凭据和模型可用。
    // 请求隔离于会话 Journal，禁止工具，并限制输出和总等待时间。
    request.tool_choice = ToolChoice::None;
    request.max_output_tokens = Some(1024);
    request.metadata.insert(
        keencode_provider::REQUEST_METADATA_PURPOSE.to_owned(),
        "model-test".to_owned(),
    );
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|e| map_error("provider_verify_runtime", e))?;
    // 设置适配器在 spawn_blocking 中执行本函数，复用宿主运行时而不另建线程池。
    let response = handle.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            resolved.complete(request),
        )
        .await
        .map_err(|_| error("provider_verify_timeout", "模型测试超过 60 秒", true))?
        .map_err(|e| map_error("provider_verify", e))
    })?;
    if !response
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Text { text } if !text.trim().is_empty()))
    {
        return Err(error(
            "provider_verify_empty",
            "模型生成已结束，但没有返回可见正文",
            true,
        ));
    }
    let mut snapshot = provider_snapshot(paths, runtime, list, revisions)?;
    snapshot.notices.push(SettingsNotice {
        level: NoticeLevel::Success,
        code: "provider_model_verified".to_owned(),
        message: "模型真实生成测试通过".to_owned(),
    });
    Ok(snapshot)
}

fn sync_runtime(
    runtime: &AgentRuntime,
    list: &providers::ProvidersListResult,
) -> SettingsResult<()> {
    providers::replace_runtime_registry(runtime.provider_registry(), list)
        .map_err(|e| map_error("provider_runtime_reload", e))?;
    if let Some(provider_id) = list.active_provider_id.as_deref()
        && let Some(model) = list.default_model.as_deref()
    {
        runtime
            .set_default_provider_selection(provider_id, model)
            .map_err(|e| map_error("provider_runtime_select", e))?;
    }
    Ok(())
}

fn error(
    code: impl Into<String>,
    message: impl Into<String>,
    retryable: bool,
) -> NativeSettingsError {
    NativeSettingsError {
        code: code.into(),
        message: message.into(),
        retryable,
    }
}

fn map_error(code: &str, error: impl std::fmt::Display) -> NativeSettingsError {
    NativeSettingsError::new(code, redact_message(&error.to_string()))
}

fn redact_message(message: &str) -> String {
    // Domain error 文本可能包含 URL 查询串或凭据；这里只保留通用摘要，避免把
    // provider secret 通过设置错误回传给窗口。
    let mut value = keencode_model::redact_error_secrets_bounded(message, 1_000);
    if let Some((prefix, _)) = value.split_once("?api_key=") {
        value = prefix.to_owned();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        GeneralPatch, ModelSelection, NativeSettingsAdapter, NativeSettingsDomain,
        NativeSettingsError, NativeSettingsService, SettingsCommand, SettingsEvent, SettingsPage,
        SettingsResult, SettingsSnapshot, publish_invalidation, save_general,
    };
    use crate::app_settings::TerminalShell;
    use crate::native_paths::NativePaths;
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};
    use tempfile::TempDir;
    use tokio::sync::broadcast;

    struct TestSettingsDomain;

    impl NativeSettingsDomain for TestSettingsDomain {
        fn load(&self, _page: SettingsPage) -> SettingsResult<SettingsSnapshot> {
            Err(NativeSettingsError::new(
                "settings_test_domain",
                "测试 domain 不处理该页面",
            ))
        }

        fn execute(&self, _command: SettingsCommand) -> SettingsResult<SettingsSnapshot> {
            Err(NativeSettingsError::new(
                "settings_test_domain",
                "测试 domain 不处理该命令",
            ))
        }
    }

    #[derive(Debug)]
    struct CapturedResponsesRequest {
        method: String,
        path: String,
        body: Value,
    }

    /// 本地 Responses fixture 只处理一条请求；头、正文和等待均设上界，避免
    /// Provider 在异常时让测试线程永久阻塞。
    fn spawn_responses_fixture(
        status: u16,
        body: Value,
    ) -> (String, JoinHandle<Result<CapturedResponsesRequest, String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("本地 Responses 端口应绑定");
        listener
            .set_nonblocking(true)
            .expect("本地 Responses 监听器应设为非阻塞");
        let address = listener.local_addr().expect("本地 Responses 地址应读取");
        let body = body.to_string();
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err("等待本地 Responses 请求超时".to_owned());
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(format!("接受本地 Responses 请求失败：{error}")),
                }
            };
            stream
                .set_nonblocking(false)
                .map_err(|error| format!("恢复本地 Responses 连接阻塞模式失败：{error}"))?;
            let request = read_responses_request(&mut stream)?;
            let reason = match status {
                200 => "OK",
                500 => "Internal Server Error",
                _ => "Fixture Error",
            };
            let retry_after = if status == 500 {
                // 500 的错误正文使用不可重试类别，避免测试触发默认指数退避。
                "Retry-After: 0\r\n"
            } else {
                ""
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n{retry_after}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .and_then(|_| stream.flush())
                .map_err(|error| format!("写入本地 Responses 响应失败：{error}"))?;
            Ok(request)
        });
        (format!("http://{address}/v1"), server)
    }

    /// 读取带 Content-Length 的一次 HTTP JSON 请求，并限制 fixture 的内存和等待。
    fn read_responses_request(stream: &mut TcpStream) -> Result<CapturedResponsesRequest, String> {
        const MAX_HEADER_BYTES: usize = 64 * 1024;
        const MAX_BODY_BYTES: usize = 1024 * 1024;

        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|error| format!("设置本地 Responses 读取超时失败：{error}"))?;
        let mut wire = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            if let Some(position) = wire.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            if wire.len() > MAX_HEADER_BYTES {
                return Err("本地 Responses 请求头超过大小限制".to_owned());
            }
            let count = stream
                .read(&mut buffer)
                .map_err(|error| format!("读取本地 Responses 请求头失败：{error}"))?;
            if count == 0 {
                return Err("本地 Responses 请求头提前结束".to_owned());
            }
            wire.extend_from_slice(&buffer[..count]);
        };
        let head = std::str::from_utf8(&wire[..header_end])
            .map_err(|error| format!("本地 Responses 请求头不是 UTF-8：{error}"))?;
        let mut request_line = head
            .lines()
            .next()
            .ok_or_else(|| "本地 Responses 请求缺少请求行".to_owned())?
            .split_whitespace();
        let method = request_line
            .next()
            .ok_or_else(|| "本地 Responses 请求缺少 HTTP 方法".to_owned())?
            .to_owned();
        let path = request_line
            .next()
            .ok_or_else(|| "本地 Responses 请求缺少请求路径".to_owned())?
            .to_owned();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>())
            })
            .ok_or_else(|| "本地 Responses 请求缺少 Content-Length".to_owned())?
            .map_err(|error| format!("本地 Responses Content-Length 无效：{error}"))?;
        if content_length > MAX_BODY_BYTES {
            return Err("本地 Responses 请求正文超过大小限制".to_owned());
        }
        while wire.len().saturating_sub(header_end) < content_length {
            let count = stream
                .read(&mut buffer)
                .map_err(|error| format!("读取本地 Responses 请求正文失败：{error}"))?;
            if count == 0 {
                return Err("本地 Responses 请求正文提前结束".to_owned());
            }
            wire.extend_from_slice(&buffer[..count]);
        }
        let body = serde_json::from_slice(&wire[header_end..header_end + content_length])
            .map_err(|error| format!("本地 Responses 请求正文不是 JSON：{error}"))?;
        Ok(CapturedResponsesRequest { method, path, body })
    }

    fn responses_body(text: &str) -> Value {
        json!({
            "id": "resp-test",
            "object": "response",
            "status": "completed",
            "model": "model-test",
            "output": [{
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}]
            }]
        })
    }

    fn model_test_adapter(directory: &TempDir) -> Arc<NativeSettingsAdapter> {
        // 模型请求测试使用本地 HTTP fixture，目录自愈也必须保持离线。
        std::fs::write(directory.path().join("models-dev-catalog.json"), b"{}")
            .expect("写入离线模型目录快照");
        let paths = Arc::new(NativePaths::from_data_root(directory.path().to_owned()));
        let runtime =
            crate::agent_runtime::AgentRuntime::new_for_control_test(directory.path().to_owned())
                .expect("测试 Runtime 应创建");
        NativeSettingsAdapter::new_with_operation_gate(
            paths,
            runtime,
            Arc::new(TestSettingsDomain),
            tokio::runtime::Handle::current(),
            Arc::new(tokio::sync::Mutex::new(())),
        )
    }

    async fn execute_model_test(
        status: u16,
        response: Value,
    ) -> (
        SettingsResult<SettingsSnapshot>,
        Result<CapturedResponsesRequest, String>,
    ) {
        let directory = tempfile::tempdir().expect("创建模型测试目录");
        let (base_url, server) = spawn_responses_fixture(status, response);
        let adapter = model_test_adapter(&directory);
        adapter
            .execute(SettingsCommand::CreateProvider {
                id: "provider-test".to_owned(),
                name: "Provider Test".to_owned(),
                base_url,
                api_backend: "responses".to_owned(),
                api_key: None,
                models: vec!["model-test".to_owned()],
            })
            .await
            .expect("适配器创建 Provider 应成功");
        let result = adapter
            .execute(SettingsCommand::TestModel {
                selection: ModelSelection {
                    provider_id: "provider-test".to_owned(),
                    model_id: "model-test".to_owned(),
                },
            })
            .await;
        let request = server.join().expect("本地 Responses 线程不应 panic");
        (result, request)
    }

    fn assert_model_test_request(request: &CapturedResponsesRequest) {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/responses");
        assert_eq!(request.body["model"], "model-test");
        assert_eq!(request.body["tool_choice"], "none");
        assert!(request.body.get("tools").is_none());
        assert_eq!(request.body["max_output_tokens"], 1024);
        assert_eq!(
            request.body["input"][0]["content"][0]["text"],
            "Reply with OK only."
        );
    }

    #[test]
    fn save_appearance_returns_an_appearance_snapshot() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let paths = NativePaths::from_data_root(directory.path().to_owned());
        let runtime =
            crate::agent_runtime::AgentRuntime::new_for_control_test(directory.path().to_owned())
                .expect("测试 Runtime 应创建");
        let revisions = AtomicU64::new(0);

        let snapshot = save_general(
            &paths,
            &runtime,
            GeneralPatch {
                appearance: Some(super::AppearanceMode::Dark),
                ..GeneralPatch::default()
            },
            SettingsPage::Appearance,
            false,
            &revisions,
            None,
            None,
        )
        .expect("外观设置应保存");

        assert_eq!(snapshot.page, SettingsPage::Appearance);
        assert_eq!(
            snapshot.general.expect("应返回常规快照").appearance,
            super::AppearanceMode::Dark
        );
    }

    #[test]
    fn save_general_persists_terminal_settings_in_app_settings() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let paths = NativePaths::from_data_root(directory.path().to_owned());
        let runtime =
            crate::agent_runtime::AgentRuntime::new_for_control_test(directory.path().to_owned())
                .expect("测试 Runtime 应创建");
        let revisions = AtomicU64::new(0);

        save_general(
            &paths,
            &runtime,
            GeneralPatch {
                terminal_shell: Some(TerminalShell::Cmd),
                terminal_font_family: Some("Maple Mono".to_owned()),
                terminal_inherit_system_profile: Some(false),
                ..GeneralPatch::default()
            },
            SettingsPage::General,
            true,
            &revisions,
            None,
            None,
        )
        .expect("常规终端设置应保存");

        let settings = crate::app_settings::get(&paths).expect("读取应用设置");
        assert_eq!(settings.terminal_shell, TerminalShell::Cmd);
        assert_eq!(settings.terminal_font_family, "Maple Mono");
        assert!(!settings.terminal_inherit_system_profile);
    }

    /// 真实适配器创建 Provider 后重新加载页面，确认新模型的能力键完整持久化。
    #[tokio::test]
    async fn create_provider_initializes_and_loads_all_vision_capability_keys() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        // 本测试只验证 Provider 持久化；新鲜目录快照防止自愈任务访问公网，
        // 也避免 Tokio 退出时等待不可取消的 blocking 网络下载。
        std::fs::write(directory.path().join("models-dev-catalog.json"), b"{}")
            .expect("写入离线模型目录快照");
        let paths = Arc::new(NativePaths::from_data_root(directory.path().to_owned()));
        let runtime =
            crate::agent_runtime::AgentRuntime::new_for_control_test(directory.path().to_owned())
                .expect("测试 Runtime 应创建");
        let adapter = NativeSettingsAdapter::new_with_operation_gate(
            Arc::clone(&paths),
            runtime,
            Arc::new(TestSettingsDomain),
            tokio::runtime::Handle::current(),
            Arc::new(tokio::sync::Mutex::new(())),
        );
        let models = vec!["Provider30b".to_owned(), "ProviderVision".to_owned()];

        let created = adapter
            .execute(SettingsCommand::CreateProvider {
                id: "provider-test".to_owned(),
                name: "Provider Test".to_owned(),
                base_url: "https://api.example.com/v1".to_owned(),
                api_backend: "responses".to_owned(),
                api_key: None,
                models: models.clone(),
            })
            .await
            .expect("适配器创建 Provider 应成功");
        let created_provider = created
            .providers
            .as_ref()
            .and_then(|settings| settings.providers.first())
            .expect("创建结果应包含 Provider");
        assert_eq!(
            created_provider
                .models
                .iter()
                .map(|model| (model.id.as_str(), model.supports_vision))
                .collect::<Vec<_>>(),
            [("Provider30b", false), ("ProviderVision", false)]
        );

        let persisted = crate::providers::list(paths.as_ref()).expect("Provider 应持久化");
        let persisted_provider = persisted.providers.first().expect("应存在持久化 Provider");
        assert_eq!(
            persisted_provider
                .supports_vision
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            models
        );
        assert!(
            persisted_provider
                .supports_vision
                .values()
                .all(|supports_vision| !supports_vision)
        );

        let loaded = adapter
            .load(SettingsPage::Providers)
            .await
            .expect("适配器重新加载 Provider 应成功");
        let loaded_provider = loaded
            .providers
            .as_ref()
            .and_then(|settings| settings.providers.first())
            .expect("重新加载结果应包含 Provider");
        assert_eq!(
            loaded_provider
                .models
                .iter()
                .map(|model| (model.id.as_str(), model.supports_vision))
                .collect::<Vec<_>>(),
            [("Provider30b", false), ("ProviderVision", false)]
        );
    }

    /// 新进程必须从磁盘恢复完整代码显示设置；错误补丁不能覆盖已确认的偏好。
    #[test]
    fn code_settings_cold_read_and_invalid_patch_preserve_confirmed_file() {
        let directory = tempfile::tempdir().expect("创建设置隔离目录");
        let paths = NativePaths::from_data_root(directory.path().to_owned());
        let runtime =
            crate::agent_runtime::AgentRuntime::new_for_control_test(directory.path().to_owned())
                .expect("创建测试 Runtime");
        let revisions = AtomicU64::new(0);
        let code = super::CodeAppearanceSettings {
            font_family: "Consolas".to_owned(),
            font_size_px: 16.0,
            show_line_numbers: false,
            wrap_long_lines: true,
            light_theme: super::CodeSyntaxTheme::GitHubDark,
            dark_theme: super::CodeSyntaxTheme::GitHubLight,
        };
        save_general(
            &paths,
            &runtime,
            GeneralPatch {
                code: Some(code.clone()),
                ..GeneralPatch::default()
            },
            SettingsPage::Appearance,
            false,
            &revisions,
            None,
            None,
        )
        .expect("保存完整代码设置");
        let cold_paths = NativePaths::from_data_root(directory.path().to_owned());
        let restored = super::read_general_file(&cold_paths).expect("冷读取设置");
        assert_eq!(restored.version, 2);
        assert_eq!(
            serde_json::to_value(restored.code).unwrap(),
            serde_json::to_value(&code).unwrap()
        );
        let confirmed = std::fs::read(super::general_file_path(&paths)).unwrap();
        let mut invalid = code;
        invalid.font_size_px = 21.0;
        assert!(
            save_general(
                &paths,
                &runtime,
                GeneralPatch {
                    code: Some(invalid),
                    ..GeneralPatch::default()
                },
                SettingsPage::Appearance,
                false,
                &revisions,
                None,
                None
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(super::general_file_path(&paths)).unwrap(),
            confirmed
        );
    }

    /// 旧版本、缺失代码配置和未知字段均严格拒绝，不能悄悄恢复为默认值。
    #[test]
    fn general_settings_reject_old_incomplete_and_unknown_code_documents() {
        let directory = tempfile::tempdir().expect("创建设置隔离目录");
        let paths = NativePaths::from_data_root(directory.path().to_owned());
        let current = serde_json::to_value(super::GeneralFile::default()).unwrap();
        let mut old = current.clone();
        old["version"] = serde_json::json!(1);
        let mut missing = current.clone();
        missing.as_object_mut().unwrap().remove("code");
        let mut unknown = current.clone();
        unknown["code"]["legacyTheme"] = serde_json::json!("dark");
        let mut invalid_theme = current;
        invalid_theme["code"]["lightTheme"] = serde_json::json!("githubDark");
        for document in [old, missing, unknown, invalid_theme] {
            let bytes = serde_json::to_vec(&document).unwrap();
            let path = super::general_file_path(&paths);
            std::fs::write(&path, &bytes).unwrap();
            assert!(super::read_general_file(&paths).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn shared_general_invalidation_covers_general_and_appearance() {
        let (events, mut receiver) = broadcast::channel(4);
        publish_invalidation(&events, SettingsPage::Appearance);

        assert!(matches!(
            receiver.try_recv().expect("应收到常规失效事件"),
            SettingsEvent::Invalidate(SettingsPage::General)
        ));
        assert!(matches!(
            receiver.try_recv().expect("应收到外观失效事件"),
            SettingsEvent::Invalidate(SettingsPage::Appearance)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_model_requires_real_responses_generation_and_reports_success() {
        let (result, request) = execute_model_test(200, responses_body("OK")).await;
        let snapshot = result.expect("真实模型生成测试应成功");
        assert!(
            snapshot
                .notices
                .iter()
                .any(|notice| notice.code == "provider_model_verified")
        );
        assert_model_test_request(&request.expect("应捕获模型测试请求"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_model_rejects_empty_responses_text() {
        let (result, request) = execute_model_test(200, responses_body(" \n\t ")).await;
        let error = result.expect_err("空正文不能被视为模型测试成功");
        assert_eq!(error.code, "provider_verify_empty");
        assert_model_test_request(&request.expect("应捕获空正文测试请求"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_model_rejects_http_generation_error() {
        let (result, request) = execute_model_test(
            500,
            json!({
                "error": {
                    "type": "authentication_error",
                    "code": "authentication_error",
                    "message": "fixture generation request rejected"
                }
            }),
        )
        .await;
        let error = result.expect_err("HTTP 500 不能被视为模型测试成功");
        assert_eq!(error.code, "provider_verify");
        assert_model_test_request(&request.expect("应捕获 HTTP 错误测试请求"));
    }
}
