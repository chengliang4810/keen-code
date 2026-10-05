//! 原分支恢复菜单的数据接口；所有 Git 参数固定，暂存身份用真实对象 SHA 校验。
use serde_json::{Value, json};
use std::path::Path;
use tauri::AppHandle;

// 序列化本接口的 stash 读改流程，避免同进程操作改变 reflog 的位置身份。
pub(crate) static STASH_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    // 本接口没有 pathspec；Git 2.51 的 --literal-pathspecs 会使 stash -u 不清理已保存的未跟踪文件。
    let output = crate::workspace::run_git_with_timeout(root, args)?;
    if !output.status.success() {
        let error = keencode_model::redact_error_secrets(&String::from_utf8_lossy(&output.stderr));
        return Err(if error.trim().is_empty() {
            format!("Git 操作失败：{:?}", output.status.code())
        } else {
            error
        });
    }
    if output.stdout.len() > 16 * 1024 * 1024 {
        return Err("stash 输出超过限制".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "stash 输出不是 UTF-8".into())
}

pub(crate) fn entries(root: &Path) -> Result<Vec<(String, String, String)>, String> {
    let output = git(root, &["stash", "list", "--format=%H%x09%gd%x09%gs"])?;
    output
        .lines()
        .map(|line| {
            let mut fields = line.splitn(3, '\t');
            let hash = fields.next().ok_or("stash 对象缺失")?;
            valid_hash(hash)?;
            let reference = fields.next().ok_or("stash 引用缺失")?;
            let message = fields.next().ok_or("stash 主题缺失")?;
            Ok((hash.to_owned(), reference.to_owned(), message.to_owned()))
        })
        .collect()
}

fn valid_hash(hash: &str) -> Result<(), String> {
    if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("stash 身份必须是实际对象 SHA".into());
    }
    Ok(())
}

/// 删除的是已确认对象，不能因新的 stash 插入而删除另一个 stash@{0}。
pub(crate) fn drop_hash(root: &Path, hash: &str) -> Result<(), String> {
    valid_hash(hash)?;
    let (_, reference, _) = entries(root)?
        .into_iter()
        .find(|(candidate, _, _)| candidate == hash)
        .ok_or("所选 stash 已失效，请重新查看")?;
    git(root, &["stash", "drop", &reference])?;
    Ok(())
}

fn info(root: &Path, cwd: &str) -> Result<Value, String> {
    let (hash, _, message) = entries(root)?
        .into_iter()
        .next()
        .ok_or("No stash entry is available.")?;
    let branch = git(root, &["branch", "--show-current"])?;
    // -z 保留带空格、中文或换行的真实文件名，不依赖 Git 的 quotePath 输出。
    let files = git(
        root,
        &[
            "stash",
            "show",
            "--include-untracked",
            "--name-only",
            "-z",
            &hash,
        ],
    )?;
    Ok(
        json!({"cwd":cwd,"branch":if branch.trim().is_empty(){None}else{Some(branch.trim())},
        "stashRef":hash,"message":message,"files":files.split('\0').filter(|file|!file.is_empty()).collect::<Vec<_>>()}),
    )
}

