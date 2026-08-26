use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, FilePath};

/// 文本预览最多读取的字节数。
const MAX_TEXT_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
/// Git 文本结果最多返回的字节数。
const MAX_GIT_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// 单次按需展开未跟踪目录最多返回的状态项数量。
const MAX_UNTRACKED_DIRECTORY_ENTRIES: usize = 2_000;
/// Windows 子进程不创建控制台窗口的进程标志。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// 后缀搜索最多检查的目录项数量。
const MAX_SUFFIX_SEARCH_ENTRIES: usize = 20_000;
/// 项目元数据读改写的进程内互斥锁。
static PROJECTS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
/// 原子临时文件名的进程内序号。
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);
/// 项目标识的进程内序号。
static PROJECT_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// 前端使用的项目记录。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    /// 项目稳定标识。
    pub id: String,
    /// 项目显示名称。
    pub name: String,
    /// 项目规范化绝对路径。
    pub path: String,
    /// 项目目录当前是否可访问。
    pub path_ok: bool,
}

/// KeenCode 当前唯一的项目持久化记录；可访问状态由文件系统实时计算，不写入配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StoredProjectRecord {
    /// 项目稳定标识。
    id: String,
    /// 项目显示名称。
    name: String,
    /// 项目规范化绝对路径。
    path: String,
}

/// 拖放路径的基础分类结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathEntry {
    /// 规范化绝对路径或原始不可用路径。
    pub path: String,
    /// 路径末级名称。
    pub name: String,
    /// 路径是否为目录。
    pub is_dir: bool,
    /// 路径是否存在。
    pub exists: bool,
}

/// Git worktree 条目。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeEntry {
    /// Worktree 规范化绝对路径。
    pub path: String,
    /// Worktree 当前提交。
    pub head: Option<String>,
    /// Worktree 分支短名称。
    pub branch: Option<String>,
    /// Worktree 是否为游离 HEAD。
    pub detached: bool,
    /// Worktree 是否为主工作树。
    pub is_main: bool,
    /// Worktree 是否已锁定。
    pub locked: bool,
    /// Worktree 是否可清理。
    pub prunable: bool,
}

/// Git worktree 列表结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreesResult {
    /// Git 和仓库是否可用。
    pub available: bool,
    /// 仓库关联的工作树。
    pub worktrees: Vec<GitWorktreeEntry>,
    /// 不可用原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 新建 Git worktree 的结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeAddResult {
    /// 新工作树绝对路径。
    pub path: String,
    /// 新工作树名称。
    pub name: String,
    /// 新工作树起点。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_point: Option<String>,
    /// 新工作树分支。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Git worktree 清理结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeGcResult {
    /// 是否只预览清理。
    pub dry_run: bool,
    /// 是否立即过期所有可清理记录。
    pub force: bool,
    /// 清理或预计清理的记录数量。
    pub pruned_count: usize,
    /// 执行前标记为可清理的路径。
    pub prunable: Vec<String>,
    /// Git 标准输出。
    pub stdout: String,
    /// Git 标准错误。
    pub stderr: String,
    /// 合并后的可读输出。
    pub output: String,
}

/// Git 工作区文件状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusEntry {
    /// 仓库相对路径。
    pub path: String,
    /// 文件绝对路径。
    pub absolute_path: String,
    /// Git porcelain 双字符状态。
    pub status: String,
    /// 暂存区状态字符。
    pub index_status: String,
    /// 工作区状态字符。
    pub worktree_status: String,
    /// 前端使用的粗粒度状态。
    pub kind: String,
    /// 文件末级名称。
    pub name: String,
    /// 当前状态项是否代表被 normal 模式折叠的未跟踪目录。
    pub is_directory: bool,
    /// 当前目录是否为独立嵌套 Git 仓库，主仓库不得继续展开其内容。
    pub is_nested_repository: bool,
    /// 重命名或复制前的相对路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_path: Option<String>,
}

/// 按需展开未跟踪目录的结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitUntrackedDirectoryResult {
    /// Git 过滤后的未跟踪文件或嵌套仓库边界。
    pub files: Vec<GitStatusEntry>,
    /// 结果是否达到单次返回上限。
    pub truncated: bool,
}

/// Git 工作区状态结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusResult {
    /// Git 和仓库是否可用。
    pub available: bool,
    /// 工作区变更文件。
    pub files: Vec<GitStatusEntry>,
    /// 当前分支名称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// 工作区与暂存区合计新增行数。
    pub additions: u64,
    /// 工作区与暂存区合计删除行数。
    pub deletions: u64,
    /// 是否存在未暂存的已跟踪改动。
    pub has_unstaged_changes: bool,
    /// 不可用原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Git 提交结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitResult {
    /// 新提交短哈希。
    pub commit: String,
    /// 提交所在分支。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Git 合并输出。
    pub output: String,
}

/// Git 推送结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPushResult {
    /// 推送所在分支。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Git 合并输出。
    pub output: String,
}

/// 单文件 Git diff 结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiffResult {
    /// Git 和仓库是否可用。
    pub available: bool,
    /// 统一 diff 文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// 仓库相对路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    /// 不可用或无结果原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Git HEAD 文件内容结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitShowFileResult {
    /// Git 和仓库是否可用。
    pub available: bool,
    /// HEAD 中的 UTF-8 文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// 仓库相对路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    /// 不可用或无结果原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 文件树目录项。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    /// 文件或目录名称。
    pub name: String,
    /// 项目根目录下的相对路径。
    pub relative_path: String,
    /// 是否为目录。
    pub is_dir: bool,
    /// 文件字节数，目录固定为零。
    pub size: u64,
    /// 小写扩展名。
    pub ext: String,
}

/// 文件预览结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsReadResult {
    /// 项目相对路径或外部绝对路径。
    pub relative_path: String,
    /// 文件名称。
    pub name: String,
    /// 规范化绝对路径。
    pub absolute_path: String,
    /// 文件字节数。
    pub size: u64,
    /// 前端预览类型。
    pub kind: String,
    /// 文件 MIME 类型。
    pub mime: String,
    /// UTF-8 文本预览。
    pub text: Option<String>,
    /// 小型二进制的 Base64；首版不在后端编码。
    pub base64: Option<String>,
    /// 是否应通过 Tauri 资源协议流式读取。
    pub stream: bool,
    /// 文本是否因预览上限被截断。
    pub truncated: bool,
    /// 文件级软错误。
    pub error: Option<String>,
    /// 最后修改时间的 Unix 毫秒值。
    pub mtime_ms: u64,
}

/// 文件写入结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FsWriteResult {
    /// 项目相对路径或外部绝对路径。
    pub relative_path: String,
    /// 规范化绝对路径。
    pub absolute_path: String,
    /// 写入后的文件字节数。
    pub size: u64,
    /// 写入后的最后修改时间。
    pub mtime_ms: u64,
}

/// 项目文件采用唯一的顶层数组结构。
type ProjectsDocument = Vec<StoredProjectRecord>;

/// 返回项目元数据锁。
fn projects_lock() -> &'static Mutex<()> {
    PROJECTS_LOCK.get_or_init(|| Mutex::new(()))
}

/// 返回应用项目元数据文件路径。
fn projects_file_path(app: &AppHandle) -> Result<PathBuf, String> {
    let data_dir =
        crate::storage::root_dir(app).map_err(|error| format!("无法确定应用数据目录：{error}"))?;
    fs::create_dir_all(&data_dir)
        .map_err(|error| format!("无法创建应用数据目录 {}：{error}", data_dir.display()))?;
    Ok(data_dir.join("projects.json"))
}

/// 从磁盘读取项目文档。
fn load_projects_document(app: &AppHandle) -> Result<ProjectsDocument, String> {
    let path = projects_file_path(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&path).map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    if bytes.is_empty() {
        return Err(format!("项目配置不能为空：{}", path.display()));
    }
    let document: ProjectsDocument = serde_json::from_slice(&bytes)
        .map_err(|error| format!("无法解析 {}：{error}", path.display()))?;
    validate_projects_document(&document)?;
    Ok(document)
}

/// 校验项目配置必须完整符合当前唯一结构，不修正重复项或非规范文本。
fn validate_projects_document(document: &ProjectsDocument) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut paths = HashSet::new();
    for project in document {
        let mut id_characters = project.id.chars();
        if project.id.len() > 128
            || !id_characters
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
        {
            return Err(format!("项目 {} 的路径必须是绝对路径", project.id));
        }
        if project_path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(format!("项目 {} 的路径必须是规范路径", project.id));
        }
        if !paths.insert(project.path.as_str()) {
            return Err(format!("项目路径重复：{}", project.path));
        }
    }
    Ok(())
}

/// 将持久化项目投影为包含实时可访问状态的前端记录。
fn project_record(record: &StoredProjectRecord) -> ProjectRecord {
    ProjectRecord {
        id: record.id.clone(),
        name: record.name.clone(),
        path: record.path.clone(),
        path_ok: Path::new(&record.path).is_dir(),
    }
}

/// 使用同目录临时文件原子保存项目文档。
fn save_projects_document(app: &AppHandle, document: &ProjectsDocument) -> Result<(), String> {
    validate_projects_document(document)?;
    let path = projects_file_path(app)?;
    let mut bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| format!("无法序列化项目记录：{error}"))?;
    bytes.push(b'\n');
    atomic_write_bytes(&path, &bytes)
}

/// 生成同目录唯一临时文件路径。
fn temporary_path_for(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("路径没有父目录：{}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("keencode-data");
    let counter = TEMP_FILE_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(
        ".{name}.{}.{}.{}.tmp",
        std::process::id(),
        nanos,
        counter
    )))
}

