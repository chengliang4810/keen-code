use std::path::{Path, PathBuf};

use crate::modules::workspace::{resolve_path, WorkspaceEnv, WorkspaceRegistry};
use rcode_runtime::security::check_file_path;

use super::file::{BinaryFileSnapshot, FileWriteResult, ReadResult};
use super::grep::{GlobResponse, GrepResponse};
use super::tree::DirEntry;

pub(super) struct AgentFileScope {
    pub root: PathBuf,
    display_root: String,
    workspace: WorkspaceEnv,
}

impl AgentFileScope {
    pub(super) fn new(
        root: String,
        workspace: WorkspaceEnv,
        registry: &WorkspaceRegistry,
    ) -> Result<Self, String> {
        if root.trim().is_empty() {
            return Err("current task has no workspace root".into());
        }
        let canonical = std::fs::canonicalize(resolve_path(&root, &workspace))
            .map_err(|error| error.to_string())?;
        if !canonical.is_dir() || !registry.is_authorized(&canonical) {
            return Err("task root is outside the authorized workspace".into());
        }
        check_file_path(&canonical, &canonical.to_string_lossy())?;
        let display_root = root.trim_end_matches(['/', '\\']);
        Ok(Self {
            root: canonical,
            display_root: if display_root.is_empty() {
                "/".into()
            } else {
                display_root.into()
            },
            workspace,
        })
    }

    pub(super) fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        let resolved = resolve_path(path, &self.workspace);
        check_file_path(&self.root, &resolved.to_string_lossy())
    }

    pub(super) fn display(&self, path: &Path) -> String {
        if self.workspace.is_wsl() {
            if let Ok(relative) = path.strip_prefix(&self.root) {
                let relative = super::to_canon(relative);
                return if relative.is_empty() {
                    self.display_root.clone()
                } else {
                    format!("{}/{}", self.display_root.trim_end_matches('/'), relative)
                };
            }
        }
        super::to_canon(path)
    }
}

#[tauri::command]
pub async fn agent_fs_canonicalize(
    path: String,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
) -> Result<String, String> {
    let scope = AgentFileScope::new(task_root, WorkspaceEnv::from_option(workspace), &registry)?;
    let path = scope.resolve(&path)?;
    Ok(scope.display(&path))
}

#[tauri::command]
pub async fn agent_fs_read_file(
    path: String,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
) -> Result<ReadResult, String> {
    let scope = AgentFileScope::new(task_root, WorkspaceEnv::from_option(workspace), &registry)?;
    let path = scope.resolve(&path)?;
    tauri::async_runtime::spawn_blocking(move || super::file::read_file_sync(&path, false))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn agent_fs_binary_snapshot(
    path: String,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
) -> Result<Option<BinaryFileSnapshot>, String> {
    let scope = AgentFileScope::new(task_root, WorkspaceEnv::from_option(workspace), &registry)?;
    let path = scope.resolve(&path)?;
    tauri::async_runtime::spawn_blocking(move || super::file::binary_file_snapshot_sync(&path))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn agent_fs_write_file(
    path: String,
    content: String,
    expected_version: Option<String>,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
    app: tauri::AppHandle,
) -> Result<FileWriteResult, String> {
    let scope = AgentFileScope::new(task_root, WorkspaceEnv::from_option(workspace), &registry)?;
    let target = scope.resolve(&path)?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        super::file::write_file_sync(&target, &content, expected_version.as_deref(), false)
    })
    .await
    .map_err(|error| error.to_string())??;
    super::file::emit_file_written(&app, path, Some("agent".into()));
    Ok(result)
}

