pub mod agents;
pub mod images;

use serde::Serialize;
use std::collections::BTreeMap;

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

#[cfg(test)]
use rcode_runtime::storage::root_at;
pub use rcode_runtime::storage::{directory, path, root};
pub(crate) use rcode_runtime::storage::{directory_at, path_at, reject_link};

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