/// 使用同目录临时文件和 rename 原子写入字节。
fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("路径没有父目录：{}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("无法创建目录 {}：{error}", parent.display()))?;
    let temporary = temporary_path_for(path)?;
    let write_result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("无法创建临时文件 {}：{error}", temporary.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("无法写入临时文件 {}：{error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("无法同步临时文件 {}：{error}", temporary.display()))?;
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary, metadata.permissions())
                .map_err(|error| format!("无法保留文件权限 {}：{error}", temporary.display()))?;
        }
        fs::rename(&temporary, path).map_err(|error| {
            format!(
                "无法原子替换 {}（临时文件 {}）：{error}",
                path.display(),
                temporary.display()
            )
        })?;
        #[cfg(unix)]
        {
            if let Ok(directory) = File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

/// 查找指定项目记录下标。
fn find_project_index(records: &[StoredProjectRecord], id: &str) -> Result<usize, String> {
    records
        .iter()
        .position(|record| record.id == id)
        .ok_or_else(|| format!("找不到项目：{id}"))
}

/// 生成当前文档中不重复的项目标识。
fn generate_project_id(records: &[StoredProjectRecord]) -> String {
    loop {
        let counter = PROJECT_ID_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let id = format!("project-{nanos:x}-{:x}-{counter:x}", std::process::id());
        if !records.iter().any(|record| record.id == id) {
            return id;
        }
    }
}

/// 规范化并确认现有目录。
fn canonical_existing_dir(path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
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

/// 从目录路径生成默认项目名称。
fn project_name_from_path(path: &Path) -> String {
    path.file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// 校验并规范化用户输入的项目显示名称。
fn normalize_project_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("项目名称不能为空".to_owned());
    }
    if name.chars().count() > 120 {
        return Err("项目名称不能超过 120 个字符".to_owned());
    }
    Ok(name.to_owned())
}

/// 返回规范化后的项目列表。
#[tauri::command]
pub fn projects_list(
    app: AppHandle,
    diagnostics: State<'_, Arc<crate::diagnostics::Diagnostics>>,
) -> Result<Vec<ProjectRecord>, String> {
    diagnostics.log("info", "ipc.projects_list", "命令进入");
    let _guard = projects_lock().lock().map_err(|_| {
        diagnostics.error("ipc.projects_list", "项目元数据锁已损坏");
        "项目元数据锁已损坏".to_owned()
    })?;
    let result = load_projects_document(&app);
    match result {
        Ok(projects) => {
            diagnostics.log(
                "info",
                "ipc.projects_list",
                format!("命令完成 count={}", projects.len()),
            );
            Ok(projects.iter().map(project_record).collect())
        }
        Err(error) => {
            diagnostics.error("ipc.projects_list", format!("命令失败: {error}"));
            Err(error)
        }
    }
}

/// 添加本地项目；项目登记成功后即获得该目录的访问授权。
#[tauri::command]
pub fn project_add(
    app: AppHandle,
    path: String,
    name: Option<String>,
) -> Result<ProjectRecord, String> {
    let canonical = canonical_existing_dir(&path)?;
    let canonical_text = canonical.to_string_lossy().into_owned();
    let name = name.as_deref().map(normalize_project_name).transpose()?;
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let mut records = load_projects_document(&app)?;

    let existing_index = records.iter().position(|record| {
        fs::canonicalize(&record.path)
            .ok()
            .is_some_and(|stored| stored == canonical)
    });
    let project = if let Some(index) = existing_index {
        project_record(&records[index])
    } else {
        let stored = StoredProjectRecord {
            id: generate_project_id(&records),
            name: name.unwrap_or_else(|| project_name_from_path(&canonical)),
            path: canonical_text,
        };
        let project = project_record(&stored);
        records.push(stored);
        project
    };
    save_projects_document(&app, &records)?;
    Ok(project)
}

/// 从应用列表移除项目，不删除磁盘内容。
#[tauri::command]
pub fn project_remove(app: AppHandle, id: String) -> Result<ProjectRecord, String> {
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let mut records = load_projects_document(&app)?;
    let index = find_project_index(&records, &id)?;
    let removed = project_record(&records.remove(index));
    save_projects_document(&app, &records)?;
    Ok(removed)
}

/// 将项目记录指向新的现有目录。
#[tauri::command]
pub fn project_relocate(app: AppHandle, id: String, path: String) -> Result<ProjectRecord, String> {
    let canonical = canonical_existing_dir(&path)?;
    let canonical_text = canonical.to_string_lossy().into_owned();
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let mut records = load_projects_document(&app)?;
    let index = find_project_index(&records, &id)?;
    if records.iter().enumerate().any(|(other_index, record)| {
        other_index != index
            && fs::canonicalize(&record.path)
                .ok()
                .is_some_and(|stored| stored == canonical)
    }) {
        return Err(format!("该目录已属于其他项目：{}", canonical.display()));
    }
    records[index].path = canonical_text;
    let project = project_record(&records[index]);
    save_projects_document(&app, &records)?;
    Ok(project)
}

/// 修改项目显示名称。
#[tauri::command]
pub fn project_rename(app: AppHandle, id: String, name: String) -> Result<ProjectRecord, String> {
    let name = normalize_project_name(&name)?;
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let mut records = load_projects_document(&app)?;
    let index = find_project_index(&records, &id)?;
    records[index].name = name;
    let project = project_record(&records[index]);
    save_projects_document(&app, &records)?;
    Ok(project)
}

/// 按前端提交的完整标识顺序重排项目。
#[tauri::command]
pub fn projects_reorder(app: AppHandle, ids: Vec<String>) -> Result<Vec<ProjectRecord>, String> {
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let records = load_projects_document(&app)?;
    if ids.len() != records.len() || ids.iter().collect::<HashSet<_>>().len() != records.len() {
        return Err("项目顺序必须包含全部且不重复的项目标识".to_owned());
    }
    let mut by_id = records
        .into_iter()
        .map(|record| (record.id.clone(), record))
        .collect::<std::collections::HashMap<_, _>>();
    let records = ids
        .into_iter()
        .map(|id| by_id.remove(&id).ok_or_else(|| format!("找不到项目：{id}")))
        .collect::<Result<Vec<_>, _>>()?;
    save_projects_document(&app, &records)?;
    Ok(records.iter().map(project_record).collect())
}

/// 在系统文件管理器中定位项目目录。
#[tauri::command]
pub fn project_reveal(app: AppHandle, id: String) -> Result<(), String> {
    let path = {
        let _guard = projects_lock()
            .lock()
            .map_err(|_| "项目元数据锁已损坏".to_owned())?;
        let records = load_projects_document(&app)?;
        let record = records
            .iter()
            .find(|record| record.id == id)
            .ok_or_else(|| format!("找不到项目：{id}"))?;
        record.path.clone()
    };
    let canonical = canonical_existing_dir(&path)?;
    reveal_in_file_manager(&canonical)
}

/// 将原生选择器路径转换为规范化本机绝对路径。
fn selected_file_path_to_absolute(selected: FilePath) -> Result<String, String> {
    let path = selected
        .into_path()
        .map_err(|error| format!("选择结果不是本机文件路径：{error}"))?;
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| format!("无法确定当前目录：{error}"))?
            .join(path)
    };
    let canonical = fs::canonicalize(&absolute)
        .map_err(|error| format!("无法访问选择路径 {}：{error}", absolute.display()))?;
    Ok(path_to_frontend(&canonical))
}

/// 打开单目录选择器。
#[tauri::command]
pub async fn pick_directory(app: AppHandle) -> Result<Option<String>, String> {
    app.dialog()
        .file()
        .blocking_pick_folder()
        .map(selected_file_path_to_absolute)
        .transpose()
}

/// 打开多文件附件选择器。
#[tauri::command]
pub async fn pick_attach_files(app: AppHandle) -> Result<Vec<String>, String> {
    app.dialog()
        .file()
        .blocking_pick_files()
        .unwrap_or_default()
        .into_iter()
        .map(selected_file_path_to_absolute)
        .collect()
}

/// 将 WebView 剪贴板中的文件持久化为 Agent 可读取的本机附件。
#[tauri::command]
pub fn save_pasted_attachment(
    app: AppHandle,
    name: String,
    bytes: Vec<u8>,
) -> Result<String, String> {
    let safe_name = Path::new(&name)
        .file_name()
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "pasted-file".to_owned());
    let sequence = TEMP_FILE_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let directory = crate::storage::root_dir(&app)
        .map_err(|error| error.to_string())?
        .join("attachments");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建粘贴附件目录 {}：{error}", directory.display()))?;
    let target = directory.join(format!("{stamp}-{sequence}-{safe_name}"));
    crate::storage::atomic_write_private(&target, &bytes)
        .map_err(|error| format!("无法保存粘贴附件 {}：{error}", target.display()))?;
    Ok(path_to_frontend(&target))
}

/// 读取任意现有绝对路径下的本地图片，供 WebView 生成 Blob 预览。
#[tauri::command]
pub async fn read_local_image(path: String) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || read_local_image_bytes(Path::new(&path)))
        .await
        .map_err(|error| format!("本地图片读取任务失败：{error}"))?
        .map(tauri::ipc::Response::new)
}

fn read_local_image_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let path = canonical_existing_path(path)?;
    let metadata =
        fs::metadata(&path).map_err(|error| format!("无法读取图片 {}：{error}", path.display()))?;
    if !metadata.is_file() || preview_classification(&path).0 != "image" {
        return Err(format!("目标不是支持的图片：{}", path.display()));
    }
    fs::read(&path).map_err(|error| format!("无法读取图片 {}：{error}", path.display()))
}

