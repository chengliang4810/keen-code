//! Git linked worktree 的原生生命周期服务。

use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::native_paths::NativePaths;

use super::git::git;

const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub is_main: bool,
    pub locked: bool,
    pub prunable: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorktreeRequest {
    pub root: PathBuf,
    pub reference: String,
    pub path: Option<PathBuf>,
    pub new_branch: Option<String>,
    pub copy_changes_from: Option<PathBuf>,
    pub checkout_branch: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeResult {
    pub path: PathBuf,
    pub reference: String,
    pub branch: Option<String>,
    pub copied_changes: bool,
}

#[derive(Clone)]
pub struct NativeWorktrees {
    paths: Arc<NativePaths>,
}

impl NativeWorktrees {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &Arc<NativePaths> {
        &self.paths
    }

    pub fn list(&self, root: &Path) -> Result<Vec<WorktreeEntry>, String> {
        let root = canonical_directory(root)?;
        parse_worktrees(&git(&root, &["worktree", "list", "--porcelain"])?, &root)
    }

    pub fn create(&self, input: WorktreeRequest) -> Result<WorktreeResult, String> {
        let root = canonical_directory(&input.root)?;
        validate_reference(&input.reference)?;
        if let Some(branch) = &input.new_branch {
            validate_reference(branch)?;
            git(&root, &["check-ref-format", "--branch", branch])?;
        } else if input.checkout_branch {
            git(&root, &["check-ref-format", "--branch", &input.reference])?;
            git(
                &root,
                &[
                    "show-ref",
                    "--verify",
                    &format!("refs/heads/{}", input.reference),
                ],
            )?;
        }
        let commit = git(
            &root,
            &[
                "rev-parse",
                "--verify",
                &format!("{}^{{commit}}", input.reference),
            ],
        )?;
        let target = target_path(&root, input.path.as_deref())?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err("工作树目标目录已存在，未修改".to_owned());
        }
        let changes = input
            .copy_changes_from
            .as_deref()
            .map(|path| snapshot_changes(&root, path))
            .transpose()?;
        // 只有 new_branch 成功创建的分支才允许回滚删除；checkout_branch 可能指向既有分支。
        let created_branch = if let Some(branch) = input.new_branch.as_deref() {
            git(&root, &["branch", "--", branch, commit.trim()])?;
            Some(branch.to_owned())
        } else {
            None
        };
        let branch = if let Some(branch) = created_branch.as_deref() {
            Some(branch.to_owned())
        } else if input.checkout_branch {
            Some(input.reference.clone())
        } else {
            None
        };
        // Git for Windows 拒绝 Rust 的 `\\?\\` 扩展路径前缀作为工作树目标，
        // 但文件系统 API 仍需要该前缀来访问长路径；这里只转换传给 Git 的文本。
        let target_text = git_path(&target);
        let mut args = vec!["worktree", "add"];
        if branch.is_none() {
            args.push("--detach");
        }
        args.extend([
            "--",
            &target_text,
            branch.as_deref().unwrap_or(commit.trim()),
        ]);
        if let Err(error) = git(&root, &args) {
            let cleanup = rollback_created_worktree(&root, &target, created_branch.as_deref());
            return Err(match cleanup {
                Ok(()) => format!("工作树创建失败，已回滚新建分支：{error}"),
                Err(cleanup_error) => {
                    format!("工作树创建失败：{error}；回滚新建分支也失败：{cleanup_error}")
                }
            });
        }
        let target = match fs::canonicalize(&target) {
            Ok(target) => target,
            Err(error) => {
                let cleanup = rollback_created_worktree(&root, &target, created_branch.as_deref());
                return Err(match cleanup {
                    Ok(()) => format!("工作树路径解析失败，已回滚：{error}"),
                    Err(cleanup_error) => {
                        format!("工作树路径解析失败：{error}；回滚也失败：{cleanup_error}")
                    }
                });
            }
        };
        if let Err(error) = register_managed(&self.paths.data_root, &root, &target) {
            let cleanup = rollback_created_worktree(&root, &target, created_branch.as_deref());
            return Err(match cleanup {
                Ok(()) => format!("工作树登记失败，已回滚 linked worktree：{error}"),
                Err(cleanup_error) => {
                    format!("工作树登记失败：{error}；回滚 linked worktree 也失败：{cleanup_error}")
                }
            });
        }
        if let Some(changes) = changes
            && let Err(error) = copy_changes(&target, changes)
        {
            return Err(format!(
                "本地修改复制失败，工作树已保留在 {}：{error}",
                target.display()
            ));
        }
        Ok(WorktreeResult {
            path: target,
            reference: input.reference,
            branch,
            copied_changes: input.copy_changes_from.is_some(),
        })
    }

    pub fn remove(&self, root: &Path, target: &Path, force: bool) -> Result<(), String> {
        let root = canonical_directory(root)?;
        reject_link_target(target)?;
        let target = validate_linked_worktree(&root, target)?;
        if !force {
            let status = git(&target, &["status", "--porcelain", "-z"])?;
            if !status.is_empty() {
                return Err("工作树有未提交修改；确认后才可强制移除".to_owned());
            }
            if has_ignored_files(&target)? {
                return Err("工作树包含 ignored 文件；确认后才可强制移除".to_owned());
            }
        }
        let target = git_path(&target);
        if force {
            git(&root, &["worktree", "remove", "--force", "--", &target])?;
        } else {
            git(&root, &["worktree", "remove", "--", &target])?;
        }
        Ok(())
    }

    /// 在同一仓库内完成会话目录交接；stash 以对象 SHA 标识，切换失败会尝试
    /// 恢复原始 index 和工作文件，恢复失败时保留 stash 供人工处理。
    pub fn handoff(
        &self,
        source: &Path,
        target: &Path,
        target_branch: Option<&str>,
    ) -> Result<PathBuf, String> {
        let source = canonical_directory(source)?;
        let target = canonical_directory(target)?;
        let root = common_root(&source)?;
        if common_root(&target)? != root {
            return Err("交接目标不是同一 Git 仓库的工作树".to_owned());
        }
        if let Some(branch) = target_branch {
            validate_reference(branch)?;
        }
        let saved = stash_if_dirty(&source)?;
        let result = match target_branch {
            Some(branch) => match git(&target, &["switch", "--", branch]) {
                Ok(_) => target.clone(),
                Err(error) => {
                    if let Some(hash) = saved.as_deref() {
                        return Err(match restore_stash(&source, hash) {
                            Ok(()) => format!(
                                "目标工作树切换失败；源目录修改已恢复，stash {hash} 仍保留：{error}"
                            ),
                            Err(restore_error) => format!(
                                "目标工作树切换失败，源目录 stash {hash} 恢复也失败：{error}；{restore_error}"
                            ),
                        });
                    }
                    return Err(error);
                }
            },
            None => target.clone(),
        };
        if let Some(hash) = saved {
            if let Err(error) = restore_stash(&target, &hash) {
                return Err(format!(
                    "工作树已切换，但 stash {hash} 恢复失败，修改仍保留在 stash：{error}"
                ));
            }
            if let Err(error) = drop_stash(&target, &hash) {
                return Err(format!(
                    "工作树已切换且 stash {hash} 已恢复，但清理 stash 失败，stash 仍保留：{error}"
                ));
            }
        }
        Ok(result)
    }

    /// 先写入受控回执，再移除 checkout；回执用于崩溃恢复，不包含工作文件正文。
    pub fn archive(&self, root: &Path, target: &Path, session_id: &str) -> Result<PathBuf, String> {
        if session_id.is_empty()
            || session_id.len() > 128
            || session_id.chars().any(char::is_control)
        {
            return Err("归档会话标识无效".to_owned());
        }
        let root = canonical_directory(root)?;
        reject_link_target(target)?;
        let target = validate_linked_worktree(&root, target)?;
        validate_managed(&self.paths.data_root, &root, &target)?;
        let head = git(&target, &["rev-parse", "--verify", "HEAD^{commit}"])?;
        let branch = git(&target, &["branch", "--show-current"]).ok();
        let receipt_dir = self.paths.data_root.join("native-worktree-archive");
        fs::create_dir_all(&receipt_dir).map_err(|error| error.to_string())?;
        let receipt = receipt_dir.join(format!("{}.json", sha256_hex(session_id.as_bytes())));
        let payload = serde_json::json!({
            "sessionId": session_id,
            "root": root,
            "path": target,
            "head": head.trim(),
            "branch": branch.map(|value| value.trim().to_owned()),
            "phase": "prepared",
        });
        let bytes = serde_json::to_vec(&payload).map_err(|error| error.to_string())?;
        let temporary = receipt.with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        fs::rename(&temporary, &receipt).map_err(|error| error.to_string())?;
        let target_text = git_path(&target);
        if let Err(error) = git(&root, &["worktree", "remove", "--", &target_text]) {
            return Err(format!(
                "工作树未删除，归档回执保留在 {}：{error}",
                receipt.display()
            ));
        }
        Ok(receipt)
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let path = path.canonicalize().map_err(|error| error.to_string())?;
    if !path.is_dir() {
        return Err("工作树路径不是目录".to_owned());
    }
    Ok(path)
}

fn common_root(path: &Path) -> Result<PathBuf, String> {
    let value = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Path::new(value.trim())
        .canonicalize()
        .map_err(|error| error.to_string())
}

fn parse_worktrees(raw: &str, root: &Path) -> Result<Vec<WorktreeEntry>, String> {
    Ok(raw
        .replace("\r\n", "\n")
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
                    path = Some(PathBuf::from(value.trim()));
                } else if let Some(value) = line.strip_prefix("HEAD ") {
                    head = Some(value.trim().to_owned());
                } else if let Some(value) = line.strip_prefix("branch ") {
                    branch = Some(
                        value
                            .trim()
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value.trim())
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
            path.map(|path| WorktreeEntry {
                is_main: path.canonicalize().is_ok_and(|candidate| candidate == root),
                path,
                head,
                branch: if detached { None } else { branch },
                detached,
                locked,
                prunable,
            })
        })
        .collect())
}

