//! 原页面的 Git 查询契约适配；只执行固定操作，客户端不能提交任意 Git 参数。
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
use tauri::AppHandle;

/// 行归属按原页面的比较基线查询；工作区默认保留未提交行的零 SHA 语义。
fn blame(
    root: &Path,
    file: &str,
    line: u32,
    revision: Option<&str>,
    base: bool,
) -> Result<Value, String> {
    valid_file(file)?;
    if line == 0 {
        return Err("Git 行号必须大于零".into());
    }
    if let Some(revision) = revision {
        valid_revision(revision)?;
    }
    let resolved = if base {
        Some(branch_base(root)?)
    } else {
        revision.map(str::to_owned)
    };
    let range = format!("{line},{line}");
    let mut args = vec!["blame", "--line-porcelain", "--no-ext-diff", "-L", &range];
    if let Some(revision) = resolved.as_deref() {
        args.push(revision);
    }
    args.extend(["--", file]);
    let output = git(root, &args)?;
    let sha = output
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .ok_or("Git 行归属响应为空")?;
    if sha.len() != 40 && sha.len() != 64 {
        return Err("Git 行归属 SHA 无效".into());
    }
    let field = |key: &str| {
        output
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_default()
    };
    let time = field("author-time ")
        .parse::<i64>()
        .map_err(|_| "Git 行归属时间无效")?;
    let time = chrono::DateTime::from_timestamp(time, 0).ok_or("Git 行归属时间越界")?;
    Ok(
        json!({"sha":sha,"shortSha":&sha[..7],"author":field("author "),
        "authorEmail":field("author-mail ").trim_start_matches('<').trim_end_matches('>'),
        "authorTime":time.to_rfc3339(),"summary":field("summary "),"uncommitted":sha.bytes().all(|b|b == b'0')}),
    )
}

/// 原页面只允许固定 Git 操作。分支创建不切换 checkout；发布失败保留已创建的本地分支。
fn command(
    root: &Path,
    operation: &str,
    branch: Option<&str>,
    publish: bool,
) -> Result<Value, String> {
    match operation {
        "init" => {
            git(root, &["init"])?;
            Ok(Value::Null)
        }
        "create-branch" => {
            let branch = branch.ok_or("缺少分支名称")?;
            valid_revision(branch)?;
            git(root, &["check-ref-format", "--branch", branch])?;
            git(root, &["branch", branch])?;
            if publish {
                let output = crate::workspace::run_git_with_timeout(
                    root,
                    &["push", "--set-upstream", "origin", branch],
                )?;
                if !output.status.success() {
                    return Err(keencode_model::redact_error_secrets(
                        &String::from_utf8_lossy(&output.stderr),
                    ));
                }
            }
            Ok(Value::Null)
        }
        "pull" => {
            let branch = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
            let upstream = git(root, &["rev-parse", "--abbrev-ref", "@{upstream}"])?;
            let before = git(root, &["rev-parse", "HEAD"])?;
            // 只允许快进，冲突或分叉交由原页面显示真实失败，不自动改写用户提交。
            let output = crate::workspace::run_git_with_timeout(root, &["pull", "--ff-only"])?;
            if !output.status.success() {
                return Err(keencode_model::redact_error_secrets(
                    &String::from_utf8_lossy(&output.stderr),
                ));
            }
            let after = git(root, &["rev-parse", "HEAD"])?;
            Ok(
                json!({"status":if before == after {"skipped_up_to_date"}else{"pulled"},"branch":branch.trim(),"upstreamBranch":upstream.trim()}),
            )
        }
        _ => Err("未知 Git 页面操作".into()),
    }
}

#[tauri::command]
pub async fn ui_git_command(
    app: AppHandle,
    cwd: String,
    operation: String,
    branch: Option<String>,
    publish: Option<bool>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &cwd)?;
        command(
            &root,
            &operation,
            branch.as_deref(),
            publish.unwrap_or(false),
        )
    })
    .await
    .map_err(|error| format!("Git 操作后台任务失败：{error}"))?
}