/// 分类一组绝对路径，单个无效路径不会中止整批结果。
#[tauri::command]
pub fn paths_classify(paths: Vec<String>) -> Vec<PathEntry> {
    paths
        .into_iter()
        .map(|raw| {
            let input = PathBuf::from(&raw);
            let canonical = if input.is_absolute() {
                fs::canonicalize(&input).ok()
            } else {
                None
            };
            let resolved = canonical.as_deref().unwrap_or(&input);
            let metadata = canonical.as_ref().and_then(|path| fs::metadata(path).ok());
            PathEntry {
                path: if canonical.is_some() {
                    resolved.to_string_lossy().into_owned()
                } else {
                    raw
                },
                name: resolved
                    .file_name()
                    .map(|value| value.to_string_lossy().into_owned())
                    .unwrap_or_else(|| resolved.to_string_lossy().into_owned()),
                is_dir: metadata.as_ref().is_some_and(fs::Metadata::is_dir),
                exists: metadata.is_some(),
            }
        })
        .collect()
}

/// 使用系统默认应用打开现有路径。
#[tauri::command]
pub fn path_open(app: AppHandle, path: String) -> Result<(), String> {
    let canonical = authorize_existing_absolute(&app, Path::new(&path))?;
    open_with_default_application(&canonical)
}

/// 使用系统默认浏览器打开 HTTP 或 HTTPS 地址。
#[tauri::command]
pub fn url_open(url: String) -> Result<(), String> {
    let validated = validate_external_url(&url)?;
    open_url_with_default_browser(&validated)
}

/// 校验并规范化允许交给系统浏览器的外部 URL。
fn validate_external_url(raw: &str) -> Result<String, String> {
    let parsed = url::Url::parse(raw.trim()).map_err(|error| format!("URL 无效：{error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("只允许打开 HTTP 或 HTTPS 地址".to_owned());
    }
    Ok(parsed.to_string())
}

/// 在系统文件管理器中定位现有路径。
#[tauri::command]
pub fn path_reveal(app: AppHandle, path: String) -> Result<(), String> {
    let canonical = authorize_existing_absolute(&app, Path::new(&path))?;
    reveal_in_file_manager(&canonical)
}

/// 通过系统默认应用打开路径。
pub(crate) fn open_with_default_application(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]).arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .spawn()
        .map_err(|error| format!("无法打开 {}：{error}", path.display()))?;
    Ok(())
}

/// 通过系统默认浏览器打开已校验的 URL。
fn open_url_with_default_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler").arg(url);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("无法使用默认浏览器打开 URL：{error}"))
}

/// 在系统文件管理器中定位路径。
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer");
        command.arg(format!("/select,{}", path.to_string_lossy()));
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        });
        command
    };
    command
        .spawn()
        .map_err(|error| format!("无法定位 {}：{error}", path.display()))?;
    Ok(())
}

/// 规范化现有文件或目录。
fn canonical_existing_path(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("路径必须是绝对路径".to_owned());
    }
    fs::canonicalize(path).map_err(|error| format!("无法访问 {}：{error}", path.display()))
}

/// 从当前项目文档中查找与规范化目录完全相同的已登记项目根目录。
fn registered_project_root_from_document(
    projects: &ProjectsDocument,
    canonical: &Path,
) -> Option<PathBuf> {
    projects.iter().find_map(|project| {
        let stored = fs::canonicalize(&project.path).ok()?;
        (stored.is_dir() && stored == canonical).then_some(stored)
    })
}

/// 返回所有已添加项目的规范化根目录。
fn registered_project_roots(app: &AppHandle) -> Result<Vec<PathBuf>, String> {
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let projects = load_projects_document(app)?;
    let mut roots = Vec::new();
    for project in projects {
        let Ok(canonical) = fs::canonicalize(&project.path) else {
            continue;
        };
        if canonical.is_dir() && !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
    Ok(roots)
}

/// 返回已添加且与调用参数完全匹配的项目根目录。
pub(crate) fn registered_project_root(
    app: &AppHandle,
    project_path: &str,
) -> Result<PathBuf, String> {
    let canonical = canonical_existing_dir(project_path)?;
    let _guard = projects_lock()
        .lock()
        .map_err(|_| "项目元数据锁已损坏".to_owned())?;
    let projects = load_projects_document(app)?;
    registered_project_root_from_document(&projects, &canonical)
        .ok_or_else(|| format!("项目尚未添加：{}", canonical.display()))
}

/// 返回无项目 Session 唯一允许使用的规范化应用数据目录。
pub(crate) fn app_data_session_root(app: &AppHandle) -> Result<PathBuf, String> {
    let data_dir =
        crate::storage::root_dir(app).map_err(|error| format!("无法确定应用数据目录：{error}"))?;
    fs::create_dir_all(&data_dir)
        .map_err(|error| format!("无法创建应用数据目录 {}：{error}", data_dir.display()))?;
    fs::canonicalize(&data_dir)
        .map_err(|error| format!("无法访问应用数据目录 {}：{error}", data_dir.display()))
}

/// 按当前唯一授权规则解析 Session 工作目录。
pub(crate) fn authorized_session_root(
    app: &AppHandle,
    project_path: Option<&str>,
) -> Result<PathBuf, String> {
    match project_path {
        Some(project_path) => registered_project_root(app, project_path),
        None => app_data_session_root(app),
    }
}

/// 规范化持久 Session 的工作目录，供加载前执行精确授权比对。
pub(crate) fn canonical_session_root(path: &str) -> Result<PathBuf, String> {
    canonical_existing_dir(path)
}

/// 返回绝对路径是否位于根目录内。
fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

/// 校验相对路径并移除无意义的当前目录片段。
fn validate_relative_path(relative: &str, allow_empty: bool) -> Result<PathBuf, String> {
    if relative.trim().is_empty() {
        return if allow_empty {
            Ok(PathBuf::new())
        } else {
            Err("相对路径不能为空".to_owned())
        };
    }
    let input = Path::new(relative);
    if input.is_absolute() {
        return Err("此操作只接受项目相对路径".to_owned());
    }
    let mut clean = PathBuf::new();
    for component in input.components() {
        match component {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("路径包含越界片段：{relative}"));
            }
        }
    }
    if clean.as_os_str().is_empty() && !allow_empty {
        return Err("相对路径不能为空".to_owned());
    }
    Ok(clean)
}

/// 解析并校验项目内已存在路径。
fn resolve_existing_under_root(
    root: &Path,
    relative: &str,
    allow_root: bool,
) -> Result<PathBuf, String> {
    let clean = validate_relative_path(relative, allow_root)?;
    let candidate = if clean.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(clean)
    };
    let canonical = fs::canonicalize(&candidate)
        .map_err(|error| format!("无法访问 {}：{error}", candidate.display()))?;
    if !path_is_within(&canonical, root) {
        return Err(format!("路径超出项目目录：{}", candidate.display()));
    }
    Ok(canonical)
}

/// 解析并校验项目内可写文件路径。
fn resolve_writable_under_root(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let clean = validate_relative_path(relative, false)?;
    let candidate = root.join(clean);
    if candidate.exists() {
        let canonical = fs::canonicalize(&candidate)
            .map_err(|error| format!("无法访问 {}：{error}", candidate.display()))?;
        if !path_is_within(&canonical, root) {
            return Err(format!("路径超出项目目录：{}", candidate.display()));
        }
        if canonical.is_dir() {
            return Err(format!("目标路径是目录：{}", canonical.display()));
        }
        return Ok(canonical);
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| format!("路径没有父目录：{}", candidate.display()))?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| format!("无法访问父目录 {}：{error}", parent.display()))?;
    if !path_is_within(&canonical_parent, root) {
        return Err(format!("路径超出项目目录：{}", candidate.display()));
    }
    let name = candidate
        .file_name()
        .ok_or_else(|| format!("目标缺少文件名：{}", candidate.display()))?;
    Ok(canonical_parent.join(name))
}

/// 返回应用数据目录和已添加项目组成的授权根目录。
fn authorized_roots(app: &AppHandle) -> Result<Vec<PathBuf>, String> {
    let mut roots = registered_project_roots(app)?;
    let data_dir =
        crate::storage::root_dir(app).map_err(|error| format!("无法确定应用数据目录：{error}"))?;
    fs::create_dir_all(&data_dir)
        .map_err(|error| format!("无法创建应用数据目录 {}：{error}", data_dir.display()))?;
    let data_dir = fs::canonicalize(&data_dir)
        .map_err(|error| format!("无法访问应用数据目录 {}：{error}", data_dir.display()))?;
    if !roots.contains(&data_dir) {
        roots.push(data_dir);
    }
    Ok(roots)
}

/// 授权并规范化现有绝对路径。
fn authorize_existing_absolute(app: &AppHandle, path: &Path) -> Result<PathBuf, String> {
    let canonical = canonical_existing_path(path)?;
    if authorized_roots(app)?
        .iter()
        .any(|root| path_is_within(&canonical, root))
    {
        Ok(canonical)
    } else {
        Err(format!("路径不属于已添加项目：{}", canonical.display()))
    }
}

/// 授权并规范化可写绝对文件路径。
fn authorize_writable_absolute(app: &AppHandle, path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("路径必须是绝对路径".to_owned());
    }
    let roots = authorized_roots(app)?;
    if path.exists() {
        let canonical = fs::canonicalize(path)
            .map_err(|error| format!("无法访问 {}：{error}", path.display()))?;
        if canonical.is_dir() {
            return Err(format!("目标路径是目录：{}", canonical.display()));
        }
        if roots.iter().any(|root| path_is_within(&canonical, root)) {
            return Ok(canonical);
        }
        return Err(format!("路径不属于已添加项目：{}", canonical.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("路径没有父目录：{}", path.display()))?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| format!("无法访问父目录 {}：{error}", parent.display()))?;
    if !roots
        .iter()
        .any(|root| path_is_within(&canonical_parent, root))
    {
        return Err(format!("路径不属于已添加项目：{}", path.display()));
    }
    let name = path
        .file_name()
        .ok_or_else(|| format!("目标缺少文件名：{}", path.display()))?;
    Ok(canonical_parent.join(name))
}

