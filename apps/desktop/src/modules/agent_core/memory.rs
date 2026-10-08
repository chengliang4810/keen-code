use crate::modules::{storage, workspace::WorkspaceEnv};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Manager};

const LIMIT: usize = 64 * 1024;
static ACCESS: Mutex<()> = Mutex::new(());

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryFile {
    name: String,
    size: u64,
    updated_at: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryWorkspace {
    id: String,
    label: String,
    root: String,
    files: Vec<MemoryFile>,
}

#[derive(Serialize)]
pub struct MemoryContext {
    id: String,
    index: String,
}

fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn valid_name(name: &str) -> bool {
    name == "MEMORY.md"
        || (name.len() <= 100
            && name.ends_with(".md")
            && name != "memory.md"
            && !matches!(
                name.split('.').next().unwrap_or_default(),
                "con"
                    | "prn"
                    | "aux"
                    | "nul"
                    | "com1"
                    | "com2"
                    | "com3"
                    | "com4"
                    | "com5"
                    | "com6"
                    | "com7"
                    | "com8"
                    | "com9"
                    | "lpt1"
                    | "lpt2"
                    | "lpt3"
                    | "lpt4"
                    | "lpt5"
                    | "lpt6"
                    | "lpt7"
                    | "lpt8"
                    | "lpt9"
            )
            && name.len() > 3
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
            && !name.starts_with('.')
            && !name.contains(".."))
}

fn identity(root: &str) -> String {
    let mut hash = 0x6c62272e07bb014262b821756295c58du128;
    for byte in root.bytes() {
        hash = (hash ^ u128::from(byte)).wrapping_mul(0x0000000001000000000000000000013b);
    }
    format!("{hash:032x}")
}

fn project_dir(base: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("Invalid memory workspace.".into());
    }
    storage::path_at(base, &format!("memories/projects/{id}"))
}

