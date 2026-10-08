pub mod modules;

// Tauri 只给应用二进制链接资源，测试宿主也需要 Common Controls v6 的清单。
#[cfg(all(test, target_os = "windows", target_env = "msvc"))]
#[link(
    name = "resource.lib",
    kind = "static",
    modifiers = "-bundle,+verbatim"
)]
extern "C" {}

#[cfg(target_os = "macos")]
use modules::app_menu;
use modules::{
    agent, control, fs, git, history, lsp, net, pty, secrets, shell, vibrancy, workspace,
};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use tauri_plugin_window_state::StateFlags;

/// Drained on first read so HMR / re-mounts can't replay the launch dir.
#[derive(Default)]
struct LaunchDir(Mutex<Option<String>>);

/// Drained on first read so HMR / re-mounts can't replay the launch files.
#[derive(Default)]
struct LaunchFiles(Mutex<Vec<String>>);

#[tauri::command]
fn get_launch_dir(state: State<'_, LaunchDir>) -> Option<String> {
    state.0.lock().expect("LaunchDir mutex poisoned").take()
}

#[tauri::command]
fn get_launch_files(state: State<'_, LaunchFiles>) -> Vec<String> {
    std::mem::take(&mut *state.0.lock().expect("LaunchFiles mutex poisoned"))
}

enum LaunchEntry {
    Dir(PathBuf),
    File(PathBuf),
}

#[derive(Default, Debug, PartialEq)]
struct LaunchTarget {
    dir: Option<String>,
    files: Vec<String>,
}

/// First dir arg (else the first file's parent) becomes the workspace; every
/// file arg is opened. Kept free of fs/env access so it stays unit-testable.
fn resolve_launch_target(entries: Vec<LaunchEntry>) -> LaunchTarget {
    let mut dir = None;
    let mut files = Vec::new();
    for entry in entries {
        match entry {
            LaunchEntry::Dir(path) => {
                if dir.is_none() {
                    dir = Some(fs::to_canon(&path));
                }
            }
            LaunchEntry::File(path) => {
                if dir.is_none() {
                    dir = path.parent().map(fs::to_canon);
                }
                files.push(fs::to_canon(&path));
            }
        }
    }
    LaunchTarget { dir, files }
}

fn parse_launch_target() -> LaunchTarget {
    let entries = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .filter_map(|arg| std::fs::canonicalize(arg).ok())
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            Some(if meta.is_dir() {
                LaunchEntry::Dir(path)
            } else {
                LaunchEntry::File(path)
            })
        })
        .collect();
    resolve_launch_target(entries)
}