/// 将平台路径转换为前端统一的斜杠路径。
fn path_to_frontend(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// 将项目内路径转换为统一相对路径。
fn relative_to_frontend(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("路径不属于项目：{}", path.display()))?;
    Ok(path_to_frontend(relative))
}

/// 返回小写文件扩展名。
fn lowercase_extension(path: &Path) -> String {
    path.extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 列出已添加项目内的直接子项。
#[tauri::command]
pub async fn fs_list_dir(
    app: AppHandle,
    project_path: String,
    relative: Option<String>,
) -> Result<Vec<FsEntry>, String> {
    tauri::async_runtime::spawn_blocking(move || fs_list_dir_blocking(app, project_path, relative))
        .await
        .map_err(|error| format!("目录读取后台任务失败：{error}"))?
}

/// 在线程池中列出已添加项目内的直接子项。
fn fs_list_dir_blocking(
    app: AppHandle,
    project_path: String,
    relative: Option<String>,
) -> Result<Vec<FsEntry>, String> {
    let root = registered_project_root(&app, &project_path)?;
    let directory =
        resolve_existing_under_root(&root, relative.as_deref().unwrap_or_default(), true)?;
    if !directory.is_dir() {
        return Err(format!("目标不是目录：{}", directory.display()));
    }
    let mut entries = Vec::new();
    for item in fs::read_dir(&directory)
        .map_err(|error| format!("无法读取目录 {}：{error}", directory.display()))?
    {
        let item =
            item.map_err(|error| format!("无法读取目录项 {}：{error}", directory.display()))?;
        let item_path = item.path();
        let canonical = match fs::canonicalize(&item_path) {
            Ok(path) if path_is_within(&path, &root) => path,
            _ => continue,
        };
        let metadata = match fs::metadata(&canonical) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        let is_dir = metadata.is_dir();
        entries.push(FsEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            relative_path: relative_to_frontend(&root, &item_path)?,
            is_dir,
            size: if is_dir { 0 } else { metadata.len() },
            ext: if is_dir {
                String::new()
            } else {
                lowercase_extension(&item_path)
            },
        });
    }
    entries.sort_by(|left, right| match right.is_dir.cmp(&left.is_dir) {
        Ordering::Equal => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase()),
        other => other,
    });
    Ok(entries)
}

/// 读取已添加项目内的文件预览。
#[tauri::command]
pub fn fs_read_file(
    app: AppHandle,
    project_path: String,
    relative: String,
) -> Result<FsReadResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    let path = resolve_existing_under_root(&root, &relative, false)?;
    if !path.is_file() {
        return Err(format!("目标不是文件：{}", path.display()));
    }
    let relative_path = relative_to_frontend(&root, &path)?;
    read_file_preview(&path, relative_path)
}

/// 写入已添加项目内的 UTF-8 文件。
#[tauri::command]
pub fn fs_write_file(
    app: AppHandle,
    project_path: String,
    relative: String,
    content: String,
    expected_mtime_ms: Option<u64>,
) -> Result<FsWriteResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    let path = resolve_writable_under_root(&root, &relative)?;
    let relative_path = relative_to_frontend(&root, &path)?;
    write_text_file(&path, relative_path, &content, expected_mtime_ms)
}

/// 读取已添加项目或应用数据目录内的绝对文件。
#[tauri::command]
pub fn fs_read_absolute(app: AppHandle, path: String) -> Result<FsReadResult, String> {
    let path = authorize_existing_absolute(&app, Path::new(&path))?;
    if !path.is_file() {
        return Err(format!("目标不是文件：{}", path.display()));
    }
    read_file_preview(&path, path_to_frontend(&path))
}

/// 写入已添加项目或应用数据目录内的绝对 UTF-8 文件。
#[tauri::command]
pub fn fs_write_absolute(
    app: AppHandle,
    path: String,
    content: String,
    expected_mtime_ms: Option<u64>,
) -> Result<FsWriteResult, String> {
    let path = authorize_writable_absolute(&app, Path::new(&path))?;
    write_text_file(&path, path_to_frontend(&path), &content, expected_mtime_ms)
}

/// 智能解析绝对路径、项目相对路径或项目内后缀路径。
#[tauri::command]
pub fn fs_open_path(
    app: AppHandle,
    path: String,
    project_path: Option<String>,
) -> Result<FsReadResult, String> {
    let input = Path::new(path.trim());
    if input.as_os_str().is_empty() {
        return Err("文件路径不能为空".to_owned());
    }

    if input.is_absolute() {
        let resolved = if let Some(project_path) = project_path.as_deref() {
            let root = registered_project_root(&app, project_path)?;
            let canonical = fs::canonicalize(input)
                .map_err(|error| format!("无法访问 {}：{error}", input.display()))?;
            if !path_is_within(&canonical, &root) {
                return Err(format!("路径超出项目目录：{}", input.display()));
            }
            canonical
        } else {
            authorize_existing_absolute(&app, input)?
        };
        if !resolved.is_file() {
            return Err(format!("目标不是文件：{}", resolved.display()));
        }
        let relative_path = if let Some(project_path) = project_path.as_deref() {
            let root = registered_project_root(&app, project_path)?;
            relative_to_frontend(&root, &resolved)?
        } else {
            path_to_frontend(&resolved)
        };
        return read_file_preview(&resolved, relative_path);
    }

    let project_path = project_path.ok_or_else(|| "打开相对路径时必须提供项目目录".to_owned())?;
    let root = registered_project_root(&app, &project_path)?;
    let clean = validate_relative_path(path.trim(), false)?;
    let clean_text = path_to_frontend(&clean);
    let direct = resolve_existing_under_root(&root, &clean_text, false).ok();
    let resolved = match direct {
        Some(path) if path.is_file() => path,
        _ => find_file_by_suffix(&root, &clean)?
            .ok_or_else(|| format!("项目中找不到文件：{}", clean.display()))?,
    };
    let relative_path = relative_to_frontend(&root, &resolved)?;
    read_file_preview(&resolved, relative_path)
}

/// 在项目中执行有上限的文件后缀搜索。
fn find_file_by_suffix(root: &Path, suffix: &Path) -> Result<Option<PathBuf>, String> {
    let mut stack = vec![root.to_path_buf()];
    let mut visited = 0usize;
    let mut matches = Vec::new();
    let ignored: HashSet<&str> = [".git", "node_modules", "target"].into_iter().collect();
    while let Some(directory) = stack.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for item in entries {
            visited += 1;
            if visited > MAX_SUFFIX_SEARCH_ENTRIES {
                break;
            }
            let Ok(item) = item else {
                continue;
            };
            let name = item.file_name();
            let name_text = name.to_string_lossy();
            let Ok(file_type) = item.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = item.path();
            if file_type.is_dir() {
                if !ignored.contains(name_text.as_ref()) {
                    stack.push(path);
                }
                continue;
            }
            if file_type.is_file() {
                let Ok(relative) = path.strip_prefix(root) else {
                    continue;
                };
                if relative.ends_with(suffix) {
                    matches.push(path);
                }
            }
        }
        if visited > MAX_SUFFIX_SEARCH_ENTRIES {
            break;
        }
    }
    matches.sort_by(|left, right| {
        let left_depth = left.components().count();
        let right_depth = right.components().count();
        left_depth
            .cmp(&right_depth)
            .then_with(|| path_to_frontend(left).cmp(&path_to_frontend(right)))
    });
    Ok(matches.into_iter().next())
}

/// 返回扩展名对应的预览类型、MIME 和文本标记。
fn preview_classification(path: &Path) -> (&'static str, &'static str, bool) {
    match lowercase_extension(path).as_str() {
        "md" | "mdx" | "markdown" => ("markdown", "text/markdown", true),
        "json" | "jsonl" | "geojson" => ("json", "application/json", true),
        "html" | "htm" => ("html", "text/html", true),
        "css" | "scss" | "sass" | "less" => ("css", "text/css", true),
        "csv" | "tsv" => ("csv", "text/csv", true),
        "txt" | "log" | "text" => ("text", "text/plain", true),
        "toml" | "yaml" | "yml" | "ini" | "conf" | "env" | "properties" => {
            ("config", "text/plain", true)
        }
        "rs" | "go" | "c" | "h" | "cc" | "cpp" | "hpp" | "java" | "kt" | "kts" | "swift" | "m"
        | "mm" | "py" | "rb" | "php" | "js" | "jsx" | "ts" | "tsx" | "vue" | "svelte" | "sql"
        | "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "xml" | "gradle" | "lua"
        | "dart" | "ex" | "exs" | "erl" | "hrl" | "clj" | "cljs" | "scala" | "groovy" | "r" => {
            ("code", "text/plain", true)
        }
        "png" => ("image", "image/png", false),
        "jpg" | "jpeg" => ("image", "image/jpeg", false),
        "gif" => ("image", "image/gif", false),
        "webp" => ("image", "image/webp", false),
        "bmp" => ("image", "image/bmp", false),
        "ico" => ("image", "image/x-icon", false),
        "svg" => ("image", "image/svg+xml", false),
        "avif" => ("image", "image/avif", false),
        "mp4" | "m4v" => ("video", "video/mp4", false),
        "webm" => ("video", "video/webm", false),
        "mov" => ("video", "video/quicktime", false),
        "mkv" => ("video", "video/x-matroska", false),
        "avi" => ("video", "video/x-msvideo", false),
        "mp3" => ("audio", "audio/mpeg", false),
        "wav" => ("audio", "audio/wav", false),
        "ogg" | "oga" => ("audio", "audio/ogg", false),
        "m4a" => ("audio", "audio/mp4", false),
        "flac" => ("audio", "audio/flac", false),
        "docx" => (
            "docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            false,
        ),
        "xlsx" | "xlsm" => (
            "xlsx",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            false,
        ),
        "pptx" => (
            "pptx",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            false,
        ),
        "odt" | "ods" | "odp" => ("odf", "application/vnd.oasis.opendocument", false),
        _ => ("binary", "application/octet-stream", false),
    }
}

