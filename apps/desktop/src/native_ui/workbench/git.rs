//! 本地 Git 查询与修改服务。

use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::native_paths::NativePaths;

const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileStatus {
    pub path: String,
    pub status: String,
    pub index_status: String,
    pub worktree_status: String,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub is_repo: bool,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    pub files: Vec<GitFileStatus>,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StashEntry {
    pub hash: String,
    pub reference: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub enum GitAction {
    Init,
    CreateBranch {
        name: String,
        publish: bool,
    },
    PullFastForward,
    Stage {
        paths: Vec<String>,
    },
    Unstage {
        paths: Vec<String>,
    },
    Commit {
        message: String,
        paths: Option<Vec<String>>,
    },
    Stash {
        message: String,
    },
    Push,
    StashAndCheckout {
        branch: String,
    },
    DropStash {
        hash: String,
    },
}

#[derive(Clone)]
pub struct GitService {
    paths: Arc<NativePaths>,
}

impl GitService {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &Arc<NativePaths> {
        &self.paths
    }

    pub fn status(&self, root: &Path) -> Result<GitStatus, String> {
        let root = canonical_root(root)?;
        if !is_repo(&root)? {
            return Ok(GitStatus {
                is_repo: false,
                branch: None,
                upstream: None,
                ahead: 0,
                behind: 0,
                files: Vec::new(),
                insertions: 0,
                deletions: 0,
            });
        }
        let branch = git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .ok()
            .map(|value| value.trim().to_owned());
        let upstream = git(
            &root,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )
        .ok()
        .map(|value| value.trim().to_owned());
        let mut stats = BTreeMap::new();
        let has_head = git(&root, &["rev-parse", "--verify", "HEAD"]).is_ok();
        if has_head {
            stats = parse_numstat(&git(
                &root,
                &["diff", "--numstat", "-z", "--no-renames", "HEAD"],
            )?);
        }
        let porcelain = git(
            &root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        )?;
        let mut files = Vec::new();
        let mut fields = porcelain.split('\0');
        while let Some(field) = fields.next() {
            if field.len() < 3 {
                continue;
            }
            let bytes = field.as_bytes();
            let index = bytes[0] as char;
            let worktree = bytes[1] as char;
            let path = field[3..].to_owned();
            if matches!(index, 'R' | 'C') || matches!(worktree, 'R' | 'C') {
                let _ = fields.next();
            }
            let (insertions, deletions) = stats.get(&path).copied().unwrap_or_default();
            files.push(GitFileStatus {
                path,
                status: format!("{index}{worktree}"),
                index_status: index.to_string(),
                worktree_status: worktree.to_string(),
                insertions,
                deletions,
            });
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let (ahead, behind) = if upstream.is_some() {
            let output = git(
                &root,
                &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
            )?;
            let mut values = output.split_whitespace();
            (
                values
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_default(),
                values
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_default(),
            )
        } else {
            (0, 0)
        };
        Ok(GitStatus {
            is_repo: true,
            branch,
            upstream,
            ahead,
            behind,
            insertions: files.iter().map(|file| file.insertions).sum(),
            deletions: files.iter().map(|file| file.deletions).sum(),
            files,
        })
    }

    pub fn action(&self, root: &Path, action: GitAction) -> Result<String, String> {
        let root = canonical_root(root)?;
        match action {
            GitAction::Init => {
                git(&root, &["init"])?;
                Ok("initialized".to_owned())
            }
            GitAction::CreateBranch { name, publish } => {
                valid_revision(&name)?;
                git(&root, &["check-ref-format", "--branch", &name])?;
                git(&root, &["branch", &name])?;
                if publish {
                    run_checked(&root, &["push", "--set-upstream", "origin", &name])?;
                }
                Ok(name)
            }
            GitAction::PullFastForward => {
                run_checked(&root, &["pull", "--ff-only"])?;
                Ok("pulled".to_owned())
            }
            GitAction::Stage { paths } => {
                validate_paths(&paths)?;
                let mut args = vec!["add", "--"];
                args.extend(paths.iter().map(String::as_str));
                git(&root, &args)?;
                Ok("staged".to_owned())
            }
            GitAction::Unstage { paths } => {
                validate_paths(&paths)?;
                let mut args = if git(&root, &["rev-parse", "--verify", "HEAD"]).is_ok() {
                    vec!["restore", "--staged", "--"]
                } else {
                    vec!["rm", "--cached", "--force", "--ignore-unmatch", "--"]
                };
                args.extend(paths.iter().map(String::as_str));
                git(&root, &args)?;
                Ok("unstaged".to_owned())
            }
            GitAction::Commit { message, paths } => {
                validate_commit_message(&message)?;
                if let Some(paths) = paths {
                    validate_paths(&paths)?;
                    let mut args = vec!["add", "--"];
                    args.extend(paths.iter().map(String::as_str));
                    git(&root, &args)?;
                }
                run_checked(&root, &["commit", "-m", &message])?;
                Ok(git(&root, &["rev-parse", "--short", "HEAD"])?
                    .trim()
                    .to_owned())
            }
            GitAction::Stash { message } => {
                validate_commit_message(&message)?;
                git(
                    &root,
                    &["stash", "push", "--include-untracked", "-m", &message],
                )?;
                Ok("stashed".to_owned())
            }
            GitAction::Push => {
                run_checked(&root, &["push"])?;
                Ok("pushed".to_owned())
            }
            GitAction::StashAndCheckout { branch } => {
                stash_and_checkout(&root, &branch)?;
                Ok(branch)
            }
            GitAction::DropStash { hash } => {
                drop_stash(&root, &hash)?;
                Ok("dropped".to_owned())
            }
        }
    }

    pub fn diff(&self, root: &Path, staged: bool, path: Option<&str>) -> Result<String, String> {
        let root = canonical_root(root)?;
        if let Some(path) = path {
            valid_file(path)?;
        }
        let mut args = vec!["diff", "--no-ext-diff", "--no-color", "--no-renames"];
        if staged {
            args.push("--cached");
        }
        if let Some(path) = path {
            args.extend(["--", path]);
        }
        let output = git(&root, &args)?;
        if output.is_empty()
            && !staged
            && let Some(path) = path
        {
            let output = crate::workspace::run_git(
                &root,
                &[
                    "--literal-pathspecs",
                    "diff",
                    "--no-index",
                    "--no-color",
                    "--",
                    empty_diff_path(),
                    path,
                ],
            )
            .map_err(|error| error.to_string())?;
            if !matches!(output.status.code(), Some(0 | 1)) {
                return Err("无法读取新文件差异".to_owned());
            }
            return String::from_utf8(output.stdout).map_err(|_| "Git 差异不是 UTF-8".to_owned());
        }
        Ok(output)
    }

    pub fn blame(
        &self,
        root: &Path,
        file: &str,
        line: u32,
        revision: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let root = canonical_root(root)?;
        valid_file(file)?;
        if line == 0 {
            return Err("Git 行号必须大于零".to_owned());
        }
        if let Some(revision) = revision {
            valid_revision(revision)?;
        }
        let range = format!("{line},{line}");
        let mut args = vec!["blame", "--line-porcelain", "--no-ext-diff", "-L", &range];
        if let Some(revision) = revision {
            args.push(revision);
        }
        args.extend(["--", file]);
        let output = git(&root, &args)?;
        let sha = output
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().next())
            .ok_or("Git 行归属响应为空")?;
        if !(sha.len() == 40 || sha.len() == 64)
            || !sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("Git 行归属 SHA 无效".to_owned());
        }
        let field = |key: &str| {
            output
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .unwrap_or_default()
        };
        let time = field("author-time")
            .trim()
            .parse::<i64>()
            .map_err(|_| "Git 行归属时间无效")?;
        let time = chrono::DateTime::from_timestamp(time, 0).ok_or("Git 行归属时间越界")?;
        Ok(serde_json::json!({
            "sha": sha,
            "shortSha": &sha[..7],
            "author": field("author "),
            "authorEmail": field("author-mail ").trim_matches(['<', '>']),
            "authorTime": time.to_rfc3339(),
            "summary": field("summary "),
            "uncommitted": sha.bytes().all(|byte| byte == b'0'),
        }))
    }

    pub fn stash_list(&self, root: &Path) -> Result<Vec<StashEntry>, String> {
        list_stashes(&canonical_root(root)?)
    }
}

fn canonical_root(root: &Path) -> Result<PathBuf, String> {
    if !root.is_absolute() {
        return Err("Git 根目录必须是绝对路径".to_owned());
    }
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    if !root.is_dir() {
        return Err("Git 根目录不是目录".to_owned());
    }
    Ok(root)
}

fn is_repo(root: &Path) -> Result<bool, String> {
    Ok(
        git(root, &["rev-parse", "--is-inside-work-tree"])
            .is_ok_and(|value| value.trim() == "true"),
    )
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let args: Vec<_> = std::iter::once("--literal-pathspecs")
        .chain(args.iter().copied())
        .collect();
    let output = crate::workspace::run_git_with_timeout(root, &args)?;
    if !output.status.success() {
        return Err(git_error(&output));
    }
    if output.stdout.len() > MAX_OUTPUT_BYTES {
        return Err("Git 返回结果超过大小限制".to_owned());
    }
    String::from_utf8(output.stdout).map_err(|_| "Git 返回非 UTF-8 数据".to_owned())
}

fn run_checked(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = crate::workspace::run_git_with_timeout(root, args)?;
    if !output.status.success() {
        return Err(git_error(&output));
    }
    if output.stdout.len() > MAX_OUTPUT_BYTES {
        return Err("Git 返回结果超过大小限制".to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_error(output: &std::process::Output) -> String {
    let detail = keencode_model::redact_error_secrets(&String::from_utf8_lossy(&output.stderr));
    if detail.trim().is_empty() {
        format!("Git 操作失败，退出码 {:?}", output.status.code())
    } else {
        detail
    }
}

pub(crate) fn valid_file(file: &str) -> Result<(), String> {
    let path = Path::new(file);
    if file.is_empty()
        || file.len() > 2048
        || path.is_absolute()
        || file.chars().any(char::is_control)
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err("Git 文件路径必须位于项目内".to_owned());
    }
    Ok(())
}

fn valid_revision(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err("Git revision 无效".to_owned());
    }
    Ok(())
}

fn validate_paths(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() || paths.len() > 4096 {
        return Err("Git 文件列表为空或超过限制".to_owned());
    }
    paths.iter().try_for_each(|path| valid_file(path))
}

fn validate_commit_message(message: &str) -> Result<(), String> {
    if message.trim().is_empty() || message.len() > 64 * 1024 || message.contains('\0') {
        return Err("Git 提交消息为空或超过限制".to_owned());
    }
    Ok(())
}

fn parse_numstat(raw: &str) -> BTreeMap<String, (u64, u64)> {
    let mut result = BTreeMap::new();
    for entry in raw.split('\0').filter(|entry| !entry.is_empty()) {
        let mut columns = entry.splitn(3, '\t');
        let additions = columns
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        let deletions = columns
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        if let Some(path) = columns.next() {
            result.insert(path.to_owned(), (additions, deletions));
        }
    }
    result
}

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

fn list_stashes(root: &Path) -> Result<Vec<StashEntry>, String> {
    let output = git(root, &["stash", "list", "--format=%H%x09%gd%x09%gs"])?;
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut values = line.splitn(3, '\t');
            let hash = values.next().ok_or("stash 对象缺失")?.to_owned();
            validate_hash(&hash)?;
            Ok(StashEntry {
                hash,
                reference: values.next().ok_or("stash 引用缺失")?.to_owned(),
                message: values.next().ok_or("stash 主题缺失")?.to_owned(),
            })
        })
        .collect()
}

