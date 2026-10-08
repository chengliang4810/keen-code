//! 原生工作区项目登记、路径授权和 Git 进程边界。
//!
//! 该模块只提供 NativeHost 与工作台服务需要的同步 Rust API。文件树、文本编辑、
//! diff 和工作树生命周期分别由 `native_ui::workbench` 中的服务持有；本模块不依赖
//! 窗口句柄或具体界面适配器。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::native_paths::NativePaths;
use crate::path_utils::{path_text_to_frontend, path_to_frontend};

const MAX_PROJECTS_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const PROJECTS_SCHEMA: &str = "keencode/projects";
const PROJECTS_VERSION: u32 = 1;
const GIT_NETWORK_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static PROJECTS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// 界面使用的项目记录；`path_ok` 只表示最近一次显式校验结果。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub path: String,
    pub path_ok: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredProjectRecord {
    id: String,
    name: String,
    path: String,
}

type ProjectsDocument = Vec<StoredProjectRecord>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProjectsFile {
    schema: String,
    version: u32,
    projects: ProjectsDocument,
}

impl ProjectsFile {
    fn from_projects(projects: ProjectsDocument) -> Self {
        Self {
            schema: PROJECTS_SCHEMA.to_owned(),
            version: PROJECTS_VERSION,
            projects,
        }
    }

    fn into_projects(self) -> Result<ProjectsDocument, String> {
        if self.schema != PROJECTS_SCHEMA || self.version != PROJECTS_VERSION {
            return Err("项目配置 schema 或版本不受支持".to_owned());
        }
        Ok(self.projects)
    }
}

fn projects_lock() -> &'static Mutex<()> {
    PROJECTS_LOCK.get_or_init(|| Mutex::new(()))
}

fn projects_file_path(paths: &NativePaths) -> Result<PathBuf, String> {
    let root = crate::storage::root_dir(paths).map_err(|error| error.to_string())?;
    fs::create_dir_all(&root)
        .map_err(|error| format!("无法创建应用数据目录 {}：{error}", root.display()))?;
    Ok(root.join("projects.json"))
}

fn read_projects_config_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法检查 {}：{error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("项目配置路径不是普通文件：{}", path.display()));
    }
    if metadata.len() > MAX_PROJECTS_CONFIG_BYTES {
        return Err(format!(
            "项目配置超过 {MAX_PROJECTS_CONFIG_BYTES} 字节：{}",
            path.display()
        ));
    }
    let file = crate::storage::open_readonly_regular_file(path)
        .map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("无法读取 {} 的句柄元数据：{error}", path.display()))?;
    if !opened.is_file() || opened.len() != metadata.len() {
        return Err(format!("项目配置在打开期间发生变化：{}", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PROJECTS_CONFIG_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_len > MAX_PROJECTS_CONFIG_BYTES || actual_len != opened.len() {
        return Err(format!(
            "项目配置在读取期间发生变化或超过大小上限：{}",
            path.display()
        ));
    }
    let final_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法复核 {}：{error}", path.display()))?;
    if final_metadata.file_type().is_symlink()
        || !final_metadata.is_file()
        || final_metadata.len() != metadata.len()
    {
        return Err(format!("项目配置在读取期间发生变化：{}", path.display()));
    }
    Ok(bytes)
}

fn load_projects_document(paths: &NativePaths) -> Result<ProjectsDocument, String> {
    let path = projects_file_path(paths)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("无法检查 {}：{error}", path.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("项目配置路径不是普通文件：{}", path.display()));
    }
    let bytes = read_projects_config_bytes(&path)?;
    if bytes.is_empty() {
        return Err(format!("项目配置为空：{}", path.display()));
    }
    let file: ProjectsFile =
        serde_json::from_slice(&bytes).map_err(|error| format!("项目配置格式无效：{error}"))?;
    let document = file.into_projects()?;
    validate_projects_document(&document)?;
    Ok(document)
}

fn validate_projects_document(document: &ProjectsDocument) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for project in document {
        let mut characters = project.id.chars();
        if project.id.is_empty()
            || project.id.len() > 128
            || !characters
                .next()
                .is_some_and(|character| character.is_ascii_alphanumeric())
            || !project
                .id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
        {
            return Err(format!("项目标识格式无效：{}", project.id));
        }
        if !ids.insert(project.id.as_str()) {
            return Err(format!("项目标识重复：{}", project.id));
        }
        if project.name.trim().is_empty()
            || project.name.trim() != project.name
            || project.name.chars().count() > 120
            || project.name.chars().any(char::is_control)
        {
            return Err(format!("项目 {} 的名称不能为空或包含首尾空白", project.id));
        }
        let project_path = Path::new(&project.path);
        if project.path.trim() != project.path
            || project.path.chars().any(char::is_control)
            || !project_path.is_absolute()
            || project_path
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(format!("项目 {} 的路径必须是规范绝对路径", project.id));
        }
        if !paths.insert(project.path.as_str()) {
            return Err(format!("项目路径重复：{}", project.path));
        }
    }
    Ok(())
}

