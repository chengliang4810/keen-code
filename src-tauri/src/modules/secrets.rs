//! Secret storage with platform-appropriate backends.
//!
//! - macOS: macOS Keychain (via `keyring` crate)
//! - Windows: Credential Manager (via `keyring` crate)
//! - Linux: a file at ~/.rcode/credentials/secrets.json, mode 0600. The default
//!   `keyring` backend on Linux is the Secret Service over D-Bus, which
//!   silently fails on systems without gnome-keyring/kwallet (and on the
//!   "login" collection not being created). For an open-source desktop
//!   app shipped via AppImage/deb/rpm, we cannot assume a keyring daemon
//!   exists. The file backend is the same approach Brave/Chromium fall
//!   back to in that scenario; user-only file permissions provide the
//!   isolation the secret-service collection would have otherwise.
//!
//! The frontend talks to `secrets_get`, `secrets_set`, `secrets_delete`,
//! and `secrets_get_all` — no platform branching in JS.
//!
//! File fallback paths use the shared RCode user storage layout.

use std::sync::Mutex;

use tauri::AppHandle;

#[cfg(any(target_os = "linux", test))]
use std::collections::HashMap;
#[cfg(any(target_os = "linux", test))]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[derive(Default)]
pub struct SecretsState {
    #[cfg(target_os = "linux")]
    cache: Mutex<Option<HashMap<String, String>>>,
    #[cfg(not(target_os = "linux"))]
    _phantom: Mutex<()>,
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn key(service: &str, account: &str) -> String {
    format!("{}::{}", service, account)
}

#[cfg(target_os = "linux")]
fn store_path(_app: &AppHandle) -> Result<PathBuf, String> {
    crate::modules::storage::directory("credentials")?;
    crate::modules::storage::path("credentials/secrets.json")
}

#[cfg(target_os = "linux")]
fn read_store(app: &AppHandle) -> Result<HashMap<String, String>, String> {
    read_store_at(&store_path(app)?)
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn read_store_at(path: &std::path::Path) -> Result<HashMap<String, String>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_slice::<HashMap<String, String>>(&bytes).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
fn write_store(app: &AppHandle, map: &HashMap<String, String>) -> Result<(), String> {
    write_store_at(&store_path(app)?, map)
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn write_store_at(
    path: &std::path::Path,
    map: &HashMap<String, String>,
) -> Result<(), String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(map).map_err(|e| e.to_string())?;
    let parent = path.parent().ok_or("missing secrets directory")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.error.to_string())?;
    Ok(())
}

#[cfg(any(target_os = "linux", test))]
fn commit_store(
    cache: &Mutex<Option<HashMap<String, String>>>,
    load: impl FnOnce() -> Result<HashMap<String, String>, String>,
    update: impl FnOnce(&mut HashMap<String, String>),
    persist: impl FnOnce(&HashMap<String, String>) -> Result<(), String>,
) -> Result<(), String> {
    let mut guard = cache.lock().map_err(|e| e.to_string())?;
    let mut candidate = match guard.as_ref() {
        Some(map) => map.clone(),
        None => load()?,
    };
    update(&mut candidate);
    persist(&candidate)?;
    *guard = Some(candidate);
    Ok(())
}

#[cfg(target_os = "linux")]
fn with_store<F, R>(app: &AppHandle, state: &SecretsState, f: F) -> Result<R, String>
where
    F: FnOnce(&mut HashMap<String, String>) -> R,
{
    let mut guard = state.cache.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        *guard = Some(read_store(app)?);
    }
    let map = guard.as_mut().expect("cache initialized above");
    Ok(f(map))
}

#[cfg(not(target_os = "linux"))]
fn entry(service: &str, account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, account).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn secrets_get(
    app: AppHandle,
    state: tauri::State<'_, SecretsState>,
    service: String,
    account: String,
) -> Result<Option<String>, String> {
    read_secret(&app, &state, &service, &account)
}

pub(crate) fn read_secret(
    app: &AppHandle,
    state: &SecretsState,
    service: &str,
    account: &str,
) -> Result<Option<String>, String> {
    #[cfg(target_os = "linux")]
    {
        let key = key(service, account);
        with_store(app, state, |m| m.get(&key).cloned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, state);
        let e = entry(service, account)?;
        match e.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
    }
}

#[tauri::command]
pub async fn secrets_set(
    app: AppHandle,
    state: tauri::State<'_, SecretsState>,
    service: String,
    account: String,
    password: String,
) -> Result<(), String> {
    write_secret(&app, &state, &service, &account, &password)
}