#[tauri::command]
pub async fn ui_git_blame(
    app: AppHandle,
    cwd: String,
    file: String,
    line: u32,
    revision: Option<String>,
    base: Option<String>,
) -> Result<Value, String> {
    if base.as_deref().is_some_and(|base| base != "branch") {
        return Err("Git 行归属基线无效".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &cwd)?;
        blame(&root, &file, line, revision.as_deref(), base.is_some())
    })
    .await
    .map_err(|error| format!("Git 行归属查询失败：{error}"))?
}

pub(super) fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let args: Vec<&str> = std::iter::once("--literal-pathspecs")
        .chain(args.iter().copied())
        .collect();
    let output = crate::workspace::run_git(root, &args)?;
    if !output.status.success() {
        let detail = keencode_model::redact_error_secrets(&String::from_utf8_lossy(&output.stderr));
        // Git hook 可以只返回非零退出码；原错误契约仍必须有可显示的消息。
        return Err(if detail.trim().is_empty() {
            format!("Git 操作失败，退出码 {:?}", output.status.code())
        } else {
            detail
        });
    }
    if output.stdout.len() > 16 * 1024 * 1024 {
        return Err("Git 查询结果超过大小限制".to_owned());
    }
    String::from_utf8(output.stdout).map_err(|_| "Git 返回非 UTF-8 数据".to_owned())
}

/// 未跟踪文件和尚无 HEAD 的仓库以空文件为基线，展示真实工作区内容。
fn new_file_patches(
    root: &Path,
    file: Option<&str>,
    include_cached: bool,
    max_bytes: usize,
) -> Result<String, String> {
    let mut args = vec!["ls-files", "-z", "--others", "--exclude-standard"];
    if include_cached {
        args.push("--cached");
    }
    args.push("--");
    if let Some(file) = file {
        args.push(file);
    }
    let paths = git(root, &args)?;
    let mut patch = String::new();
    // --cached 与 --others 可能合并返回；只读一次每个路径，不执行外部 diff 工具。
    let unique: std::collections::BTreeSet<_> =
        paths.split('\0').filter(|path| !path.is_empty()).collect();
    for path in unique {
        if !root.join(path).is_file() {
            continue;
        }
        let output = crate::workspace::run_git(
            root,
            &[
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-renames",
                "--",
                "/dev/null",
                path,
            ],
        )?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err("无法读取新文件差异".to_owned());
        }
        if output.stdout.len() > 16 * 1024 * 1024 {
            return Err("Git 查询结果超过大小限制".to_owned());
        }
        let content =
            String::from_utf8(output.stdout).map_err(|_| "Git 返回非 UTF-8 数据".to_owned())?;
        patch.push_str(&content);
        if patch.len() > max_bytes {
            break;
        }
    }
    Ok(patch)
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

pub(super) fn valid_file(file: &str) -> Result<(), String> {
    let path = Path::new(file);
    if file.is_empty()
        || file.len() > 2048
        || path.is_absolute()
        || file.chars().any(char::is_control)
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err("Git 文件路径必须位于项目内".to_owned());
    }
    Ok(())
}

