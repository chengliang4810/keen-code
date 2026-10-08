use super::security::check_file_path;
use crate::modules::{fs::to_canon, workspace::WorkspaceEnv};
use serde::Serialize;
use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Manager, State};
use tempfile::NamedTempFile;

const MAX_INSTRUCTION_BYTES: usize = 64 * 1024;
static GLOBAL_INSTRUCTIONS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionFile {
    path: String,
    content: String,
    exists: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstructions {
    global: InstructionFile,
    project: Option<InstructionFile>,
}

fn validate_content(content: &str) -> Result<(), String> {
    if content.len() > MAX_INSTRUCTION_BYTES {
        return Err("AGENTS.md exceeds the 64 KiB instruction limit.".into());
    }
    if content.contains('\0') {
        return Err("AGENTS.md must be UTF-8 text without null bytes.".into());
    }
    Ok(())
}

fn read_file(path: &Path, display: String) -> Result<InstructionFile, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(InstructionFile {
                path: display,
                content: String::new(),
                exists: false,
            });
        }
        Err(error) => return Err(format!("Unable to read {}: {error}", path.display())),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err(format!("{} must be a regular file.", path.display()));
    }
    if metadata.len() > MAX_INSTRUCTION_BYTES as u64 {
        return Err(format!(
            "{} exceeds the 64 KiB instruction limit.",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take((MAX_INSTRUCTION_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let content =
        String::from_utf8(bytes).map_err(|_| format!("{} must be UTF-8 text.", path.display()))?;
    validate_content(&content)?;
    Ok(InstructionFile {
        path: display,
        content,
        exists: true,
    })
}

fn global_path(home: &Path) -> Result<PathBuf, String> {
    let root = home.canonicalize().map_err(|e| e.to_string())?;
    let expected = rcode_control_protocol::paths::user_root(&root).join("AGENTS.md");
    let checked = check_file_path(&root, &expected.to_string_lossy())?;
    if checked != expected
        || fs::symlink_metadata(&expected).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(
            "Global AGENTS.md must stay in the user's .rcode directory without links.".into(),
        );
    }
    Ok(checked)
}

fn read_global(home: &Path) -> Result<InstructionFile, String> {
    let _guard = GLOBAL_INSTRUCTIONS_LOCK.lock().map_err(|e| e.to_string())?;
    let path = global_path(home)?;
    read_file(
        &path,
        to_canon(rcode_control_protocol::paths::user_root(home).join("AGENTS.md")),
    )
}

fn global_watch_paths(home: &Path) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let file = global_path(home)?;
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    let parent = file
        .parent()
        .ok_or("Global AGENTS.md has no parent directory.")?;
    let mut directories = vec![home];
    match fs::metadata(parent) {
        Ok(metadata) if metadata.is_dir() => directories.push(parent.to_path_buf()),
        Ok(_) => return Err("The .rcode path must be a directory.".into()),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    Ok((file, directories))
}

#[derive(Serialize)]
pub struct InstructionsWatch {
    path: String,
    directories: Vec<String>,
}

#[tauri::command]
pub fn agent_instructions_watch(
    app: AppHandle,
    state: State<'_, crate::modules::fs::watch::FsWatchState>,
    rebind: bool,
) -> Result<InstructionsWatch, String> {
    let home = dirs::home_dir().ok_or("User home directory is unavailable.")?;
    let (path, directories) = global_watch_paths(&home)?;
    crate::modules::fs::watch::add_fixed_paths(&state, &app, &directories, rebind)?;
    Ok(InstructionsWatch {
        path: to_canon(path),
        directories: directories.into_iter().map(to_canon).collect(),
    })
}

fn read_project(root: &Path, display_root: &str) -> Result<InstructionFile, String> {
    let path = check_file_path(root, "AGENTS.md")?;
    read_file(
        &path,
        format!("{}/AGENTS.md", display_root.trim_end_matches(['/', '\\'])),
    )
}

fn save_global(
    home: &Path,
    content: &str,
    expected_content: Option<&str>,
) -> Result<InstructionFile, String> {
    validate_content(content)?;
    if let Some(expected) = expected_content {
        validate_content(expected)?;
    }
    let _guard = GLOBAL_INSTRUCTIONS_LOCK.lock().map_err(|e| e.to_string())?;
    let path = global_path(home)?;
    let display = to_canon(rcode_control_protocol::paths::user_root(home).join("AGENTS.md"));
    let current = read_file(&path, display.clone())?;
    let actual = current.exists.then_some(current.content.as_str());
    if actual != expected_content {
        return Err("Global AGENTS.md changed on disk. Reload it before saving.".into());
    }
    let parent = path
        .parent()
        .ok_or("Global AGENTS.md has no parent directory.")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let path = global_path(home)?;
    let permissions = fs::metadata(&path).ok().map(|m| m.permissions());
    let mut temporary = NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary
        .write_all(content.as_bytes())
        .map_err(|e| e.to_string())?;
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|e| e.to_string())?;
    }
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    if current.exists {
        temporary.persist(&path).map_err(|e| e.error.to_string())?;
    } else {
        temporary
            .persist_noclobber(&path)
            .map_err(|e| e.error.to_string())?;
    }
    Ok(InstructionFile {
        path: display,
        content: content.into(),
        exists: true,
    })
}

#[tauri::command]
pub async fn agent_instructions_read(
    app: AppHandle,
    cwd: Option<String>,
    workspace: Option<WorkspaceEnv>,
) -> Result<AgentInstructions, String> {
    let workspace = WorkspaceEnv::from_option(workspace);
    let root =
        crate::modules::workspace::authorize_spawn_cwd(&app.state(), cwd.as_deref(), &workspace)?;
    let home = dirs::home_dir().ok_or("User home directory is unavailable.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let global = read_global(&home)?;
        let project = root
            .as_ref()
            .map(|root| read_project(root, cwd.as_deref().unwrap_or_default()))
            .transpose()?;
        Ok(AgentInstructions { global, project })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_instructions_save(
    content: String,
    expected_content: Option<String>,
) -> Result<InstructionFile, String> {
    let home = dirs::home_dir().ok_or("User home directory is unavailable.")?;
    tauri::async_runtime::spawn_blocking(move || {
        save_global(&home, &content, expected_content.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_files_are_empty_and_old_project_memory_is_not_loaded() {
        let home = tempfile::tempdir().unwrap();
        assert!(!read_global(home.path()).unwrap().exists);
        fs::write(home.path().join("RCODE.md"), "obsolete").unwrap();
        let root = home.path().canonicalize().unwrap();
        assert!(!read_project(&root, "project").unwrap().exists);
        fs::write(root.join("AGENTS.md"), "project instructions").unwrap();
        assert_eq!(
            read_project(&root, "project").unwrap().content,
            "project instructions"
        );
    }

    #[test]
    fn global_saves_are_atomic_and_detect_external_changes_or_deletion() {
        let home = tempfile::tempdir().unwrap();
        let first = save_global(home.path(), "use Chinese", None).unwrap();
        assert!(first.exists);
        assert_eq!(read_global(home.path()).unwrap().content, "use Chinese");
        assert!(save_global(home.path(), "wrong", None).is_err());
        let path = home.path().join(".rcode/AGENTS.md");
        fs::write(&path, "external change").unwrap();
        assert!(save_global(home.path(), "wrong", Some("use Chinese")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "external change");
        save_global(home.path(), "updated", Some("external change")).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(save_global(home.path(), "wrong", Some("updated")).is_err());
    }

    #[test]
    fn global_watch_follows_directory_creation_and_recreation_without_creating_it() {
        let home = tempfile::tempdir().unwrap();
        let canonical = home.path().canonicalize().unwrap();
        let root = canonical.join(".rcode");
        let (file, directories) = global_watch_paths(home.path()).unwrap();
        assert_eq!(file, root.join("AGENTS.md"));
        assert_eq!(directories, vec![canonical.clone()]);
        assert!(!root.exists());
        fs::create_dir(&root).unwrap();
        assert_eq!(
            global_watch_paths(home.path()).unwrap().1,
            vec![canonical.clone(), root.clone()]
        );
        fs::remove_dir(&root).unwrap();
        assert_eq!(global_watch_paths(home.path()).unwrap().1, vec![canonical]);
        fs::write(&root, "not a directory").unwrap();
        assert!(global_watch_paths(home.path()).is_err());
    }

    #[test]
    fn instruction_limits_count_utf8_bytes_and_reject_binary_content() {
        let home = tempfile::tempdir().unwrap();
        assert!(save_global(
            home.path(),
            &"中".repeat(MAX_INSTRUCTION_BYTES / 3 + 1),
            None
        )
        .is_err());
        assert!(save_global(home.path(), "text\0binary", None).is_err());
        let root = home.path().canonicalize().unwrap();
        let path = root.join("AGENTS.md");
        fs::write(&path, vec![b'x'; MAX_INSTRUCTION_BYTES]).unwrap();
        assert_eq!(
            read_project(&root, "project").unwrap().content.len(),
            MAX_INSTRUCTION_BYTES
        );
        for bytes in [vec![b'x'; MAX_INSTRUCTION_BYTES + 1], vec![0xff], vec![0]] {
            fs::write(&path, bytes).unwrap();
            assert!(read_project(&root, "project").is_err());
        }
    }

    #[test]
    fn concurrent_saves_cannot_overwrite_the_same_snapshot() {
        let home = tempfile::tempdir().unwrap();
        save_global(home.path(), "original", None).unwrap();
        std::thread::scope(|scope| {
            let first = scope.spawn(|| save_global(home.path(), "first", Some("original")));
            let second = scope.spawn(|| save_global(home.path(), "second", Some("original")));
            assert_ne!(
                first.join().unwrap().is_ok(),
                second.join().unwrap().is_ok()
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn instruction_links_cannot_escape_roots_or_expose_secrets() {
        use std::os::unix::fs::{symlink as link_dir, symlink as link_file};
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("AGENTS.md"), "outside").unwrap();
        link_dir(outside.path(), home.path().join(".rcode")).unwrap();
        assert!(global_watch_paths(home.path()).is_err());
        assert!(read_global(home.path()).is_err());
        assert!(save_global(home.path(), "overwrite", None).is_err());
        let root = home.path().canonicalize().unwrap();
        fs::write(root.join(".env"), "secret").unwrap();
        link_file(root.join(".env"), root.join("AGENTS.md")).unwrap();
        assert!(read_project(&root, "project").is_err());
        assert_eq!(
            fs::read_to_string(outside.path().join("AGENTS.md")).unwrap(),
            "outside"
        );
        let other_home = tempfile::tempdir().unwrap();
        fs::create_dir(other_home.path().join(".rcode")).unwrap();
        fs::write(other_home.path().join("ordinary.md"), "ordinary").unwrap();
        link_file(
            other_home.path().join("ordinary.md"),
            other_home.path().join(".rcode/AGENTS.md"),
        )
        .unwrap();
        assert!(read_global(other_home.path()).is_err());
        assert!(save_global(other_home.path(), "overwrite", None).is_err());
        assert_eq!(
            fs::read_to_string(other_home.path().join("ordinary.md")).unwrap(),
            "ordinary"
        );
    }

    #[cfg(windows)]
    #[test]
    fn junctions_cannot_redirect_instruction_reads_or_writes() {
        fn junction(target: &Path, link: &Path) {
            let output = std::process::Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("AGENTS.md"), "outside").unwrap();
        junction(outside.path(), &home.path().join(".rcode"));
        assert!(global_watch_paths(home.path()).is_err());
        assert!(read_global(home.path()).is_err());
        assert!(save_global(home.path(), "overwrite", None).is_err());
        assert_eq!(
            fs::read_to_string(outside.path().join("AGENTS.md")).unwrap(),
            "outside"
        );

        let root = home.path().canonicalize().unwrap();
        fs::create_dir(root.join(".ssh")).unwrap();
        junction(&root.join(".ssh"), &root.join("AGENTS.md"));
        assert!(read_project(&root, "project").is_err());

        let other_home = tempfile::tempdir().unwrap();
        fs::create_dir(other_home.path().join("ordinary")).unwrap();
        fs::write(other_home.path().join("ordinary/AGENTS.md"), "ordinary").unwrap();
        junction(
            &other_home.path().join("ordinary"),
            &other_home.path().join(".rcode"),
        );
        assert!(read_global(other_home.path()).is_err());
        assert!(save_global(other_home.path(), "overwrite", None).is_err());
        assert_eq!(
            fs::read_to_string(other_home.path().join("ordinary/AGENTS.md")).unwrap(),
            "ordinary"
        );
    }
}
