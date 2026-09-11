//! 项目目录与会话定位；定位记录仅决定存储位置，不代替事件中的项目授权。
use crate::atomic::{
    BoundedRead, atomic_write, exclusive_lock, prepare_root, read_file_bounded, secure_child_dir,
};
use crate::{ResourceError, SessionId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// 一个稳定项目的数据目录描述。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectStorage {
    /// 与桌面登记表一致的稳定项目 ID。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 工作目录；重定位时更新，ID 不变。
    pub path: String,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, ResourceError> {
    crate::atomic::ensure_regular_file_or_absent(path)?;
    let read = match read_file_bounded(path, 64 * 1024) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ResourceError::io("read_storage_location", error)),
    };
    match read {
        BoundedRead::Bytes(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| ResourceError::UnsafePath("存储定位记录无效".into())),
        _ => Err(ResourceError::UnsafePath("存储定位记录过大".into())),
    }
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), ResourceError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|_| ResourceError::UnsafePath("存储定位记录序列化失败".into()))?;
    atomic_write(path, &bytes, true)
}
fn normalized_path(path: &str) -> String {
    #[cfg(windows)]
    {
        let text = path.replace('\\', "/");
        let text = text
            .strip_prefix("//?/UNC/")
            .map(|suffix| format!("//{suffix}"))
            .unwrap_or_else(|| text.strip_prefix("//?/").unwrap_or(&text).to_owned());
        text.to_lowercase().trim_end_matches('/').to_owned()
    }
    #[cfg(not(windows))]
    {
        path.trim_end_matches('/').to_owned()
    }
}
fn path_key(path: &str) -> String {
    format!("{:x}", Sha256::digest(normalized_path(path).as_bytes()))
}

/// 创建或更新项目描述与路径定位，保持稳定 ID。
pub fn register_project_storage(
    root: &Path,
    project: &ProjectStorage,
) -> Result<PathBuf, ResourceError> {
    let root = prepare_root(root)?;
    let _guard = exclusive_lock(&root.join("project-storage.lock"))?;
    register_project_storage_locked(&root, project)
}

fn register_project_storage_locked(
    root: &Path,
    project: &ProjectStorage,
) -> Result<PathBuf, ResourceError> {
    SessionId::new(project.id.clone())?;
    let root = prepare_root(root)?;
    let projects = secure_child_dir(&root, "projects")?;
    let directory = secure_child_dir(&projects, &project.id)?;
    let locations = secure_child_dir(&root, "project-locations")?;
    if read_json::<ProjectStorage>(&directory.join("project.json"))?.as_ref() != Some(project) {
        write_json(&directory.join("project.json"), project)?;
    }
    let locator = locations.join(format!("{}.json", path_key(&project.path)));
    if read_json::<String>(&locator)?.as_ref() != Some(&project.id) {
        write_json(&locator, &project.id)?;
    }
    Ok(directory)
}

/// 按项目路径精确取得数据目录；不检查工作目录是否存在。
pub fn project_storage_for_path(root: &Path, path: &str) -> Result<Option<PathBuf>, ResourceError> {
    let root = prepare_root(root)?;
    let locations = secure_child_dir(&root, "project-locations")?;
    let Some(id) = read_json::<String>(&locations.join(format!("{}.json", path_key(path))))? else {
        return Ok(None);
    };
    SessionId::new(id.clone())?;
    let projects = secure_child_dir(&root, "projects")?;
    let directory = secure_child_dir(&projects, &id)?;
    let descriptor = read_json::<ProjectStorage>(&directory.join("project.json"))?
        .ok_or_else(|| ResourceError::UnsafePath("项目描述缺失".into()))?;
    if descriptor.id != id || normalized_path(&descriptor.path) != normalized_path(path) {
        return Ok(None);
    }
    Ok(Some(directory))
}

/// 独立 Runtime 也可以创建稳定项目目录；桌面预先登记的 ID 优先使用。
pub fn ensure_project_storage(root: &Path, path: &str) -> Result<PathBuf, ResourceError> {
    let root = prepare_root(root)?;
    let _guard = exclusive_lock(&root.join("project-storage.lock"))?;
    if let Some(directory) = project_storage_for_path(&root, path)? {
        return Ok(directory);
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let project = ProjectStorage {
        id: format!(
            "project-{}",
            &path_key(&format!("{path}:{nonce}:{}", std::process::id()))[..32]
        ),
        name: Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: path.into(),
    };
    register_project_storage_locked(&root, &project)
}

/// 发布对话 ID 到项目 ID 的精确定位记录。
pub fn register_session_location(
    root: &Path,
    session_id: &SessionId,
    project_directory: &Path,
) -> Result<(), ResourceError> {
    let root = prepare_root(root)?;
    let projects = secure_child_dir(&root, "projects")?;
    if project_directory.parent() != Some(projects.as_path()) {
        return Err(ResourceError::UnsafePath("会话项目目录越界".into()));
    }
    let id = project_directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ResourceError::UnsafePath("项目 ID 无效".into()))?;
    SessionId::new(id)?;
    let locations = secure_child_dir(&root, "session-locations")?;
    let destination = locations.join(format!("{}.json", session_id.as_str()));
    let _guard = exclusive_lock(&locations.join("locations.lock"))?;
    if let Some(existing) = read_json::<String>(&destination)? {
        if existing != id {
            return Err(ResourceError::UnsafePath("对话 ID 已属于其他项目".into()));
        }
        return Ok(());
    }
    write_json(&destination, &id)
}