fn save_projects_document(paths: &NativePaths, document: &ProjectsDocument) -> Result<(), String> {
    validate_projects_document(document)?;
    let path = projects_file_path(paths)?;
    let root = crate::storage::root_dir(paths).map_err(|error| error.to_string())?;
    for project in document {
        keencode_resources::register_project_storage(
            &root,
            &keencode_resources::ProjectStorage {
                id: project.id.clone(),
                name: project.name.clone(),
                path: project.path.clone(),
            },
        )
        .map_err(|error| error.to_string())?;
    }
    let mut bytes = serde_json::to_vec_pretty(&ProjectsFile::from_projects(document.clone()))
        .map_err(|error| format!("无法序列化项目记录：{error}"))?;
    bytes.push(b'\n');
    crate::storage::atomic_write_private(&path, &bytes).map_err(|error| error.to_string())
}

fn project_record(record: &StoredProjectRecord) -> ProjectRecord {
    ProjectRecord {
        id: record.id.clone(),
        name: record.name.clone(),
        path: path_to_frontend(Path::new(&record.path)),
        path_ok: None,
    }
}

fn generate_project_id(records: &[StoredProjectRecord]) -> String {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    loop {
        let sequence = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = format!("project-{nanos:x}-{:x}-{sequence:x}", std::process::id());
        if !records.iter().any(|record| record.id == id) {
            return id;
        }
    }
}

fn find_project_index(records: &[StoredProjectRecord], id: &str) -> Result<usize, String> {
    records
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| format!("找不到项目：{id}"))
}

fn canonical_existing_dir(path: &str) -> Result<PathBuf, String> {
    let normalized = path_text_to_frontend(path);
    let raw = Path::new(&normalized);
    if !raw.is_absolute() {
        return Err("项目路径必须是绝对路径".to_owned());
    }
    let canonical = fs::canonicalize(raw)
        .map_err(|error| format!("无法访问项目目录 {}：{error}", raw.display()))?;
    if !canonical.is_dir() {
        return Err(format!("项目路径不是目录：{}", canonical.display()));
    }
    Ok(canonical)
}

fn normalize_project_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err("项目名称不能为空且不能超过 120 个字符".to_owned());
    }
    if name == "."
        || name == ".."
        || name.ends_with(' ')
        || name.ends_with('.')
        || name
            .chars()
            .any(|character| character.is_control() || r#"<>:\"/|?*"#.contains(character))
    {
        return Err("项目名称不能包含路径字符或以空格、句点结尾".to_owned());
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                number.len() == 1
                    && number
                        .as_bytes()
                        .first()
                        .is_some_and(|digit| (b'1'..=b'9').contains(digit))
            })
    {
        return Err("项目名称是系统保留名称，请换一个名称".to_owned());
    }
    Ok(name.to_owned())
}

fn create_project_directory(root: &Path, name: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("无法创建默认项目目录 {}：{error}", root.display()))?;
    let root = fs::canonicalize(root)
        .map_err(|error| format!("无法访问默认项目目录 {}：{error}", root.display()))?;
    let target = root.join(name);
    fs::create_dir(&target).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "项目目录已存在，请选择该目录作为已有项目：{}",
                target.display()
            )
        } else {
            format!("无法创建项目目录 {}：{error}", target.display())
        }
    })?;
    fs::canonicalize(&target).map_err(|error| {
        let _ = fs::remove_dir(&target);
        format!("无法访问新项目目录 {}：{error}", target.display())
    })
}

/// 返回当前项目登记的稳定投影。
pub(crate) fn project_records(paths: &NativePaths) -> Result<Vec<ProjectRecord>, String> {
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    Ok(load_projects_document(paths)?
        .iter()
        .map(project_record)
        .collect())
}

