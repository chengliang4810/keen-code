//! Git diff 与本地 review 标注。

use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::native_paths::NativePaths;

use super::git::{git, valid_file};

const MAX_DIFF_BYTES: usize = 16 * 1024 * 1024;
const MAX_REVIEW_BYTES: usize = 2 * 1024 * 1024;
const MAX_UNTRACKED_FILES: usize = 4_096;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiffOptions {
    pub staged: bool,
    pub path: Option<String>,
    pub context_lines: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffDocument {
    pub patch: String,
    pub staged: bool,
    pub path: Option<String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewComment {
    pub id: String,
    pub path: String,
    pub line: u32,
    pub side: ReviewSide,
    pub body: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSide {
    Old,
    New,
}

#[derive(Clone)]
pub struct DiffService {
    paths: Arc<NativePaths>,
}

impl DiffService {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self { paths }
    }

    pub fn read(&self, root: &Path, options: DiffOptions) -> Result<DiffDocument, String> {
        let root = canonical_root(root)?;
        if let Some(path) = options.path.as_deref() {
            valid_file(path)?;
        }
        let context = options.context_lines.unwrap_or(3).min(64).to_string();
        let unified = format!("--unified={context}");
        // UI 直接展示补丁路径，关闭 Git 默认的 Unicode 路径转义。
        let mut args = vec![
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--no-renames",
            &unified,
        ];
        if options.staged {
            args.push("--cached");
        }
        if let Some(path) = options.path.as_deref() {
            args.extend(["--", path]);
        }
        let mut patch = git(&root, &args)?;
        if !options.staged {
            let untracked = untracked_files(&root, options.path.as_deref())?;
            for path in untracked {
                let fragment = untracked_diff(&root, &path, &context)?;
                if !fragment.is_empty() {
                    if !patch.is_empty() && !patch.ends_with('\n') {
                        patch.push('\n');
                    }
                    patch.push_str(&fragment);
                }
            }
        }
        let (patch, truncated) = truncate_utf8_bytes(patch, MAX_DIFF_BYTES);
        Ok(DiffDocument {
            patch,
            staged: options.staged,
            path: options.path,
            truncated,
        })
    }

    pub fn comments_path(&self, root: &Path) -> Result<PathBuf, String> {
        let root = canonical_root(root)?;
        let key = sha256_hex(root.to_string_lossy().as_bytes());
        Ok(self
            .paths
            .data_root
            .join("native-review")
            .join(format!("{key}.json")))
    }

    pub fn load_comments(&self, root: &Path) -> Result<Vec<ReviewComment>, String> {
        let path = self.comments_path(root)?;
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("读取 review 标注失败：{error}")),
        };
        if bytes.len() > MAX_REVIEW_BYTES {
            return Err("review 标注文件超过大小限制".to_owned());
        }
        let comments: Vec<ReviewComment> = serde_json::from_slice(&bytes)
            .map_err(|error| format!("读取 review 标注失败：{error}"))?;
        if comments.len() > 4_096 {
            return Err("review 标注数量超过限制".to_owned());
        }
        for comment in &comments {
            validate_comment(comment)?;
        }
        Ok(comments)
    }

    pub fn save_comments(&self, root: &Path, comments: Vec<ReviewComment>) -> Result<(), String> {
        if comments.len() > 4_096 {
            return Err("review 标注数量超过限制".to_owned());
        }
        for comment in &comments {
            validate_comment(comment)?;
        }
        let path = self.comments_path(root)?;
        let data = serde_json::to_vec(&comments).map_err(|error| error.to_string())?;
        if data.len() > MAX_REVIEW_BYTES {
            return Err("review 标注文件超过限制".to_owned());
        }
        fs::create_dir_all(path.parent().ok_or("review 标注目录缺失")?)
            .map_err(|error| error.to_string())?;
        let temporary = path.with_extension(format!("tmp-{}-{}", std::process::id(), next_nonce()));
        if let Err(error) = fs::write(&temporary, data) {
            let _ = fs::remove_file(&temporary);
            return Err(error.to_string());
        }
        let result = replace_file(&temporary, &path);
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(windows)]
    if destination.exists() {
        fs::remove_file(destination).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, destination).map_err(|error| error.to_string())
}