fn target_path(root: &Path, target: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(target) = target {
        if !target.is_absolute()
            || target
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err("工作树目标必须是绝对目录且不能含 ..".to_owned());
        }
        let parent = target
            .parent()
            .ok_or("工作树目标缺少父目录")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        return Ok(parent.join(target.file_name().ok_or("工作树目标缺少名称")?));
    }
    let name = root
        .file_name()
        .ok_or("项目没有目录名称")?
        .to_string_lossy();
    let nonce = unique_nonce();
    Ok(root
        .parent()
        .ok_or("项目没有父目录")?
        .join(format!("{name}-worktree-{nonce:x}")))
}

fn git_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{}", rest)
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
    }
}

fn validate_linked_worktree(root: &Path, target: &Path) -> Result<PathBuf, String> {
    let target = target.canonicalize().map_err(|error| error.to_string())?;
    if target == root
        || !target.join(".git").is_file()
        || common_root(&target)? != common_root(root)?
    {
        return Err("移除目标不是该项目的 linked worktree；主目录不会被删除".to_owned());
    }
    let listed = git(root, &["worktree", "list", "--porcelain"])?;
    let matches = listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|path| {
            Path::new(path)
                .canonicalize()
                .is_ok_and(|candidate| candidate == target)
        });
    matches
        .then_some(target)
        .ok_or_else(|| "工作树不在 Git 关联列表中".to_owned())
}

