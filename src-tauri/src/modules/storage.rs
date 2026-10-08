pub mod agents;
pub mod images;

use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

const STORES: &[(&str, &str)] = &[
    ("settings", "config/settings.json"),
    ("sessions", "sessions/conversations.json"),
    ("agents", "agents/state.json"),
    ("projects", "projects/workspaces.json"),
    ("todos", "sessions/todos.json"),
    ("themes", "themes/custom.json"),
    ("navigation", "state/navigation.json"),
    ("taskSidebars", "state/task-sidebars.json"),
    ("ui", "state/ui.json"),
];

pub fn root() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("User home directory is unavailable.")?;
    root_at(&home)
}

fn root_at(home: &Path) -> Result<PathBuf, String> {
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    let root = rcode_control_protocol::paths::user_root(&home);
    reject_link(&root)?;
    Ok(root)
}

pub(crate) fn reject_link(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            let linked = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let linked = metadata.file_type().is_symlink();
            if linked {
                return Err(format!(
                    "RCode storage does not allow redirected paths: {}",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn path(relative: &str) -> Result<PathBuf, String> {
    path_at(&root()?, relative)
}

pub(crate) fn path_at(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.is_empty() || relative.contains(['\\', ':', '\0']) {
        return Err("Invalid RCode storage path.".into());
    }
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        let Component::Normal(name) = part else {
            return Err("RCode storage paths must be relative without traversal.".into());
        };
        path.push(name);
        reject_link(&path)?;
    }
    Ok(path)
}

pub fn directory(relative: &str) -> Result<PathBuf, String> {
    directory_at(&root()?, relative)
}

pub(crate) fn directory_at(root: &Path, relative: &str) -> Result<PathBuf, String> {
    reject_link(root)?;
    let path = path_at(root, relative)?;
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    reject_link(root)?;
    path_at(root, relative)
}

#[derive(Serialize)]
pub struct StoragePaths {
    stores: BTreeMap<&'static str, String>,
}

#[tauri::command]
pub fn storage_paths() -> Result<StoragePaths, String> {
    let root = root()?;
    let mut stores = BTreeMap::new();
    for &(id, relative) in STORES {
        stores.insert(id, path_at(&root, relative)?.to_string_lossy().into_owned());
    }
    Ok(StoragePaths { stores })
}

#[tauri::command]
pub fn storage_store_missing(id: String) -> Result<bool, String> {
    let relative = STORES
        .iter()
        .find(|(name, _)| *name == id)
        .ok_or("Unknown RCode store.")?
        .1;
    match std::fs::metadata(path(relative)?) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_under_user_root_and_rejects_traversal() {
        let home = tempfile::tempdir().unwrap();
        let root = root_at(home.path()).unwrap();
        assert_eq!(root, home.path().canonicalize().unwrap().join(".rcode"));
        for &(_, relative) in STORES {
            assert!(path_at(&root, relative).unwrap().starts_with(&root));
        }
        for invalid in [
            "../outside",
            "/outside",
            "config/../other",
            "C:/outside",
            "config\\other",
            "",
        ] {
            assert!(path_at(&root, invalid).is_err(), "{invalid}");
        }
        assert_eq!(
            rcode_control_protocol::paths::control_descriptor(home.path()),
            home.path().join(".rcode/run/control.json")
        );
    }

    #[test]
    fn storage_rejects_redirected_directories() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = root_at(home.path()).unwrap();
        std::fs::create_dir(&root).unwrap();
        let link = root.join("config");
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        #[cfg(windows)]
        assert!(std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap()
            .status
            .success());
        assert!(path_at(&root, "config/settings.json").is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}