/// 分支比较和基线文件使用同一 merge-base；只查已有本地引用，不隐式联网 fetch。
fn branch_base(root: &Path) -> Result<String, String> {
    let branch = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
    let configured = if branch.trim().is_empty() {
        None
    } else {
        git(
            root,
            &[
                "config",
                "--get",
                &format!("branch.{}.gh-merge-base", branch.trim()),
            ],
        )
        .ok()
    };
    let upstream = git(root, &["rev-parse", "--verify", "@{upstream}"]).ok();
    let default = git(
        root,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .ok();
    for candidate in configured
        .into_iter()
        .chain(upstream)
        .chain(default)
        .chain(["refs/heads/main".to_owned(), "refs/heads/master".to_owned()])
    {
        let candidate = candidate.trim();
        valid_revision(candidate)?;
        if let Ok(base) = git(root, &["merge-base", candidate, "HEAD"]) {
            return Ok(base.trim().to_owned());
        }
    }
    Err("无法确定分支比较基线，请先配置上游或默认分支".to_owned())
}

fn recent_commits(root: &Path, limit: usize) -> Result<Value, String> {
    if limit == 0 || limit > 50 {
        return Err("Git 提交查询数量必须为 1 到 50".into());
    }
    if git(root, &["rev-parse", "--verify", "HEAD"]).is_err() {
        return Ok(json!({"commits": []}));
    }
    let raw = git(
        root,
        &[
            "log",
            "--no-show-signature",
            "-z",
            &format!("-n{limit}"),
            "--format=%H%x00%h%x00%s%x00%cI",
        ],
    )?;
    let values: Vec<_> = raw.split('\0').collect();
    let commits: Vec<_> = values
        .chunks(4)
        .filter(|v| v.len() == 4 && !v[0].is_empty())
        .map(|v| json!({"sha":v[0], "shortSha":v[1], "subject":v[2], "committedAt":v[3]}))
        .collect();
    Ok(json!({"commits":commits}))
}

/// 全部路径先校验，再用一次固定 Git 命令修改 index，避免无效成员导致部分暂存。
fn mutate(root: &Path, operation: &str, paths: &[String]) -> Result<Value, String> {
    if paths.is_empty() || paths.len() > 4096 {
        return Err("Git 文件列表为空或超过限制".into());
    }
    for path in paths {
        valid_file(path)?;
    }
    let has_head = git(root, &["rev-parse", "--verify", "HEAD"]).is_ok();
    let mut args = match operation {
        "stage" => vec!["add", "--"],
        "unstage" if has_head => vec!["restore", "--staged", "--"],
        // 初始仓库不能 reset HEAD；只从 index 删除，保留实际工作区文件。
        "unstage" => vec!["rm", "--cached", "--force", "--ignore-unmatch", "--"],
        _ => return Err("未知 Git index 操作".into()),
    };
    args.extend(paths.iter().map(String::as_str));
    git(root, &args)?;
    Ok(json!({"ok":true}))
}

/// numstat 使用 NUL 分隔并禁用 rename 推断，文件名中的制表符/换行不影响解析。
fn stats(raw: &str) -> BTreeMap<String, (u64, u64)> {
    let mut files = BTreeMap::new();
    for entry in raw.split('\0').filter(|s| !s.is_empty()) {
        let mut columns = entry.splitn(3, '\t');
        let added = columns
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let deleted = columns
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if let Some(path) = columns.next() {
            let entry = files.entry(path.to_owned()).or_insert((0u64, 0u64));
            entry.0 = entry.0.saturating_add(added);
            entry.1 = entry.1.saturating_add(deleted);
        }
    }
    files
}

pub(crate) fn query(
    root: &Path,
    operation: &str,
    scope: &str,
    revision: Option<&str>,
    file: Option<&str>,
    max_bytes: usize,
) -> Result<Value, String> {
    if let Some(revision) = revision {
        valid_revision(revision)?;
    }
    if let Some(file) = file {
        valid_file(file)?;
    }
    let is_repo = git(root, &["rev-parse", "--is-inside-work-tree"]).is_ok();
    match operation {
        "branches" => {
            if !is_repo {
                return Ok(json!({"branches": [], "isRepo": false, "hasOriginRemote": false}));
            }
            let current =
                git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
            let default = git(
                root,
                &[
                    "symbolic-ref",
                    "--quiet",
                    "--short",
                    "refs/remotes/origin/HEAD",
                ],
            )
            .unwrap_or_default();
            let raw = git(
                root,
                &[
                    "for-each-ref",
                    "--format=%(refname:short)",
                    "refs/heads",
                    "refs/remotes",
                ],
            )?;
            let worktrees = git(root, &["worktree", "list", "--porcelain"])?;
            let mut paths = BTreeMap::new();
            let mut worktree = String::new();
            for line in worktrees.lines() {
                if let Some(path) = line.strip_prefix("worktree ") {
                    worktree = path.replace('\\', "/");
                }
                if let Some(branch) = line.strip_prefix("branch refs/heads/") {
                    paths.insert(branch.to_owned(), worktree.clone());
                }
            }
            let branches: Vec<Value> = raw.lines().filter(|name| !name.ends_with("/HEAD")).map(|name| {
                let remote = git(root, &["show-ref", "--verify", "--quiet", &format!("refs/remotes/{name}")]).is_ok();
                let mut branch = json!({"name": name, "isRemote": remote, "current": name == current.trim(),
                    "isDefault": name == default.trim() || format!("origin/{name}") == default.trim(), "worktreePath": paths.get(name)});
                // 原契约的 remoteName 是可选字符串，本地分支必须缺省，不能返回 null。
                if remote { branch["remoteName"] = json!(name.split('/').next()); }
                branch
            }).collect();
            Ok(
                json!({"branches": branches, "isRepo": true, "hasOriginRemote": git(root, &["remote", "get-url", "origin"]).is_ok()}),
            )
        }
        "status" => {
            if !is_repo {
                return Ok(
                    json!({"branch": null, "hasWorkingTreeChanges": false, "workingTree": {"files": [], "insertions": 0, "deletions": 0}, "hasUpstream": false, "upstreamBranch": null, "aheadCount": 0, "behindCount": 0, "pr": null}),
                );
            }
            let branch = git(root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
                .ok()
                .map(|s| s.trim().to_owned());
            let changes = git(
                root,
                &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
            )?;
            let has_head = git(root, &["rev-parse", "--verify", "HEAD"]).is_ok();
            let mut files = if has_head {
                stats(&git(
                    root,
                    &["diff", "--numstat", "-z", "--no-renames", "HEAD"],
                )?)
            } else {
                // 首次提交前基线为空树；不能把暂存与未暂存统计相加重复计数。
                BTreeMap::new()
            };
            // 二进制和未跟踪路径同样属于真实变更，行数没有 numstat 时保持零。
            let mut entries = changes.split('\0');
            while let Some(entry) = entries.next() {
                if entry.len() < 4 {
                    continue;
                }
                let path = &entry[3..];
                files.entry(path.to_owned()).or_insert((0, 0));
                if (entry.starts_with("?? ") || !has_head) && root.join(path).is_file() {
                    let output = crate::workspace::run_git(
                        root,
                        &[
                            "diff",
                            "--no-index",
                            "--numstat",
                            "-z",
                            "--",
                            "/dev/null",
                            path,
                        ],
                    )?;
                    if !matches!(output.status.code(), Some(0 | 1)) {
                        return Err("无法读取未跟踪文件的行数".to_owned());
                    }
                    let counts = stats(&String::from_utf8_lossy(&output.stdout));
                    files.insert(
                        path.to_owned(),
                        (
                            counts.values().map(|v| v.0).sum(),
                            counts.values().map(|v| v.1).sum(),
                        ),
                    );
                }
                if entry.as_bytes()[..2].contains(&b'R') || entry.as_bytes()[..2].contains(&b'C') {
                    entries.next();
                }
            }
            let upstream = git(
                root,
                &[
                    "rev-parse",
                    "--abbrev-ref",
                    "--symbolic-full-name",
                    "@{upstream}",
                ],
            )
            .ok()
            .map(|s| s.trim().to_owned());
            let (ahead, behind) = if upstream.is_some() {
                let counts = git(
                    root,
                    &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
                )?;
                let mut values = counts.split_whitespace();
                (
                    values
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or("Git ahead 计数无效")?,
                    values
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .ok_or("Git behind 计数无效")?,
                )
            } else {
                (0, 0)
            };
            Ok(
                json!({"branch": branch, "hasWorkingTreeChanges": !changes.is_empty(), "workingTree": {"files": files.iter().map(|(path,(insertions,deletions))| json!({"path":path,"insertions":insertions,"deletions":deletions})).collect::<Vec<_>>(), "insertions": files.values().map(|v| v.0).sum::<u64>(), "deletions": files.values().map(|v| v.1).sum::<u64>()}, "hasUpstream": upstream.is_some(), "upstreamBranch": upstream, "aheadCount": ahead, "behindCount": behind, "pr": null}),
            )
        }
        "diff" | "diff-stats" => {
            let mut args = vec!["diff", "--no-ext-diff", "--no-renames"];
            let base = if scope == "branch" {
                Some(branch_base(root)?)
            } else {
                None
            };
            let has_head = git(root, &["rev-parse", "--verify", "HEAD"]).is_ok();
            match scope {
                "unstaged" => {}
                "staged" => args.push("--cached"),
                "workingTree" => {
                    if has_head {
                        args.push("HEAD");
                    }
                }
                "ref" => args.push(revision.ok_or("缺少比较 revision")?),
                "branch" => args.push(base.as_deref().ok_or("缺少分支比较基线")?),
                _ => return Err("未知 Git diff 作用域".to_owned()),
            }
            if operation == "diff-stats" {
                args.extend(["--numstat", "-z"]);
            }
            args.push("--");
            if let Some(file) = file {
                args.push(file);
            }
            let mut patch = if scope == "workingTree" && !has_head {
                String::new()
            } else {
                git(root, &args)?
            };
            if operation == "diff-stats" {
                let mut counts = stats(&patch);
                if matches!(scope, "workingTree" | "unstaged" | "branch" | "ref") {
                    let mut list_args = vec!["ls-files", "-z", "--others", "--exclude-standard"];
                    if scope == "workingTree" && !has_head {
                        list_args.push("--cached");
                    }
                    list_args.push("--");
                    if let Some(file) = file {
                        list_args.push(file);
                    }
                    let paths = git(root, &list_args)?;
                    for path in paths.split('\0').filter(|p| !p.is_empty()) {
                        if !root.join(path).is_file() {
                            continue;
                        }
                        let output = crate::workspace::run_git(
                            root,
                            &[
                                "diff",
                                "--no-index",
                                "--no-ext-diff",
                                "--numstat",
                                "-z",
                                "--",
                                "/dev/null",
                                path,
                            ],
                        )?;
                        if !matches!(output.status.code(), Some(0 | 1)) {
                            return Err("无法计算新文件差异".into());
                        }
                        let raw = String::from_utf8(output.stdout)
                            .map_err(|_| "Git 返回非 UTF-8 数据")?;
                        let count = stats(&raw).values().copied().next().unwrap_or((0, 0));
                        counts.insert(path.to_owned(), count);
                    }
                }
                return Ok(
                    json!({"additions":counts.values().map(|v|v.0).sum::<u64>(), "deletions":counts.values().map(|v|v.1).sum::<u64>(), "fileCount":counts.len()}),
                );
            }
            if matches!(scope, "workingTree" | "unstaged" | "branch" | "ref")
                && patch.len() <= max_bytes
            {
                patch.push_str(&new_file_patches(
                    root,
                    file,
                    scope == "workingTree" && !has_head,
                    max_bytes - patch.len(),
                )?);
            }
            let truncated = patch.len() > max_bytes;
            if truncated {
                let mut end = max_bytes;
                while !patch.is_char_boundary(end) {
                    end -= 1;
                }
                patch.truncate(end);
            }
            Ok(json!({"patch":patch,"truncated":truncated}))
        }
        "read-file" => {
            let file = file.ok_or("缺少文件路径")?;
            let resolved = if scope == "index" {
                "index".to_owned()
            } else if scope == "branch" {
                branch_base(root)?
            } else if revision.is_none() && git(root, &["rev-parse", "--verify", "HEAD"]).is_err() {
                return Ok(
                    json!({"contents":"", "resolvedRev":"", "missing":true, "truncated":false}),
                );
            } else {
                git(root, &["rev-parse", "--verify", revision.unwrap_or("HEAD")])?
                    .trim()
                    .to_owned()
            };
            let spec = if scope == "index" {
                format!(":{file}")
            } else {
                format!("{resolved}:{file}")
            };
            if git(root, &["cat-file", "-e", &spec]).is_err() {
                return Ok(
                    json!({"contents":"", "resolvedRev":resolved, "missing":true, "truncated":false}),
                );
            }
            let mut content = git(root, &["show", &spec])?;
            let truncated = content.len() > max_bytes;
            if truncated {
                let mut end = max_bytes;
                while !content.is_char_boundary(end) {
                    end -= 1;
                }
                content.truncate(end);
            }
            Ok(
                json!({"contents":content,"resolvedRev":resolved,"missing":false,"truncated":truncated}),
            )
        }
        _ => Err("未知 Git 页面查询".to_owned()),
    }
}

#[tauri::command]
pub async fn ui_git_recent_commits(
    app: AppHandle,
    cwd: String,
    limit: Option<usize>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &cwd)?;
        recent_commits(&root, limit.unwrap_or(7))
    })
    .await
    .map_err(|e| format!("Git 提交查询失败：{e}"))?
}