fn validate_hash(hash: &str) -> Result<(), String> {
    if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("stash 身份必须是实际对象 SHA".to_owned());
    }
    Ok(())
}

fn drop_stash(root: &Path, hash: &str) -> Result<(), String> {
    validate_hash(hash)?;
    let entry = list_stashes(root)?
        .into_iter()
        .find(|entry| entry.hash == hash)
        .ok_or("所选 stash 已失效，请重新查看")?;
    git(root, &["stash", "drop", &entry.reference])?;
    Ok(())
}

fn stash_and_checkout(root: &Path, branch: &str) -> Result<(), String> {
    valid_revision(branch)?;
    git(root, &["check-ref-format", "--branch", branch])?;
    git(
        root,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    )?;
    let before = list_stashes(root)?;
    git(
        root,
        &[
            "stash",
            "push",
            "--include-untracked",
            "-m",
            &format!("KeenCode: stash before switching to {branch}"),
        ],
    )?;
    let after = list_stashes(root)?;
    let created = after
        .iter()
        .find(|entry| !before.iter().any(|old| old.hash == entry.hash))
        .cloned()
        .or_else(|| (after.len() > before.len()).then(|| after[0].clone()));
    if let Err(error) = git(root, &["switch", branch]) {
        if let Some(entry) = created {
            return match git(root, &["stash", "apply", "--index", &entry.hash]) {
                Ok(_) => Err(format!(
                    "切换分支失败，原工作区修改已恢复；临时 stash {} 仍保留：{error}",
                    entry.hash
                )),
                Err(restore_error) => Err(format!(
                    "切换分支失败，恢复临时 stash {} 也失败：{error}；{restore_error}",
                    entry.hash
                )),
            };
        }
        return Err(error);
    }
    if let Some(entry) = created {
        if let Err(error) = git(root, &["stash", "apply", "--index", &entry.hash]) {
            return Err(format!(
                "已切换到 {branch}，但临时 stash {} 恢复失败，修改仍保留在 stash：{error}",
                entry.hash
            ));
        }
        if let Err(error) = drop_stash(root, &entry.hash) {
            return Err(format!(
                "已切换到 {branch} 且修改已恢复，但临时 stash {} 清理失败，stash 仍保留：{error}",
                entry.hash
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_file_rejects_paths_outside_workspace() {
        assert!(valid_file("../outside.txt").is_err());
        assert!(valid_file("nested/../outside.txt").is_err());
        assert!(
            valid_file(
                &std::env::temp_dir()
                    .join("outside.txt")
                    .display()
                    .to_string()
            )
            .is_err()
        );
        assert!(valid_file("line\nfeed.txt").is_err());
    }

    #[test]
    fn valid_file_accepts_literal_relative_paths() {
        assert!(valid_file("src/main.rs").is_ok());
        assert!(valid_file("notes:today.txt").is_ok());
    }

    #[test]
    fn stash_hash_requires_a_full_hex_object_id() {
        assert!(validate_hash(&"a".repeat(40)).is_ok());
        assert!(validate_hash(&"a".repeat(39)).is_err());
        assert!(validate_hash(&format!("{}g", "a".repeat(39))).is_err());
    }
}
