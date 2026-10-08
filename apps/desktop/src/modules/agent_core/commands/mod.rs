mod files;
mod import;
#[cfg(test)]
mod tests;

use files::Directory;
pub use files::{CommandCatalog, CommandConfig, CommandFile, CommandScope};
pub use import::{ImportCatalog, ImportResult, ImportSelection};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Manager};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommand {
    name: String,
    description: String,
    path: String,
    plugin_name: String,
    marketplace: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSnapshot {
    #[serde(flatten)]
    catalog: CommandCatalog,
    plugin_commands: Vec<PluginCommand>,
}

static WRITES: Mutex<()> = Mutex::new(());

fn home() -> Result<PathBuf, String> {
    dirs::home_dir()
        .ok_or("User home directory is unavailable.")?
        .canonicalize()
        .map_err(|e| e.to_string())
}

fn project(app: &AppHandle, cwd: Option<&str>) -> Result<Option<PathBuf>, String> {
    if cwd.is_none() {
        return Ok(None);
    }
    crate::modules::workspace::authorize_spawn_cwd(
        &app.state(),
        cwd,
        &crate::modules::workspace::WorkspaceEnv::Local,
    )
}

pub(super) fn list_at(home: &Path, project: Option<&Path>) -> Result<CommandCatalog, String> {
    let mut catalog = Directory::user(home).list()?;
    if let Some(project) = project {
        let scoped = Directory::project(project).list()?;
        catalog.commands.extend(scoped.commands);
        catalog.diagnostics.extend(scoped.diagnostics);
    }
    Ok(catalog)
}

#[tauri::command]
pub async fn agent_commands_list(
    app: AppHandle,
    cwd: Option<String>,
) -> Result<CommandSnapshot, String> {
    let project = project(&app, cwd.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || {
        let home = home()?;
        let mut catalog = list_at(&home, project.as_deref())?;
        let mut plugin_commands = Vec::new();
        let result = (|| -> Result<(), String> {
            let data = super::extensions::data_root(&app)?;
            let snapshot = rcode_plugins::PluginManager::new(&data)
                .runtime_snapshot(
                    project.as_deref().unwrap_or(&home),
                    &Default::default(),
                    &super::extensions::extension_secrets(&app),
                )
                .map_err(|e| e.to_string())?;
            let commands = rcode_plugins::PluginCommandCatalog::from_snapshot(&snapshot)
                .map_err(|e| e.to_string())?;
            for entry in commands.entries().take(500) {
                let plugin = snapshot
                    .plugins
                    .iter()
                    .find(|plugin| plugin.root == entry.root)
                    .ok_or("Plugin command is unavailable.")?;
                plugin_commands.push(PluginCommand {
                    name: entry.name.clone(),
                    description: rcode_plugins::plugin_command_description(
                        &entry.root,
                        &entry.path,
                    )
                    .unwrap_or_default(),
                    path: entry.path.to_string_lossy().into_owned(),
                    plugin_name: plugin.id.plugin.clone(),
                    marketplace: plugin.id.marketplace.clone(),
                });
            }
            Ok(())
        })();
        if let Err(error) = result {
            catalog.diagnostics.push(error);
        }
        Ok(CommandSnapshot {
            catalog,
            plugin_commands,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_commands_read(name: String) -> Result<CommandFile, String> {
    tauri::async_runtime::spawn_blocking(move || {
        Directory::user(&home()?)
            .read(&name)?
            .ok_or_else(|| "Command file no longer exists.".into())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_commands_save(
    config: CommandConfig,
    expected_content: Option<String>,
) -> Result<CommandFile, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        Directory::user(&home()?).save(&config, expected_content.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_commands_delete(name: String, expected_content: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        Directory::user(&home()?).delete(&name, &expected_content)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn resolve_at(
    home: &Path,
    project: Option<&Path>,
    name: &str,
    arguments: &str,
) -> Result<Option<String>, String> {
    files::validate_name(name)?;
    let scoped = project
        .map(|root| Directory::project(root).find(name))
        .transpose()?
        .flatten();
    let file = match scoped {
        Some(file) => Some(file),
        None => Directory::user(home).find(name)?,
    };
    let Some(file) = file else {
        return Ok(None);
    };
    if !file.config.enabled {
        return Err("Command is disabled.".into());
    }
    if arguments.len() > files::MAX_BYTES || arguments.contains('\0') {
        return Err("Invalid command arguments or size.".into());
    }
    rcode_skills::render_skill_arguments(&file.config.prompt, arguments, 2 * files::MAX_BYTES)
        .map(Some)
        .ok_or_else(|| "Invalid command arguments or size.".into())
}

#[tauri::command]
pub async fn agent_commands_resolve(
    app: AppHandle,
    cwd: Option<String>,
    name: String,
    arguments: String,
) -> Result<Option<String>, String> {
    let project = project(&app, cwd.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || {
        resolve_at(&home()?, project.as_deref(), &name, &arguments)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_commands_discover() -> Result<ImportCatalog, String> {
    tauri::async_runtime::spawn_blocking(move || Ok(import::discover(&home()?, None)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_commands_import(
    selections: Vec<ImportSelection>,
) -> Result<Vec<ImportResult>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        let home = home()?;
        import::copy_selected(&home, None, &Directory::user(&home), &selections)
    })
    .await
    .map_err(|e| e.to_string())?
}
