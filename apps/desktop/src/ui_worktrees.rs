//! 原页面工作树创建接口；Git 与文件复制在宿主执行，页面只消费真实目录和进度。
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
};
use tauri::{AppHandle, Emitter};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// 复制输入在创建前一次读取，避免巨量差异或未跟踪文件占满宿主内存。
const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorktreeCreateInput {
    pub(crate) cwd: String,
    #[serde(rename = "ref")]
    pub(crate) reference: String,
    pub(crate) path: Option<String>,
    pub(crate) new_branch: Option<String>,
    pub(crate) copy_changes_from: Option<String>,
    pub(crate) progress_id: Option<String>,
    /// true 表示在既有分支创建工作树；false 允许 detached HEAD。
    pub(crate) checkout_branch: bool,
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = crate::workspace::run_git(root, args)?;
    if !output.status.success() {
        let error = keencode_model::redact_error_secrets(&String::from_utf8_lossy(&output.stderr));
        // 某些 Windows Git 失败路径不会写 stderr，仍须保留子命令和退出码，避免上层出现空错误。
        return Err(if error.trim().is_empty() {
            format!(
                "Git {} 操作失败（退出码 {:?}）",
                args.first().unwrap_or(&"命令"),
                output.status.code()
            )
        } else {
            error
        });
    }
    if output.stdout.len() > MAX_COPY_BYTES {
        return Err("Git 工作树输入超过大小限制".into());
    }
    Ok(output.stdout)
}

fn common_dir(root: &Path) -> Result<PathBuf, String> {
    let output = git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let path = String::from_utf8(output).map_err(|_| "Git 目录不是 UTF-8")?;
    fs::canonicalize(path.trim()).map_err(|error| error.to_string())
}

/// Git 的双向仓库身份及 worktree 列表共同证明关联，不能只信任路径前缀或客户端字段。
pub(crate) fn is_linked_worktree(root: &Path, candidate: &Path) -> bool {
    if root == candidate {
        return false;
    }
    let Ok(common) = common_dir(root) else {
        return false;
    };
    if !common_dir(candidate).is_ok_and(|other| other == common) {
        return false;
    }
    let Ok(list) = git(root, &["worktree", "list", "--porcelain", "-z"]) else {
        return false;
    };
    String::from_utf8_lossy(&list)
        .split('\0')
        .filter_map(|field| field.strip_prefix("worktree "))
        .any(|path| fs::canonicalize(path).is_ok_and(|path| path == candidate))
}

fn valid_reference(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        return Err("工作树引用或分支无效".into());
    }
    Ok(())
}

struct Changes {
    staged: Vec<u8>,
    unstaged: Vec<u8>,
    untracked: Vec<(PathBuf, Vec<u8>)>,
}

/// 交接前复用创建接口的文件、链接与总大小校验；只校验，不复制或清理文件。
pub(crate) fn validate_transfer_changes(root: &Path) -> Result<(), String> {
    snapshot_changes(root).map(|_| ())
}

