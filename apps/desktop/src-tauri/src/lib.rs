mod acp_host;
mod agent_prompt;
pub mod agent_runtime;
mod analytics;
mod app_exit;
#[cfg(target_os = "macos")]
mod app_menu;
mod app_settings;
mod app_updates;
mod browser;
mod client_request;
mod diagnostics;
mod elicitation;
mod extensions;
mod http_response;
mod mcp_oauth;
mod memories;
mod model_metadata;
#[cfg(all(test, windows, feature = "native-desktop-tests"))]
mod native_command_tests;
#[cfg(all(test, windows, feature = "native-desktop-tests"))]
mod native_mailbox_tests;
#[cfg(all(test, windows, feature = "native-desktop-tests"))]
mod native_visual_tests;
mod network_proxy;
mod path_utils;
mod personalization;
mod plugin_secrets;
mod plugins;
mod power_management;
mod providers;
mod remote_host_client;
mod session_commands;
mod shell_env;
mod storage;
mod task_notifications;
mod terminal;
mod tray;
mod web_host;
mod workspace;

use crate::agent_runtime::AgentRuntime;
use crate::providers::{ProviderModelsResult, ProviderUpsert, ProvidersListResult};
use keencode_acp::{HostDiscoveryRecord, HostOwnerKind, HostTransportKind};
use keencode_cli::{
    HostDispatch, LocalHostServer, LocalHostServerConfig, LocalHostServerHandle, NoopHostActivity,
};
use keencode_runtime::{HostRuntime, HostRuntimeAcquire};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager, State};

const OBSERVABILITY_EVENT: &str = "keencode://observability";

/// Desktop 持有的本地 Host transport 与根级 lease；只在明确退出时释放。
struct DesktopHostState {
    runtime: Arc<HostRuntime>,
    server: Mutex<Option<LocalHostServerHandle>>,
    /// Remote Client 模式只保留 transport 句柄；不拥有也不关闭既有 Host。
    remote_client: Mutex<Option<Arc<remote_host_client::RemoteHostClient>>>,
    shutdown_started: AtomicBool,
}

impl DesktopHostState {
    /// 先停止接受本地连接，再删除 discovery 并释放根级 lease。
    fn shutdown(&self) {
        if self.shutdown_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let server = self.server.lock().ok().and_then(|mut server| server.take());
        if let Some(server) = server
            && let Err(error) = tauri::async_runtime::block_on(server.stop())
        {
            tracing::error!(%error, "Desktop Host transport shutdown failed");
        }
        if self
            .remote_client
            .lock()
            .map(|client| client.is_some())
            .unwrap_or(false)
        {
            // Drop 由 DesktopHostState 生命周期完成；Client 断开不会触发远程 Host
            // shutdown，也不会删除其 discovery 或 lease。
            if let Ok(mut client) = self.remote_client.lock()
                && let Some(client) = client.take()
            {
                client.shutdown();
            }
        } else if let Err(error) = self.runtime.explicit_shutdown() {
            tracing::error!(%error, "Desktop Host runtime shutdown failed");
        }
    }
}

/// 为当前数据根生成与 headless Host 一致的本机端点。
fn desktop_host_endpoint(root: &Path, fingerprint: &str) -> (HostTransportKind, String) {
    #[cfg(windows)]
    {
        let _ = root;
        return (
            HostTransportKind::NamedPipe,
            format!(r"\\.\pipe\keencode-{fingerprint}"),
        );
    }
    #[cfg(unix)]
    {
        let _ = fingerprint;
        return (
            HostTransportKind::UnixSocket,
            root.join("host.sock").to_string_lossy().into_owned(),
        );
    }
    #[allow(unreachable_code)]
    (
        HostTransportKind::UnixSocket,
        root.join("host.sock").to_string_lossy().into_owned(),
    )
}

/// OS lease 已归当前进程所有时，清理 Unix 上一次异常退出留下的 socket 节点。
fn cleanup_stale_desktop_host_endpoint(transport: HostTransportKind, endpoint: &str) {
    #[cfg(unix)]
    if transport == HostTransportKind::UnixSocket {
        use std::os::unix::fs::FileTypeExt;
        let path = Path::new(endpoint);
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && metadata.file_type().is_socket()
        {
            let _ = std::fs::remove_file(path);
        }
    }
    let _ = (transport, endpoint);
}