fn stash_and_checkout(root: &Path, branch: &str) -> Result<(), String> {
    if branch.is_empty()
        || branch.len() > 256
        || branch.starts_with('-')
        || branch.chars().any(char::is_control)
    {
        return Err("Git 分支名称无效".into());
    }
    git(root, &["check-ref-format", "--branch", branch])?;
    // 先验证目标存在，错误分支不应先清空用户工作区；分支冲突由 Git switch 自身保护。
    git(
        root,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    )?;
    let before = entries(root)?;
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
    let after = entries(root)?;
    let created = after
        .iter()
        .find(|(hash, _, _)| !before.iter().any(|(old, _, _)| old == hash))
        // 同一秒内相同内容可能产生相同对象；新增 reflog 条目仍须恢复。
        .or_else(|| (after.len() > before.len()).then(|| &after[0]))
        .cloned();
    if let Err(checkout_error) = git(root, &["switch", branch]) {
        if let Some((hash, _, _)) = created {
            // 切换失败时恢复原目录的修改和 index；恢复失败保留 stash 和错误，不强制 reset/clean。
            if let Err(restore_error) = git(root, &["stash", "apply", "--index", &hash]) {
                return Err(format!(
                    "{checkout_error}\n恢复失败，修改仍保存在 stash {hash}：{restore_error}"
                ));
            }
            drop_hash(root, &hash)?;
        }
        return Err(checkout_error);
    }
    if let Some((hash, _, _)) = created {
        if let Err(error) = git(root, &["stash", "apply", "--index", &hash]) {
            // Git 的 --index 冲突也会报 could not write index；原页面先匹配写入错误。
            // 用实际未合并条目确认冲突，返回原冲突契约，避免误导用户重试暂存。
            let conflicts =
                git(root, &["diff", "--name-only", "--diff-filter=U", "-z"]).unwrap_or_default();
            if !conflicts.is_empty() {
                let files = conflicts
                    .split('\0')
                    .filter(|file| !file.is_empty())
                    .map(|file| format!("- {file}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(format!(
                    "Stash applied with merge conflicts. Your changes are still saved in the stash.\n{hash}\n{files}"
                ));
            }
            // 无实际冲突时保留 Git 原错误；锁定和写入失败仍进入原页面各自的恢复流程。
            return Err(format!(
                "Stash could not be applied. Your changes are still saved in the stash.\n{hash}\n{error}"
            ));
        }
        drop_hash(root, &hash)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn ui_git_stash(
    app: AppHandle,
    cwd: String,
    operation: String,
    branch: Option<String>,
    stash_ref: Option<String>,
) -> Result<Value, String> {
    let _guard = STASH_GATE.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        let root = crate::workspace::registered_project_root(&app, &cwd)?;
        match operation.as_str() {
            "info" => info(&root, &cwd),
            "drop" => {
                drop_hash(&root, stash_ref.as_deref().ok_or("缺少 stash 身份")?)?;
                Ok(Value::Null)
            }
            "checkout" => {
                stash_and_checkout(&root, branch.as_deref().ok_or("缺少目标分支")?)?;
                Ok(Value::Null)
            }
            _ => Err("未知 stash 操作".into()),
        }
    })
    .await
    .map_err(|error| format!("stash 后台任务失败：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path) {
        git(root, &["init", "--initial-branch=main"]).unwrap();
        // 合成夹具固定换行规则，避免用户全局 autocrlf 改变 Git 恢复文件的字节。
        git(root, &["config", "core.autocrlf", "false"]).unwrap();
        git(root, &["config", "user.name", "UI Acceptance"]).unwrap();
        git(root, &["config", "user.email", "ui@example.invalid"]).unwrap();
        std::fs::write(root.join("proof.txt"), "base\n").unwrap();
        git(root, &["add", "--all"]).unwrap();
        git(
            root,
            &["commit", "-m", "测试：暂存基线 / test: stash baseline"],
        )
        .unwrap();
        git(root, &["branch", "feat/target"]).unwrap();
    }

    #[test]
    fn successful_switch_preserves_index_untracked_files_and_prior_stashes() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fixture(root);
        std::fs::write(root.join("prior.txt"), "prior\n").unwrap();
        git(root, &["stash", "push", "-u", "-m", "prior"]).unwrap();
        let prior = entries(root).unwrap();
        std::fs::write(root.join("proof.txt"), "staged\n").unwrap();
        git(root, &["add", "proof.txt"]).unwrap();
        std::fs::write(root.join("proof.txt"), "staged\nunstaged\n").unwrap();
        std::fs::write(root.join("未跟踪[1].txt"), "untracked\n").unwrap();
        stash_and_checkout(root, "feat/target").unwrap();
        assert_eq!(
            git(root, &["branch", "--show-current"]).unwrap().trim(),
            "feat/target"
        );
        assert_eq!(git(root, &["show", ":proof.txt"]).unwrap(), "staged\n");
        assert_eq!(
            std::fs::read_to_string(root.join("proof.txt")).unwrap(),
            "staged\nunstaged\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("未跟踪[1].txt")).unwrap(),
            "untracked\n"
        );
        assert_eq!(entries(root).unwrap(), prior);
    }

    #[test]
    fn conflicting_reapply_preserves_stash_and_files_and_drop_uses_object_identity() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fixture(root);
        git(root, &["switch", "feat/target"]).unwrap();
        std::fs::write(root.join("proof.txt"), "target\n").unwrap();
        git(root, &["add", "--all"]).unwrap();
        git(
            root,
            &["commit", "-m", "测试：冲突目标 / test: conflict target"],
        )
        .unwrap();
        git(root, &["switch", "main"]).unwrap();
        std::fs::write(root.join("proof.txt"), "local\n").unwrap();
        let error = stash_and_checkout(root, "feat/target").unwrap_err();
        assert!(error.contains("Stash applied with merge conflicts"));
        assert!(error.contains("- proof.txt"));
        assert!(!error.to_lowercase().contains("could not write index"));
        let result = info(root, "fixture").unwrap();
        let saved = result["stashRef"].as_str().unwrap();
        assert!(
            git(root, &["show", &format!("{saved}:proof.txt")])
                .unwrap()
                .contains("local")
        );
        assert!(root.join("proof.txt").exists());
        assert_eq!(result["files"], json!(["proof.txt"]));
        // 清理仅发生在合成夹具中；新增 stash 后旧 SHA 仍定位到旧条目。
        git(root, &["reset", "--hard"]).unwrap();
        std::fs::write(root.join("later.txt"), "later\n").unwrap();
        git(root, &["stash", "push", "-u", "-m", "later"]).unwrap();
        let latest = entries(root).unwrap()[0].0.clone();
        drop_hash(root, saved).unwrap();
        assert_eq!(entries(root).unwrap()[0].0, latest);
        assert!(drop_hash(root, "stash@{0}").is_err());
        assert!(drop_hash(root, saved).is_err());
    }

    #[test]
    fn invalid_target_does_not_stash_or_change_checkout() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fixture(root);
        std::fs::write(root.join("proof.txt"), "local\n").unwrap();
        assert!(stash_and_checkout(root, "missing").is_err());
        assert!(stash_and_checkout(root, "--detach").is_err());
        assert!(entries(root).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(root.join("proof.txt")).unwrap(),
            "local\n"
        );
        assert_eq!(
            git(root, &["branch", "--show-current"]).unwrap().trim(),
            "main"
        );
    }

    #[test]
    fn checkout_failure_restores_original_index_and_working_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        std::fs::create_dir(&root).unwrap();
        fixture(&root);
        let target = directory.path().join("target");
        git(
            &root,
            &["worktree", "add", target.to_str().unwrap(), "feat/target"],
        )
        .unwrap();
        std::fs::write(root.join("proof.txt"), "staged\n").unwrap();
        git(&root, &["add", "proof.txt"]).unwrap();
        std::fs::write(root.join("proof.txt"), "staged\nunstaged\n").unwrap();
        std::fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
        assert!(stash_and_checkout(&root, "feat/target").is_err());
        assert_eq!(
            git(&root, &["branch", "--show-current"]).unwrap().trim(),
            "main"
        );
        assert_eq!(git(&root, &["show", ":proof.txt"]).unwrap(), "staged\n");
        assert_eq!(
            std::fs::read_to_string(root.join("proof.txt")).unwrap(),
            "staged\nunstaged\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("untracked.txt")).unwrap(),
            "untracked\n"
        );
        assert!(entries(&root).unwrap().is_empty());
    }
}