fn reject_link_target(target: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(target).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err("不能移除链接或 junction 形式的工作树目标".to_owned());
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &fs::Metadata) -> bool {
    false
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedRecord {
    root: PathBuf,
    path: PathBuf,
    git_dir: PathBuf,
    token: String,
}

fn register_managed(data_root: &Path, root: &Path, target: &Path) -> Result<(), String> {
    let git_dir = git_dir(target)?;
    let token = sha256_hex(
        format!(
            "{}\0{}\0{}",
            root.display(),
            target.display(),
            unique_nonce()
        )
        .as_bytes(),
    );
    let marker = git_dir.join("keencode-native-managed");
    fs::write(&marker, token.as_bytes()).map_err(|error| error.to_string())?;
    let record = ManagedRecord {
        root: root.to_owned(),
        path: target.to_owned(),
        git_dir,
        token,
    };
    let directory = data_root.join("native-worktree-managed");
    if let Err(error) = fs::create_dir_all(&directory) {
        let _ = fs::remove_file(&marker);
        return Err(error.to_string());
    }
    let path = directory.join(format!(
        "{}.json",
        sha256_hex(target.to_string_lossy().as_bytes())
    ));
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let data = match serde_json::to_vec(&record) {
        Ok(data) => data,
        Err(error) => {
            let _ = fs::remove_file(&marker);
            return Err(error.to_string());
        }
    };
    if let Err(error) = fs::write(&temporary, data) {
        let _ = fs::remove_file(&marker);
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        let _ = fs::remove_file(&marker);
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(())
}

fn rollback_created_worktree(
    root: &Path,
    target: &Path,
    created_branch: Option<&str>,
) -> Result<(), String> {
    let target_text = git_path(target);
    let mut errors = Vec::new();
    if target.exists()
        && let Err(error) = git(root, &["worktree", "remove", "--force", "--", &target_text])
    {
        errors.push(format!("移除 linked worktree 失败：{error}"));
    }
    if let Some(branch) = created_branch
        && let Err(error) = git(root, &["branch", "-D", "--", branch])
    {
        errors.push(format!("删除新建分支 {branch} 失败：{error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

fn validate_managed(data_root: &Path, root: &Path, target: &Path) -> Result<(), String> {
    let path = data_root.join("native-worktree-managed").join(format!(
        "{}.json",
        sha256_hex(target.to_string_lossy().as_bytes())
    ));
    let bytes = fs::read(path).map_err(|_| "未找到工作树创建记录，已保留".to_owned())?;
    let record: ManagedRecord =
        serde_json::from_slice(&bytes).map_err(|error| format!("工作树登记损坏：{error}"))?;
    if record.root != root || record.path != target || git_dir(target)? != record.git_dir {
        return Err("工作树登记身份已改变，未操作目录".to_owned());
    }
    let token = fs::read_to_string(record.git_dir.join("keencode-native-managed"))
        .map_err(|_| "工作树创建标记缺失，未操作目录".to_owned())?;
    if token != record.token {
        return Err("工作树创建标记已改变，未操作目录".to_owned());
    }
    Ok(())
}

fn git_dir(checkout: &Path) -> Result<PathBuf, String> {
    PathBuf::from(
        git(
            checkout,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        )?
        .trim(),
    )
    .canonicalize()
    .map_err(|error| error.to_string())
}

fn has_ignored_files(root: &Path) -> Result<bool, String> {
    Ok(!git(root, &["status", "--porcelain", "--ignored", "-z"])?
        .split('\0')
        .filter(|field| !field.is_empty())
        .all(|field| !field.starts_with("!! ")))
}

fn stash_if_dirty(root: &Path) -> Result<Option<String>, String> {
    if git(root, &["status", "--porcelain", "-z"])?.is_empty() {
        return Ok(None);
    }
    let before = entries_for_handoff(root)?;
    git(
        root,
        &[
            "stash",
            "push",
            "--include-untracked",
            "-m",
            "KeenCode native handoff",
        ],
    )?;
    let entries = git(root, &["stash", "list", "--format=%H%x09%gs"])?;
    let created = entries
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(hash, _)| !before.iter().any(|old| old == *hash))
        .map(|(hash, _)| hash.to_owned())
        .or_else(|| {
            entries
                .lines()
                .filter(|line| line.contains("KeenCode native handoff"))
                .find_map(|line| line.split_once('\t').map(|(hash, _)| hash.to_owned()))
        })
        .ok_or_else(|| "Git 未返回交接恢复点".to_owned())?;
    Ok(Some(created))
}

fn entries_for_handoff(root: &Path) -> Result<Vec<String>, String> {
    Ok(git(root, &["stash", "list", "--format=%H"])?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn drop_stash(root: &Path, hash: &str) -> Result<(), String> {
    let entries = git(root, &["stash", "list", "--format=%H%x09%gd"])?;
    let reference = entries
        .lines()
        .find_map(|line| {
            let (candidate, reference) = line.split_once('\t')?;
            (candidate == hash).then_some(reference)
        })
        .ok_or("交接 stash 已失效，请保留对象后人工处理")?;
    git(root, &["stash", "drop", reference]).map(|_| ())
}

fn restore_stash(root: &Path, hash: &str) -> Result<(), String> {
    if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("交接 stash 身份无效".to_owned());
    }
    git(root, &["stash", "apply", "--index", hash]).map(|_| ())
}

struct ChangeSnapshot {
    staged: Vec<u8>,
    unstaged: Vec<u8>,
    untracked: Vec<(PathBuf, Vec<u8>)>,
}

fn snapshot_changes(root: &Path, source: &Path) -> Result<ChangeSnapshot, String> {
    let source = canonical_directory(source)?;
    if source != root && common_root(&source)? != common_root(root)? {
        return Err("只能复制同一仓库的本地修改".to_owned());
    }
    let staged = git_bytes(
        &source,
        &[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
        ],
    )?;
    let unstaged = git_bytes(
        &source,
        &["diff", "--binary", "--no-ext-diff", "--no-textconv"],
    )?;
    let names = git(
        &source,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    let mut untracked = Vec::new();
    let mut total = staged.len() + unstaged.len();
    for path in names.split('\0').filter(|path| !path.is_empty()) {
        let relative = Path::new(path);
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("未跟踪文件路径越界".to_owned());
        }
        let file = source.join(relative);
        let mut current = source.to_owned();
        for component in relative.components() {
            current.push(component);
            let metadata = fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
            if is_reparse_point(&metadata) || metadata.file_type().is_symlink() {
                return Err("未跟踪路径不能经过符号链接或 junction".to_owned());
            }
        }
        let metadata = fs::symlink_metadata(&file).map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("未跟踪路径必须是普通文件".to_owned());
        }
        if !file
            .canonicalize()
            .is_ok_and(|path| path.starts_with(&source))
        {
            return Err("未跟踪文件真实路径越界".to_owned());
        }
        let bytes = fs::read(&file).map_err(|error| error.to_string())?;
        total = total.saturating_add(bytes.len());
        if total > MAX_COPY_BYTES {
            return Err("待复制的本地修改超过 16 MiB 限制".to_owned());
        }
        untracked.push((relative.to_owned(), bytes));
    }
    if total > MAX_COPY_BYTES {
        return Err("待复制的本地修改超过 16 MiB 限制".to_owned());
    }
    Ok(ChangeSnapshot {
        staged,
        unstaged,
        untracked,
    })
}

fn copy_changes(target: &Path, changes: ChangeSnapshot) -> Result<(), String> {
    apply_patch(target, changes.staged, true)?;
    apply_patch(target, changes.unstaged, false)?;
    for (relative, bytes) in changes.untracked {
        let path = target.join(relative);
        let parent = path.parent().ok_or("未跟踪文件缺少父目录")?;
        let mut current = target.to_owned();
        if let Ok(relative_parent) = parent.strip_prefix(target) {
            for component in relative_parent.components() {
                current.push(component);
                if !current.exists() {
                    fs::create_dir(&current).map_err(|error| error.to_string())?;
                }
                let metadata = fs::symlink_metadata(&current).map_err(|error| error.to_string())?;
                if !metadata.is_dir()
                    || metadata.file_type().is_symlink()
                    || is_reparse_point(&metadata)
                    || !current
                        .canonicalize()
                        .is_ok_and(|path| path.starts_with(target))
                {
                    return Err("目标工作树目录越界".to_owned());
                }
            }
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn apply_patch(root: &Path, patch: Vec<u8>, staged: bool) -> Result<(), String> {
    if patch.is_empty() {
        return Ok(());
    }
    let mut command = crate::workspace::git_command_for_root(root);
    command
        .arg("--literal-pathspecs")
        .arg("-C")
        .arg(root)
        .args(["apply", "--binary"]);
    if staged {
        command.arg("--index");
    }
    let mut child = command
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().ok_or("无法打开 Git patch 输入")?;
    let writer = std::thread::spawn(move || stdin.write_all(&patch));
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    writer
        .join()
        .map_err(|_| "Git patch 写入线程失败")?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(())
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = crate::workspace::run_git(root, args)?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    if output.stdout.len() > MAX_COPY_BYTES {
        return Err("Git 工作树输入超过大小限制".to_owned());
    }
    Ok(output.stdout)
}

fn validate_reference(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err("工作树引用或分支无效".to_owned());
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

fn unique_nonce() -> u128 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or_default()
        ^ u128::from(counter)
}

#[cfg(test)]
mod tests {
    use super::git_path;
    use std::path::Path;

    #[test]
    fn git_path_removes_windows_extended_prefix_only_for_git() {
        assert_eq!(
            git_path(Path::new(r"\\?\D:\projects\keen-code")),
            r"D:\projects\keen-code"
        );
        assert_eq!(
            git_path(Path::new(r"\\?\UNC\server\share\project")),
            r"\\server\share\project"
        );
        assert_eq!(
            git_path(Path::new(r"D:\projects\keen-code")),
            r"D:\projects\keen-code"
        );
    }
}