#[tauri::command]
pub async fn ui_git_mutate(
    app: AppHandle,
    cwd: String,
    operation: String,
    paths: Vec<String>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &cwd)?;
        mutate(&root, &operation, &paths)
    })
    .await
    .map_err(|e| format!("Git index 修改失败：{e}"))?
}

#[tauri::command]
pub async fn ui_git_query(
    app: AppHandle,
    cwd: String,
    operation: String,
    scope: Option<String>,
    revision: Option<String>,
    file: Option<String>,
    max_bytes: Option<usize>,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::read_only_workspace_root(&app, &cwd)?;
        query(
            &root,
            &operation,
            scope.as_deref().unwrap_or("workingTree"),
            revision.as_deref(),
            file.as_deref(),
            max_bytes.unwrap_or(1_000_000).clamp(1, 1_000_000),
        )
    })
    .await
    .map_err(|error| format!("Git 页面查询失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit_fixture(root: &Path, message: &str) {
        git(root, &["add", "--all"]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=UI",
                "-c",
                "user.email=ui@example.invalid",
                "commit",
                "-m",
                message,
            ],
        )
        .unwrap();
    }

    #[test]
    fn blame_preserves_committed_and_uncommitted_lines_and_literal_paths() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        command(root, "init", None, false).unwrap();
        std::fs::write(root.join("file[1].txt"), "base\n").unwrap();
        commit_fixture(root, "测试：行归属 / test: line attribution");
        let committed = blame(root, "file[1].txt", 1, None, false).unwrap();
        assert_eq!(committed["author"], "UI");
        assert_eq!(committed["authorEmail"], "ui@example.invalid");
        assert_eq!(committed["uncommitted"], false);
        std::fs::write(root.join("file[1].txt"), "base\nnew\n").unwrap();
        assert_eq!(
            blame(root, "file[1].txt", 2, None, false).unwrap()["uncommitted"],
            true
        );
        assert!(blame(root, "file[1].txt", 0, None, false).is_err());
        assert!(blame(root, "../escape", 1, None, false).is_err());
        assert!(blame(root, "file[1].txt", 1, Some("--contents=outside"), false).is_err());
    }

    #[test]
    fn create_branch_publishes_exact_branch_without_switching_current_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        let remote = directory.path().join("remote.git");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&remote).unwrap();
        git(&root, &["init", "--initial-branch=main"]).unwrap();
        git(&remote, &["init", "--bare"]).unwrap();
        std::fs::write(root.join("file.txt"), "base\n").unwrap();
        commit_fixture(&root, "测试：分支发布 / test: branch publication");
        git(
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        )
        .unwrap();
        command(&root, "create-branch", Some("feat/ui-created"), true).unwrap();
        assert_eq!(
            git(&root, &["symbolic-ref", "--short", "HEAD"])
                .unwrap()
                .trim(),
            "main"
        );
        assert_eq!(
            git(&root, &["rev-parse", "feat/ui-created"]).unwrap(),
            git(&remote, &["rev-parse", "refs/heads/feat/ui-created"]).unwrap()
        );
        assert!(command(&root, "create-branch", Some("--force"), false).is_err());
        assert!(command(&root, "create-branch", Some("feat/ui-created"), false).is_err());
    }

    #[test]
    fn pull_fast_forwards_and_refuses_diverged_history_without_rewriting_local_commit() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        let other = directory.path().join("other");
        let remote = directory.path().join("remote.git");
        for path in [&root, &remote] {
            std::fs::create_dir(path).unwrap();
        }
        git(&root, &["init", "--initial-branch=main"]).unwrap();
        git(&remote, &["init", "--bare", "--initial-branch=main"]).unwrap();
        // 临时仓库固定换行策略，避免机器级 autocrlf 改变验收文件的字节。
        git(&root, &["config", "core.autocrlf", "false"]).unwrap();
        std::fs::write(root.join("file.txt"), "base\n").unwrap();
        commit_fixture(&root, "测试：拉取基线 / test: pull baseline");
        git(
            &root,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        )
        .unwrap();
        git(&root, &["push", "--set-upstream", "origin", "main"]).unwrap();
        git(
            directory.path(),
            &["clone", remote.to_str().unwrap(), other.to_str().unwrap()],
        )
        .unwrap();
        std::fs::write(other.join("file.txt"), "remote\n").unwrap();
        commit_fixture(&other, "测试：远端修改 / test: remote change");
        git(&other, &["push"]).unwrap();
        assert_eq!(
            command(&root, "pull", None, false).unwrap()["status"],
            "pulled"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "remote\n"
        );
        assert_eq!(
            command(&root, "pull", None, false).unwrap()["status"],
            "skipped_up_to_date"
        );
        std::fs::write(root.join("local.txt"), "local\n").unwrap();
        commit_fixture(&root, "测试：本地提交 / test: local commit");
        let local_head = git(&root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(other.join("other.txt"), "remote\n").unwrap();
        commit_fixture(&other, "测试：分叉提交 / test: diverged commit");
        git(&other, &["push"]).unwrap();
        assert!(command(&root, "pull", None, false).is_err());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).unwrap(), local_head);
    }
    #[test]
    fn index_mutations_preserve_working_files_and_validate_all_paths_first() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        assert!(mutate(root, "stage", &["file.txt".into(), "../escape".into()]).is_err());
        assert!(git(root, &["ls-files"]).unwrap().is_empty());
        mutate(root, "stage", &["file.txt".into()]).unwrap();
        std::fs::write(root.join("file.txt"), "final\nsecond\n").unwrap();
        let staged = query(root, "diff-stats", "staged", None, None, 1000).unwrap();
        assert_eq!(staged["additions"], 1);
        let final_stats = query(root, "diff-stats", "workingTree", None, None, 1000).unwrap();
        assert_eq!(final_stats["additions"], 2);
        assert_eq!(final_stats["fileCount"], 1);
        mutate(root, "unstage", &["file.txt".into()]).unwrap();
        assert!(git(root, &["ls-files"]).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "final\nsecond\n"
        );
        assert_eq!(
            query(
                root,
                "read-file",
                "workingTree",
                None,
                Some("file.txt"),
                1000
            )
            .unwrap()["missing"],
            true
        );
        assert_eq!(recent_commits(root, 7).unwrap()["commits"], json!([]));
        assert!(recent_commits(root, 51).is_err());
    }

    #[test]
    fn branch_baseline_file_and_diff_use_same_merge_base_with_recent_commits() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(root.join("file.txt"), "base\n").unwrap();
        git(root, &["add", "file.txt"]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=UI",
                "-c",
                "user.email=ui@example.invalid",
                "commit",
                "-m",
                "测试：基线 / test: baseline",
            ],
        )
        .unwrap();
        let base = git(root, &["rev-parse", "HEAD"]).unwrap().trim().to_owned();
        git(root, &["checkout", "-b", "feat/ui-proof"]).unwrap();
        std::fs::write(root.join("file.txt"), "base\nfeature\n").unwrap();
        git(root, &["add", "file.txt"]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=UI",
                "-c",
                "user.email=ui@example.invalid",
                "commit",
                "-m",
                "测试：功能 / test: feature",
            ],
        )
        .unwrap();
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        assert_eq!(branch_base(root).unwrap(), base);
        let file = query(root, "read-file", "branch", None, Some("file.txt"), 1000).unwrap();
        assert_eq!(file["resolvedRev"], base);
        assert_eq!(file["contents"], "base\n");
        let diff = query(root, "diff", "branch", None, None, 10000).unwrap();
        assert!(diff["patch"].as_str().unwrap().contains("+feature"));
        assert!(diff["patch"].as_str().unwrap().contains("+new"));
        let counts = query(root, "diff-stats", "branch", None, None, 1).unwrap();
        assert_eq!(counts["additions"], 2);
        assert_eq!(counts["fileCount"], 2);
        let commits = recent_commits(root, 1).unwrap();
        assert_eq!(commits["commits"].as_array().unwrap().len(), 1);
        assert_eq!(
            commits["commits"][0]["subject"],
            "测试：功能 / test: feature"
        );
        mutate(root, "stage", &["new.txt".into()]).unwrap();
        assert_eq!(
            query(root, "read-file", "index", None, Some("new.txt"), 1000).unwrap()["resolvedRev"],
            "index"
        );
        mutate(root, "unstage", &["new.txt".into()]).unwrap();
        assert!(root.join("new.txt").is_file());
    }
    #[test]
    fn numstat_keeps_unusual_names_and_binary_files() {
        let data = stats("2\t1\t中文\t文件\n.txt\0-\t-\timage.png\0");
        assert_eq!(data["中文\t文件\n.txt"], (2, 1));
        assert_eq!(data["image.png"], (0, 0));
    }
    #[test]
    fn revision_cannot_be_a_git_option() {
        assert!(valid_revision("--output=secret").is_err());
        assert!(valid_revision("HEAD\n--other").is_err());
        assert!(valid_revision("HEAD~2").is_ok());
    }
    #[test]
    fn repository_queries_report_real_changes_and_revision_content() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(root.join("file.txt"), "first\n").unwrap();
        git(root, &["add", "file.txt"]).unwrap();
        git(
            root,
            &[
                "-c",
                "user.name=UI test",
                "-c",
                "user.email=ui@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        std::fs::write(root.join("file.txt"), "first\nsecond\n").unwrap();
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        let status = query(root, "status", "workingTree", None, None, 1000).unwrap();
        assert_eq!(status["branch"], "main");
        assert_eq!(status["hasWorkingTreeChanges"], true);
        assert_eq!(status["workingTree"]["insertions"], 2);
        assert_eq!(status["hasUpstream"], false);
        let branches = query(root, "branches", "workingTree", None, None, 1000).unwrap();
        let main = branches["branches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|branch| branch["name"] == "main")
            .unwrap();
        assert_eq!(main["current"], true);
        assert!(main.get("remoteName").is_none());
        let diff = query(root, "diff", "workingTree", None, Some("file.txt"), 1000).unwrap();
        assert!(diff["patch"].as_str().unwrap().contains("+second"));
        let diff = query(root, "diff", "workingTree", None, None, 10000).unwrap();
        assert!(diff["patch"].as_str().unwrap().contains("+new"));
        let only_new = query(root, "diff", "workingTree", None, Some("new.txt"), 10000).unwrap();
        assert!(only_new["patch"].as_str().unwrap().contains("+new"));
        assert!(!only_new["patch"].as_str().unwrap().contains("+second"));
        let file = query(
            root,
            "read-file",
            "workingTree",
            Some("HEAD"),
            Some("file.txt"),
            1000,
        )
        .unwrap();
        assert_eq!(file["contents"], "first\n");
        assert_eq!(file["missing"], false);
        let missing = query(
            root,
            "read-file",
            "workingTree",
            Some("HEAD"),
            Some("absent.txt"),
            1000,
        )
        .unwrap();
        assert_eq!(missing["missing"], true);
        assert!(
            query(
                root,
                "read-file",
                "workingTree",
                None,
                Some("../secret.txt"),
                1000
            )
            .is_err()
        );
    }

    #[test]
    fn unborn_working_tree_diff_uses_final_contents_without_duplicate_index_patch() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        git(root, &["init", "--initial-branch=main"]).unwrap();
        std::fs::write(root.join("file.txt"), "staged\n").unwrap();
        git(root, &["add", "file.txt"]).unwrap();
        std::fs::write(root.join("file.txt"), "current\n").unwrap();
        let diff = query(root, "diff", "workingTree", None, None, 10000).unwrap();
        let patch = diff["patch"].as_str().unwrap();
        assert!(patch.contains("+current"));
        assert!(!patch.contains("staged"));
        assert_eq!(patch.matches("diff --git ").count(), 1);
        let status = query(root, "status", "workingTree", None, None, 10000).unwrap();
        assert_eq!(status["workingTree"]["insertions"], 1);
        assert_eq!(status["workingTree"]["deletions"], 0);
    }
}
