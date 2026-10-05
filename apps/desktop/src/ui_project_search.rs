//! 原内容搜索面板的数据接口。宿主按需扫描 UTF-8 文件，不持有第二套项目或会话事实。

use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    path::Path,
    time::{Duration, Instant},
};
use tauri::AppHandle;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LocalSearchInput {
    root_path: String,
    query: String,
    limit: Option<usize>,
    include_files: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEntry {
    path: String,
    name: String,
    parent_path: String,
    kind: &'static str,
}

#[derive(Debug, Serialize)]
pub struct LocalSearchResult {
    entries: Vec<LocalEntry>,
    truncated: bool,
}

/// 本机选择器只查询名称，不授予文件读取权限；读取仍需单文件预览授权。
#[tauri::command]
pub async fn ui_project_search_local(input: LocalSearchInput) -> Result<LocalSearchResult, String> {
    let limit = input.limit.unwrap_or(100);
    if !(1..=100).contains(&limit)
        || input.query.encode_utf16().count() > 256
        || input.query.contains('\0')
    {
        return Err("本机路径搜索参数无效".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let root = Path::new(&input.root_path);
        if !root.is_absolute() {
            return Err("搜索根目录必须是绝对路径".into());
        }
        let root = root
            .canonicalize()
            .map_err(|error| format!("无法访问搜索目录：{error}"))?;
        if !root.is_dir() {
            return Err("搜索根目录不是目录".into());
        }
        search_local(
            &root,
            &input.query,
            limit,
            input.include_files.unwrap_or(true),
            Duration::from_secs(4),
        )
    })
    .await
    .map_err(|error| format!("本机路径搜索后台任务失败：{error}"))?
}

/// 去掉提及和相对路径前缀，但保留 .env 等隐藏文件前缀。
fn local_query(input: &str) -> String {
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

/// 连续名称优先，缩写按字符顺序匹配；间隔与首字符位置决定模糊匹配优先级。
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

fn search_local(
    root: &Path,
    query: &str,
    limit: usize,
    include_files: bool,
    budget: Duration,
) -> Result<LocalSearchResult, String> {
    let query = local_query(query);
    let mut result = LocalSearchResult {
        entries: vec![],
        truncated: false,
    };
    if query.is_empty() {
        return Ok(result);
    }
    let deadline = Instant::now() + budget;
    let mut ranked = Vec::new();
    // 本机选择器不应用项目 Git 忽略规则；普通查询隐藏点文件，显式 . 前缀才显示。
    let walker = WalkBuilder::new(root)
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
        if index >= MAX_ENTRIES || Instant::now() >= deadline {
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
        if entry.depth() == 6 && kind.is_dir() {
            result.truncated = true;
        }
        if !kind.is_dir() && !(include_files && kind.is_file()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(score) = name_score(&name, &query) else {
            continue;
        };
        let canonical = match entry.path().canonicalize() {
            Ok(path) if path.starts_with(root) => path,
            _ => {
                result.truncated = true;
                continue;
            }
        };
        let item = LocalEntry {
            path: crate::path_utils::path_to_frontend(&canonical),
            name,
            parent_path: crate::path_utils::path_to_frontend(
                canonical.parent().ok_or("搜索项缺少父目录")?,
            ),
            kind: if kind.is_dir() { "directory" } else { "file" },
        };
        ranked.push((score, entry.depth(), item));
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

const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_SCAN_BYTES: u64 = 100 * 1024 * 1024;
const MAX_ENTRIES: usize = 25_000;
const MAX_LINE_UTF16: usize = 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SearchInput {
    cwd: String,
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatch {
    path: String,
    line_number: usize,
    line_text: String,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    matches: Vec<ContentMatch>,
    truncated: bool,
}

/// 保留原面板的大小写不敏感字面量语义，不把用户查询解释成正则或命令。
#[tauri::command]
pub async fn ui_project_search_content(
    app: AppHandle,
    input: SearchInput,
) -> Result<SearchResult, String> {
    let limit = input.limit.unwrap_or(50);
    if !(1..=100).contains(&limit)
        || input.query.encode_utf16().count() > 256
        || input.query.contains('\0')
    {
        return Err("内容搜索参数无效".into());
    }
    let query = input.query.trim().to_lowercase();
    if query.encode_utf16().count() < 2 {
        return Ok(SearchResult {
            matches: vec![],
            truncated: false,
        });
    }
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &input.cwd)?;
        search(&root, &query, limit, Duration::from_secs(4))
    })
    .await
    .map_err(|error| format!("内容搜索后台任务失败：{error}"))?
}

/// 截断文本按 JS UTF-16 长度限制，避免中文或 emoji 令原契约解码失败。
fn preview_line(line: &str) -> String {
    let line = line.trim();
    if line.encode_utf16().count() <= MAX_LINE_UTF16 {
        return line.to_owned();
    }
    let mut units = 0;
    let mut preview = String::new();
    for ch in line.chars() {
        if units + ch.len_utf16() >= MAX_LINE_UTF16 {
            break;
        }
        units += ch.len_utf16();
        preview.push(ch);
    }
    preview.push('…');
    preview
}

fn search(
    root: &Path,
    query: &str,
    limit: usize,
    budget: Duration,
) -> Result<SearchResult, String> {
    let mut result = SearchResult {
        matches: vec![],
        truncated: false,
    };
    let deadline = Instant::now() + budget;
    let mut scan_bytes = 0;
    // 不跟随符号链接/Windows junction，并在打开文件前再核对实际路径归属。
    // 隐藏源文件可搜索；Git 忽略、依赖和构建产物不参与扫描。
    let walker = WalkBuilder::new(root)
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
        if index >= MAX_ENTRIES || Instant::now() >= deadline || scan_bytes >= MAX_SCAN_BYTES {
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
        if entry.depth() == 32 && entry.file_type().is_some_and(|kind| kind.is_dir()) {
            result.truncated = true;
        }
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let canonical = match path.canonicalize() {
            Ok(path) if path.starts_with(root) => path,
            _ => {
                result.truncated = true;
                continue;
            }
        };
        let file = match File::open(&canonical) {
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
        if size == 0 || size > MAX_FILE_BYTES {
            continue;
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            result.truncated = true;
            continue;
        }
        scan_bytes += bytes.len() as u64;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            result.truncated = true;
            continue;
        }
        if bytes.contains(&0) {
            continue;
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text.trim_start_matches('\u{feff}'),
            Err(_) => continue,
        };
        let relative = crate::path_utils::path_to_frontend(
            path.strip_prefix(root).map_err(|_| "搜索路径越界")?,
        );
        let mut per_file = 0;
        for (line_index, line) in text.lines().enumerate() {
            if !line.to_lowercase().contains(query) {
                continue;
            }
            if result.matches.len() == limit {
                result.truncated = true;
                return Ok(result);
            }
            result.matches.push(ContentMatch {
                path: relative.clone(),
                line_number: line_index + 1,
                line_text: preview_line(line),
            });
            per_file += 1;
            if per_file == 5 {
                break;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn local_names_support_prefixes_fuzzy_hidden_files_and_limits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join("project")).unwrap();
        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("project/ComposerLocalMenu.tsx"), "").unwrap();
        fs::write(root.join("project/clm.txt"), "").unwrap();
        fs::write(root.join(".env.local"), "").unwrap();
        fs::write(root.join("node_modules/ComposerLocalMenu.tsx"), "").unwrap();
        let result = search_local(&root, "@./clm", 10, true, Duration::from_secs(4)).unwrap();
        assert_eq!(
            result
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["clm.txt", "ComposerLocalMenu.tsx"]
        );
        assert!(
            result
                .entries
                .iter()
                .all(|entry| Path::new(&entry.path).is_absolute())
        );
        assert!(
            search_local(&root, "env", 10, true, Duration::from_secs(4))
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            search_local(&root, "@.en", 10, true, Duration::from_secs(4))
                .unwrap()
                .entries[0]
                .name,
            ".env.local"
        );
        assert!(
            search_local(&root, "clm", 10, false, Duration::from_secs(4))
                .unwrap()
                .entries
                .is_empty()
        );
        assert!(
            search_local(&root, "clm", 1, true, Duration::from_secs(4))
                .unwrap()
                .truncated
        );
        assert!(
            search_local(&root, "clm", 10, true, Duration::ZERO)
                .unwrap()
                .truncated
        );
    }

    #[test]
    fn literal_case_insensitive_search_preserves_lines_and_ignores_binary_and_gitignored_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(
            root.join("a.txt"),
            "first\r\n中文 Needle.*\r\nthird\nNEEDLE.*\n",
        )
        .unwrap();
        fs::write(root.join("ignored.txt"), "needle.*").unwrap();
        fs::write(root.join("binary.dat"), b"needle.*\0").unwrap();
        fs::write(root.join(".hidden"), "needle.*").unwrap();
        let result = search(&root, "needle.*", 100, Duration::from_secs(4)).unwrap();
        assert!(!result.truncated);
        assert_eq!(result.matches.len(), 3);
        let lines: Vec<_> = result
            .matches
            .iter()
            .filter(|m| m.path == "a.txt")
            .map(|m| m.line_number)
            .collect();
        assert_eq!(lines, [2, 4]);
        assert!(
            result
                .matches
                .iter()
                .all(|m| m.path != "binary.dat" && m.path != "ignored.txt")
        );
    }

    #[test]
    fn truncation_and_unicode_preview_are_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::write(root.join("a.txt"), "needle\nneedle\n").unwrap();
        let result = search(&root, "needle", 1, Duration::from_secs(4)).unwrap();
        assert!(result.truncated);
        assert_eq!(result.matches.len(), 1);
        assert!(
            search(&root, "needle", 1, Duration::ZERO)
                .unwrap()
                .truncated
        );
        let preview = preview_line(&"😀".repeat(1024));
        assert!(preview.encode_utf16().count() <= 1024);
        assert!(preview.ends_with('…'));
    }
}