/// 回滚前复核准备阶段后的 index、工作文件及未跟踪文件，避免清理期间覆盖外部新修改。
pub(crate) fn transfer_fingerprint(root: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let changes = snapshot_changes(root)?;
    let mut digest = Sha256::new();
    for bytes in [changes.staged, changes.unstaged] {
        digest.update(bytes.len().to_le_bytes());
        digest.update(bytes);
    }
    for (path, bytes) in changes.untracked {
        let path = crate::path_utils::path_to_frontend(&path);
        digest.update(path.len().to_le_bytes());
        digest.update(path.as_bytes());
        digest.update(bytes.len().to_le_bytes());
        digest.update(bytes);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Windows junction 等 reparse point 也可能绕过普通符号链接判断。
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// 先校验全部未跟踪路径和大小再创建目录；拒绝通过符号链接复制其他位置的内容。
fn snapshot_changes(root: &Path) -> Result<Changes, String> {
    let root = fs::canonicalize(root).map_err(|error| error.to_string())?;
    let root = root.as_path();
    let staged = git(
        root,
        &[
            "diff",
            "--cached",
            "--binary",
            "--no-ext-diff",
            "--no-textconv",
        ],
    )?;
    let unstaged = git(
        root,
        &["diff", "--binary", "--no-ext-diff", "--no-textconv"],
    )?;
    let paths = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    let paths = String::from_utf8(paths).map_err(|_| "未跟踪文件路径不是 UTF-8")?;
    let mut untracked = Vec::new();
    let mut total = staged.len() + unstaged.len();
    for path in paths.split('\0').filter(|path| !path.is_empty()) {
        let relative = Path::new(path);
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("未跟踪文件路径越界".into());
        }
        let mut source = root.to_path_buf();
        for part in relative.components() {
            source.push(part);
            if is_link(&fs::symlink_metadata(&source).map_err(|error| error.to_string())?) {
                return Err("不复制符号链接形式的未跟踪文件".into());
            }
        }
        if !fs::canonicalize(&source)
            .map_err(|error| error.to_string())?
            .starts_with(root)
        {
            return Err("未跟踪文件真实路径越界".into());
        }
        let metadata = fs::metadata(&source).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("未跟踪路径不是普通文件".into());
        }
        let length = metadata.len();
        if length > MAX_COPY_BYTES as u64 || total.saturating_add(length as usize) > MAX_COPY_BYTES
        {
            return Err("待复制的本地修改超过 16 MiB 限制".into());
        }
        let bytes = fs::read(&source).map_err(|error| error.to_string())?;
        total = total.saturating_add(bytes.len());
        if total > MAX_COPY_BYTES {
            return Err("待复制的本地修改超过大小限制".into());
        }
        untracked.push((relative.to_owned(), bytes));
    }
    if total > MAX_COPY_BYTES {
        return Err("待复制的本地修改超过大小限制".into());
    }
    Ok(Changes {
        staged,
        unstaged,
        untracked,
    })
}