// 保留旧命令入口，但设置只在主窗口覆盖显示，不能再创建独立窗口。
#[tauri::command]
async fn open_settings_window(app: tauri::AppHandle, tab: Option<String>) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or("Conversation window not found")?;
    main.show().map_err(|e| e.to_string())?;
    main.set_focus().map_err(|e| e.to_string())?;
    main.emit("rcode:settings-open", tab.unwrap_or_default())
        .map_err(|e| e.to_string())
}
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(windows)]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.get(1).map(String::as_str) == Some("__rcode_notify") {
            if let (Some(agent), Some(event)) = (args.get(2), args.get(3)) {
                agent::emit_conout_marker(agent, event);
            }
            use std::io::Write;
            let mut out = std::io::stdout();
            let _ = out.write_all(b"{}");
            let _ = out.flush();
            std::process::exit(0);
        }
    }

    let storage_root = modules::storage::root().expect("RCode user directory is unavailable");
    modules::storage::directory("state").expect("RCode state directory is unavailable");
    modules::storage::directory("logs").expect("RCode log directory is unavailable");
    let launch = parse_launch_target();
    let cli_dir = launch.dir.clone();
    workspace::init_launch_cwd(cli_dir.as_deref());
    let control_state = control::ControlState::default();
    let control_for_setup = control_state.clone();

    let builder = tauri::Builder::default();
    let builder = builder.plugin(tauri_plugin_clipboard_manager::init());
    #[cfg(target_os = "macos")]
    let builder = builder
        .menu(app_menu::build)
        .on_menu_event(app_menu::handle_event);
    builder
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Skip restoring VISIBLE — frontend calls window.show() after first
        // paint so the user never sees a transparent window-shadow flash on
        // Windows/Linux.
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(StateFlags::all() & !StateFlags::VISIBLE)
                .with_filename(
                    modules::storage::path("state/windows.json")
                        .expect("RCode window state is unavailable")
                        .to_string_lossy(),
                )
                .build(),
        )
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(tauri_plugin_log::log::LevelFilter::Info)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                        path: storage_root.join("logs"),
                        file_name: Some("rcode".into()),
                    }),
                ])
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |_app| {
            let config = _app
                .config()
                .app
                .windows
                .first()
                .ok_or("Main window configuration is missing")?;
            let window = tauri::WebviewWindowBuilder::from_config(_app, config)?;
            #[cfg(not(target_os = "macos"))]
            let window = window.data_directory(modules::storage::directory("cache/webview")?);
            #[cfg(target_os = "macos")]
            let window = window.incognito(true);
            window.build()?;
            #[cfg(target_os = "macos")]
            modules::window_presentation::macos::install(_app.handle());
            if let Err(error) = control::start(_app.handle().clone(), control_for_setup.clone()) {
                log::warn!("could not start RCode control server: {error}");
            }
            Ok(())
        })
        .manage(pty::PtyState::default())
        .manage(modules::window_presentation::WindowPresentationState::default())
        .manage(control_state)
        .manage(shell::ShellState::default())
        .manage(secrets::SecretsState::default())
        .manage(modules::agent_core::AgentRuntime::default())
        .manage(fs::watch::FsWatchState::default())
        .manage(history::HistoryState::default())
        .manage(lsp::LspState::default())
        .manage(modules::model_catalog::ModelCatalogState::default())
        .manage(fs::grep::ContentSearchState::default())
        .manage({
            let registry = workspace::WorkspaceRegistry::default();
            workspace::bootstrap_registry(&registry);
            if let Some(ref launch_dir) = cli_dir {
                let _ = registry.authorize(launch_dir);
            }
            registry
        })
        .manage(LaunchDir(Mutex::new(cli_dir)))
        .manage(LaunchFiles(Mutex::new(launch.files)))
        .invoke_handler(tauri::generate_handler![
            pty::pty_open,
            pty::pty_write,
            pty::pty_ack_output,
            pty::pty_diagnostics,
            pty::pty_resize,
            pty::pty_close,
            pty::pty_close_all,
            pty::pty_has_foreground_process,
            pty::pty_has_foreground_job,
            pty::pty_shell_name,
            pty::pty_list_shells,
            fs::tree::list_subdirs,
            fs::tree::fs_read_dir,
            fs::file::fs_read_file,
            fs::file::fs_write_file,
            fs::file::fs_stat,
            fs::file::fs_canonicalize,
            fs::agent::agent_fs_canonicalize,
            fs::agent::agent_fs_read_file,
            fs::agent::agent_fs_binary_snapshot,
            fs::agent::agent_fs_write_file,
            fs::agent::agent_fs_create_dir,
            fs::agent::agent_fs_read_dir,
            fs::agent::agent_fs_grep,
            fs::agent::agent_fs_glob,
            fs::mutate::fs_create_file,
            fs::mutate::fs_create_dir,
            fs::mutate::fs_rename,
            fs::mutate::fs_move,
            fs::mutate::fs_delete,
            fs::mutate::fs_delete_batch,
            fs::mutate::fs_copy,
            fs::watch::fs_watch_add,
            fs::watch::fs_watch_remove,
            lsp::lsp_detect,
            lsp::lsp_host_pid,
            lsp::lsp_resolve_root,
            lsp::lsp_spawn,
            lsp::lsp_send,
            lsp::lsp_kill,
            fs::search::fs_search,
            fs::search::fs_list_files,
            fs::grep::fs_grep,
            fs::grep::fs_grep_interactive,
            fs::grep::fs_glob,
            git::commands::git_resolve_repo,
            git::commands::git_panel_snapshot,
            git::commands::git_status,
            git::commands::git_diff,
            git::commands::git_diff_content,
            git::commands::git_stage,
            git::commands::git_unstage,
            git::commands::git_discard,
            git::commands::git_commit,
            git::commands::git_fetch,
            git::commands::git_pull_ff_only,
            git::commands::git_push,
            git::commands::git_log,
            git::commands::git_show_commit,
            git::commands::git_commit_files,
            git::commands::git_commit_file_diff,
            git::commands::git_remote_url,
            git::commands::git_list_branches,
            git::commands::git_checkout_branch,
            shell::shell_run_command,
            shell::shell_session_open,
            shell::shell_session_run,
            shell::shell_session_cancel,
            shell::shell_session_close,
            shell::shell_bg_spawn,
            shell::shell_bg_logs,
            shell::shell_bg_kill,
            shell::shell_bg_list,
            workspace::wsl_list_distros,
            workspace::wsl_default_distro,
            workspace::wsl_home,
            workspace::workspace_authorize,
            workspace::workspace_pick_directory,
            workspace::workspace_current_dir,
            workspace::workspace_default_chat_dir,
            control::control_frontend_ready,
            control::control_respond,
            modules::storage::storage_paths,
            modules::storage::storage_store_missing,
            modules::storage::agents::storage_agents_read,
            modules::storage::agents::storage_agents_save,
            modules::storage::images::storage_image_put,
            modules::storage::images::storage_image_get,
            modules::storage::images::storage_image_delete,
            get_launch_dir,
            get_launch_files,
            open_settings_window,
            modules::locale::ui_set_locale,
            agent::agent_enable_hooks,
            agent::agent_hooks_status,
            modules::agent_core::agent_core_start,
            modules::agent_core::control::agent_core_cancel,
            modules::agent_core::control::agent_core_approve,
            modules::agent_core::control::agent_core_tool_result,
            modules::agent_core::extensions::agent_extensions_get,
            modules::agent_core::extensions::agent_extensions_save,
            modules::agent_core::extensions::agent_mcp_set_secrets,
            modules::agent_core::extensions::agent_plugins_list,
            modules::agent_core::resources::agent_resources_list,
            modules::agent_core::commands::agent_commands_list,
            modules::agent_core::commands::agent_commands_read,
            modules::agent_core::commands::agent_commands_save,
            modules::agent_core::commands::agent_commands_delete,
            modules::agent_core::commands::agent_commands_resolve,
            modules::agent_core::commands::agent_commands_discover,
            modules::agent_core::commands::agent_commands_import,
            modules::agent_core::instructions::agent_instructions_read,
            modules::agent_core::instructions::agent_instructions_watch,
            modules::agent_core::instructions::agent_instructions_save,
            modules::agent_core::memory::agent_memory_prepare,
            modules::agent_core::memory::agent_memory_catalog,
            modules::agent_core::memory::agent_memory_read,
            modules::agent_core::memory::agent_memory_change,
            modules::agent_core::extensions::agent_plugins_install,
            modules::agent_core::extensions::agent_plugins_enable,
            modules::agent_core::extensions::agent_plugins_uninstall,
            modules::agent_core::extensions::agent_plugins_configure,
            modules::agent_core::marketplace::agent_marketplace_catalog,
            modules::agent_core::marketplace::agent_marketplace_add,
            modules::agent_core::marketplace::agent_marketplace_remove,
            modules::agent_core::marketplace::agent_marketplace_refresh,
            modules::agent_core::marketplace::agent_marketplace_install,
            modules::agent_core::marketplace::agent_plugins_update,
            modules::agent_core::marketplace::agent_plugins_details,
            modules::agent_core::marketplace::agent_plugins_pick_path,
            secrets::secrets_get,
            secrets::secrets_set,
            secrets::secrets_delete,
            secrets::secrets_get_all,
            net::lm_ping,
            net::ai_http_request,
            net::ai_http_stream,
            modules::model_catalog::model_catalog_load,
            modules::model_catalog::model_catalog_refresh,
            history::history_suggest,
            history::history_commands,
            history::history_record,
            history::history_list,
            vibrancy::window_backdrop_kind,
            vibrancy::window_set_backdrop,
            modules::window_presentation::window_presentation_state,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            match event {
                // Servers exit on stdin EOF, but destructors are not guaranteed
                // on process exit; kill explicitly.
                tauri::RunEvent::Exit => {
                    app.state::<modules::agent_core::AgentRuntime>().shutdown();
                    app.state::<shell::ShellState>().shutdown();
                    #[cfg(target_os = "macos")]
                    modules::window_presentation::macos::uninstall();
                    if let Some(state) = app.try_state::<lsp::LspState>() {
                        state.kill_all();
                    }
                    if let Some(state) = app.try_state::<control::ControlState>() {
                        state.shutdown();
                    }
                }
                // macOS delivers "Open With" files here, not as argv (cold and
                // warm start, several at once). Seed the drain-once state and
                // emit; canonicalize so the /tmp -> /private/tmp symlink can't
                // defeat openFileTab's path dedupe against a CLI launch.
                #[cfg(target_os = "macos")]
                tauri::RunEvent::Opened { urls } => {
                    let entries = urls
                        .iter()
                        .filter_map(|u| u.to_file_path().ok())
                        .filter_map(|p| std::fs::canonicalize(p).ok())
                        .filter(|p| p.is_file())
                        .map(LaunchEntry::File)
                        .collect();
                    let target = resolve_launch_target(entries);
                    if target.files.is_empty() {
                        return;
                    }
                    if let Some(dir) = &target.dir {
                        if let Some(registry) = app.try_state::<workspace::WorkspaceRegistry>() {
                            let _ = registry.authorize(dir);
                        }
                        if let Some(state) = app.try_state::<LaunchDir>() {
                            *state.0.lock().expect("LaunchDir mutex poisoned") = Some(dir.clone());
                        }
                    }
                    if let Some(state) = app.try_state::<LaunchFiles>() {
                        *state.0.lock().expect("LaunchFiles mutex poisoned") = target.files.clone();
                    }
                    let _ = app.emit("rcode:open-file", target.files);
                }
                _ => {}
            }
        });
}