pub(crate) fn write_secret(
    app: &AppHandle,
    state: &SecretsState,
    service: &str,
    account: &str,
    password: &str,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let key = key(service, account);
        commit_store(
            &state.cache,
            || read_store(app),
            |map| {
                map.insert(key, password.to_owned());
            },
            |map| write_store(app, map),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, state);
        let e = entry(service, account)?;
        e.set_password(password).map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub async fn secrets_delete(
    app: AppHandle,
    state: tauri::State<'_, SecretsState>,
    service: String,
    account: String,
) -> Result<(), String> {
    remove_secret(&app, &state, &service, &account)
}

pub(crate) fn remove_secret(
    app: &AppHandle,
    state: &SecretsState,
    service: &str,
    account: &str,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let key = key(service, account);
        commit_store(
            &state.cache,
            || read_store(app),
            |map| {
                map.remove(&key);
            },
            |map| write_store(app, map),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, state);
        let e = entry(service, account)?;
        match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    #[test]
    fn key_format_is_service_double_colon_account() {
        assert_eq!(key("openai", "alice"), "openai::alice");
        assert_eq!(key("", ""), "::");
    }

    #[test]
    fn read_store_at_missing_path_is_empty() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("nope.json");
        let map = read_store_at(&p).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.json");
        let mut m = HashMap::new();
        m.insert(key("svc", "alice"), "p1".into());
        m.insert(key("svc", "bob"), "p2".into());

        write_store_at(&p, &m).unwrap();
        let loaded = read_store_at(&p).unwrap();
        assert_eq!(loaded, m);
    }

    #[test]
    #[cfg(unix)]
    fn write_uses_mode_0600() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.json");
        write_store_at(&p, &HashMap::new()).unwrap();

        let mode = fs::metadata(&p).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets file must be user-only readable");
    }

    #[test]
    fn write_does_not_leave_tmp_file_on_success() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.json");
        write_store_at(&p, &HashMap::new()).unwrap();

        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_overwrites_existing_atomically() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.json");

        let mut first = HashMap::new();
        first.insert("a".into(), "1".into());
        write_store_at(&p, &first).unwrap();

        let mut second = HashMap::new();
        second.insert("b".into(), "2".into());
        write_store_at(&p, &second).unwrap();

        let loaded = read_store_at(&p).unwrap();
        assert_eq!(loaded, second);
        assert!(!loaded.contains_key("a"));
    }

    #[test]
    fn read_store_at_garbage_file_errors() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("secrets.json");
        fs::write(&p, b"not json").unwrap();
        assert!(read_store_at(&p).is_err());
    }

    #[test]
    fn failed_set_and_delete_preserve_cache_and_disk() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("secrets.json");
        let original = HashMap::from([("keep".to_owned(), "synthetic".to_owned())]);
        write_store_at(&path, &original).unwrap();
        let cache = Mutex::new(Some(original.clone()));
        for deleting in [false, true] {
            let result = commit_store(
                &cache,
                || read_store_at(&path),
                |map| {
                    if deleting {
                        map.remove("keep");
                    } else {
                        map.insert("new".to_owned(), "synthetic".to_owned());
                    }
                },
                |_| Err("injected disk failure".into()),
            );
            assert!(result.is_err());
            assert_eq!(cache.lock().unwrap().as_ref(), Some(&original));
            assert_eq!(read_store_at(&path).unwrap(), original);
        }
    }

    #[test]
    fn concurrent_transactions_preserve_each_successful_update_after_reload() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("secrets.json");
        let cache = std::sync::Arc::new(Mutex::new(None));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let threads: Vec<_> = (0..12)
            .map(|index| {
                let cache = cache.clone();
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    commit_store(
                        &cache,
                        || read_store_at(&path),
                        |map| {
                            map.insert(format!("account-{index}"), "synthetic".to_owned());
                        },
                        |map| write_store_at(&path, map),
                    )
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let reloaded = read_store_at(&path).unwrap();
        assert_eq!(reloaded.len(), 12);
        assert_eq!(cache.lock().unwrap().as_ref(), Some(&reloaded));
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_atomic_replace_removes_only_its_own_temporary_file() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("secrets.json");
        fs::create_dir(&path).unwrap();
        let legacy_temp = directory.path().join("secrets.json.tmp");
        fs::write(&legacy_temp, "foreign file").unwrap();
        assert!(write_store_at(&path, &HashMap::new()).is_err());
        assert_eq!(fs::read_to_string(legacy_temp).unwrap(), "foreign file");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn fixed_temporary_symlink_cannot_change_another_file() {
        use std::os::unix::fs::symlink;
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("secrets.json");
        let unrelated = directory.path().join("unrelated.txt");
        fs::write(&unrelated, "keep").unwrap();
        symlink(&unrelated, directory.path().join("secrets.json.tmp")).unwrap();
        write_store_at(&path, &HashMap::new()).unwrap();
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep");
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o600);
    }
}

/// Batch read — single IPC roundtrip for the cold-boot fan-out.
#[tauri::command]
pub async fn secrets_get_all(
    app: AppHandle,
    state: tauri::State<'_, SecretsState>,
    service: String,
    accounts: Vec<String>,
) -> Result<Vec<Option<String>>, String> {
    #[cfg(target_os = "linux")]
    {
        with_store(&app, &state, |m| {
            accounts
                .iter()
                .map(|a| m.get(&key(&service, a)).cloned())
                .collect()
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, state);
        Ok(accounts
            .into_iter()
            .map(|a| {
                keyring::Entry::new(&service, &a)
                    .ok()
                    .and_then(|e| e.get_password().ok())
            })
            .collect())
    }
}
