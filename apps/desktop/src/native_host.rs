//! 纯 Rust 桌面组合根与 GPUI 的类型化应用端口。

use anyhow::Result;
use keencode_runtime::RuntimeSession;
use std::{
    collections::HashMap,
    future::Future,
    sync::atomic::AtomicU64,
    sync::{Arc, Mutex, OnceLock},
};

use crate::{
    agent_runtime::AgentRuntime,
    native_memory::NativeMemoryCoordinator,
    native_paths::NativePaths,
    native_ui::{model::*, workbench::NativeWorkbench},
};

mod actions;
mod extensions;
mod hot;
mod interactions;
mod projection;
mod queue;
mod subscription;
#[cfg(test)]
mod tests;
mod window_controls;

/// 窗口与后台任务共享同一 Host；UI 被丢弃不释放执行中的 Agent/PTY。
#[derive(Clone)]
pub struct NativeHost {
    inner: Arc<NativeHostInner>,
}

struct NativeHostInner {
    paths: Arc<NativePaths>,
    runtime: Arc<AgentRuntime>,
    executor: Arc<tokio::runtime::Runtime>,
    memory: Arc<NativeMemoryCoordinator>,
    workbench: Arc<NativeWorkbench>,
    settings: OnceLock<Arc<dyn crate::native_ui::settings::NativeSettingsService>>,
    services: OnceLock<Arc<crate::native_services::NativeServices>>,
    notification_pump: OnceLock<Arc<crate::native_controls::NativeNotificationPump>>,
    drafts: crate::native_drafts::NativeDraftStore,
    // 草稿代次只存在于当前 Host 生命周期；它用于拒绝发送边界之前迟到的 UI 写入。
    draft_generations: Mutex<HashMap<String, DraftGenerationState>>,
    sidebar: crate::native_sidebar::NativeSidebarStore,
    attention: crate::native_attention::NativeAttentionStore,
    question_answers: Mutex<HashMap<String, HashMap<String, serde_json::Value>>>,
    hot: Mutex<HashMap<String, hot::HotState>>,
    hot_changed: tokio::sync::Notify,
    queue_supervisors: Mutex<HashMap<String, Arc<queue::QueueSupervisorState>>>,
    // 订阅计数与空闲回收共用一把锁，窗口切换不会释放刚重新打开的会话。
    native_session_subscribers: Mutex<HashMap<String, usize>>,
    // 原生端口打开/订阅与空闲释放串行，避免释放过程中重建另一份 Session lease。
    native_operations: Arc<tokio::sync::Mutex<()>>,
    // 同一 Host 内串行构建项目扩展候选，避免并行启动重复的 MCP/LSP 进程。
    extension_build_lock: Arc<tokio::sync::Mutex<()>>,
    // Native 候选代次只递增，不复用旧代次。
    next_extension_generation: AtomicU64,
    // 草稿和部分问答不属于执行 Journal，通过有界原生广播更新当前窗口。
    local_changes: tokio::sync::broadcast::Sender<(String, Option<NativeUiEvent>)>,
}

#[derive(Default)]
struct DraftGenerationState {
    latest_edit_generation: Option<u64>,
    cleared_edit_generation: Option<u64>,
}

impl DraftGenerationState {
    fn has_newer_than(&self, edit_generation: u64) -> bool {
        self.latest_edit_generation
            .is_some_and(|latest| latest > edit_generation)
    }

    fn accepts(&mut self, edit_generation: u64) -> bool {
        if self
            .cleared_edit_generation
            .is_some_and(|cleared| edit_generation <= cleared)
            || self
                .latest_edit_generation
                .is_some_and(|latest| edit_generation < latest)
        {
            return false;
        }
        self.latest_edit_generation = Some(
            self.latest_edit_generation
                .map_or(edit_generation, |latest| latest.max(edit_generation)),
        );
        true
    }

    fn mark_cleared(&mut self, edit_generation: u64) {
        self.cleared_edit_generation = Some(
            self.cleared_edit_generation
                .map_or(edit_generation, |cleared| cleared.max(edit_generation)),
        );
        // Send 可能早于首个 SetDraft 到达；先登记这次发送代次，确保同代迟到写入
        // 也会被拒绝，同时保留更高代次的新编辑。
        self.latest_edit_generation = Some(
            self.latest_edit_generation
                .map_or(edit_generation, |latest| latest.max(edit_generation)),
        );
    }
}