/// 解析会话所属项目；没有定位记录时返回 None，绝不回退到旧目录。
pub fn session_project_directory(
    root: &Path,
    session_id: &SessionId,
) -> Result<Option<PathBuf>, ResourceError> {
    let root = prepare_root(root)?;
    let locations = secure_child_dir(&root, "session-locations")?;
    let Some(id) = read_json::<String>(&locations.join(format!("{}.json", session_id.as_str())))?
    else {
        return Ok(None);
    };
    SessionId::new(id.clone())?;
    let projects = secure_child_dir(&root, "projects")?;
    Ok(Some(secure_child_dir(&projects, &id)?))
}

/// 解析对话相关文件的统一目录。
pub fn session_storage_directory(
    root: &Path,
    session_id: &SessionId,
) -> Result<PathBuf, ResourceError> {
    let project = session_project_directory(root, session_id)?
        .ok_or_else(|| ResourceError::UnsafePath("会话定位记录不存在".into()))?;
    secure_child_dir(&project, session_id.as_str())
}

/// 枚举已经登记的项目数据目录，不访问用户工作目录。
pub fn project_storage_directories(root: &Path) -> Result<Vec<PathBuf>, ResourceError> {
    let root = prepare_root(root)?;
    let projects = secure_child_dir(&root, "projects")?;
    let mut result = Vec::new();
    for entry in fs::read_dir(&projects).map_err(|e| ResourceError::io("list_projects", e))? {
        let entry = entry.map_err(|e| ResourceError::io("read_project", e))?;
        let id = entry
            .file_name()
            .into_string()
            .map_err(|_| ResourceError::UnsafePath("项目 ID 无效".into()))?;
        SessionId::new(id.clone())?;
        result.push(secure_child_dir(&projects, &id)?);
    }
    Ok(result)
}

/// 删除会话后的派生定位记录；历史删除失败时不得调用。
pub fn remove_session_location(root: &Path, session_id: &SessionId) -> Result<(), ResourceError> {
    let root = prepare_root(root)?;
    let locations = secure_child_dir(&root, "session-locations")?;
    let _guard = exclusive_lock(&locations.join("locations.lock"))?;
    let path = locations.join(format!("{}.json", session_id.as_str()));
    crate::atomic::ensure_regular_file_or_absent(&path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ResourceError::io("remove_session_location", error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_project_id_survives_relocation_without_probing_workspace() {
        let root = tempfile::tempdir().unwrap();
        let mut project = ProjectStorage {
            id: "project-stable".into(),
            name: "demo".into(),
            path: "/absent/workspace-one".into(),
        };
        let directory = register_project_storage(root.path(), &project).unwrap();
        assert_eq!(
            ensure_project_storage(root.path(), &project.path).unwrap(),
            directory
        );
        project.path = "/absent/workspace-two".into();
        assert_eq!(
            register_project_storage(root.path(), &project).unwrap(),
            directory
        );
        assert!(
            project_storage_for_path(root.path(), "/absent/workspace-one")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            project_storage_for_path(root.path(), &project.path).unwrap(),
            Some(directory)
        );
    }

    #[test]
    fn session_location_is_exact_isolated_and_removable() {
        let root = tempfile::tempdir().unwrap();
        let first = ensure_project_storage(root.path(), "/project-one").unwrap();
        let second = ensure_project_storage(root.path(), "/project-two").unwrap();
        let id = SessionId::new("session-one").unwrap();
        register_session_location(root.path(), &id, &first).unwrap();
        assert!(register_session_location(root.path(), &id, &second).is_err());
        assert_eq!(
            session_storage_directory(root.path(), &id).unwrap(),
            first.join(id.as_str())
        );
        fs::write(second.join("project.json"), b"corrupt unrelated project").unwrap();
        assert_eq!(
            session_project_directory(root.path(), &id).unwrap(),
            Some(first)
        );
        remove_session_location(root.path(), &id).unwrap();
        assert!(
            session_project_directory(root.path(), &id)
                .unwrap()
                .is_none()
        );
        assert!(session_storage_directory(root.path(), &id).is_err());
    }
}