/// 读取有上限的文件预览并避免二进制 Base64 膨胀。
fn read_file_preview(path: &Path, relative_path: String) -> Result<FsReadResult, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("目标不是文件：{}", path.display()));
    }
    let (mut kind, mut mime, classified_text) = preview_classification(path);
    let mut text = None;
    let mut truncated = false;
    let mut stream = !classified_text;

    if classified_text || kind == "binary" {
        let mut file =
            File::open(path).map_err(|error| format!("无法打开 {}：{error}", path.display()))?;
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(MAX_TEXT_PREVIEW_BYTES)
                .min(MAX_TEXT_PREVIEW_BYTES + 1),
        );
        Read::by_ref(&mut file)
            .take((MAX_TEXT_PREVIEW_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
        let looks_text = classified_text
            || (!bytes.iter().take(8192).any(|byte| *byte == 0)
                && std::str::from_utf8(&bytes).is_ok());
        if looks_text {
            if bytes.len() > MAX_TEXT_PREVIEW_BYTES {
                bytes.truncate(MAX_TEXT_PREVIEW_BYTES);
                truncated = true;
            }
            text = Some(String::from_utf8_lossy(&bytes).into_owned());
            stream = false;
            if kind == "binary" {
                kind = "text";
                mime = "text/plain";
            }
        }
    }

    Ok(FsReadResult {
        relative_path,
        name: path
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned()),
        absolute_path: path_to_frontend(path),
        size: metadata.len(),
        kind: kind.to_owned(),
        mime: mime.to_owned(),
        text,
        base64: None,
        stream,
        truncated,
        error: None,
        mtime_ms: modified_millis(&metadata)?,
    })
}

/// 读取文件最后修改时间的 Unix 毫秒值。
fn modified_millis(metadata: &fs::Metadata) -> Result<u64, String> {
    let modified = metadata
        .modified()
        .map_err(|error| format!("无法读取文件修改时间：{error}"))?;
    let millis = modified
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("文件修改时间早于 Unix 纪元：{error}"))?
        .as_millis();
    u64::try_from(millis).map_err(|_| "文件修改时间超出支持范围".to_owned())
}

/// 检查预期修改时间并原子写入 UTF-8 文本。
fn write_text_file(
    path: &Path,
    relative_path: String,
    content: &str,
    expected_mtime_ms: Option<u64>,
) -> Result<FsWriteResult, String> {
    match (expected_mtime_ms, fs::metadata(path)) {
        (Some(expected), Ok(metadata)) => {
            let actual = modified_millis(&metadata)?;
            if actual != expected {
                return Err(format!(
                    "CONFLICT: 文件已在磁盘上发生变化（预期 {expected}，实际 {actual}）"
                ));
            }
        }
        (Some(_), Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("CONFLICT: 文件已被删除".to_owned());
        }
        (Some(_), Err(error)) => {
            return Err(format!("无法检查文件状态 {}：{error}", path.display()));
        }
        (None, _) => {}
    }
    atomic_write_bytes(path, content.as_bytes())?;
    let metadata = fs::metadata(path)
        .map_err(|error| format!("无法读取写入结果 {}：{error}", path.display()))?;
    Ok(FsWriteResult {
        relative_path,
        absolute_path: path_to_frontend(path),
        size: metadata.len(),
        mtime_ms: modified_millis(&metadata)?,
    })
}

/// 创建不会在 Windows 桌面环境中弹出控制台窗口的 Git 命令。
fn git_command() -> Command {
    let command = Command::new("git");
    #[cfg(windows)]
    let command = {
        use std::os::windows::process::CommandExt;

        let mut command = command;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    command
}

/// 执行带项目工作目录的 Git 命令。
fn run_git(root: &Path, args: &[&str]) -> Result<Output, String> {
    git_command()
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| format!("无法执行 git：{error}"))
}

/// 执行包含路径参数的 Git 命令。
fn run_git_with_path(
    root: &Path,
    leading_args: &[&str],
    path: &Path,
    trailing_args: &[&str],
) -> Result<Output, String> {
    git_command()
        .arg("-C")
        .arg(root)
        .args(leading_args)
        .arg(path)
        .args(trailing_args)
        .output()
        .map_err(|error| format!("无法执行 git：{error}"))
}

/// 判断 Git 路径是否已经被索引跟踪。
fn git_path_is_tracked(root: &Path, path: &Path) -> Result<bool, String> {
    let output = run_git_with_path(root, &["ls-files", "--error-unmatch", "--"], path, &[])?;
    Ok(output.status.success())
}

/// 返回当前平台可传给 Git no-index 的空文件路径。
fn empty_diff_path() -> &'static str {
    #[cfg(windows)]
    {
        "NUL"
    }
    #[cfg(not(windows))]
    {
        "/dev/null"
    }
}

/// 为未跟踪文件生成“新增文件”形式的统一 diff。
fn run_git_new_file_diff(root: &Path, path: &Path) -> Result<Output, String> {
    git_command()
        .arg("-C")
        .arg(root)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-index",
            "--",
            empty_diff_path(),
        ])
        .arg(root.join(path))
        .output()
        .map_err(|error| format!("无法执行 git：{error}"))
}

/// Git no-index 发现差异时返回 1，也应视为有效 diff。
fn git_diff_status_ok(output: &Output) -> bool {
    output.status.success() || output.status.code() == Some(1)
}

/// 将 Git 失败输出转换为可读原因。
fn git_failure_reason(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        stderr
    } else {
        format!("git 命令失败，退出码 {:?}", output.status.code())
    }
}

/// 合并 Git 命令的标准输出与标准错误。
fn combined_git_output(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stdout.trim_end(), stderr.trim_end()),
        (false, true) => stdout.trim().to_owned(),
        (true, false) => stderr.trim().to_owned(),
        (true, true) => String::new(),
    }
}

/// 返回当前分支短名称；游离 HEAD 或不可用时为 None。
fn git_current_branch(root: &Path) -> Option<String> {
    match run_git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(output) if output.status.success() => {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            (!branch.is_empty()).then_some(branch)
        }
        _ => None,
    }
}

/// 检查 Git 仓库是否可用。
fn git_repository_reason(root: &Path) -> Option<String> {
    match run_git(root, &["rev-parse", "--is-inside-work-tree"]) {
        Ok(output)
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "true" =>
        {
            None
        }
        Ok(output) => Some(git_failure_reason(&output)),
        Err(error) => Some(error),
    }
}

/// 解析 `git worktree list --porcelain`。
fn parse_worktree_porcelain(raw: &str) -> Vec<GitWorktreeEntry> {
    let normalized = raw.replace("\r\n", "\n");
    normalized
        .split("\n\n")
        .filter_map(|block| {
            let mut path = None;
            let mut head = None;
            let mut branch = None;
            let mut detached = false;
            let mut locked = false;
            let mut prunable = false;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("worktree ") {
                    path = Some(value.trim().to_owned());
                } else if let Some(value) = line.strip_prefix("HEAD ") {
                    head = Some(value.trim().to_owned());
                } else if let Some(value) = line.strip_prefix("branch ") {
                    let value = value.trim();
                    branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_owned(),
                    );
                } else if line == "detached" {
                    detached = true;
                } else if line.starts_with("locked") {
                    locked = true;
                } else if line.starts_with("prunable") {
                    prunable = true;
                }
            }
            path.map(|path| GitWorktreeEntry {
                path: path.replace('\\', "/"),
                head,
                branch: if detached { None } else { branch },
                detached,
                is_main: false,
                locked,
                prunable,
            })
        })
        .enumerate()
        .map(|(index, mut entry)| {
            entry.is_main = index == 0;
            entry
        })
        .collect()
}

/// 返回项目关联的 Git worktree 列表。
#[tauri::command]
pub fn git_worktrees_list(
    app: AppHandle,
    project_path: String,
) -> Result<GitWorktreesResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Ok(GitWorktreesResult {
            available: false,
            worktrees: Vec::new(),
            reason: Some(reason),
        });
    }
    let output = run_git(&root, &["worktree", "list", "--porcelain"])?;
    if !output.status.success() {
        return Ok(GitWorktreesResult {
            available: false,
            worktrees: Vec::new(),
            reason: Some(git_failure_reason(&output)),
        });
    }
    Ok(GitWorktreesResult {
        available: true,
        worktrees: parse_worktree_porcelain(&String::from_utf8_lossy(&output.stdout)),
        reason: None,
    })
}

/// 校验新 worktree 名称。
fn validate_worktree_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Worktree 名称不能为空".to_owned());
    }
    if name == "." || name == ".." || name.starts_with('-') || name.chars().count() > 64 {
        return Err("Worktree 名称无效".to_owned());
    }
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return Err("Worktree 名称只能包含字母、数字、点、下划线和连字符".to_owned());
    }
    Ok(name.to_owned())
}

/// 校验可选 Git 起点参数。
fn validate_start_point(start_point: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = start_point else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.starts_with('-')
        || value.len() > 256
        || value.contains('\0')
        || value.contains('\r')
        || value.contains('\n')
    {
        return Err("Git 起点参数无效".to_owned());
    }
    Ok(Some(value.to_owned()))
}