/// ACP Host 使用的无凭据 Provider 模型目录条目。
pub(crate) struct AcpProviderCatalogEntry {
    /// Provider 稳定标识。
    pub(crate) id: String,
    /// Provider 用户可见名称。
    pub(crate) name: String,
    /// Provider 当前允许选择的精确模型集合。
    pub(crate) models: Vec<String>,
}

/// ACP Host 使用的当前 Provider 模型目录；不返回 API Key 或其他敏感配置。
pub(crate) struct AcpProviderCatalog {
    /// 当前全部 Provider 的无凭据模型列表。
    pub(crate) providers: Vec<AcpProviderCatalogEntry>,
    /// 全局设置中当前激活的 Provider。
    pub(crate) active_provider_id: Option<String>,
    /// 全局设置中当前激活 Provider 的模型。
    pub(crate) active_model_id: Option<String>,
}

/// 从现有 Provider 持久配置读取 ACP 可见的无凭据模型目录。
pub(crate) fn acp_provider_catalog(app: &AppHandle) -> Result<AcpProviderCatalog, String> {
    let list = providers::list(app).map_err(|_| "无法读取 Provider 模型目录".to_owned())?;
    Ok(AcpProviderCatalog {
        providers: list
            .providers
            .into_iter()
            .map(|provider| AcpProviderCatalogEntry {
                id: provider.id,
                name: provider.name,
                models: provider.models,
            })
            .collect(),
        active_provider_id: list.active_provider_id,
        active_model_id: list.default_model,
    })
}

/// 返回后端诊断日志的绝对路径。
#[tauri::command]
fn diagnostics_log_path(diagnostics: State<'_, Arc<diagnostics::Diagnostics>>) -> String {
    path_utils::path_to_frontend(diagnostics.path())
}

/// 记录前端无法完成 Tauri IPC 时的错误摘要。
#[tauri::command]
fn diagnostics_record(
    component: String,
    message: String,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) {
    diagnostics.error(&component, message);
}

/// 记录会导致前端失去正常控制流的未捕获异常；普通 console/error 仍只写日志。
#[tauri::command]
fn diagnostics_crash_record(
    kind: String,
    message: String,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) {
    diagnostics.record_crash(diagnostics::observability::CrashRecord {
        occurred_at_ms: diagnostics::observability::now_epoch_ms(),
        kind: kind.clone(),
        message: "frontend uncaught exception".to_owned(),
        backtrace: Some(message.clone()),
    });
    diagnostics.error(&kind, message);
}

/// 记录前端聚合后的性能数据；正常观测不得污染错误级诊断。
#[tauri::command]
fn performance_record(
    component: String,
    message: String,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) {
    diagnostics.log("info", &component, message);
}

/// 返回当前进程内已经脱敏、有界的结构化运行时观测快照。
#[tauri::command]
fn diagnostics_snapshot(
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> diagnostics::observability::ObservabilitySnapshot {
    diagnostics.observability().snapshot()
}

/// 导出后端生成的脱敏观测 JSON；命令边界不读取原始诊断日志。
#[tauri::command]
fn diagnostics_export(
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<String, String> {
    diagnostics.observability().export_redacted()
}

/// 记录前端或其他产生端计算完成的标量观测值。
#[tauri::command]
fn diagnostics_metric_record(
    name: String,
    value: f64,
    unit: String,
    tags: BTreeMap<String, String>,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) {
    diagnostics
        .observability()
        .record_metric(&name, value, &unit, tags);
}

/// 接收前端 WebView 资源采样；后端只保留有界、脱敏的数值摘要。
#[tauri::command]
fn diagnostics_resource_record(
    mut sample: diagnostics::observability::ResourceSample,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) {
    let process = diagnostics.process_resource_sample();
    sample.process_id = std::process::id();
    sample.cpu_percent = process.cpu_percent;
    sample.resident_bytes = process.resident_bytes;
    sample.private_bytes = process.private_bytes;
    sample.virtual_bytes = process.virtual_bytes;
    sample.process_count = process.process_count;
    diagnostics.observability().record_resource_sample(sample);
}

/// 接收产生端已经用单调时钟计算完成的 Trace/TTFT，不跨进程重算时长。
#[tauri::command]
fn diagnostics_trace_record(
    trace: diagnostics::observability::TraceSample,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) {
    let observability = diagnostics.observability();
    if let Some(ttft_ms) = trace.ttft_ms {
        observability.record_ttft(ttft_ms as f64);
    }
    observability.record_trace(trace);
}

/// 将结构化观测事件从专用转发线程送到 Tauri，避免阻塞 Agent/IPC 调用线程。
fn spawn_observability_event_bridge(
    app: &AppHandle,
    observability: Arc<diagnostics::observability::ObservabilityStore>,
) {
    let receiver = observability.subscribe();
    let app = app.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("keencode-observability-events".to_owned())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                if app.emit(OBSERVABILITY_EVENT, event).is_err() {
                    break;
                }
            }
        })
    {
        eprintln!("[keencode] 无法启动观测事件转发线程: {error}");
    }
}