#[cfg(test)]
mod launch_target_tests {
    use super::{resolve_launch_target, LaunchEntry, LaunchTarget};
    use std::path::PathBuf;

    #[test]
    fn no_entries_resolves_to_empty() {
        assert_eq!(resolve_launch_target(vec![]), LaunchTarget::default());
    }

    #[test]
    fn dir_arg_sets_workspace_and_opens_nothing() {
        let out = resolve_launch_target(vec![LaunchEntry::Dir(PathBuf::from("/home/u/proj"))]);
        assert_eq!(out.dir.as_deref(), Some("/home/u/proj"));
        assert!(out.files.is_empty());
    }

    #[test]
    fn file_arg_opens_file_and_uses_parent_as_workspace() {
        let out = resolve_launch_target(vec![LaunchEntry::File(PathBuf::from(
            "/home/u/proj/main.rs",
        ))]);
        assert_eq!(out.dir.as_deref(), Some("/home/u/proj"));
        assert_eq!(out.files, vec!["/home/u/proj/main.rs".to_string()]);
    }

    #[test]
    fn multiple_files_all_open_and_first_parent_wins() {
        let out = resolve_launch_target(vec![
            LaunchEntry::File(PathBuf::from("/a/one.txt")),
            LaunchEntry::File(PathBuf::from("/b/two.txt")),
        ]);
        assert_eq!(out.dir.as_deref(), Some("/a"));
        assert_eq!(
            out.files,
            vec!["/a/one.txt".to_string(), "/b/two.txt".to_string()]
        );
    }

    #[test]
    fn explicit_dir_takes_precedence_over_file_parent() {
        let out = resolve_launch_target(vec![
            LaunchEntry::Dir(PathBuf::from("/workspace")),
            LaunchEntry::File(PathBuf::from("/other/x.rs")),
        ]);
        assert_eq!(out.dir.as_deref(), Some("/workspace"));
        assert_eq!(out.files, vec!["/other/x.rs".to_string()]);
    }
}