/// 创建主工作树同级目录中的新 Git worktree。
#[tauri::command]
pub fn git_worktree_add(
    app: AppHandle,
    project_path: String,
    name: String,
    start_point: Option<String>,
) -> Result<GitWorktreeAddResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Err(reason);
    }
    let safe_name = validate_worktree_name(&name)?;
    let start_point = validate_start_point(start_point)?;
    let list_output = run_git(&root, &["worktree", "list", "--porcelain"])?;
    if !list_output.status.success() {
        return Err(git_failure_reason(&list_output));
    }
    let worktrees = parse_worktree_porcelain(&String::from_utf8_lossy(&list_output.stdout));
    let main_path = worktrees
        .first()
        .map(|entry| PathBuf::from(&entry.path))
        .ok_or_else(|| "Git 未返回主工作树".to_owned())?;
    let main_path = canonical_existing_dir(&main_path.to_string_lossy())?;
    let parent = main_path
        .parent()
        .ok_or_else(|| format!("主工作树没有父目录：{}", main_path.display()))?;
    let base = main_path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| format!("无法确定主工作树名称：{}", main_path.display()))?;
    let target = parent.join(format!("{base}-{safe_name}"));
    if target.exists() {
        return Err(format!("Worktree 目录已存在：{}", target.display()));
    }

    let mut command = git_command();
    command
        .arg("-C")
        .arg(&root)
        .args(["worktree", "add", "-b"])
        .arg(&safe_name)
        .arg("--")
        .arg(&target);
    if let Some(start_point) = start_point.as_deref() {
        command.arg(start_point);
    }
    let output = command
        .output()
        .map_err(|error| format!("无法执行 git worktree add：{error}"))?;
    if !output.status.success() {
        return Err(git_failure_reason(&output));
    }
    let canonical_target = fs::canonicalize(&target)
        .map_err(|error| format!("无法访问新 Worktree {}：{error}", target.display()))?;
    Ok(GitWorktreeAddResult {
        path: path_to_frontend(&canonical_target),
        name: safe_name.clone(),
        start_point,
        branch: Some(safe_name),
    })
}

/// 校验 `git worktree prune --expire` 参数。
fn validate_worktree_expire(expire: Option<String>) -> Result<Option<String>, String> {
    let Some(expire) = expire else {
        return Ok(None);
    };
    let expire = expire.trim();
    if expire.is_empty() {
        return Ok(None);
    }
    if expire.starts_with('-')
        || expire.len() > 64
        || !expire
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._".contains(character))
    {
        return Err("Worktree 过期参数无效".to_owned());
    }
    Ok(Some(expire.to_owned()))
}

/// 统计 Git prune 输出中的清理行。
fn count_prune_lines(output: &str) -> usize {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("remov") || lower.contains("prun") || lower.starts_with("would ")
        })
        .count()
}

/// 预览或执行 Git worktree 元数据清理。
#[tauri::command]
pub fn git_worktree_gc(
    app: AppHandle,
    project_path: String,
    dry_run: bool,
    force: bool,
    expire: Option<String>,
) -> Result<GitWorktreeGcResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Err(reason);
    }
    let expire = validate_worktree_expire(expire)?
        .or_else(|| if force { Some("now".to_owned()) } else { None });
    let list_output = run_git(&root, &["worktree", "list", "--porcelain"])?;
    if !list_output.status.success() {
        return Err(git_failure_reason(&list_output));
    }
    let prunable: Vec<String> =
        parse_worktree_porcelain(&String::from_utf8_lossy(&list_output.stdout))
            .into_iter()
            .filter(|entry| entry.prunable)
            .map(|entry| entry.path)
            .collect();

    let mut command = git_command();
    command
        .arg("-C")
        .arg(&root)
        .args(["worktree", "prune", "-v"]);
    if dry_run {
        command.arg("--dry-run");
    }
    if let Some(expire) = expire.as_deref() {
        command.arg("--expire").arg(expire);
    }
    let command_output = command
        .output()
        .map_err(|error| format!("无法执行 git worktree prune：{error}"))?;
    if !command_output.status.success() {
        return Err(git_failure_reason(&command_output));
    }
    let stdout = String::from_utf8_lossy(&command_output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&command_output.stderr).into_owned();
    let output = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stdout.trim_end(), stderr.trim_end()),
        (false, true) => stdout.clone(),
        (true, false) => stderr.clone(),
        (true, true) => String::new(),
    };
    let pruned_count = count_prune_lines(&output).max(if dry_run { prunable.len() } else { 0 });
    Ok(GitWorktreeGcResult {
        dry_run,
        force,
        pruned_count,
        prunable,
        stdout,
        stderr,
        output,
    })
}

/// 返回 Git porcelain 状态对应的前端类别。
fn git_status_kind(index: u8, worktree: u8) -> &'static str {
    if index == b'?' && worktree == b'?' {
        return "untracked";
    }
    if index == b'!' && worktree == b'!' {
        return "ignored";
    }
    if index == b'U'
        || worktree == b'U'
        || (index == b'A' && worktree == b'A')
        || (index == b'D' && worktree == b'D')
    {
        return "conflict";
    }
    for status in [worktree, index] {
        match status {
            b'R' => return "renamed",
            b'C' => return "copied",
            b'A' => return "added",
            b'D' => return "deleted",
            b'T' => return "typechange",
            b'M' => return "modified",
            _ => {}
        }
    }
    if index != b' ' || worktree != b' ' {
        "modified"
    } else {
        "unknown"
    }
}

/// 将 Git 返回的相对路径校验为项目内路径。
fn validated_git_relative(raw: &str) -> Result<PathBuf, String> {
    validate_relative_path(raw, false)
}

/// 解析 `git diff --numstat` 输出，统计新增与删除行数；二进制行自动跳过。
fn parse_numstat(bytes: &[u8]) -> (u64, u64) {
    let mut additions = 0u64;
    let mut deletions = 0u64;
    for line in String::from_utf8_lossy(bytes).lines() {
        let mut fields = line.split('\t');
        if let Ok(value) = fields.next().unwrap_or("0").parse::<u64>() {
            additions += value;
        }
        if let Ok(value) = fields.next().unwrap_or("0").parse::<u64>() {
            deletions += value;
        }
    }
    (additions, deletions)
}

/// 解析 NUL 分隔的 Git porcelain v1 状态。
fn parse_git_status(root: &Path, bytes: &[u8]) -> Result<Vec<GitStatusEntry>, String> {
    let fields: Vec<&[u8]> = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect();
    let mut entries = Vec::new();
    let mut index = 0usize;
    while index < fields.len() {
        let field = fields[index];
        if field.len() < 4 || field[2] != b' ' {
            index += 1;
            continue;
        }
        let index_status = field[0];
        let worktree_status = field[1];
        let path_text = String::from_utf8_lossy(&field[3..]).into_owned();
        let is_directory =
            index_status == b'?' && worktree_status == b'?' && path_text.ends_with('/');
        let relative = validated_git_relative(&path_text)?;
        let renamed_or_copied =
            matches!(index_status, b'R' | b'C') || matches!(worktree_status, b'R' | b'C');
        let original_path = if renamed_or_copied && index + 1 < fields.len() {
            index += 1;
            let original = String::from_utf8_lossy(fields[index]).into_owned();
            Some(path_to_frontend(&validated_git_relative(&original)?))
        } else {
            None
        };
        let relative_text = path_to_frontend(&relative);
        let absolute = root.join(&relative);
        let is_nested_repository = is_directory && absolute.join(".git").exists();
        entries.push(GitStatusEntry {
            path: relative_text,
            absolute_path: path_to_frontend(&absolute),
            status: String::from_utf8_lossy(&[index_status, worktree_status]).into_owned(),
            index_status: char::from(index_status).to_string(),
            worktree_status: char::from(worktree_status).to_string(),
            kind: git_status_kind(index_status, worktree_status).to_owned(),
            name: relative
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| path_text.clone()),
            is_directory,
            is_nested_repository,
            original_path,
        });
        index += 1;
    }
    Ok(entries)
}

/// 按需列出一个未跟踪目录内由 Git 确认的文件，并排除 ignored 内容。
#[tauri::command]
pub async fn git_untracked_directory(
    app: AppHandle,
    project_path: String,
    path: String,
) -> Result<GitUntrackedDirectoryResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        git_untracked_directory_blocking(app, project_path, path)
    })
    .await
    .map_err(|error| format!("未跟踪目录读取后台任务失败：{error}"))?
}

/// 在线程池中按需读取未跟踪目录，避免 normal 状态查询扫描全部文件。
fn git_untracked_directory_blocking(
    app: AppHandle,
    project_path: String,
    path: String,
) -> Result<GitUntrackedDirectoryResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Err(reason);
    }
    let relative = resolve_git_relative(&root, &path)?;
    let directory = root.join(&relative);
    if !directory.is_dir() {
        return Err(format!("目标不是目录：{}", directory.display()));
    }
    if directory.join(".git").exists() {
        return Ok(GitUntrackedDirectoryResult {
            files: Vec::new(),
            truncated: false,
        });
    }
    let output = run_git_with_path(
        &root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
        ],
        &relative,
        &[],
    )?;
    if !output.status.success() {
        return Err(git_failure_reason(&output));
    }
    if output.stdout.len() > MAX_GIT_TEXT_BYTES {
        return Err("未跟踪目录状态超过 8 MB 读取上限".to_owned());
    }
    let mut files: Vec<GitStatusEntry> = parse_git_status(&root, &output.stdout)?
        .into_iter()
        .filter(|entry| entry.kind == "untracked")
        .collect();
    let truncated = files.len() > MAX_UNTRACKED_DIRECTORY_ENTRIES;
    files.truncate(MAX_UNTRACKED_DIRECTORY_ENTRIES);
    Ok(GitUntrackedDirectoryResult { files, truncated })
}