/// 前端完成首次绘制后报告可交互时间点。
#[tauri::command]
fn startup_frontend_ready(diagnostics: State<'_, Arc<diagnostics::Diagnostics>>) {
    diagnostics.startup_phase("frontend_interactive");
}

/// 返回当前进程持有的本地 Runtime；Desktop Client 不得在本地读写第二份
/// Provider/Session 事实，必须明确提示调用方由远程 Host 负责。
fn require_owned_runtime(app: &AppHandle) -> Result<Arc<AgentRuntime>, String> {
    app.try_state::<Arc<AgentRuntime>>()
        .map(|state| Arc::clone(state.inner()))
        .ok_or_else(|| {
            "当前 Desktop Client 未持有 Agent Runtime；请通过远程 Host 执行此操作".to_owned()
        })
}

/// 按需读取当前 Session 工具图片，二进制返回且不接受任意文件路径。
#[tauri::command]
async fn read_tool_image(
    app: AppHandle,
    session_id: String,
    artifact_id: String,
) -> Result<tauri::ipc::Response, String> {
    let runtime = require_owned_runtime(&app)?;
    tauri::async_runtime::spawn_blocking(move || runtime.read_tool_image(&session_id, &artifact_id))
        .await
        .map_err(|_| "图片读取任务失败".to_owned())?
        .map(tauri::ipc::Response::new)
        .map_err(|_| "无法读取工具图片快照".to_owned())
}

/// 返回当前完整应用设置。
#[tauri::command]
fn settings_get(app: AppHandle) -> Result<app_settings::AppSettings, String> {
    app_settings::get(&app).map_err(|error| error.to_string())
}

/// 回滚设置更新已经应用的运行时副作用，避免持久化失败留下半应用状态。
fn rollback_settings_side_effects(
    runtime: &AgentRuntime,
    power_management: &power_management::PowerManagement,
    previous: &app_settings::AppSettings,
    previous_web_service: Option<keencode_tools::WebServiceConfig>,
    restore_background_agent_limit: bool,
    restore_keep_computer_awake: bool,
    restore_web_service: bool,
) {
    if restore_web_service {
        let _ = runtime.set_web_service_config(previous_web_service);
    }
    if restore_keep_computer_awake {
        let _ = power_management.set_keep_awake(previous.keep_computer_awake);
    }
    if restore_background_agent_limit {
        let _ = runtime.set_background_agent_limit(previous.background_agent_limit as usize);
    }
}

