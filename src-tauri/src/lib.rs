mod analytics;
mod app_exit;
mod app_settings;
mod app_updates;
mod claude_plugins;
mod diagnostics;
mod extensions;
mod memories;
mod model_metadata;
mod peri_runtime;
mod personalization;
mod power_management;
mod providers;
mod session_commands;
mod storage;
mod task_notifications;
mod terminal;
mod workspace;

use crate::peri_runtime::PeriRuntime;
use crate::providers::{ProviderModelsResult, ProviderUpsert, ProvidersListResult};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};

/// 返回后端诊断日志的绝对路径。
#[tauri::command]
fn diagnostics_log_path(diagnostics: State<'_, Arc<diagnostics::Diagnostics>>) -> String {
    diagnostics.path().display().to_string()
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

/// 返回当前完整应用设置。
#[tauri::command]
fn settings_get(app: AppHandle) -> Result<app_settings::AppSettings, String> {
    app_settings::get(&app).map_err(|error| error.to_string())
}

/// 应用并保存一个严格类型的设置补丁。
#[tauri::command]
fn settings_set(
    settings: app_settings::AppSettingsPatch,
    app: AppHandle,
    power_management: State<'_, Arc<power_management::PowerManagement>>,
) -> Result<app_settings::AppSettings, String> {
    let previous = app_settings::get(&app).map_err(|error| error.to_string())?;
    if let Some(enabled) = settings.keep_computer_awake {
        power_management
            .set_keep_awake(enabled)
            .map_err(|error| error.to_string())?;
    }
    match app_settings::set(&app, settings) {
        Ok(saved) => Ok(saved),
        Err(error) => {
            let _ = power_management.set_keep_awake(previous.keep_computer_awake);
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
fn providers_upsert(
    id: String,
    models: Vec<String>,
    base_url: String,
    name: Option<String>,
    api_key: Option<String>,
    api_backend: String,
    context_windows: std::collections::BTreeMap<String, u64>,
    context_1m: std::collections::BTreeMap<String, bool>,
    create_only: bool,
    app: AppHandle,
    runtime: State<'_, Arc<PeriRuntime>>,
) -> Result<ProvidersListResult, String> {
    runtime.log(
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
            create_only,
        },
    )
    .map_err(|error| {
        runtime.log(
            "error",
            "ipc.providers_upsert",
            format!("保存失败: {error:#}"),
        );
        error.to_string()
    })?;
    runtime.reload_provider(&app).map_err(|error| {
        runtime.log(
            "error",
            "ipc.providers_upsert",
            format!("热加载失败: {error:#}"),
        );
        error.to_string()
    })?;
    runtime.log("info", "ipc.providers_upsert", "命令完成");
    Ok(result)
}

/// 删除一个自定义模型供应商。
#[tauri::command]
fn providers_remove(
    id: String,
    app: AppHandle,
    runtime: State<'_, Arc<PeriRuntime>>,
) -> Result<ProvidersListResult, String> {
    runtime.log(
        "info",
        "ipc.providers_remove",
        format!("命令进入 provider_id={id}"),
    );
    let result = providers::remove(&app, &id).map_err(|error| {
        runtime.log(
            "error",
            "ipc.providers_remove",
            format!("删除失败: {error:#}"),
        );
        error.to_string()
    })?;
    runtime.reload_provider(&app).map_err(|error| {
        runtime.log(
            "error",
            "ipc.providers_remove",
            format!("热加载失败: {error:#}"),
        );
        error.to_string()
    })?;
    runtime.log("info", "ipc.providers_remove", "命令完成");
    Ok(result)
}

/// 选择任务使用的模型并切换到对应供应商。
#[tauri::command]
fn providers_select_model(
    provider_id: String,
    model_id: String,
    app: AppHandle,
    runtime: State<'_, Arc<PeriRuntime>>,
) -> Result<ProvidersListResult, String> {
    runtime.log(
        "info",
        "ipc.providers_select_model",
        format!("命令进入 provider_id={} model_id={}", provider_id, model_id),
    );
    let result = providers::select_model(&app, &provider_id, &model_id).map_err(|error| {
        runtime.log(
            "error",
            "ipc.providers_select_model",
            format!("选择失败: {error:#}"),
        );
        error.to_string()
    })?;
    runtime.reload_provider(&app).map_err(|error| {
        runtime.log(
            "error",
            "ipc.providers_select_model",
            format!("热加载失败: {error:#}"),
        );
        error.to_string()
    })?;
    runtime.log("info", "ipc.providers_select_model", "命令完成");
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
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            use tauri::Manager;
            let diagnostics = diagnostics::Diagnostics::init(app.handle());
            diagnostics.log(
                "info",
                "startup",
                format!("应用启动，日志路径={}", diagnostics.path().display()),
            );
            app.manage(Arc::clone(&diagnostics));
            app.manage(app_exit::ExitState::default());
            app.manage(app_updates::PendingUpdate::default());
            let current_settings = app_settings::get(app.handle())?;
            let power_management = Arc::new(power_management::PowerManagement::new());
            power_management.set_keep_awake(current_settings.keep_computer_awake)?;
            app.manage(power_management);
            app.manage(Arc::new(task_notifications::TaskNotifications::default()));
            app.manage(Arc::new(terminal::TerminalManager::default()));
            // Claude 插件状态必须先进入 Tauri state，PeriRuntime 初次装配时才能读取
            // 插件 Skills、Hooks 与敏感配置的当前进程快照。
            app.manage(extensions::ExtensionsState::default());
            // build 已返回 Arc<PeriRuntime>；再包 Arc::new 会变成
            // Arc<Arc<PeriRuntime>>，导致命令 State<'_, Arc<PeriRuntime>>
            // 查找失败（"state not managed for field `runtime`"）。
            let runtime = PeriRuntime::build(app.handle())?;
            let memories = memories::MemoryService::new(app.handle())?;
            app.manage(Arc::clone(&memories));
            app.manage(Arc::clone(&runtime));
            if current_settings.local_memories {
                memories.trigger(runtime, None);
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
            // ── 会话命令（ACP 后端）──
            session_commands::session_get_state,
            session_commands::mcp_list,
            session_commands::mcp_oauth_start,
            session_commands::mcp_oauth_callback,
            session_commands::mcp_oauth_cancel,
            session_commands::background_tasks_list,
            session_commands::background_task_cancel,
            session_commands::background_tasks_cancel_all,
            session_commands::sessions_list,
            session_commands::session_connect,
            session_commands::session_send,
            session_commands::session_steer,
            session_commands::session_stop,
            session_commands::session_fork,
            session_commands::session_rename,
            session_commands::session_set_model,
            session_commands::session_set_effort,
            session_commands::session_generate_title,
            session_commands::session_messages,
            session_commands::session_delete,
            session_commands::session_disconnect,
            session_commands::session_resolve_ask_user,
            // ── Goal / replay（peri 新增 ACP 面）──
            session_commands::goals_list,
            session_commands::goal_upsert,
            session_commands::goal_transition,
            session_commands::goal_clear,
            session_commands::session_replay,
            // ── 用量统计与个性化（不涉及 Agent 内核）──
            analytics::request_records_list,
            analytics::usage_stats_get,
            personalization::custom_instructions_get,
            personalization::custom_instructions_set,
            memories::memories_status,
            memories::memories_reset,
            // ── 扩展与工作区（不涉及 Agent 内核）──
            extensions::extensions_set_mcp,
            extensions::extensions_enable_all_mcp,
            extensions::skills_list,
            extensions::agents_list,
            extensions::agents_tool_catalog,
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
            extensions::mcp_remove,
            extensions::mcp_doctor,
            extensions::marketplace_list,
            extensions::marketplace_available,
            extensions::marketplace_add,
            extensions::marketplace_remove,
            extensions::marketplace_update,
            workspace::projects_list,
            workspace::project_add,
            workspace::project_remove,
            workspace::project_relocate,
            workspace::project_rename,
            workspace::project_set_pinned,
            workspace::project_reveal,
            workspace::paths_classify,
            workspace::path_open,
            workspace::url_open,
            workspace::path_reveal,
            workspace::pick_directory,
            workspace::pick_attach_files,
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
            workspace::git_file_diff,
            workspace::git_show_file,
            workspace::git_commit,
            workspace::git_push,
            terminal::terminal_create,
            terminal::terminal_write,
            terminal::terminal_resize,
            terminal::terminal_close
        ])
        .build(tauri::generate_context!())
        .expect("构建 KeenCode 失败");
    app.run(|app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let exit_state = app.state::<app_exit::ExitState>();
            if !exit_state.is_approved() {
                api.prevent_exit();
                let _ = app_exit::request_exit(app);
            }
        }
    });
}

/// 在桌面运行时创建前应用需要重启生效的设置。
pub fn configure_before_start() {
    app_settings::configure_hardware_acceleration_before_start();
}