fn next_nonce() -> u64 {
    static NEXT_NONCE: AtomicU64 = AtomicU64::new(1);
    NEXT_NONCE.fetch_add(1, Ordering::Relaxed)
}

fn validate_comment(comment: &ReviewComment) -> Result<(), String> {
    if comment.id.is_empty() || comment.id.len() > 128 || comment.id.chars().any(char::is_control) {
        return Err("review 标注 ID 无效".to_owned());
    }
    valid_file(&comment.path)?;
    if comment.line == 0 || comment.body.trim().is_empty() || comment.body.len() > 16 * 1024 {
        return Err("review 标注内容无效".to_owned());
    }
    Ok(())
}

fn untracked_files(root: &Path, path: Option<&str>) -> Result<Vec<String>, String> {
    // Windows canonicalize 会补充 `\\?\` 前缀；先统一工作区根路径，才能与
    // 未跟踪文件的 canonical 路径进行可靠的边界比较。
    let root = canonical_root(root)?;
    let status = match path {
        Some(path) => git(
            &root,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--",
                path,
            ],
        )?,
        None => git(
            &root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        )?,
    };
    let mut files = Vec::new();
    for field in status.split('\0') {
        let Some(path) = field.strip_prefix("?? ") else {
            continue;
        };
        valid_file(path)?;
        if files.len() >= MAX_UNTRACKED_FILES {
            return Err("未跟踪文件数量超过 Diff 限制".to_owned());
        }
        let candidate = root.join(path);
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|error| format!("无法读取未跟踪文件 {path}：{error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let canonical = candidate
            .canonicalize()
            .map_err(|error| format!("无法解析未跟踪文件 {path}：{error}"))?;
        if !canonical.starts_with(&root) {
            return Err(format!("未跟踪文件路径不属于工作区：{path}"));
        }
        files.push(path.to_owned());
    }
    Ok(files)
}

fn untracked_diff(root: &Path, path: &str, context: &str) -> Result<String, String> {
    // Windows 的 `NUL` 不是当前 Git 可用于 `--no-index` 的真实空文件路径。
    let empty_file = tempfile::NamedTempFile::new()
        .map_err(|error| format!("创建未跟踪文件 Diff 基准失败：{error}"))?;
    let empty_path = empty_file.path().to_string_lossy().into_owned();
    let unified = format!("--unified={context}");
    let output = crate::workspace::run_git_with_timeout(
        root,
        &[
            // 与已跟踪 diff 一致，保留 UI 可读的 Unicode 路径。
            "--literal-pathspecs",
            "-c",
            "core.quotePath=false",
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-color",
            &unified,
            "--",
            &empty_path,
            path,
        ],
    )
    .map_err(|error| format!("读取未跟踪文件 Diff 失败：{error}"))?;
    if !matches!(output.status.code(), Some(0 | 1)) {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("读取未跟踪文件 Diff 失败：{}", detail.trim()));
    }
    String::from_utf8(output.stdout).map_err(|_| "未跟踪文件 Diff 不是 UTF-8".to_owned())
}

fn truncate_utf8_bytes(value: String, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value, false);
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("Git 工作区不可访问：{error}"))?;
    if !root.is_dir() {
        return Err("Git 工作区不是目录".to_owned());
    }
    Ok(root)
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_utf8_bytes_never_splits_a_codepoint() {
        let (value, truncated) = truncate_utf8_bytes("a中b".to_owned(), 2);
        assert_eq!(value, "a");
        assert!(truncated);
    }

    #[test]
    fn untracked_file_diff_is_included() {
        let temporary = tempfile::tempdir().expect("创建临时 Git 目录");
        let root = temporary.path();
        if git(root, &["init"]).is_err() {
            return;
        }
        let path = "空 格.txt";
        fs::write(root.join(path), "hello\n").expect("写入未跟踪文件");
        let files = untracked_files(root, None).expect("读取未跟踪文件");
        assert_eq!(files, vec![path]);
        let patch = untracked_diff(root, &files[0], "3").expect("读取未跟踪 Diff");
        assert!(patch.contains(path));
        assert!(patch.contains("+hello"));
    }
}