/// 返回项目 Git 工作区状态。
#[tauri::command]
pub async fn git_status(app: AppHandle, project_path: String) -> Result<GitStatusResult, String> {
    tauri::async_runtime::spawn_blocking(move || git_status_blocking(app, project_path))
        .await
        .map_err(|error| format!("Git 状态后台任务失败：{error}"))?
}

/// 在线程池中读取项目 Git 工作区状态。
fn git_status_blocking(app: AppHandle, project_path: String) -> Result<GitStatusResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Ok(GitStatusResult {
            available: false,
            files: Vec::new(),
            branch: None,
            additions: 0,
            deletions: 0,
            has_unstaged_changes: false,
            reason: Some(reason),
        });
    }
    let output = run_git(
        &root,
        // 只列未跟踪目录，避免扫描全部未跟踪文件。
        &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
    )?;
    if !output.status.success() {
        return Ok(GitStatusResult {
            available: false,
            files: Vec::new(),
            branch: None,
            additions: 0,
            deletions: 0,
            has_unstaged_changes: false,
            reason: Some(git_failure_reason(&output)),
        });
    }
    let branch_output = run_git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let branch = if branch_output.status.success() {
        let branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_owned();
        (!branch.is_empty()).then_some(branch)
    } else {
        None
    };
    let files = parse_git_status(&root, &output.stdout)?;
    let has_unstaged_changes = files
        .iter()
        .any(|entry| entry.worktree_status != " " && entry.worktree_status != "?");
    let mut additions = 0u64;
    let mut deletions = 0u64;
    for args in [
        &["diff", "--numstat"][..],
        &["diff", "--cached", "--numstat"][..],
    ] {
        if let Ok(numstat_output) = run_git(&root, args)
            && numstat_output.status.success()
        {
            let (added, deleted) = parse_numstat(&numstat_output.stdout);
            additions += added;
            deletions += deleted;
        }
    }
    Ok(GitStatusResult {
        available: true,
        files,
        branch,
        additions,
        deletions,
        has_unstaged_changes,
        reason: None,
    })
}

/// 提交项目的暂存改动；include_unstaged 为真时先暂存全部未暂存改动。
#[tauri::command]
pub fn git_commit(
    app: AppHandle,
    project_path: String,
    message: String,
    include_unstaged: bool,
) -> Result<GitCommitResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Err(reason);
    }
    let message = message.trim();
    if message.is_empty() {
        return Err("提交消息不能为空".to_owned());
    }
    if include_unstaged {
        let add_output = run_git(&root, &["add", "-A"])?;
        if !add_output.status.success() {
            return Err(git_failure_reason(&add_output));
        }
    }
    let mut command = git_command();
    command
        .arg("-C")
        .arg(&root)
        .arg("commit")
        .arg("-m")
        .arg(message);
    let output = command
        .output()
        .map_err(|error| format!("无法执行 git commit：{error}"))?;
    if !output.status.success() {
        return Err(git_failure_reason(&output));
    }
    let head_output = run_git(&root, &["rev-parse", "--short", "HEAD"])?;
    let commit = if head_output.status.success() {
        String::from_utf8_lossy(&head_output.stdout)
            .trim()
            .to_owned()
    } else {
        return Err(git_failure_reason(&head_output));
    };
    Ok(GitCommitResult {
        commit,
        branch: git_current_branch(&root),
        output: combined_git_output(&output),
    })
}

/// 推送当前分支到已配置的远端。
#[tauri::command]
pub fn git_push(app: AppHandle, project_path: String) -> Result<GitPushResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Err(reason);
    }
    let output = run_git(&root, &["push"])?;
    if !output.status.success() {
        return Err(git_failure_reason(&output));
    }
    Ok(GitPushResult {
        branch: git_current_branch(&root),
        output: combined_git_output(&output),
    })
}

/// 将绝对或相对文件参数转换为安全的 Git 相对路径。
fn resolve_git_relative(root: &Path, path: &str) -> Result<PathBuf, String> {
    let input = Path::new(path);
    if input.is_absolute() {
        if input.exists() {
            let canonical = fs::canonicalize(input)
                .map_err(|error| format!("无法访问 {}：{error}", input.display()))?;
            if !path_is_within(&canonical, root) {
                return Err(format!("路径超出项目目录：{}", input.display()));
            }
            return canonical
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .map_err(|_| format!("路径超出项目目录：{}", input.display()));
        }
        let relative = input
            .strip_prefix(root)
            .map_err(|_| format!("路径超出项目目录：{}", input.display()))?;
        return validated_git_relative(&path_to_frontend(relative));
    }
    validated_git_relative(path)
}

/// 返回项目文件相对 HEAD 的统一 diff。
#[tauri::command]
pub async fn git_file_diff(
    app: AppHandle,
    project_path: String,
    path: String,
) -> Result<GitFileDiffResult, String> {
    tauri::async_runtime::spawn_blocking(move || git_file_diff_blocking(app, project_path, path))
        .await
        .map_err(|error| format!("Git 文件差异后台任务失败：{error}"))?
}

/// 在线程池中读取项目文件相对 HEAD 的统一 diff。
fn git_file_diff_blocking(
    app: AppHandle,
    project_path: String,
    path: String,
) -> Result<GitFileDiffResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Ok(GitFileDiffResult {
            available: false,
            diff: None,
            relative_path: None,
            reason: Some(reason),
        });
    }
    let relative = resolve_git_relative(&root, &path)?;
    let relative_text = path_to_frontend(&relative);
    let mut output = run_git_with_path(
        &root,
        &["diff", "--no-ext-diff", "--no-color", "HEAD", "--"],
        &relative,
        &[],
    )?;
    if !output.status.success() {
        output = run_git_with_path(
            &root,
            &["diff", "--no-ext-diff", "--no-color", "--"],
            &relative,
            &[],
        )?;
    }
    if output.status.success()
        && output.stdout.is_empty()
        && !git_path_is_tracked(&root, &relative)?
    {
        output = run_git_new_file_diff(&root, &relative)?;
    }
    if !git_diff_status_ok(&output) {
        return Ok(GitFileDiffResult {
            available: true,
            diff: None,
            relative_path: Some(relative_text),
            reason: Some(git_failure_reason(&output)),
        });
    }
    if output.stdout.len() > MAX_GIT_TEXT_BYTES {
        return Ok(GitFileDiffResult {
            available: true,
            diff: None,
            relative_path: Some(relative_text),
            reason: Some("Git diff 超过 8 MB 预览上限".to_owned()),
        });
    }
    let diff = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(GitFileDiffResult {
        available: true,
        diff: (!diff.is_empty()).then_some(diff),
        relative_path: Some(relative_text),
        reason: None,
    })
}

/// 返回项目文件在 HEAD 中的 UTF-8 内容。
#[tauri::command]
pub async fn git_show_file(
    app: AppHandle,
    project_path: String,
    path: String,
) -> Result<GitShowFileResult, String> {
    tauri::async_runtime::spawn_blocking(move || git_show_file_blocking(app, project_path, path))
        .await
        .map_err(|error| format!("Git 文件读取后台任务失败：{error}"))?
}

