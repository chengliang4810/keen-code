//! 本地文件树、内容查询和文本文件编辑。

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use crate::native_paths::NativePaths;

const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SEARCH_FILE_BYTES: u64 = 512 * 1024;
const MAX_SEARCH_SCAN_BYTES: u64 = 100 * 1024 * 1024;
const MAX_SEARCH_ENTRIES: usize = 25_000;
const MAX_TREE_ENTRIES: usize = 4_000;
const MAX_QUERY_UNITS: usize = 256;
const MAX_LINE_UNITS: usize = 1024;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub modified_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileTreeInput {
    pub root: PathBuf,
    pub relative: Option<PathBuf>,
    pub include_hidden: Option<bool>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeResult {
    pub root: PathBuf,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileSearchInput {
    pub root: PathBuf,
    pub query: String,
    pub limit: Option<usize>,
    pub include_files: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchEntry {
    pub path: PathBuf,
    pub name: String,
    pub parent_path: PathBuf,
    pub kind: FileKind,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResult {
    pub entries: Vec<FileSearchEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContentSearchInput {
    pub root: PathBuf,
    pub query: String,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatch {
    pub path: PathBuf,
    pub line_number: usize,
    pub line_text: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchResult {
    pub matches: Vec<ContentMatch>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDocument {
    pub path: PathBuf,
    pub text: String,
    pub byte_len: usize,
    pub modified_unix_ms: Option<u64>,
}

#[derive(Clone)]
pub struct FileService {
    paths: Arc<NativePaths>,
}

impl FileService {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &Arc<NativePaths> {
        &self.paths
    }

    pub fn list_tree(&self, input: FileTreeInput) -> Result<FileTreeResult, String> {
        let root = canonical_directory(&input.root)?;
        let directory = match input.relative {
            Some(relative) => resolve_existing(&root, &relative, true)?,
            None => root.clone(),
        };
        if !directory.is_dir() {
            return Err("文件树目标不是目录".to_owned());
        }
        let limit = input.limit.unwrap_or(500).clamp(1, MAX_TREE_ENTRIES);
        let include_hidden = input.include_hidden.unwrap_or(false);
        let mut entries = Vec::new();
        let mut truncated = false;
        let directory_entries = fs::read_dir(&directory).map_err(io_error)?;
        for entry in directory_entries {
            let entry = entry.map_err(io_error)?;
            let Some(file) = tree_entry(&root, entry, include_hidden)? else {
                continue;
            };
            if entries.len() >= limit {
                // 继续使用当前 read_dir 迭代器探测剩余可见项；重新打开目录会
                // 把隐藏项或已过滤的越界项误判为截断。
                truncated = true;
                break;
            }
            entries.push(file);
        }
        entries.sort_by(|left, right| {
            let left_dir = left.kind == FileKind::Directory;
            let right_dir = right.kind == FileKind::Directory;
            right_dir
                .cmp(&left_dir)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(FileTreeResult {
            root: directory,
            entries,
            truncated,
        })
    }

    pub fn search_files(&self, input: FileSearchInput) -> Result<FileSearchResult, String> {
        validate_query(&input.query)?;
        let root = canonical_directory(&input.root)?;
        let limit = input.limit.unwrap_or(100).clamp(1, 100);
        search_files(
            &root,
            &input.query,
            limit,
            input.include_files.unwrap_or(true),
            Duration::from_secs(4),
        )
    }

    pub fn search_content(&self, input: ContentSearchInput) -> Result<ContentSearchResult, String> {
        validate_query(&input.query)?;
        let query = input.query.trim().to_lowercase();
        if query.encode_utf16().count() < 2 {
            return Ok(ContentSearchResult {
                matches: Vec::new(),
                truncated: false,
            });
        }
        let root = canonical_directory(&input.root)?;
        search_content(
            &root,
            &query,
            input.limit.unwrap_or(50).clamp(1, 100),
            Duration::from_secs(4),
        )
    }

    pub fn read_text(&self, root: &Path, relative: &Path) -> Result<TextDocument, String> {
        let root = canonical_directory(root)?;
        let path = resolve_existing(&root, relative, false)?;
        let metadata = fs::metadata(&path).map_err(io_error)?;
        if !metadata.is_file() {
            return Err("文本编辑目标不是普通文件".to_owned());
        }
        if metadata.len() > MAX_TEXT_BYTES {
            return Err("文本文件超过 8 MiB 编辑上限".to_owned());
        }
        let bytes = fs::read(&path).map_err(io_error)?;
        let text = String::from_utf8(bytes).map_err(|_| "文件不是 UTF-8 文本".to_owned())?;
        Ok(TextDocument {
            path,
            byte_len: text.len(),
            text,
            modified_unix_ms: modified_unix_ms(&metadata),
        })
    }

    pub fn write_text(
        &self,
        root: &Path,
        relative: &Path,
        text: &str,
        expected_modified_unix_ms: Option<u64>,
    ) -> Result<TextDocument, String> {
        if text.len() as u64 > MAX_TEXT_BYTES {
            return Err("文本文件超过 8 MiB 编辑上限".to_owned());
        }
        let root = canonical_directory(root)?;
        let path = resolve_for_write(&root, relative)?;
        if let Some(expected) = expected_modified_unix_ms {
            let actual = fs::metadata(&path)
                .ok()
                .and_then(|metadata| modified_unix_ms(&metadata));
            if actual != Some(expected) {
                return Err("文件已在编辑器外修改，请重新读取后保存".to_owned());
            }
        }
        let parent = path.parent().ok_or("文本文件缺少父目录")?;
        let name = path
            .file_name()
            .ok_or("文本文件缺少名称")?
            .to_string_lossy();
        let temporary = parent.join(format!(
            ".{name}.keencode-{}-{}.tmp",
            std::process::id(),
            unique_nonce()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(io_error)?;
            file.write_all(text.as_bytes()).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            replace_file(&temporary, &path)?;
            let metadata = fs::metadata(&path).map_err(io_error)?;
            Ok(TextDocument {
                path: path.clone(),
                text: text.to_owned(),
                byte_len: text.len(),
                modified_unix_ms: modified_unix_ms(&metadata),
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn tree_entry(
    root: &Path,
    entry: fs::DirEntry,
    include_hidden: bool,
) -> Result<Option<FileEntry>, String> {
    let name = entry.file_name().to_string_lossy().into_owned();
    if !include_hidden && name.starts_with('.') {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(entry.path()).map_err(io_error)?;
    let kind = if metadata.file_type().is_symlink() {
        FileKind::Symlink
    } else if metadata.is_dir() {
        FileKind::Directory
    } else if metadata.is_file() {
        FileKind::File
    } else {
        return Ok(None);
    };
    let path = if kind == FileKind::Symlink {
        // 不把链接目标纳入编辑权限；树中仍展示链接本身供用户识别。
        entry.path()
    } else {
        entry.path().canonicalize().map_err(io_error)?
    };
    if kind != FileKind::Symlink && !path.starts_with(root) {
        return Ok(None);
    }
    let modified_unix_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|time| u64::try_from(time.as_millis()).ok());
    Ok(Some(FileEntry {
        path,
        name,
        kind,
        size: metadata.is_file().then_some(metadata.len()),
        modified_unix_ms,
    }))
}

fn validate_query(query: &str) -> Result<(), String> {
    if query.encode_utf16().count() > MAX_QUERY_UNITS || query.contains('\0') {
        return Err("文件查询参数无效".to_owned());
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("工作区路径必须是绝对路径".to_owned());
    }
    let path = path.canonicalize().map_err(io_error)?;
    if !path.is_dir() {
        return Err("工作区路径不是目录".to_owned());
    }
    Ok(path)
}

fn resolve_existing(root: &Path, relative: &Path, directory: bool) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err("文件路径必须位于工作区内".to_owned());
    }
    let path = root.join(relative).canonicalize().map_err(io_error)?;
    if !path.starts_with(root) || (directory && !path.is_dir()) || (!directory && !path.is_file()) {
        return Err("文件路径不属于工作区".to_owned());
    }
    Ok(path)
}

fn resolve_for_write(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err("写入路径必须是工作区内的相对路径".to_owned());
    }
    let parent = root
        .join(relative)
        .parent()
        .ok_or("写入路径缺少父目录")?
        .canonicalize()
        .map_err(io_error)?;
    if !parent.starts_with(root) {
        return Err("写入路径父目录不属于工作区".to_owned());
    }
    let path = parent.join(relative.file_name().ok_or("写入文件缺少名称")?);
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && metadata.file_type().is_symlink()
    {
        return Err("不能通过符号链接写入文件".to_owned());
    }
    Ok(path)
}

fn search_files(
    root: &Path,
    query: &str,
    limit: usize,
    include_files: bool,
    budget: Duration,
) -> Result<FileSearchResult, String> {
    // 内部调用方和单测都可能传入未 canonicalize 的临时目录；统一根路径后，
    // Windows 的扩展路径前缀不会让工作区边界检查误判。
    let root = canonical_directory(root)?;
    let query = normalize_local_query(query);
    let mut result = FileSearchResult {
        entries: Vec::new(),
        truncated: false,
    };
    if query.is_empty() {
        return Ok(result);
    }
    let deadline = Instant::now() + budget;
    let mut ranked = Vec::new();
    let walker = WalkBuilder::new(&root)
        .standard_filters(false)
        .hidden(!query.starts_with('.'))
        .follow_links(false)
        .max_depth(Some(6))
        .filter_entry(|entry| {
            entry.depth() == 0
                || ![".git", "node_modules", "target", "dist", "build", ".next"]
                    .contains(&entry.file_name().to_string_lossy().as_ref())
        })
        .build();
    for (index, entry) in walker.enumerate() {
        if index >= MAX_SEARCH_ENTRIES || Instant::now() >= deadline {
            result.truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() && !(include_files && kind.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(score) = name_score(&name, &query) else {
            continue;
        };
        let canonical = match entry.path().canonicalize() {
            Ok(path) if path.starts_with(&root) => path,
            _ => {
                result.truncated = true;
                continue;
            }
        };
        let parent_path = canonical.parent().ok_or("搜索项缺少父目录")?.to_owned();
        ranked.push((
            score,
            entry.depth(),
            FileSearchEntry {
                path: canonical,
                name,
                parent_path,
                kind: if kind.is_dir() {
                    FileKind::Directory
                } else {
                    FileKind::File
                },
            },
        ));
        ranked.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then(left.1.cmp(&right.1))
                .then(left.2.path.cmp(&right.2.path))
        });
        if ranked.len() > limit {
            result.truncated = true;
            ranked.pop();
        }
    }
    result.entries = ranked.into_iter().map(|(_, _, entry)| entry).collect();
    Ok(result)
}

fn search_content(
    root: &Path,
    query: &str,
    limit: usize,
    budget: Duration,
) -> Result<ContentSearchResult, String> {
    // 与 search_files 保持相同的路径不变量，避免 canonical 子路径和普通根路径
    // 在 Windows 上因 `\\?\` 前缀不同而被错误跳过。
    let root = canonical_directory(root)?;
    let mut result = ContentSearchResult {
        matches: Vec::new(),
        truncated: false,
    };
    let deadline = Instant::now() + budget;
    let mut scan_bytes = 0u64;
    let walker = WalkBuilder::new(&root)
        .hidden(false)
        .follow_links(false)
        .max_depth(Some(32))
        .sort_by_file_path(|left, right| left.cmp(right))
        .filter_entry(|entry| {
            entry.depth() == 0
                || ![".git", "node_modules", "target", "dist", "build", ".next"]
                    .contains(&entry.file_name().to_string_lossy().as_ref())
        })
        .build();
    for (index, entry) in walker.enumerate() {
        if index >= MAX_SEARCH_ENTRIES
            || Instant::now() >= deadline
            || scan_bytes >= MAX_SEARCH_SCAN_BYTES
        {
            result.truncated = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = match entry.path().canonicalize() {
            Ok(path) if path.starts_with(&root) => path,
            _ => {
                result.truncated = true;
                continue;
            }
        };
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        let size = match file.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                result.truncated = true;
                continue;
            }
        };
        if size == 0 || size > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_SEARCH_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            result.truncated = true;
            continue;
        }
        scan_bytes = scan_bytes.saturating_add(bytes.len() as u64);
        if bytes.len() as u64 > MAX_SEARCH_FILE_BYTES {
            result.truncated = true;
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        for (line_number, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(query) {
                result.matches.push(ContentMatch {
                    path: path.clone(),
                    line_number: line_number + 1,
                    line_text: preview_line(line),
                });
                if result.matches.len() >= limit {
                    result.truncated = true;
                    return Ok(result);
                }
            }
        }
    }
    Ok(result)
}

fn normalize_local_query(input: &str) -> String {
    let mut query = input.trim();
    loop {
        if let Some(rest) = query.strip_prefix("./") {
            query = rest;
        } else if let Some(rest) = query.strip_prefix('@').or_else(|| query.strip_prefix('/')) {
            query = rest;
        } else {
            break;
        }
    }
    query.to_lowercase()
}

fn name_score(name: &str, query: &str) -> Option<usize> {
    let name = name.to_lowercase();
    if name == query {
        return Some(0);
    }
    if name.starts_with(query) {
        return Some(2);
    }
    if name.contains(query) {
        return Some(5);
    }
    let mut offset = 0;
    let mut penalty = 0;
    for ch in query.chars() {
        let gap = name[offset..].find(ch)?;
        penalty += gap;
        offset += gap + ch.len_utf8();
    }
    Some(100 + penalty + name.len().saturating_sub(query.len()))
}

fn preview_line(line: &str) -> String {
    let line = line.trim();
    if line.encode_utf16().count() <= MAX_LINE_UNITS {
        return line.to_owned();
    }
    let mut units = 0;
    let mut preview = String::new();
    for ch in line.chars() {
        if units + ch.len_utf16() >= MAX_LINE_UNITS {
            break;
        }
        units += ch.len_utf16();
        preview.push(ch);
    }
    preview.push('\u{2026}');
    preview
}

fn modified_unix_ms(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|time| u64::try_from(time.as_millis()).ok())
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        if destination.exists() {
            fs::remove_file(destination).map_err(io_error)?;
        }
    }
    fs::rename(temporary, destination).map_err(io_error)
}

fn unique_nonce() -> u128 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default())
        ^ u128::from(counter)
}

fn io_error(error: std::io::Error) -> String {
    format!("本地文件操作失败：{error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_paths::NativePaths;
    use std::{fs, sync::Arc, time::Duration};

    #[test]
    fn name_score_prefers_exact_and_prefix_matches() {
        assert!(
            name_score("Cargo.toml", "cargo.toml").unwrap()
                < name_score("src-Cargo.toml", "cargo").unwrap()
        );
    }

    #[test]
    fn preview_respects_utf16_boundaries() {
        let value = preview_line(&"中".repeat(MAX_LINE_UNITS));
        assert!(value.encode_utf16().count() <= MAX_LINE_UNITS);
    }

    #[test]
    fn resolve_for_write_rejects_parent_and_absolute_paths() {
        let root = std::env::temp_dir();
        assert!(resolve_for_write(&root, Path::new("../outside")).is_err());
        assert!(resolve_for_write(&root, &root.join("outside")).is_err());
    }

    #[test]
    fn list_tree_only_marks_visible_entries_as_truncated() {
        let temporary = tempfile::tempdir().expect("创建临时目录");
        let root = temporary.path().join("project");
        fs::create_dir(&root).expect("创建项目目录");
        File::create(root.join("visible.txt")).expect("创建可见文件");
        File::create(root.join(".hidden.txt")).expect("创建隐藏文件");
        let service = FileService::new(Arc::new(NativePaths::from_data_root(
            temporary.path().join("data"),
        )));
        let result = service
            .list_tree(FileTreeInput {
                root: root.clone(),
                relative: None,
                include_hidden: Some(false),
                limit: Some(1),
            })
            .expect("读取文件树");
        assert_eq!(result.entries.len(), 1);
        assert!(!result.truncated);
        File::create(root.join("second.txt")).expect("创建第二个可见文件");
        let result = service
            .list_tree(FileTreeInput {
                root,
                relative: None,
                include_hidden: Some(false),
                limit: Some(1),
            })
            .expect("再次读取文件树");
        assert!(result.truncated);
    }

    #[test]
    fn content_search_returns_line_number_and_bounded_preview() {
        let temporary = tempfile::tempdir().expect("创建临时目录");
        let root = temporary.path().join("project");
        fs::create_dir(&root).expect("创建项目目录");
        fs::write(root.join("main.txt"), "first\nNeedle here\nthird\n").expect("写入文本");
        let result =
            search_content(&root, "needle", 10, Duration::from_secs(1)).expect("执行内容搜索");
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].line_number, 2);
        assert_eq!(result.matches[0].line_text, "Needle here");
        assert!(!result.truncated);
    }
}