/// 创建项目；没有显式路径时使用设置中已解析的默认父目录。
pub(crate) fn project_create(
    paths: &NativePaths,
    path: Option<String>,
    name: String,
    create_workspace_root_if_missing: bool,
) -> Result<ProjectRecord, String> {
    let name = normalize_project_name(&name)?;
    let (canonical, created) = if let Some(path) = path {
        let directory = PathBuf::from(&path);
        let created = create_workspace_root_if_missing && !directory.exists();
        if created {
            if !directory.is_absolute() {
                return Err("项目目录必须是绝对路径".to_owned());
            }
            fs::create_dir_all(&directory).map_err(|error| format!("创建项目目录失败：{error}"))?;
        }
        (canonical_existing_dir(&path)?, created)
    } else {
        let settings = crate::app_settings::get(paths).map_err(|error| error.to_string())?;
        let parent = PathBuf::from(settings.project_directory);
        (create_project_directory(&parent, &name)?, true)
    };
    let canonical_text = path_to_frontend(&canonical);
    let result = (|| {
        let _guard = projects_lock()
            .lock()
            .map_err(|_| "项目元数据锁已损坏".to_owned())?;
        let mut records = load_projects_document(paths)?;
        if records.iter().any(|record| {
            fs::canonicalize(&record.path)
                .ok()
                .is_some_and(|stored| stored == canonical)
        }) {
            return Err(format!(
                "该目录已经是 KeenCode 项目：{}",
                canonical.display()
            ));
        }
        let stored = StoredProjectRecord {
            id: generate_project_id(&records),
            name,
            path: canonical_text,
        };
        let project = project_record(&stored);
        records.push(stored);
        save_projects_document(paths, &records)?;
        Ok(project)
    })();
    if result.is_err() && created {
        let _ = fs::remove_dir(&canonical);
    }
    result
}

pub(crate) fn project_remove(paths: &NativePaths, id: &str) -> Result<ProjectRecord, String> {
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let mut records = load_projects_document(paths)?;
    let index = find_project_index(&records, id)?;
    let removed = project_record(&records.remove(index));
    save_projects_document(paths, &records)?;
    Ok(removed)
}

pub(crate) fn project_rename(
    paths: &NativePaths,
    id: &str,
    name: &str,
) -> Result<ProjectRecord, String> {
    let name = normalize_project_name(name)?;
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let mut records = load_projects_document(paths)?;
    let index = find_project_index(&records, id)?;
    records[index].name = name;
    let project = project_record(&records[index]);
    save_projects_document(paths, &records)?;
    Ok(project)
}

pub(crate) fn projects_reorder(
    paths: &NativePaths,
    ids: &[String],
) -> Result<Vec<ProjectRecord>, String> {
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let records = load_projects_document(paths)?;
    if ids.len() != records.len() || ids.iter().collect::<HashSet<_>>().len() != records.len() {
        return Err("项目顺序必须包含全部且不重复的项目标识".to_owned());
    }
    let mut by_id = records
        .into_iter()
        .map(|record| (record.id.clone(), record))
        .collect::<std::collections::HashMap<_, _>>();
    let records = ids
        .iter()
        .map(|id| by_id.remove(id).ok_or_else(|| format!("找不到项目：{id}")))
        .collect::<Result<Vec<_>, _>>()?;
    save_projects_document(paths, &records)?;
    Ok(records.iter().map(project_record).collect())
}

fn registered_project_root_from_document(
    projects: &ProjectsDocument,
    canonical: &Path,
) -> Option<PathBuf> {
    projects.iter().find_map(|project| {
        let stored = fs::canonicalize(&project.path).ok()?;
        (stored.is_dir() && stored == canonical).then_some(stored)
    })
}

/// 只有完整匹配已登记目录才有项目执行权限；子目录不会自动成为项目根。
pub(crate) fn registered_project_root(
    paths: &NativePaths,
    project_path: &str,
) -> Result<PathBuf, String> {
    let canonical = canonical_existing_dir(project_path)?;
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let projects = load_projects_document(paths)?;
    registered_project_root_from_document(&projects, &canonical)
        .ok_or_else(|| format!("项目尚未添加：{}", canonical.display()))
}

/// 返回无项目 Session 唯一允许使用的规范化应用数据目录。
pub(crate) fn app_data_session_root(paths: &NativePaths) -> Result<PathBuf, String> {
    let data_dir = crate::storage::root_dir(paths).map_err(|error| error.to_string())?;
    fs::create_dir_all(&data_dir)
        .map_err(|error| format!("无法创建应用数据目录 {}：{error}", data_dir.display()))?;
    fs::canonicalize(&data_dir)
        .map_err(|error| format!("无法访问应用数据目录 {}：{error}", data_dir.display()))
}

/// 对话草稿唯一使用的内部工作区根目录；其余数据目录仍不获项目授权。
pub(crate) fn conversation_workspace_root(paths: &NativePaths) -> Result<PathBuf, String> {
    let data_dir = crate::storage::root_dir(paths).map_err(|error| error.to_string())?;
    let conversation = data_dir.join("chat-workspaces").join("conversation");
    fs::create_dir_all(&conversation)
        .map_err(|error| format!("无法创建对话工作区 {}：{error}", conversation.display()))?;
    fs::canonicalize(&conversation)
        .map_err(|error| format!("无法访问对话工作区 {}：{error}", conversation.display()))
}