/// 在线程池中读取项目文件在 HEAD 中的 UTF-8 内容。
fn git_show_file_blocking(
    app: AppHandle,
    project_path: String,
    path: String,
) -> Result<GitShowFileResult, String> {
    let root = registered_project_root(&app, &project_path)?;
    if let Some(reason) = git_repository_reason(&root) {
        return Ok(GitShowFileResult {
            available: false,
            content: None,
            relative_path: None,
            reason: Some(reason),
        });
    }
    let relative = resolve_git_relative(&root, &path)?;
    let relative_text = path_to_frontend(&relative);
    let object = format!("HEAD:{relative_text}");
    let output = run_git(&root, &["show", &object])?;
    if !output.status.success() {
        return Ok(GitShowFileResult {
            available: true,
            content: None,
            relative_path: Some(relative_text),
            reason: Some(git_failure_reason(&output)),
        });
    }
    if output.stdout.len() > MAX_GIT_TEXT_BYTES {
        return Ok(GitShowFileResult {
            available: true,
            content: None,
            relative_path: Some(relative_text),
            reason: Some("HEAD 文件超过 8 MB 预览上限".to_owned()),
        });
    }
    match String::from_utf8(output.stdout) {
        Ok(content) => Ok(GitShowFileResult {
            available: true,
            content: Some(content),
            relative_path: Some(relative_text),
            reason: None,
        }),
        Err(_) => Ok(GitShowFileResult {
            available: true,
            content: None,
            relative_path: Some(relative_text),
            reason: Some("HEAD 文件不是 UTF-8 文本".to_owned()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 项目配置只接受当前定义的字段。
    #[test]
    fn stored_project_record_rejects_unknown_and_derived_fields() {
        let path = std::env::temp_dir().to_string_lossy().into_owned();
        let value = serde_json::json!({
            "id": "project-current",
            "name": "Current",
            "path": path,
            "pathOk": true
        });
        let result = serde_json::from_value::<StoredProjectRecord>(value);

        assert!(result.is_err());
    }

    /// 创建与重命名共用同一套项目名称边界。
    #[test]
    fn project_name_normalization_trims_and_rejects_invalid_values() {
        assert_eq!(normalize_project_name("  KeenCode  ").unwrap(), "KeenCode");
        assert!(normalize_project_name("   ").is_err());
        assert!(normalize_project_name(&"项".repeat(121)).is_err());
    }

    /// 项目配置不得静默接受重复标识或路径。
    #[test]
    fn project_document_rejects_duplicate_records() {
        let path = std::env::temp_dir().to_string_lossy().into_owned();
        let document = vec![
            StoredProjectRecord {
                id: "project-current".to_owned(),
                name: "Current".to_owned(),
                path: path.clone(),
            },
            StoredProjectRecord {
                id: "project-current".to_owned(),
                name: "Duplicate".to_owned(),
                path,
            },
        ];

        assert!(validate_projects_document(&document).is_err());
    }

    /// Session 项目授权只接受规范化后与登记根目录完全一致的目录，不接受其子目录。
    #[test]
    fn session_project_root_requires_exact_registered_directory() {
        let base = std::env::temp_dir().join(format!(
            "keencode-session-project-auth-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let root = base.join("project");
        let child = root.join("nested");
        fs::create_dir_all(&child).expect("create project directories");
        let canonical_root = fs::canonicalize(&root).expect("canonicalize project root");
        let canonical_child = fs::canonicalize(&child).expect("canonicalize child directory");
        let document = vec![StoredProjectRecord {
            id: "project-current".to_owned(),
            name: "Current".to_owned(),
            path: canonical_root.to_string_lossy().into_owned(),
        }];

        assert_eq!(
            registered_project_root_from_document(&document, &canonical_root),
            Some(canonical_root.clone())
        );
        assert_eq!(
            registered_project_root_from_document(&document, &canonical_child),
            None
        );
        assert_eq!(
            canonical_session_root(&format!("{}/.", canonical_root.display())).unwrap(),
            canonical_root
        );

        fs::remove_dir_all(&base).expect("remove project authorization fixture");
    }

    /// 相对路径校验必须拒绝父目录越界。
    #[test]
    fn relative_path_rejects_parent_traversal() {
        assert!(validate_relative_path("../secret", false).is_err());
        assert!(validate_relative_path("src/../../secret", false).is_err());
        assert_eq!(
            validate_relative_path("./src/lib.rs", false).unwrap(),
            PathBuf::from("src/lib.rs")
        );
    }

    /// 外部链接只允许交给系统浏览器处理 HTTP 和 HTTPS。
    #[test]
    fn external_url_rejects_non_web_schemes() {
        assert_eq!(
            validate_external_url(" https://example.com/a?q=1 ").unwrap(),
            "https://example.com/a?q=1"
        );
        assert!(validate_external_url("file:///tmp/secret").is_err());
        assert!(validate_external_url("javascript:alert(1)").is_err());
    }

    /// PDF 未纳入首版预览类型，统一按普通二进制文件处理。
    #[test]
    fn pdf_uses_generic_binary_classification() {
        assert_eq!(
            preview_classification(Path::new("manual.pdf")),
            ("binary", "application/octet-stream", false)
        );
    }

    /// Worktree porcelain 解析必须保留主树和状态字段。
    #[test]
    fn worktree_porcelain_parses_entries() {
        let raw = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /repo-feat\nHEAD def\ndetached\nlocked reason\nprunable stale\n";
        let entries = parse_worktree_porcelain(raw);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].is_main);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert!(!entries[1].is_main);
        assert!(entries[1].detached);
        assert!(entries[1].locked);
        assert!(entries[1].prunable);
    }

    /// 所有 Git 子进程都必须复用 Windows 隐藏窗口命令构造器。
    #[test]
    fn git_processes_use_hidden_window_command_builder() {
        let source = include_str!("workspace.rs");
        let raw_git_command = ["Command::new(", "\"git\"", ")"].concat();
        let helper = source
            .split("fn git_command()")
            .nth(1)
            .and_then(|rest| rest.split("fn run_git(").next())
            .expect("git command helper");

        assert_eq!(source.matches(&raw_git_command).count(), 1);
        assert!(helper.contains("creation_flags(CREATE_NO_WINDOW)"));
        assert!(source.contains("const CREATE_NO_WINDOW: u32 = 0x0800_0000;"));
    }

    /// 目录和 Git 读取命令必须转移到 blocking 线程池，避免阻塞窗口事件处理。
    #[test]
    fn slow_workspace_reads_run_in_blocking_pool() {
        let source = include_str!("workspace.rs");
        for command in [
            "fs_list_dir",
            "git_status",
            "git_untracked_directory",
            "git_file_diff",
            "git_show_file",
        ] {
            let signature = format!("pub async fn {command}(");
            let helper_signature = format!("fn {command}_blocking(");
            let wrapper = source
                .split(&signature)
                .nth(1)
                .and_then(|rest| rest.split(&helper_signature).next())
                .unwrap_or_else(|| panic!("missing async wrapper for {command}"));

            assert!(wrapper.contains("tauri::async_runtime::spawn_blocking"));
            assert!(source.contains(&helper_signature));
        }
    }

    /// 变更面板只列未跟踪目录，避免扫描全部未跟踪文件。
    #[test]
    fn git_status_lists_untracked_directories() {
        let source = include_str!("workspace.rs");
        let command = source
            .split("fn git_status_blocking(")
            .nth(1)
            .and_then(|rest| rest.split("pub fn git_commit(").next())
            .expect("git_status command");
        assert!(command.contains("--untracked-files=normal"));
        assert!(!command.contains("--untracked-files=all"));
    }

    /// NUL porcelain 解析必须正确处理重命名后的原路径。
    #[test]
    fn git_status_parses_rename_and_untracked() {
        let bytes = b"R  new name\0old name\0?? untracked.txt\0";
        let entries = parse_git_status(Path::new("/repo"), bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, "renamed");
        assert_eq!(entries[0].original_path.as_deref(), Some("old name"));
        assert_eq!(entries[1].kind, "untracked");
        assert_eq!(entries[1].path, "untracked.txt");
    }

    /// `normal` 未跟踪模式下列出目录本身，而不是目录内每个文件。
    #[test]
    fn git_status_parses_untracked_directory() {
        let bytes = b"?? vendor/\0?? new.txt\0";
        let entries = parse_git_status(Path::new("/repo"), bytes).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].kind, "untracked");
        assert_eq!(entries[0].path, "vendor");
        assert_eq!(entries[0].name, "vendor");
        assert!(entries[0].is_directory);
        assert_eq!(entries[1].kind, "untracked");
        assert_eq!(entries[1].path, "new.txt");
        assert!(!entries[1].is_directory);
    }

    /// 按需展开必须由 Git 排除 ignored 文件，并把嵌套仓库保留为目录边界。
    #[test]
    fn untracked_directory_query_filters_ignored_and_nested_repository_files() {
        let root = std::env::temp_dir().join(format!(
            "keencode-untracked-directory-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let bundle = root.join("bundle");
        let nested = bundle.join("nested");
        fs::create_dir_all(&nested).expect("create untracked directories");
        assert!(
            run_git(&root, &["init"])
                .expect("init root")
                .status
                .success()
        );
        assert!(
            run_git(&nested, &["init"])
                .expect("init nested")
                .status
                .success()
        );
        fs::write(root.join(".gitignore"), "bundle/ignored.tmp\n").expect("write ignore rule");
        fs::write(bundle.join("visible.txt"), "visible\n").expect("write visible file");
        fs::write(bundle.join("ignored.tmp"), "ignored\n").expect("write ignored file");
        fs::write(nested.join("inside.txt"), "nested\n").expect("write nested file");

        let output = run_git_with_path(
            &root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--",
            ],
            Path::new("bundle"),
            &[],
        )
        .expect("query untracked directory");
        assert!(output.status.success(), "git status failed: {output:?}");
        let entries = parse_git_status(&root, &output.stdout).expect("parse status");

        assert!(
            entries
                .iter()
                .any(|entry| entry.path == "bundle/visible.txt")
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.path.contains("ignored.tmp"))
        );
        let nested_entry = entries
            .iter()
            .find(|entry| entry.path == "bundle/nested")
            .expect("nested repository boundary");
        assert!(nested_entry.is_directory);
        assert!(nested_entry.is_nested_repository);
        assert!(
            !entries
                .iter()
                .any(|entry| entry.path.contains("inside.txt"))
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// 未跟踪文件应能生成新增文件 diff，而不是空结果。
    #[test]
    fn new_file_diff_outputs_added_file_patch() {
        let root = std::env::temp_dir().join(format!(
            "keencode-git-diff-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create temp git repo");
        let init = run_git(&root, &["init"]).expect("run git init");
        assert!(init.status.success(), "git init failed: {init:?}");
        fs::write(root.join("new.txt"), "hello\n").expect("write untracked file");

        assert!(!git_path_is_tracked(&root, Path::new("new.txt")).expect("check tracking"));
        let output =
            run_git_new_file_diff(&root, Path::new("new.txt")).expect("diff untracked file");
        assert!(git_diff_status_ok(&output), "diff failed: {output:?}");
        let diff = String::from_utf8_lossy(&output.stdout);
        assert!(diff.contains("new file mode"), "diff was: {diff}");
        assert!(diff.contains("+hello"), "diff was: {diff}");

        let _ = fs::remove_dir_all(&root);
    }

    /// Worktree 名称和过期参数必须拒绝选项注入。
    #[test]
    fn worktree_arguments_reject_option_injection() {
        assert!(validate_worktree_name("-force").is_err());
        assert!(validate_start_point(Some("--detach".to_owned())).is_err());
        assert!(validate_worktree_expire(Some("--all".to_owned())).is_err());
    }

    #[test]
    fn local_image_preview_reads_arbitrary_absolute_images_only() {
        let root = std::env::temp_dir().join(format!(
            "keencode-image-preview-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create image preview directory");
        let image = root.join("preview.png");
        let text = root.join("preview.txt");
        let image_bytes = include_bytes!("../../public/logo.png");
        fs::write(&image, image_bytes).expect("write image fixture");
        fs::write(&text, b"not an image").expect("write text fixture");

        assert_eq!(read_local_image_bytes(&image).unwrap(), image_bytes);
        assert!(read_local_image_bytes(&text).is_err());
        assert!(read_local_image_bytes(&root).is_err());
        assert!(read_local_image_bytes(Path::new("preview.png")).is_err());

        let _ = fs::remove_dir_all(root);
    }
}