#[tauri::command]
pub async fn agent_fs_create_dir(
    path: String,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
) -> Result<(), String> {
    let scope = AgentFileScope::new(task_root, WorkspaceEnv::from_option(workspace), &registry)?;
    let target = scope.resolve(&path)?;
    if target.exists() {
        return Err("directory already exists".into());
    }
    std::fs::create_dir_all(target).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn agent_fs_read_dir(
    path: String,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
) -> Result<Vec<DirEntry>, String> {
    let scope = AgentFileScope::new(
        task_root,
        WorkspaceEnv::from_option(workspace.clone()),
        &registry,
    )?;
    let target = scope.resolve(&path)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut entries = super::tree::fs_read_dir(path, false, None, workspace)?;
        entries.retain(|entry| {
            check_file_path(&scope.root, &target.join(&entry.name).to_string_lossy()).is_ok()
        });
        Ok(entries)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn agent_fs_grep(
    pattern: String,
    root: String,
    glob: Option<Vec<String>>,
    case_insensitive: Option<bool>,
    max_results: Option<usize>,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
) -> Result<GrepResponse, String> {
    let scope = AgentFileScope::new(
        task_root,
        WorkspaceEnv::from_option(workspace.clone()),
        &registry,
    )?;
    scope.resolve(&root)?;
    tauri::async_runtime::spawn_blocking(move || {
        super::grep::grep_scoped(
            pattern,
            root,
            glob,
            case_insensitive,
            max_results,
            workspace,
            Some(scope.root),
        )
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn agent_fs_glob(
    pattern: String,
    root: String,
    max_results: Option<usize>,
    task_root: String,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
) -> Result<GlobResponse, String> {
    let scope = AgentFileScope::new(
        task_root,
        WorkspaceEnv::from_option(workspace.clone()),
        &registry,
    )?;
    scope.resolve(&root)?;
    tauri::async_runtime::spawn_blocking(move || {
        super::grep::glob_scoped(pattern, root, max_results, workspace, Some(scope.root))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_limits_even_a_broader_registered_root_and_blocks_secrets() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(directory.path().join("outside.txt"), "private").unwrap();
        let registry = WorkspaceRegistry::default();
        registry.authorize(directory.path()).unwrap();
        let scope = AgentFileScope::new(
            project.to_string_lossy().into(),
            WorkspaceEnv::Local,
            &registry,
        )
        .unwrap();
        assert!(scope
            .resolve(&project.join("../outside.txt").to_string_lossy())
            .is_err());
        assert!(scope
            .resolve(&project.join(".env").to_string_lossy())
            .is_err());
        assert!(scope
            .resolve(&project.join("new.txt").to_string_lossy())
            .is_ok());
    }

    #[test]
    fn unregistered_root_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        assert!(AgentFileScope::new(
            directory.path().to_string_lossy().into(),
            WorkspaceEnv::Local,
            &WorkspaceRegistry::default()
        )
        .is_err());
    }

    #[cfg(windows)]
    #[test]
    fn wsl_drvfs_keeps_linux_paths_and_the_same_task_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let canonical = directory.path().canonicalize().unwrap();
        let host = super::super::to_canon(&canonical);
        let drive = host.chars().next().unwrap().to_ascii_lowercase();
        let linux = format!("/mnt/{drive}{}", &host[2..]);
        let registry = WorkspaceRegistry::default();
        registry.authorize(&canonical).unwrap();
        let scope = AgentFileScope::new(
            linux.clone(),
            WorkspaceEnv::Wsl {
                distro: "Debian".into(),
            },
            &registry,
        )
        .unwrap();
        let file = scope.resolve(&format!("{linux}/new.txt")).unwrap();
        assert_eq!(scope.display(&file), format!("{linux}/new.txt"));
        assert!(scope.resolve(&format!("{linux}/../outside.txt")).is_err());
        assert!(scope.resolve(&format!("{linux}/secrets.json")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_checked_at_the_backend() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "private").unwrap();
        std::os::unix::fs::symlink(outside.path(), directory.path().join("link")).unwrap();
        let registry = WorkspaceRegistry::default();
        registry.authorize(directory.path()).unwrap();
        let scope = AgentFileScope::new(
            directory.path().to_string_lossy().into(),
            WorkspaceEnv::Local,
            &registry,
        )
        .unwrap();
        assert!(scope
            .resolve(&directory.path().join("link/secret.txt").to_string_lossy())
            .is_err());
    }
}