/// 展示偏好由原生 Host 持久化；它不保存模型上下文、消息或执行状态。
#[derive(Clone, Copy, Debug, Default)]
struct NativeSessionPreferences {
    vision_enabled: bool,
}

impl NativeHost {
    /// 装配器显式注入运行时与执行器，测试与正式窗口使用同一个真实入口。
    #[cfg(test)]
    pub(crate) fn from_runtime(
        paths: Arc<NativePaths>,
        runtime: Arc<AgentRuntime>,
        executor: Arc<tokio::runtime::Runtime>,
    ) -> Result<Self> {
        let memory_service = crate::memories::MemoryService::new(&paths)?;
        let memory = NativeMemoryCoordinator::new(
            memory_service,
            runtime.clone(),
            executor.as_ref(),
            crate::app_settings::InterfaceLanguage::SimplifiedChinese,
        );
        Self::from_runtime_with_memory(paths, runtime, executor, memory)
    }

    /// 使用启动器已经装配的唯一本地记忆协调器构造 NativeHost。
    pub(crate) fn from_runtime_with_memory(
        paths: Arc<NativePaths>,
        runtime: Arc<AgentRuntime>,
        executor: Arc<tokio::runtime::Runtime>,
        memory: Arc<NativeMemoryCoordinator>,
    ) -> Result<Self> {
        let drafts = crate::native_drafts::NativeDraftStore::new(&paths)?;
        let sidebar = crate::native_sidebar::NativeSidebarStore::new(&paths)?;
        let attention = crate::native_attention::NativeAttentionStore::new(&paths)?;
        Ok(Self {
            inner: Arc::new(NativeHostInner {
                workbench: Arc::new(NativeWorkbench::new(Arc::clone(&paths))),
                paths,
                runtime,
                executor,
                memory,
                settings: OnceLock::new(),
                services: OnceLock::new(),
                notification_pump: OnceLock::new(),
                drafts,
                draft_generations: Mutex::new(HashMap::new()),
                sidebar,
                attention,
                question_answers: Mutex::new(HashMap::new()),
                hot: Mutex::new(HashMap::new()),
                hot_changed: tokio::sync::Notify::new(),
                queue_supervisors: Mutex::new(HashMap::new()),
                native_session_subscribers: Mutex::new(HashMap::new()),
                native_operations: Arc::new(tokio::sync::Mutex::new(())),
                extension_build_lock: Arc::new(tokio::sync::Mutex::new(())),
                next_extension_generation: AtomicU64::new(0),
                local_changes: tokio::sync::broadcast::channel(64).0,
            }),
        })
    }

    pub fn native_paths(&self) -> Arc<NativePaths> {
        Arc::clone(&self.inner.paths)
    }
    pub fn runtime(&self) -> &Arc<AgentRuntime> {
        &self.inner.runtime
    }
    pub fn executor(&self) -> &Arc<tokio::runtime::Runtime> {
        &self.inner.executor
    }
    pub fn workbench(&self) -> Arc<NativeWorkbench> {
        Arc::clone(&self.inner.workbench)
    }
    pub(crate) fn settings(&self) -> Arc<dyn crate::native_ui::settings::NativeSettingsService> {
        Arc::clone(
            self.inner
                .settings
                .get()
                .expect("NativeHost 设置服务必须在窗口创建前装配"),
        )
    }

