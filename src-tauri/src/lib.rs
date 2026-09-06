mod acp_host;
pub mod agent_runtime;
mod analytics;
mod app_exit;
mod app_settings;
mod app_updates;
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
mod native_exit_tests;
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
mod session_commands;
mod storage;
mod task_notifications;
mod terminal;
mod workspace;

use crate::agent_runtime::AgentRuntime;
use crate::providers::{ProviderModelsResult, ProviderUpsert, ProvidersListResult};
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Manager, State};

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

/// 前端完成首次绘制后报告可交互时间点。
#[tauri::command]
fn startup_frontend_ready(diagnostics: State<'_, Arc<diagnostics::Diagnostics>>) {
    diagnostics.startup_phase("frontend_interactive");
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
    runtime: State<'_, Arc<AgentRuntime>>,
    memories: State<'_, Arc<memories::MemoryService>>,
) -> Result<app_settings::AppSettings, String> {
    let previous = app_settings::get(&app).map_err(|error| error.to_string())?;
    let previous_web_service = previous
        .web_service_config()
        .map_err(|error| error.to_string())?;
    let web_service_update = settings
        .web_service_config_update()
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
                runtime.inner().as_ref(),
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
            runtime.inner().as_ref(),
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
            memories.set_enabled(saved.local_memories);
            if saved.local_memories
                && (saved.interface_language != previous.interface_language
                    || !previous.local_memories)
            {
                memories.trigger(
                    runtime.inner().clone(),
                    None,
                    saved.interface_language,
                    true,
                );
            }
            Ok(saved)
        }
        Err(error) => {
            rollback_settings_side_effects(
                runtime.inner().as_ref(),
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

/// 返回 KeenCode 自定义模型供应商列表。
#[tauri::command]
fn providers_list(app: AppHandle) -> Result<ProvidersListResult, String> {
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
    context_1m: std::collections::BTreeMap<String, bool>,
    supports_vision: std::collections::BTreeMap<String, bool>,
    create_only: bool,
    app: AppHandle,
    agent_runtime: State<'_, Arc<AgentRuntime>>,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<ProvidersListResult, String> {
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
            context_1m,
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
    agent_runtime: State<'_, Arc<AgentRuntime>>,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<ProvidersListResult, String> {
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
    agent_runtime: State<'_, Arc<AgentRuntime>>,
    diagnostics: State<'_, Arc<diagnostics::Diagnostics>>,
) -> Result<ProvidersListResult, String> {
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
            .map_err(|error| error.to_string())?;
    }
    providers::list_models(&base_url, api_key.as_deref(), &api_backend)
        .map_err(|error| error.to_string())
}

/// 启动 KeenCode 桌面后端。
pub fn run() {
    let app = desktop_builder(Instant::now())
        .build(tauri::generate_context!())
        .expect("构建 KeenCode 失败");
    app.run(handle_run_event);
}

/// 共享正式桌面装配，原生测试只在独立测试进程中断开受控记录器通道。
fn desktop_builder(startup_started_at: Instant) -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            use tauri::Manager;
            let diagnostics = diagnostics::Diagnostics::init(app.handle(), startup_started_at);
            diagnostics.startup_phase("backend_setup");
            diagnostics.log(
                "info",
                "startup",
                format!("应用启动，日志路径={}", diagnostics.path().display()),
            );
            app.manage(Arc::clone(&diagnostics));
            app.manage(app_exit::ExitState::default());
            app.manage(app_updates::PendingUpdate::default());
            let loaded_settings = app_settings::load_for_startup(app.handle())?;
            for warning in &loaded_settings.warnings {
                diagnostics.log("warn", "startup.settings", warning);
            }
            let current_settings = loaded_settings.settings;
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
            let agent_runtime = AgentRuntime::build(app.handle())?;
            agent_runtime.set_web_service_config(current_settings.web_service_config()?)?;
            agent_runtime
                .set_background_agent_limit(current_settings.background_agent_limit as usize)?;
            diagnostics.startup_phase("runtime_ready");
            let memories = memories::MemoryService::new(app.handle())?;
            memories.set_enabled(current_settings.local_memories);
            app.manage(Arc::clone(&memories));
            app.manage(Arc::clone(&agent_runtime));
            acp_host::install(app.handle(), Arc::clone(&agent_runtime))?;
            if current_settings.local_memories {
                memories.trigger(
                    agent_runtime,
                    None,
                    current_settings.interface_language,
                    false,
                );
            }
            if let Some(window) = app.get_webview_window("main") {
                #[cfg(target_os = "macos")]
                {
                    // 使用不透明窗口底，避免整窗原生毛玻璃让暗色侧栏和设置导航泛灰。
                    let _ =
                        window.set_background_color(Some(tauri::window::Color(13, 13, 13, 255)));
                }
                #[cfg(not(target_os = "macos"))]
                {
                    // 非 macOS 平台使用与深色主题一致的实色背景，避免白屏闪烁。
                    let _ =
                        window.set_background_color(Some(tauri::window::Color(13, 13, 13, 255)));
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            settings_get,
            settings_set,
            diagnostics_log_path,
            diagnostics_record,
            startup_frontend_ready,
            app_exit::app_request_exit,
            app_exit::app_confirm_exit,
            app_updates::app_update_info,
            app_updates::app_update_check,
            app_updates::app_update_install,
            providers_list,
            providers_upsert,
            providers_remove,
            providers_select_model,
            providers_list_models,
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
            extensions::marketplace_list,
            extensions::marketplace_available,
            extensions::marketplace_add,
            extensions::marketplace_remove,
            extensions::marketplace_update,
            workspace::projects_list,
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
            workspace::save_pasted_attachment,
            workspace::read_local_image,
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
            terminal::terminal_create,
            terminal::terminal_shells_list,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_close
        ])
}

/// 原生退出事件始终经过同一个清理与放行入口。
fn handle_run_event(app: &AppHandle, event: tauri::RunEvent) {
    if let tauri::RunEvent::ExitRequested { api, .. } = event {
        let exit_state = app.state::<app_exit::ExitState>();
        if !exit_state.is_approved() {
            api.prevent_exit();
            let _ = app_exit::request_exit(app);
        } else {
            let runtime = app.state::<Arc<AgentRuntime>>();
            let _ = tauri::async_runtime::block_on(runtime.shutdown());
        }
    }
}

/// 在桌面运行时创建前应用需要生效的进程环境与设置。
pub fn configure_before_start() {
    network_proxy::configure_before_start();
    app_settings::configure_hardware_acceleration_before_start();
}