/// 应用并保存一个严格类型的设置补丁。
#[tauri::command]
async fn settings_set(
    settings: app_settings::AppSettingsPatch,
    app: AppHandle,
    power_management: State<'_, Arc<power_management::PowerManagement>>,
    memories: State<'_, Arc<memories::MemoryService>>,
    web_host: State<'_, Arc<web_host::WebHostManager>>,
) -> Result<app_settings::AppSettings, String> {
    let runtime = require_owned_runtime(&app)?;
    let previous = app_settings::get(&app).map_err(|error| error.to_string())?;
    let previous_web_service = previous
        .web_service_config()
        .map_err(|error| error.to_string())?;
    let web_service_update = settings
        .web_service_config_update()
        .map_err(|error| error.to_string())?;
    let web_host_update = settings
        .web_host
        .as_ref()
        .map(|value| {
            let mut candidate = previous.clone();
            candidate.web_host = value.clone();
            candidate.web_host_settings(&app)
        })
        .transpose()
        .map_err(|error| error.to_string())?;
    let mut background_agent_limit_changed = false;
    let mut keep_computer_awake_changed = false;
    let mut web_service_changed = false;
    if let Some(limit) = settings.background_agent_limit {
        if let Err(error) = runtime.set_background_agent_limit(limit as usize) {
            return Err(error.to_string());
        }
        background_agent_limit_changed = true;
    }
    if let Some(web_service) = web_service_update {
        if let Err(error) = runtime.set_web_service_config(web_service) {
            rollback_settings_side_effects(
                runtime.as_ref(),
                power_management.inner(),
                &previous,
                previous_web_service.clone(),
                background_agent_limit_changed,
                false,
                false,
            );
            return Err(error.to_string());
        }
        web_service_changed = true;
    }
    if let Some(enabled) = settings.keep_computer_awake
        && let Err(error) = power_management.set_keep_awake(enabled)
    {
        rollback_settings_side_effects(
            runtime.as_ref(),
            power_management.inner(),
            &previous,
            previous_web_service.clone(),
            background_agent_limit_changed,
            true,
            web_service_changed,
        );
        return Err(error.to_string());
    } else if settings.keep_computer_awake.is_some() {
        keep_computer_awake_changed = true;
    }
    match app_settings::set(&app, settings) {
        Ok(saved) => {
            if let Some(web_settings) = web_host_update {
                web_host
                    .set_settings(web_settings)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            memories.set_enabled(saved.local_memories);
            if saved.interface_language != previous.interface_language {
                // macOS 应用菜单是原生界面，语言变化后必须重建才能跟随界面语言。
                #[cfg(target_os = "macos")]
                app_menu::apply(&app, saved.interface_language);
            }
            if saved.local_memories
                && (saved.interface_language != previous.interface_language
                    || !previous.local_memories)
            {
                memories.trigger(runtime.clone(), None, saved.interface_language, true);
            }
            Ok(saved)
        }
        Err(error) => {
            rollback_settings_side_effects(
                runtime.as_ref(),
                power_management.inner(),
                &previous,
                previous_web_service,
                background_agent_limit_changed,
                keep_computer_awake_changed,
                web_service_changed,
            );
            Err(error.to_string())
        }
    }
}

/// 启动 Desktop Web Host；Token、静态根和上传根不从前端命令参数读取。
#[tauri::command]
async fn web_host_start(
    port: Option<u16>,
    web_host: State<'_, Arc<web_host::WebHostManager>>,
) -> Result<web_host::WebHostStatus, String> {
    web_host
        .start(port)
        .await
        .map_err(|error| error.to_string())
}

/// 停止 Desktop Web Host 并撤销旧浏览器会话。
#[tauri::command]
async fn web_host_stop(
    web_host: State<'_, Arc<web_host::WebHostManager>>,
) -> Result<web_host::WebHostStatus, String> {
    web_host.stop().await.map_err(|error| error.to_string())
}

/// 返回不含 Token 的 Desktop Web Host 状态。
#[tauri::command]
async fn web_host_status(
    web_host: State<'_, Arc<web_host::WebHostManager>>,
) -> Result<web_host::WebHostStatus, String> {
    Ok(web_host.status().await)
}

/// 保存新的 Web Token；正文只在命令调用栈与系统凭据 provider 中出现。
#[tauri::command]
async fn web_host_set_token(
    token: String,
    web_host: State<'_, Arc<web_host::WebHostManager>>,
) -> Result<web_host::WebHostStatus, String> {
    let token = keencode_web::WebToken::try_from(token).map_err(|error| error.to_string())?;
    web_host
        .set_token(token)
        .await
        .map_err(|error| error.to_string())
}

/// 返回 KeenCode 自定义模型供应商列表。
#[tauri::command]
fn providers_list(app: AppHandle) -> Result<ProvidersListResult, String> {
    require_owned_runtime(&app)?;
    providers::list(&app).map_err(|error| error.to_string())
}

/// 新增或更新一个自定义模型供应商。
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn providers_upsert(
    id: String,
    models: Vec<String>,
    base_url: String,
    name: Option<String>,
    api_key: Option<String>,
    api_backend: String,
    context_windows: std::collections::BTreeMap<String, u64>,
    max_output_tokens: std::collections::BTreeMap<String, u32>,
    chat_output_token_field: Option<keencode_provider::ChatOutputTokenField>,
    supports_vision: std::collections::BTreeMap<String, bool>,
    create_only: bool,
    app: AppHandle,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<ProvidersListResult, String> {
    let agent_runtime = require_owned_runtime(&app)?;
    diagnostics.log(
        "info",
        "ipc.providers_upsert",
        format!(
            "命令进入 provider_id={} models={} api_key_present={}",
            id,
            models.len(),
            api_key.is_some()
        ),
    );
    let result = providers::upsert(
        &app,
        ProviderUpsert {
            id: id.clone(),
            models,
            base_url,
            name,
            api_backend,
            api_key,
            context_windows,
            max_output_tokens,
            chat_output_token_field: chat_output_token_field.unwrap_or_default(),
            supports_vision,
            create_only,
        },
    )
    .map_err(|error| {
        diagnostics.log(
            "error",
            "ipc.providers_upsert",
            format!("保存失败: {error:#}"),
        );
        error.to_string()
    })?;
    agent_runtime.reload_providers(&app).map_err(|error| {
        diagnostics.log(
            "error",
            "ipc.providers_upsert",
            format!("热加载失败: {error}"),
        );
        error.to_string()
    })?;
    diagnostics.log("info", "ipc.providers_upsert", "命令完成");
    Ok(result)
}

/// 删除一个自定义模型供应商。
#[tauri::command]
async fn providers_remove(
    id: String,
    app: AppHandle,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<ProvidersListResult, String> {
    let agent_runtime = require_owned_runtime(&app)?;
    diagnostics.log(
        "info",
        "ipc.providers_remove",
        format!("命令进入 provider_id={id}"),
    );
    let result = providers::remove(&app, &id).map_err(|error| {
        diagnostics.log(
            "error",
            "ipc.providers_remove",
            format!("删除失败: {error:#}"),
        );
        error.to_string()
    })?;
    agent_runtime.reload_providers(&app).map_err(|error| {
        diagnostics.log(
            "error",
            "ipc.providers_remove",
            format!("热加载失败: {error}"),
        );
        error.to_string()
    })?;
    diagnostics.log("info", "ipc.providers_remove", "命令完成");
    Ok(result)
}

/// 选择任务使用的模型并切换到对应供应商。
#[tauri::command]
async fn providers_select_model(
    provider_id: String,
    model_id: String,
    app: AppHandle,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<ProvidersListResult, String> {
    let agent_runtime = require_owned_runtime(&app)?;
    diagnostics.log(
        "info",
        "ipc.providers_select_model",
        format!("命令进入 provider_id={} model_id={}", provider_id, model_id),
    );
    let result = providers::select_model(&app, &provider_id, &model_id).map_err(|error| {
        diagnostics.log(
            "error",
            "ipc.providers_select_model",
            format!("选择失败: {error:#}"),
        );
        error.to_string()
    })?;
    agent_runtime.reload_providers(&app).map_err(|error| {
        diagnostics.log(
            "error",
            "ipc.providers_select_model",
            format!("热加载失败: {error}"),
        );
        error.to_string()
    })?;
    diagnostics.log("info", "ipc.providers_select_model", "命令完成");
    Ok(result)
}

/// 查询一个自定义供应商公开的模型目录。
#[tauri::command]
fn providers_list_models(
    base_url: String,
    api_key: Option<String>,
    provider_id: Option<String>,
    api_backend: String,
    app: AppHandle,
) -> Result<ProviderModelsResult, String> {
    if let Some(provider_id) = provider_id.as_deref() {
        providers::validate_model_catalog_scope(&app, provider_id, &base_url, &api_backend)
            .map_err(|error| keencode_model::redact_error_secrets(&error.to_string()))?;
    }
    providers::list_models(&base_url, api_key.as_deref(), &api_backend)
        .map_err(|error| keencode_model::redact_error_secrets(&error.to_string()))
}

/// 导出单个供应商的配置 JSON 文档。
#[tauri::command]
fn providers_export(provider_id: String, app: AppHandle) -> Result<String, String> {
    require_owned_runtime(&app)?;
    providers::export(&app, &provider_id).map_err(|error| error.to_string())
}

/// 导入供应商配置并按标识合并到当前列表，随后热加载运行时。
#[tauri::command]
async fn providers_import(
    config: String,
    app: AppHandle,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<providers::ProvidersImportResult, String> {
    let agent_runtime = require_owned_runtime(&app)?;
    diagnostics.log(
        "info",
        "ipc.providers_import",
        format!("命令进入 bytes={}", config.len()),
    );
    let import_app = app.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || providers::import(&import_app, &config))
            .await
            .map_err(|error| format!("供应商导入后台任务失败：{error}"))?
            .map_err(|error| {
                diagnostics.log(
                    "error",
                    "ipc.providers_import",
                    format!("导入失败: {error:#}"),
                );
                error.to_string()
            })?;
    agent_runtime.reload_providers(&app).map_err(|error| {
        diagnostics.log(
            "error",
            "ipc.providers_import",
            format!("热加载失败: {error}"),
        );
        error.to_string()
    })?;
    diagnostics.log(
        "info",
        "ipc.providers_import",
        format!("命令完成 added={} updated={}", result.added, result.updated),
    );
    Ok(result)
}

/// 启动 KeenCode 桌面后端。
pub fn run() {
    // Finder/Dock 启动的打包应用只继承 launchd 最小 PATH；后台捕获登录
    // Shell 的 PATH 写入命令工具覆盖，不阻塞窗口启动。
    shell_env::begin_login_shell_path_capture();
    let app = desktop_builder(Instant::now())
        .build(tauri::generate_context!())
        .expect("构建 KeenCode 失败");
    app.run(handle_run_event);
}

/// 共享正式桌面装配，原生测试只在独立测试进程中断开受控记录器通道。
fn desktop_builder(startup_started_at: Instant) -> tauri::Builder<tauri::Wry> {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build());
    // 应用菜单栏是 macOS 专属界面；其他平台保持无菜单栏的原有工作台布局。
    #[cfg(target_os = "macos")]
    let builder = builder.on_menu_event(app_menu::handle_menu_event);
    builder
        .setup(move |app| {
            use tauri::Manager;
            let diagnostics = diagnostics::Diagnostics::init(app.handle(), startup_started_at);
            diagnostics.install();
            spawn_observability_event_bridge(app.handle(), diagnostics.observability());
            diagnostics.startup_phase("backend_setup");
            diagnostics.log(
                "info",
                "startup",
                format!("应用启动，日志路径={}", diagnostics.path().display()),
            );
            app.manage(Arc::clone(&diagnostics));
            // 模型目录快照在后台按需刷新；查询只读本地文件，不阻塞启动。
            model_metadata::spawn_startup_refresh(app.handle().clone());
            app.manage(app_exit::ExitState::default());
            app.manage(app_updates::PendingUpdate::default());
            let loaded_settings = app_settings::load_for_startup(app.handle())?;
            if let Some(load_error) = &loaded_settings.load_error {
                diagnostics.log(
                    "error",
                    "startup.settings",
                    format!("应用设置读取失败，本次启动使用默认设置，原文件保持原样：{load_error}"),
                );
            }
            for warning in &loaded_settings.warnings {
                diagnostics.log("warn", "startup.settings", warning);
            }
            let current_settings = loaded_settings.settings;
            #[cfg(target_os = "macos")]
            app_menu::apply(app.handle(), current_settings.interface_language);
            diagnostics.startup_phase("settings_ready");
            let power_management = Arc::new(power_management::PowerManagement::new());
            if let Err(error) =
                power_management.set_keep_awake(current_settings.keep_computer_awake)
            {
                diagnostics.log(
                    "warn",
                    "startup.settings",
                    format!("无法应用保持唤醒设置，已继续启动: {error:#}"),
                );
            }
            app.manage(power_management);
            app.manage(Arc::new(task_notifications::TaskNotifications::default()));
            app.manage(Arc::new(terminal::TerminalManager::default()));
            // 扩展状态必须先进入 Tauri state，供 Agent Runtime 原子装配完整候选代次。
            app.manage(extensions::ExtensionsState::default());
            // OAuth 注册表独立于扩展配置状态；其 token 只交给系统密钥库适配器。
            app.manage(Arc::new(mcp_oauth::McpOAuthRegistry::new_with_event_sink(
                acp_host::mcp_oauth_event_sink(app.handle()),
            )));
            let data_root = storage::root_dir(app.handle()).map_err(|error| error.to_string())?;
            let host_acquire = HostRuntime::acquire(&data_root, HostOwnerKind::Desktop)
                .map_err(|error| error.to_string())?;
            let host_runtime = host_acquire.runtime().clone();
            let (agent_runtime, remote_client) = match host_acquire {
                HostRuntimeAcquire::Owned(_) => {
                    let agent_runtime = AgentRuntime::build(app.handle())?;
                    agent_runtime.set_web_service_config(current_settings.web_service_config()?)?;
                    agent_runtime.set_background_agent_limit(
                        current_settings.background_agent_limit as usize,
                    )?;
                    diagnostics.startup_phase("agent_runtime_ready");
                    (Some(agent_runtime), None)
                }
                HostRuntimeAcquire::Client(_) => {
                    // Client 分支只连接既有 Host；禁止构造第二 Runtime、第二
                    // Journal 或第二 Provider 注册表。
                    let client = tauri::async_runtime::block_on(
                        remote_host_client::RemoteHostClient::connect(
                            app.handle().clone(),
                            remote_host_client::config_for_root(&data_root),
                        ),
                    )
                    .map_err(|error| format!("连接既有 KeenCode Host 失败: {error}"))?;
                    diagnostics.startup_phase("remote_host_connected");
                    (None, Some(Arc::new(client)))
                }
            };
            let memories = memories::MemoryService::new(app.handle())?;
            memories.set_enabled(current_settings.local_memories);
            app.manage(Arc::clone(&memories));
            if let Some(agent_runtime) = agent_runtime.as_ref() {
                app.manage(Arc::clone(agent_runtime));
            }
            let web_settings = current_settings
                .web_host_settings(app.handle())
                .map_err(|error| error.to_string())?;
            let web_manager = web_host::WebHostManager::new(
                web_settings,
                web_host::credential_provider().map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            web_manager
                .set_observability(diagnostics.observability())
                .map_err(|error| error.to_string())?;
            app.manage(Arc::new(web_manager));
            let (local_server, remote_client) = if let Some(agent_runtime) = agent_runtime {
                let acp_host = acp_host::install(
                    app.handle(),
                    Arc::clone(&agent_runtime),
                    Arc::clone(&host_runtime),
                )?;
                let (transport, endpoint) = desktop_host_endpoint(
                    host_runtime.data_root(),
                    host_runtime.data_root_fingerprint(),
                );
                cleanup_stale_desktop_host_endpoint(transport, &endpoint);
                let starting_record = HostDiscoveryRecord::new_with_host_id(
                    transport,
                    endpoint.clone(),
                    HostOwnerKind::Desktop,
                    std::process::id(),
                    host_runtime.host_id().to_owned(),
                    host_runtime.data_root_fingerprint().to_owned(),
                    "starting",
                )
                .map_err(|error| error.to_string())?;
                let dispatch: Arc<dyn HostDispatch> = acp_host;
                let activity = Arc::new(NoopHostActivity);
                // bind 和 spawn 必须处于同一个 Tokio reactor 上下文；Tauri setup
                // 本身是同步线程，离开 block_on 后再 tokio::spawn 会直接 panic。
                let local_server = tauri::async_runtime::block_on(async {
                    LocalHostServer::bind(
                        &starting_record,
                        LocalHostServerConfig::default(),
                        dispatch,
                        activity,
                    )
                    .await
                    .map(LocalHostServer::spawn)
                })
                .map_err(|error| error.to_string())?;
                host_runtime
                    .mark_ready_and_publish(transport, endpoint)
                    .map_err(|error| error.to_string())?;
                diagnostics.startup_phase("host_ready");
                (Some(local_server), None)
            } else {
                let client = remote_client
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| "Remote Host Client 未初始化".to_owned())?;
                acp_host::install_remote_bridge(
                    Arc::clone(&client) as Arc<dyn acp_host::AcpHostBridge>
                )?;
                (None, Some(client))
            };
            app.manage(DesktopHostState {
                runtime: host_runtime,
                server: Mutex::new(local_server),
                remote_client: Mutex::new(remote_client),
                shutdown_started: AtomicBool::new(false),
            });
            // 主窗口与子 WebView 的背景由前端在应用主题和皮肤生效后同步；不要在
            // Rust 启动阶段写入固定深色，否则浅色主题会在透明边缘长期露出黑底。
            // 托盘图标常驻；创建失败不阻断启动，仅记录诊断。
            if let Err(error) = tray::install(app.handle()) {
                diagnostics.log(
                    "warn",
                    "startup.tray",
                    format!("创建系统托盘图标失败，应用继续启动：{error}"),
                );
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            acp_host::workflow_start,
            acp_host::workflow_resume,
            acp_host::workflow_amend,
            settings_get,
            settings_set,
            web_host_start,
            web_host_stop,
            web_host_status,
            web_host_set_token,
            diagnostics_log_path,
            diagnostics_record,
            diagnostics_crash_record,
            performance_record,
            diagnostics_snapshot,
            diagnostics_export,
            diagnostics_metric_record,
            diagnostics_resource_record,
            diagnostics_trace_record,
            startup_frontend_ready,
            app_exit::app_confirm_exit,
            app_updates::app_update_info,
            app_updates::app_update_check,
            app_updates::app_update_install,
            providers_list,
            providers_upsert,
            providers_remove,
            providers_select_model,
            providers_list_models,
            providers_export,
            providers_import,
            model_metadata::model_metadata_get,
            model_metadata::model_metadata_get_many,
            acp_host::acp_dispatch,
            // ── 会话命令（ACP 后端）──
            session_commands::session_disconnect,
            // ── 用量统计与个性化（不涉及 Agent 内核）──
            analytics::request_records_list,
            analytics::task_cache_usage_get,
            analytics::usage_stats_get,
            personalization::custom_instructions_get,
            personalization::custom_instructions_set,
            memories::memories_status,
            memories::memories_reset,
            memories::memories_get,
            memories::memories_set,
            // ── 扩展与工作区（不涉及 Agent 内核）──
            extensions::extensions_set_mcp,
            extensions::extensions_enable_all_mcp,
            extensions::skills_list,
            extensions::agents_list,
            extensions::agents_tool_catalog,
            extensions::agent_detail,
            extensions::agent_create,
            extensions::agent_remove,
            extensions::agent_update,
            extensions::inspect_mcp,
            extensions::plugins_list,
            extensions::plugin_enable,
            extensions::plugin_disable,
            extensions::plugin_uninstall,
            extensions::plugin_details,
            extensions::plugin_user_config_get,
            extensions::plugin_user_config_set,
            extensions::plugin_install,
            extensions::plugin_update,
            extensions::mcp_add,
            extensions::mcp_import,
            extensions::mcp_remove,
            extensions::mcp_doctor,
            extensions::plugin_compatibility::plugin_model_aliases_get,
            extensions::plugin_compatibility::plugin_model_aliases_set,
            extensions::marketplace_list,
            extensions::marketplace_available,
            extensions::marketplace_add,
            extensions::marketplace_remove,
            extensions::marketplace_update,
            workspace::projects_list,
            workspace::project_validate,
            workspace::project_create,
            workspace::project_default_directory,
            workspace::project_remove,
            workspace::project_relocate,
            workspace::project_rename,
            workspace::projects_reorder,
            workspace::project_reveal,
            workspace::paths_classify,
            workspace::path_open,
            workspace::url_open,
            workspace::path_reveal,
            workspace::pick_directory,
            workspace::pick_attach_files,
            workspace::pick_text_file,
            workspace::save_pasted_attachment,
            workspace::read_local_image,
            read_tool_image,
            workspace::fs_list_dir,
            workspace::fs_read_file,
            workspace::fs_write_file,
            workspace::fs_read_absolute,
            workspace::fs_write_absolute,
            workspace::fs_open_path,
            workspace::git_worktrees_list,
            workspace::git_worktree_add,
            workspace::git_worktree_gc,
            workspace::git_status,
            workspace::git_checkout_branch,
            workspace::git_untracked_directory,
            workspace::git_file_diff,
            workspace::git_show_file,
            workspace::git_commit,
            workspace::git_push,
            tray::tray_set_menu,
            tray::tray_set_badge,
            tray::app_close_window,
            terminal::terminal_create,
            terminal::terminal_shells_list,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_close,
            // ── 右侧面板内置浏览器（原生子 WebView）──
            browser::browser_open,
            browser::browser_bounds,
            browser::browser_show,
            browser::browser_hide,
            browser::browser_close,
            browser::browser_navigate,
            browser::browser_reload,
            browser::browser_history
        ])
}

/// 原生退出事件始终经过同一个清理与放行入口。
fn handle_run_event(app: &AppHandle, event: tauri::RunEvent) {
    match event {
        tauri::RunEvent::ExitRequested { api, .. } => {
            let exit_state = app.state::<app_exit::ExitState>();
            if !exit_state.is_approved() {
                api.prevent_exit();
                // 需要用户确认时先恢复窗口，否则确认对话框在托盘常驻状态下不可见。
                if matches!(app_exit::request_exit(app), Ok(active_count) if active_count > 0) {
                    tray::show_main_window(app);
                }
            } else {
                app_exit::run_approved_shutdown(app);
                if let Some(host) = app.try_state::<DesktopHostState>() {
                    host.shutdown();
                }
            }
        }
        #[cfg(target_os = "macos")]
        // Dock 图标点击等重开请求同样恢复主窗口。
        tauri::RunEvent::Reopen { .. } => tray::show_main_window(app),
        _ => {}
    }
}

/// 在桌面运行时创建前应用需要生效的进程环境与设置。
pub fn configure_before_start() {
    network_proxy::configure_before_start();
    app_settings::configure_hardware_acceleration_before_start();
}