    fn task<T: Send + 'static>(
        &self,
        future: impl Future<Output = Result<T, NativeUiError>> + Send + 'static,
    ) -> NativeUiTask<T> {
        // GPUI 的后台执行器不是 Tokio reactor；网络、timer 和进程操作统一进入此执行器。
        let inner = self.inner.clone();
        let task = self.inner.executor.spawn(async move {
            let _operation = inner.native_operations.lock().await;
            future.await
        });
        Box::pin(async move { task.await.map_err(ui_error)? })
    }

    fn open_session(&self, session_id: &str) -> Result<RuntimeSession, NativeUiError> {
        let session = crate::session_commands::open_authorized_session(
            &self.inner.runtime,
            &self.inner.paths,
            session_id,
        )
        .map_err(ui_error)?;
        self.inner.memory.watch_session(session_id);
        Ok(session)
    }

    fn draft(&self, session_id: &str) -> Result<DraftFact, NativeUiError> {
        self.inner.drafts.load(session_id).map_err(ui_error)
    }

    pub(super) fn accept_draft_edit(&self, session_id: &str, edit_generation: u64) -> bool {
        self.inner
            .draft_generations
            .lock()
            .expect("草稿代次锁已损坏")
            .entry(session_id.to_owned())
            .or_default()
            .accepts(edit_generation)
    }

    pub(super) fn mark_draft_cleared(&self, session_id: &str, edit_generation: u64) {
        self.inner
            .draft_generations
            .lock()
            .expect("草稿代次锁已损坏")
            .entry(session_id.to_owned())
            .or_default()
            .mark_cleared(edit_generation);
    }

    fn has_newer_draft_edit(&self, session_id: &str, edit_generation: u64) -> bool {
        self.inner
            .draft_generations
            .lock()
            .expect("草稿代次锁已损坏")
            .get(session_id)
            .is_some_and(|state| state.has_newer_than(edit_generation))
    }

    /// Send 成功进入 Runtime 后清理草稿，并建立拒绝迟到 SetDraft 的发送屏障。
    pub(super) fn clear_draft_after_send(
        &self,
        session_id: &str,
        edit_generation: u64,
    ) -> Result<(), NativeUiError> {
        if self.has_newer_draft_edit(session_id, edit_generation) {
            // 用户可能在 Send 已接纳但 Host 尚未完成清理时继续编辑；较新代次属于
            // 下一份草稿，不能被本次发送的清理动作吞掉。
            self.mark_draft_cleared(session_id, edit_generation);
            return Ok(());
        }
        self.inner.drafts.clear(session_id).map_err(ui_error)?;
        self.mark_draft_cleared(session_id, edit_generation);
        let _ = self.inner.local_changes.send((
            session_id.to_owned(),
            Some(NativeUiEvent::DraftChanged(DraftFact::default())),
        ));
        Ok(())
    }

    /// 只回收无窗口观察、无活动工作及无队列的会话；新 UI 操作先取得同一 admission。
    pub(super) fn schedule_idle_native_release(&self, session_id: String) {
        let weak = Arc::downgrade(&self.inner);
        self.inner.executor.spawn(async move {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let _operation = inner.native_operations.lock().await;
            if inner
                .native_session_subscribers
                .lock()
                .expect("原生订阅锁已损坏")
                .contains_key(&session_id)
            {
                return;
            }
            if inner
                .runtime
                .runtime_manager()
                .get(session_id.clone())
                .is_err()
            {
                return;
            }
            if let Some(services) = inner.services.get()
                && !services.release_session_cache(&session_id)
            {
                return;
            }
            match inner.runtime.release_idle_native_session(&session_id).await {
                Ok(true) => {
                    inner
                        .hot
                        .lock()
                        .expect("原生热流锁已损坏")
                        .remove(&session_id);
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(%error, "原生空闲会话释放失败"),
            }
        });
    }

    fn session_preferences(&self, session_id: &str) -> NativeSessionPreferences {
        self.inner
            .runtime
            .runtime_manager()
            .get(session_id.to_owned())
            .ok()
            .and_then(|session| {
                session
                    .read_state(|state| NativeSessionPreferences {
                        vision_enabled: state.vision_enabled,
                    })
                    .ok()
            })
            .unwrap_or_default()
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.inner.memory.shutdown();
        if let Some(pump) = self.inner.notification_pump.get() {
            pump.stop();
        }
        if let Some(services) = self.inner.services.get() {
            services.shutdown_async().await;
        }
        self.cancel_queue_supervisors();
        self.inner.workbench.terminals.shutdown();
        // Native Host 连接在 Runtime 关闭前先显式断开，审批和问答 pending 都沿各自
        // 的 exactly-once 连接收口路径结束，避免窗口退出遗留等待中的工具 Future。
        let connection = interactions::connection();
        self.inner
            .runtime
            .disconnect_permission_connection(&connection);
        self.inner
            .runtime
            .elicitation_coordinator()
            .disconnect(&connection);
        let server_result = self.inner.workbench.dev_servers.shutdown().await;
        let runtime_result = self.inner.runtime.shutdown().await;
        let draft_result = self.inner.drafts.clear_cache();
        // 一个收尾失败不能跳过其余资源；退出控制器会根据汇总错误使用非零退出码。
        let mut errors = Vec::new();
        if let Err(error) = server_result {
            errors.push(format!("收尾开发服务：{error}"));
        }
        if let Err(error) = runtime_result {
            errors.push(format!("收尾 Agent Runtime：{error}"));
        }
        if let Err(error) = draft_result {
            errors.push(format!("释放草稿缓存：{error}"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("{}", errors.join("；"))
        }
    }
}

impl NativeHostApi for NativeHost {
    fn load_workspace(
        &self,
        root_path: String,
        cursor: Option<PageCursor>,
        include_archived: bool,
    ) -> NativeUiTask<WorkspacePage> {
        let host = self.clone();
        self.task(async move { host.workspace_page(&root_path, cursor, include_archived) })
    }

    fn model_catalog(&self) -> NativeUiTask<Vec<ModelCatalog>> {
        let host = self.clone();
        self.task(async move {
            let providers = crate::providers::list(&host.inner.paths).map_err(ui_error)?;
            let default_provider_id = providers.active_provider_id.clone();
            let default_model = providers.default_model.clone();
            Ok(providers
                .providers
                .into_iter()
                .map(|provider| {
                    let provider_id = provider.id;
                    let provider_name = provider.name;
                    let disabled_models = provider.disabled_models;
                    let reasoning_efforts = provider.reasoning_efforts;
                    let supports_vision = provider.supports_vision;
                    let is_default_provider =
                        default_provider_id.as_deref() == Some(provider_id.as_str());
                    ModelCatalog {
                        provider_id,
                        provider_name,
                        models: provider
                            .models
                            .into_iter()
                            .filter(|model| !disabled_models.contains(model))
                            .map(|model| {
                                let is_default = is_default_provider
                                    && default_model.as_deref() == Some(model.as_str());
                                ModelOption {
                                    label: model.clone(),
                                    reasoning_efforts: reasoning_efforts
                                        .get(&model)
                                        .cloned()
                                        .unwrap_or_default(),
                                    supports_images: supports_vision
                                        .get(&model)
                                        .copied()
                                        .unwrap_or(false),
                                    is_default,
                                    model,
                                }
                            })
                            .collect(),
                    }
                })
                .collect())
        })
    }

    fn load_conversation(
        &self,
        session_id: String,
        cursor: Option<PageCursor>,
    ) -> NativeUiTask<ConversationFact> {
        let host = self.clone();
        self.task(async move {
            let session = host.open_session(&session_id)?;
            host.ensure_queue_supervisor(&session_id)?;
            let mut conversation = projection::conversation(&host, &session, cursor)?;
            Arc::make_mut(&mut conversation.messages)
                .extend(host.hot_messages(&session_id).into_iter().map(Arc::new));
            Arc::make_mut(&mut conversation.messages)
                .extend(host.pending_messages(&session_id).into_iter().map(Arc::new));
            Ok(conversation)
        })
    }

    fn dispatch(&self, action: NativeUiAction) -> NativeUiTask<NativeActionReceipt> {
        let host = self.clone();
        if matches!(
            action,
            NativeUiAction::Stop { .. }
                | NativeUiAction::ApproveTool { .. }
                | NativeUiAction::AnswerQuestion { .. }
        ) {
            // 控制信号直接由 Runtime 的 turn/pending 边界校验。它们不能等待
            // 扩展初始化或设置网络请求，否则用户将无法及时停止或回答正在运行的任务。
            let task = self
                .inner
                .executor
                .spawn(async move { host.execute_action(action).await });
            return Box::pin(async move { task.await.map_err(ui_error)? });
        }
        self.task(async move { host.execute_action(action).await })
    }

    fn subscribe_session(
        &self,
        session_id: String,
        sink: Arc<dyn Fn(NativeEventBatch) + Send + Sync>,
    ) -> NativeUiTask<Box<dyn NativeUiSubscription>> {
        let host = self.clone();
        self.task(async move { host.subscribe_native(session_id, sink).await })
    }
}

fn ui_error(error: impl std::fmt::Display) -> NativeUiError {
    ui_error_with_code("native-operation-failed", error)
}

/// 保留领域错误的稳定 code，供需要精确处理的 Native 动作使用。
pub(super) fn ui_error_with_code(
    code: impl Into<String>,
    error: impl std::fmt::Display,
) -> NativeUiError {
    NativeUiError::new(
        code,
        keencode_model::redact_error_secrets_bounded(&error.to_string(), 2048),
        false,
    )
}

pub fn run() -> Result<()> {
    launch::run()
}

mod launch;