fn apply_patch(root: &Path, patch: Vec<u8>, staged: bool) -> Result<(), String> {
    if patch.is_empty() {
        return Ok(());
    }
    let mut command = crate::workspace::git_command();
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().ok_or("无法打开 Git patch 输入")?;
    // 排空输出与写入输入同时进行，错误输出不能堵塞较大的二进制 patch。
    let writer = std::thread::spawn(move || stdin.write_all(&patch));
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let written = writer.join().map_err(|_| "Git patch 写入线程失败")?;
    if !output.status.success() {
        return Err(keencode_model::redact_error_secrets(
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    written.map_err(|error| error.to_string())
}

fn copy_changes(target: &Path, changes: Changes) -> Result<(), String> {
    apply_patch(target, changes.staged, true)?;
    apply_patch(target, changes.unstaged, false)?;
    for (relative, bytes) in changes.untracked {
        let mut parent = target.to_path_buf();
        let relative_parent = relative.parent().ok_or("文件缺少相对目录")?;
        for part in relative_parent.components() {
            parent.push(part);
            if !parent.exists() {
                fs::create_dir(&parent).map_err(|error| error.to_string())?;
            }
            let metadata = fs::symlink_metadata(&parent).map_err(|error| error.to_string())?;
            if !metadata.is_dir()
                || is_link(&metadata)
                || !fs::canonicalize(&parent)
                    .map_err(|error| error.to_string())?
                    .starts_with(target)
            {
                return Err("目标工作树目录越界".into());
            }
        }
        let path = target.join(relative);
        // 新工作树若已存在同名文件，拒绝覆盖；失败时保留 checkout 便于审查。
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn create(
    root: &Path,
    input: &WorktreeCreateInput,
    mut progress: impl FnMut(&str),
) -> Result<Value, String> {
    valid_reference(&input.reference)?;
    if let Some(branch) = &input.new_branch {
        valid_reference(branch)?;
        git(root, &["check-ref-format", "--branch", branch])?;
    } else if input.checkout_branch {
        git(root, &["check-ref-format", "--branch", &input.reference])?;
        // HEAD、tag、远程分支不能冒充已有本地分支，避免 Git 隐式建分支或 detached。
        git(
            root,
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{}", input.reference),
            ],
        )?;
    }
    let commit = String::from_utf8(git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", input.reference),
        ],
    )?)
    .map_err(|_| "Git commit 不是 UTF-8")?;
    let commit = commit.trim();
    let target = match &input.path {
        Some(path) => {
            let target = Path::new(path);
            if !target.is_absolute() || path.chars().any(char::is_control) {
                return Err("工作树目标必须是绝对目录".into());
            }
            let parent = fs::canonicalize(target.parent().ok_or("目标没有父目录")?)
                .map_err(|error| error.to_string())?;
            parent.join(target.file_name().ok_or("目标缺少目录名称")?)
        }
        None => {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos();
            let name = root
                .file_name()
                .ok_or("项目没有目录名称")?
                .to_string_lossy();
            root.parent().ok_or("项目没有父目录")?.join(format!(
                "{name}-worktree-{nonce:x}-{}",
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ))
        }
    };
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err("工作树目标目录已存在，未修改".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("无法检查工作树目标目录：{error}")),
    }
    let changes = input
        .copy_changes_from
        .as_ref()
        .map(|path| {
            let source = fs::canonicalize(path).map_err(|error| error.to_string())?;
            if source != root && !is_linked_worktree(root, &source) {
                return Err("只能复制同一仓库的本地修改".into());
            }
            snapshot_changes(&source)
        })
        .transpose()?;
    let branch = if let Some(branch) = &input.new_branch {
        progress("branch");
        git(root, &["branch", "--", branch, commit])?;
        Some(branch.as_str())
    } else if input.checkout_branch {
        valid_reference(&input.reference)?;
        git(root, &["check-ref-format", "--branch", &input.reference])?;
        Some(input.reference.as_str())
    } else {
        None
    };
    progress("worktree");
    let target_text = crate::path_utils::path_to_frontend(&target);
    let mut args = vec!["worktree", "add"];
    if branch.is_none() {
        args.push("--detach");
    }
    args.extend(["--", &target_text, branch.unwrap_or(commit)]);
    git(root, &args)
        .map_err(|error| format!("工作树创建失败；可能已创建的新分支已保留供检查：{error}"))?;
    let target = fs::canonicalize(&target).map_err(|error| error.to_string())?;
    if let Some(changes) = changes {
        progress("copy-changes");
        copy_changes(&target, changes).map_err(|error| {
            format!(
                "本地修改复制失败，新工作树已保留在 {}：{error}",
                crate::path_utils::path_to_frontend(&target)
            )
        })?;
    }
    Ok(
        json!({ "worktree": { "path": crate::path_utils::path_to_frontend(&target), "ref": input.reference, "branch": branch } }),
    )
}

#[tauri::command]
pub async fn ui_git_worktree_create(
    app: AppHandle,
    input: WorktreeCreateInput,
) -> Result<Value, String> {
    let _git = crate::ui_git_stash::STASH_GATE.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &input.cwd)?;
        let result = create(&root, &input, |phase| {
            let _ = app.emit(
                "ui-git://worktree-setup",
                json!({"progressId": input.progress_id, "kind": "phase_started", "phase": phase}),
            );
        })?;
        let checkout = fs::canonicalize(
            result["worktree"]["path"]
                .as_str()
                .ok_or("工作树创建结果缺少路径")?,
        )
        .map_err(|error| error.to_string())?;
        let data = crate::storage::root_dir(&app).map_err(|error| error.to_string())?;
        crate::ui_worktree_archive::mark_created(&data, &root, &checkout)?;
        let _ = app.emit(
            "ui-git://worktree-setup",
            json!({"progressId": input.progress_id, "kind": "completed", "result": result}),
        );
        Ok(result)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worktree_copies_index_working_changes_and_untracked_without_modifying_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        git(&root, &["init", "--initial-branch=main"]).unwrap();
        // 隔离夹具不继承本机 autocrlf，文本字节断言与 Git index 使用同一换行约定。
        git(&root, &["config", "core.autocrlf", "false"]).unwrap();
        fs::write(root.join("file.txt"), b"base\n").unwrap();
        git(&root, &["add", "--", "file.txt"]).unwrap();
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "fixture",
            ],
        )
        .unwrap();
        fs::write(root.join("file.txt"), b"base\nstaged\n").unwrap();
        git(&root, &["add", "--", "file.txt"]).unwrap();
        fs::write(root.join("file.txt"), b"base\nstaged\nworking\n").unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/file[1].bin"), [0, 1, 255]).unwrap();
        let before = git(&root, &["status", "--porcelain", "-z"]).unwrap();
        let input = WorktreeCreateInput {
            cwd: root.to_string_lossy().into(),
            reference: "HEAD".into(),
            path: Some(temp.path().join("target").to_string_lossy().into()),
            new_branch: Some("feat/worktree-fixture".into()),
            copy_changes_from: Some(root.to_string_lossy().into()),
            progress_id: None,
            checkout_branch: false,
        };
        let mut phases = Vec::new();
        let result = create(&root, &input, |phase| phases.push(phase.to_owned())).unwrap();
        assert_eq!(phases, ["branch", "worktree", "copy-changes"]);
        let target = fs::canonicalize(result["worktree"]["path"].as_str().unwrap()).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        assert!(is_linked_worktree(&root, &target));
        assert!(!is_linked_worktree(&root, temp.path()));
        assert_eq!(
            fs::read(target.join("file.txt")).unwrap(),
            b"base\nstaged\nworking\n"
        );
        assert_eq!(
            git(&target, &["show", ":file.txt"]).unwrap(),
            b"base\nstaged\n"
        );
        assert_eq!(
            fs::read(target.join("nested/file[1].bin")).unwrap(),
            [0, 1, 255]
        );
        assert_eq!(
            git(&root, &["status", "--porcelain", "-z"]).unwrap(),
            before
        );
        assert!(
            create(&root, &input, |_| {})
                .unwrap_err()
                .contains("已存在")
        );
        assert!(valid_reference("--force").is_err());
        assert_eq!(
            git(&target, &["status", "--porcelain", "-z"]).unwrap(),
            before
        );

        let detached_input = WorktreeCreateInput {
            path: Some(temp.path().join("detached").to_string_lossy().into()),
            new_branch: None,
            copy_changes_from: None,
            ..input
        };
        let detached = create(&root, &detached_input, |_| {}).unwrap();
        assert!(detached["worktree"]["branch"].is_null());
        let detached_root = Path::new(detached["worktree"]["path"].as_str().unwrap());
        assert!(git(detached_root, &["symbolic-ref", "--quiet", "HEAD"]).is_err());

        git(&root, &["branch", "feat/existing"]).unwrap();
        let branch_input = WorktreeCreateInput {
            reference: "feat/existing".into(),
            checkout_branch: true,
            path: Some(temp.path().join("existing").to_string_lossy().into()),
            ..detached_input
        };
        let existing = create(&root, &branch_input, |_| {}).unwrap();
        assert_eq!(existing["worktree"]["branch"], "feat/existing");
        let invalid = WorktreeCreateInput {
            reference: "HEAD".into(),
            path: Some(temp.path().join("invalid").to_string_lossy().into()),
            ..branch_input
        };
        assert!(create(&root, &invalid, |_| {}).is_err());
        assert!(!temp.path().join("invalid").exists());
    }
}
