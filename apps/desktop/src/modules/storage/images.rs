use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::ipc::{InvokeBody, Request, Response};

const MAX_BYTES: usize = 30 * 1024 * 1024;
const TYPES: &[(&str, &str)] = &[
    ("image/jpeg", "jpg"),
    ("image/png", "png"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/apng", "apng"),
];
static WRITES: Mutex<()> = Mutex::new(());

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid background image ID.".into());
    }
    Ok(())
}

fn image_path(id: &str, extension: &str) -> Result<PathBuf, String> {
    validate_id(id)?;
    super::path(&format!("themes/backgrounds/{id}.{extension}"))
}

fn write_image(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err("Background image exceeds the size limit.".into());
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().ok_or("Missing image directory.")?)
            .map_err(|e| e.to_string())?;
    temporary.write_all(bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn storage_image_put(request: Request<'_>) -> Result<(), String> {
    let id = request
        .headers()
        .get("x-rcode-image-id")
        .and_then(|v| v.to_str().ok())
        .ok_or("Missing image ID.")?
        .to_owned();
    validate_id(&id)?;
    let mime = request
        .headers()
        .get("x-rcode-image-type")
        .and_then(|v| v.to_str().ok())
        .ok_or("Missing image type.")?;
    let extension = TYPES
        .iter()
        .find(|(kind, _)| *kind == mime)
        .ok_or("Unsupported image type.")?
        .1;
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("Image upload requires binary data.".into());
    };
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err("Background image exceeds the size limit.".into());
    }
    let bytes = bytes.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        super::directory("themes/backgrounds")?;
        write_image(&image_path(&id, extension)?, &bytes)?;
        for &(_, other) in TYPES {
            if other != extension {
                remove_file(&image_path(&id, other)?)?;
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn read_image(id: &str) -> Result<Vec<u8>, String> {
    validate_id(id)?;
    for (index, &(_, extension)) in TYPES.iter().enumerate() {
        let path = image_path(id, extension)?;
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.to_string()),
        };
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() > MAX_BYTES as u64 {
            return Err("Invalid background image file.".into());
        }
        let mut bytes = vec![index as u8];
        file.take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BYTES + 1 {
            return Err("Background image exceeds the size limit.".into());
        }
        return Ok(bytes);
    }
    Ok(Vec::new())
}

#[tauri::command]
pub async fn storage_image_get(id: String) -> Result<Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        read_image(&id).map(Response::new)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn storage_image_delete(id: String) -> Result<(), String> {
    validate_id(&id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        for &(_, extension) in TYPES {
            remove_file(&image_path(&id, extension)?)?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_boundaries_and_atomic_replacement() {
        for id in ["../outside", "/outside", "x:y", "", "a.b"] {
            assert!(validate_id(id).is_err());
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.png");
        write_image(&path, b"first").unwrap();
        assert!(write_image(&path, &[]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        write_image(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