fn file_path(base: &Path, id: &str, name: &str) -> Result<PathBuf, String> {
    if !valid_name(name) {
        return Err("Invalid memory file name.".into());
    }
    let dir = project_dir(base, id)?;
    let directory = storage::path_at(&dir, "memory")?;
    if directory.exists() {
        for (index, entry) in fs::read_dir(&directory)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if index >= 512 {
                return Err("Too many memory files.".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let actual = entry.file_name().to_string_lossy().into_owned();
            if actual.eq_ignore_ascii_case(name) && actual != name {
                return Err("Memory filename case does not match.".into());
            }
        }
    }
    let path = storage::path_at(&dir, &format!("memory/{name}"))?;
    rcode_runtime::security::deny_secret_path(&path)?;
    Ok(path)
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    storage::reject_link(path)?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let before = file.metadata().map_err(|e| e.to_string())?;
    if !before.is_file() || before.len() > LIMIT as u64 {
        return Err("Memory file exceeds 64 KiB or is not a regular file.".into());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    storage::reject_link(path)?;
    if bytes.len() > LIMIT
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("Memory file changed. Reload it before reading.".into());
    }
    let content = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    if content.contains('\0') {
        return Err("Memory must be UTF-8 text without NUL.".into());
    }
    Ok(Some(content))
}

fn files(base: &Path, id: &str) -> Result<Vec<MemoryFile>, String> {
    let dir = project_dir(base, id)?;
    let dir = storage::path_at(&dir, "memory")?;
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut result = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 512 {
            return Err("Too many memory files.".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_name(&name) || !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        storage::reject_link(&entry.path())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        result.push(MemoryFile {
            name,
            size: meta.len(),
            updated_at: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |t| t.as_millis() as u64),
        });
    }
    result.sort_by(|a, b| (a.name != "MEMORY.md", &a.name).cmp(&(b.name != "MEMORY.md", &b.name)));
    Ok(result)
}

fn prepare(base: &Path, root: &Path) -> Result<MemoryContext, String> {
    let root = root
        .canonicalize()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    let root = root.to_lowercase();
    let id = identity(&root);
    storage::directory_at(base, &format!("memories/projects/{id}/memory"))?;
    let manifest = storage::path_at(&project_dir(base, &id)?, "workspace.json")?;
    match read_text(&manifest)? {
        Some(saved)
            if serde_json::from_str::<String>(&saved).map_err(|e| e.to_string())? != root =>
        {
            return Err("Memory workspace identity collision.".into())
        }
        Some(_) => {}
        None => {
            let mut temp =
                tempfile::NamedTempFile::new_in(manifest.parent().ok_or("Missing memory parent.")?)
                    .map_err(|e| e.to_string())?;
            temp.write_all(
                serde_json::to_string(&root)
                    .map_err(|e| e.to_string())?
                    .as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            temp.as_file().sync_all().map_err(|e| e.to_string())?;
            temp.persist_noclobber(&manifest)
                .map_err(|e| e.to_string())?;
        }
    }
    let index = read_text(&file_path(base, &id, "MEMORY.md")?)?.unwrap_or_default();
    Ok(MemoryContext { id, index })
}

fn change(
    base: &Path,
    id: &str,
    name: &str,
    content: Option<&str>,
    expected: Option<&str>,
) -> Result<(), String> {
    let path = file_path(base, id, name)?;
    if read_text(&path)?.as_deref() != expected {
        return Err("Memory file changed on disk. Reload it before saving.".into());
    }
    if let Some(content) = content {
        if content.len() > LIMIT || content.contains('\0') {
            return Err("Memory must be UTF-8 text up to 64 KiB without NUL.".into());
        }
        let parent = path.parent().ok_or("Missing memory directory.")?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        temp.write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        file_path(base, id, name)?;
        if read_text(&path)?.as_deref() != expected {
            return Err("Memory file changed on disk. Reload it before saving.".into());
        }
        if expected.is_some() {
            temp.persist(&path).map_err(|e| e.to_string())?;
        } else {
            temp.persist_noclobber(&path).map_err(|e| e.to_string())?;
        }
    } else if expected.is_some() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn agent_memory_prepare(app: AppHandle, cwd: String) -> Result<MemoryContext, String> {
    let root = crate::modules::workspace::authorize_spawn_cwd(
        &app.state(),
        Some(&cwd),
        &WorkspaceEnv::Local,
    )?
    .ok_or("No workspace.")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = ACCESS.lock().map_err(|e| e.to_string())?;
        prepare(&storage::root()?, &root)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_memory_catalog() -> Result<Vec<MemoryWorkspace>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let base = storage::root()?;
        let path = storage::path_at(&base, "memories/projects")?;
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut result = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= 512 {
                return Err("Too many memory workspaces.".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) || !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                continue;
            }
            let dir = project_dir(&base, &id)?;
            let Some(content) = read_text(&storage::path_at(&dir, "workspace.json")?)? else {
                continue;
            };
            let root: String = serde_json::from_str(&content).map_err(|e| e.to_string())?;
            if identity(&root) != id {
                return Err("Invalid memory workspace identity.".into());
            }
            let files = files(&base, &id)?;
            if files.is_empty() {
                continue;
            }
            let label = Path::new(&root)
                .file_name()
                .map_or_else(|| root.clone(), |s| s.to_string_lossy().into_owned());
            result.push(MemoryWorkspace {
                id,
                label,
                root,
                files,
            });
        }
        result.sort_by_key(|w| {
            std::cmp::Reverse(w.files.iter().map(|f| f.updated_at).max().unwrap_or(0))
        });
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_memory_read(
    id: String,
    name: Option<String>,
) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let base = storage::root()?;
        if let Some(name) = name {
            Ok(serde_json::json!({"content": read_text(&file_path(&base, &id, &name)?)?}))
        } else {
            Ok(serde_json::json!({"files": files(&base, &id)?}))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_memory_change(
    id: String,
    name: String,
    content: Option<String>,
    expected: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = ACCESS.lock().map_err(|e| e.to_string())?;
        change(
            &storage::root()?,
            &id,
            &name,
            content.as_deref(),
            expected.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memories_are_isolated_and_changes_require_exact_snapshot() {
        let base = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let a = prepare(base.path(), first.path()).unwrap();
        let b = prepare(base.path(), second.path()).unwrap();
        assert_ne!(a.id, b.id);
        change(base.path(), &a.id, "MEMORY.md", Some("index"), None).unwrap();
        assert_eq!(prepare(base.path(), first.path()).unwrap().index, "index");
        assert!(prepare(base.path(), second.path())
            .unwrap()
            .index
            .is_empty());
        assert!(change(base.path(), &a.id, "MEMORY.md", Some("stale"), None).is_err());
        assert!(change(base.path(), &a.id, "MEMORY.md", None, Some("stale")).is_err());
        change(base.path(), &a.id, "MEMORY.md", None, Some("index")).unwrap();
        for name in [
            "../AGENTS.md",
            "A.md",
            ".env",
            "foo/secret.md",
            "CON.md",
            "con.md",
            "memory.md",
            "id_rsa.md",
        ] {
            assert!(file_path(base.path(), &a.id, name).is_err());
        }
        assert!(file_path(base.path(), "../escape", "MEMORY.md").is_err());
        assert!(change(
            base.path(),
            &a.id,
            "large.md",
            Some(&"x".repeat(LIMIT + 1)),
            None
        )
        .is_err());
        assert!(change(base.path(), &a.id, "nul.md", Some("a\0b"), None).is_err());
    }
    #[test]
    fn redirected_memory_directories_are_rejected() {
        let base = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), base.path().join("memories")).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(base.path().join("memories"))
                .arg(outside.path())
                .status()
                .unwrap();
            assert!(status.success());
        }
        assert!(prepare(base.path(), outside.path()).is_err());
    }
}