pub(crate) fn canonical_session_root(path: &str) -> Result<PathBuf, String> {
    canonical_existing_dir(path)
}

/// 创建不会在 Windows 桌面环境中弹出控制台窗口的 Git 命令。
pub(crate) fn git_command() -> Command {
    let mut command = Command::new("git");
    command.env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// 将 Git 的自动寻根限制在当前工作目录，避免把嵌套项目误解析为父仓库。
///
/// 工作台的隔离项目、临时工作树和测试 fixture 可能位于另一个 Git 仓库内；
/// Git 默认会继续向父目录搜索 `.git`，因此每个带 `-C` 的命令都必须设置上限。
pub(crate) fn git_command_for_root(root: &Path) -> Command {
    let mut command = git_command();
    if let Some(parent) = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        command.env("GIT_CEILING_DIRECTORIES", parent);
    }
    command
}

/// 执行带项目工作目录的 Git 命令。
pub(crate) fn run_git(root: &Path, args: &[&str]) -> Result<Output, String> {
    git_command_for_root(root)
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("无法执行 git：{error}"))
}

/// 限时执行 Git 命令，并独立排空 stdout/stderr，避免管道互相等待。
pub(crate) fn run_git_with_timeout(root: &Path, args: &[&str]) -> Result<Output, String> {
    let mut child = git_command_for_root(root)
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("无法执行 git：{error}"))?;
    let stdout = child.stdout.take().ok_or("git stdout 管道不可用")?;
    let stderr = child.stderr.take().ok_or("git stderr 管道不可用")?;
    fn drain(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut pipe = pipe;
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    }
    let stdout_reader = drain(stdout);
    let stderr_reader = drain(stderr);
    let deadline = std::time::Instant::now() + GIT_NETWORK_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git {} 超时（超过 {} 秒未完成，已终止）",
                    args.first().copied().unwrap_or("命令"),
                    GIT_NETWORK_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(format!("git 进程等待失败：{error}")),
        }
    };
    Ok(Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name_rejects_path_and_reserved_names() {
        assert_eq!(normalize_project_name("  demo  ").unwrap(), "demo");
        for value in ["", ".", "..", "a/b", "CON", "LPT1.txt", "demo."] {
            assert!(normalize_project_name(value).is_err(), "{value}");
        }
    }

    #[test]
    fn project_file_rejects_unknown_fields() {
        let value = serde_json::json!({
            "schema": PROJECTS_SCHEMA,
            "version": PROJECTS_VERSION,
            "projects": [],
            "unknown": true
        });
        assert!(serde_json::from_value::<ProjectsFile>(value).is_err());
    }

    #[test]
    fn git_commands_do_not_discover_a_parent_repository() {
        let temporary = tempfile::tempdir().expect("创建临时 Git 目录");
        let parent = temporary.path();
        let initialized = run_git(parent, &["init"]).expect("执行 git init");
        assert!(initialized.status.success(), "父仓库初始化失败");
        let child = parent.join("nested-project");
        fs::create_dir(&child).expect("创建嵌套项目目录");
        let output = run_git(&child, &["rev-parse", "--is-inside-work-tree"])
            .expect("执行嵌套目录 Git 查询");
        assert!(!output.status.success());
        // Windows canonicalize 会产生扩展路径前缀；实际宿主正是使用这种目录调用 Git。
        let canonical_child = fs::canonicalize(&child).expect("规范化嵌套项目目录");
        let output = run_git(&canonical_child, &["rev-parse", "--is-inside-work-tree"])
            .expect("执行规范化目录 Git 查询");
        assert!(!output.status.success());
        let initialized = run_git(&canonical_child, &["init"]).expect("初始化独立子仓库");
        assert!(initialized.status.success(), "子仓库初始化失败");
        let output = run_git(&canonical_child, &["rev-parse", "--is-inside-work-tree"])
            .expect("执行独立子仓库 Git 查询");
        assert!(output.status.success());
    }

    #[test]
    fn registered_root_requires_exact_directory() {
        let directory = tempfile::tempdir().expect("创建项目目录");
        let root = fs::canonicalize(directory.path()).expect("规范化项目目录");
        let child = root.join("child");
        fs::create_dir(&child).expect("创建子目录");
        let document = vec![StoredProjectRecord {
            id: "project-test".to_owned(),
            name: "test".to_owned(),
            path: path_to_frontend(&root),
        }];
        assert_eq!(
            registered_project_root_from_document(&document, &root),
            Some(root.clone())
        );
        assert_eq!(
            registered_project_root_from_document(&document, &child),
            None
        );
    }
}
