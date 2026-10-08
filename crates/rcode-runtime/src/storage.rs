use std::path::{Component, Path, PathBuf};

pub fn root() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("User home directory is unavailable.")?;
    root_at(&home)
}

pub fn root_at(home: &Path) -> Result<PathBuf, String> {
    let home = home.canonicalize().map_err(|e| e.to_string())?;
    let root = rcode_control_protocol::paths::user_root(&home);
    reject_link(&root)?;
    Ok(root)
}

pub fn reject_link(path: &Path) -> Result<(), String> {
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

pub fn path_at(root: &Path, relative: &str) -> Result<PathBuf, String> {
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

pub fn directory_at(root: &Path, relative: &str) -> Result<PathBuf, String> {
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
