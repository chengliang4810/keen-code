//! 原页面工作树交接的数据实现。Git 修改先保存为真实 stash，再提交权威 Session cwd。
use crate::ui_git_stash::{drop_hash, entries, git};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HandoffInput {
    pub command_id: String,
    pub thread_id: String,
    pub cwd: String,
    pub target_mode: String,
    pub current_branch: Option<String>,
    pub worktree_path: Option<String>,
    pub associated_worktree_path: Option<String>,
    pub associated_worktree_branch: Option<String>,
    pub associated_worktree_ref: Option<String>,
    pub preferred_local_branch: Option<String>,
    pub preferred_worktree_base_branch: Option<String>,
    pub preferred_new_worktree_name: Option<String>,
}

/// 回执只含目录、Git 身份与恢复点，不存工作文件正文或模型配置。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Transfer {
    pub input: HandoffInput,
    pub source: PathBuf,
    pub target: PathBuf,
    source_branch: Option<String>,
    source_head: String,
    target_branch: Option<String>,
    target_head: String,
    source_stash: Option<String>,
    target_stash: Option<String>,
    pub(crate) target_created: bool,
    target_fingerprint: Option<String>,
    pub result: Option<Value>,
    pub completed: bool,
}

fn branch(root: &Path) -> Result<Option<String>, String> {
    let value = git(root, &["branch", "--show-current"])?;
    Ok((!value.trim().is_empty()).then(|| value.trim().to_owned()))
}

fn head(root: &Path) -> Result<String, String> {
    Ok(git(root, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_owned())
}

fn clean(root: &Path) -> Result<bool, String> {
    Ok(git(root, &["status", "--porcelain", "-z"])?.is_empty())
}

fn switch(root: &Path, branch: Option<&str>, commit: &str) -> Result<(), String> {
    match branch {
        Some(branch) => {
            git(root, &["check-ref-format", "--branch", branch])?;
            git(root, &["switch", "--", branch])?;
        }
        None => {
            git(root, &["switch", "--detach", commit])?;
        }
    }
    Ok(())
}

pub(crate) fn receipt_path(data_root: &Path, input: &HandoffInput) -> PathBuf {
    let key = Sha256::digest(format!("{}\0{}", input.thread_id, input.command_id).as_bytes());
    data_root
        .join("ui-workspace-handoffs")
        .join(format!("{key:x}.json"))
}

pub(crate) fn read_receipt(path: &Path, input: &HandoffInput) -> Result<Option<Transfer>, String> {
    let bytes = crate::storage::read_private_bytes_bounded(path, 64 * 1024, "工作树交接回执")
        .map_err(|error| error.to_string())?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let record: Transfer = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if record.input != *input {
        return Err("交接 commandId 已绑定其他输入".into());
    }
    Ok(Some(record))
}

pub(crate) fn save_receipt(path: &Path, record: &Transfer) -> Result<(), String> {
    crate::storage::atomic_write_private(
        path,
        &serde_json::to_vec(record).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn validate_checkout(root: &Path) -> Result<(), String> {
    if !git(root, &["diff", "--name-only", "--diff-filter=U", "-z"])?.is_empty() {
        return Err("请先解决当前工作区的 Git 冲突".into());
    }
    crate::ui_worktrees::validate_transfer_changes(root)
}

/// 保存对象 SHA 后才允许后续 checkout。崩溃发生在写回之前时，stash 主题仍标识操作身份。
fn stash(root: &Path, label: &str) -> Result<Option<String>, String> {
    if clean(root)? {
        return Ok(None);
    }
    validate_checkout(root)?;
    git(root, &["stash", "push", "--include-untracked", "-m", label])?;
    let saved = entries(root)?
        .into_iter()
        .find(|(_, _, message)| message.contains(label))
        .map(|(hash, _, _)| hash)
        .ok_or("Git 未返回本次交接的恢复点")?;
    if !clean(root)? {
        return Err(format!(
            "工作区仍有无法暂存的修改（例如子模块），停止交接；恢复点 {saved}"
        ));
    }
    Ok(Some(saved))
}

fn apply(root: &Path, saved: &Option<String>) -> Result<(), String> {
    if let Some(hash) = saved {
        git(root, &["stash", "apply", "--index", hash])?;
    }
    Ok(())
}

pub(crate) fn begin(
    root: &Path,
    source: &Path,
    input: HandoffInput,
    path: &Path,
) -> Result<Transfer, String> {
    if !matches!(input.target_mode.as_str(), "local" | "worktree") {
        return Err("交接模式无效".into());
    }
    for id in [&input.thread_id, &input.command_id] {
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err("交接标识无效".into());
        }
    }
    validate_checkout(root)?;
    validate_checkout(source)?;
    let is_local = source == root;
    if (input.target_mode == "worktree") != is_local {
        return Err("会话执行目录已改变，请刷新后重新交接".into());
    }
    if !is_local && !crate::ui_worktrees::is_linked_worktree(root, source) {
        return Err("会话目录不是该项目的关联工作树".into());
    }
    let source_branch = branch(source)?;
    let source_head = head(source)?;
    let target = if input.target_mode == "local" {
        root.to_owned()
    } else if let Some(associated) = &input.associated_worktree_path {
        let candidate = Path::new(associated);
        if !candidate.is_absolute() || candidate == root {
            return Err("关联工作树路径无效".into());
        }
        // 已删除工作树只能在原绝对父目录重新创建；已存在路径必须通过双向 Git 身份证明。
        if candidate.exists() {
            let canonical = fs::canonicalize(candidate).map_err(|error| error.to_string())?;
            if !crate::ui_worktrees::is_linked_worktree(root, &canonical) {
                return Err("关联路径不属于该仓库".into());
            }
            canonical
        } else {
            fs::canonicalize(candidate.parent().ok_or("关联路径没有父目录")?)
                .map_err(|error| error.to_string())?
                .join(candidate.file_name().ok_or("关联路径无效")?)
        }
    } else {
        PathBuf::new()
    };
    let target_exists = !target.as_os_str().is_empty() && target.exists();
    if target_exists {
        validate_checkout(&target)?;
    }
    let record = Transfer {
        input,
        source: source.to_owned(),
        target: target.clone(),
        source_branch,
        source_head,
        target_branch: if target_exists {
            branch(&target)?
        } else {
            None
        },
        target_head: if target_exists {
            head(&target)?
        } else {
            String::new()
        },
        source_stash: None,
        target_stash: None,
        target_created: false,
        target_fingerprint: None,
        result: None,
        completed: false,
    };
    save_receipt(path, &record)?;
    Ok(record)
}

pub(crate) fn prepare(
    root: &Path,
    mut record: Transfer,
    receipt: &Path,
) -> Result<Transfer, String> {
    let operation: Result<(), String> = (|| {
        let tag = format!(
            "KeenCode handoff {} {}",
            record.input.thread_id, record.input.command_id
        );
        record.source_stash = stash(&record.source, &format!("{tag} source"))?;
        save_receipt(receipt, &record)?;
        if record.target.exists() {
            record.target_stash = stash(&record.target, &format!("{tag} target"))?;
            save_receipt(receipt, &record)?;
        }
        if record.input.target_mode == "local" {
            // 先 detach 释放分支，源 checkout 在权威目录与元数据落盘前始终保留。
            switch(&record.source, None, &record.source_head)?;
            switch(root, record.source_branch.as_deref(), &record.source_head)?;
        } else {
            let reuse_branch = record.input.associated_worktree_branch.as_deref();
            if reuse_branch.is_some() && reuse_branch == record.source_branch.as_deref() {
                switch(root, None, &record.source_head)?;
            }
            if !record.target.exists() {
                let (reference, new_branch, checkout_branch) =
                    if record.input.associated_worktree_path.is_some() {
                        (
                            reuse_branch
                                .or(record.input.associated_worktree_ref.as_deref())
                                .ok_or("关联工作树缺少恢复引用")?
                                .to_owned(),
                            None,
                            reuse_branch.is_some(),
                        )
                    } else {
                        let name = record
                            .input
                            .preferred_new_worktree_name
                            .as_deref()
                            .ok_or("请为新工作树指定名称")?;
                        if name.starts_with('-')
                            || name.len() > 200
                            || name.chars().any(char::is_control)
                        {
                            return Err("工作树名称无效".into());
                        }
                        let name = if name.contains('/') {
                            name.to_owned()
                        } else {
                            format!("feat/{name}")
                        };
                        (
                            record
                                .input
                                .preferred_worktree_base_branch
                                .clone()
                                .unwrap_or_else(|| record.source_head.clone()),
                            Some(name),
                            false,
                        )
                    };
                let value = crate::ui_worktrees::create(
                    root,
                    &crate::ui_worktrees::WorktreeCreateInput {
                        cwd: crate::path_utils::path_to_frontend(root),
                        reference,
                        path: (!record.target.as_os_str().is_empty())
                            .then(|| crate::path_utils::path_to_frontend(&record.target)),
                        new_branch,
                        copy_changes_from: None,
                        progress_id: None,
                        checkout_branch,
                    },
                    |_| {},
                )?;
                record.target = fs::canonicalize(
                    value["worktree"]["path"]
                        .as_str()
                        .ok_or("工作树创建未返回真实目录")?,
                )
                .map_err(|error| error.to_string())?;
                record.target_created = true;
                save_receipt(receipt, &record)?;
            } else if reuse_branch.is_some() {
                switch(&record.target, reuse_branch, &record.source_head)?;
            }
        }
        apply(&record.target, &record.source_stash)?;
        // 本地原有修改与传入修改可能冲突；保留真实冲突与两个 stash，返回原页面警告契约。
        let warning = apply(&record.target, &record.target_stash).err();
        let conflicts = !git(
            &record.target,
            &["diff", "--name-only", "--diff-filter=U", "-z"],
        )?
        .is_empty();
        if !conflicts {
            record.target_fingerprint =
                Some(crate::ui_worktrees::transfer_fingerprint(&record.target)?);
        }
        let target_branch = branch(&record.target)?;
        let target_head = head(&record.target)?;
        let worktree = if record.input.target_mode == "worktree" {
            &record.target
        } else {
            &record.source
        };
        record.result = Some(json!({
            "targetMode":record.input.target_mode, "branch":target_branch,
            "worktreePath":if record.input.target_mode == "worktree" {Some(crate::path_utils::path_to_frontend(&record.target))} else {None},
            "associatedWorktreePath":crate::path_utils::path_to_frontend(worktree),
            "associatedWorktreeBranch":if record.input.target_mode == "worktree" {branch(&record.target)?} else {record.source_branch.clone()},
            "associatedWorktreeRef":if record.input.target_mode == "worktree" {target_head} else {record.source_head.clone()},
            "changesTransferred":record.source_stash.is_some(), "conflictsDetected":conflicts,
            "message":warning.map(|error|format!("目标原有修改未能完整恢复，恢复点 {} 已保留：{error}",record.target_stash.as_deref().unwrap_or("unknown")))
        }));
        save_receipt(receipt, &record)?;
        Ok(())
    })();
    if let Err(error) = operation {
        let recovery = rollback(&record);
        return Err(format!(
            "工作树交接失败：{error}。{recovery}；操作回执 {}",
            receipt.display()
        ));
    }
    Ok(record)
}

/// 只在干净 checkout 上恢复分支和 stash；不 reset/clean，不覆盖交接期间的新修改或冲突。
pub(crate) fn rollback(record: &Transfer) -> String {
    let mut messages = Vec::new();
    if let Some(expected) = &record.target_fingerprint {
        let expected_head = record
            .result
            .as_ref()
            .and_then(|result| result["associatedWorktreeRef"].as_str());
        let expected_branch = record
            .result
            .as_ref()
            .and_then(|result| result["branch"].as_str());
        if crate::ui_worktrees::transfer_fingerprint(&record.target)
            .is_ok_and(|actual| actual == *expected)
            && head(&record.target).is_ok_and(|actual| Some(actual.as_str()) == expected_head)
            && branch(&record.target).is_ok_and(|actual| actual.as_deref() == expected_branch)
        {
            match stash(
                &record.target,
                &format!(
                    "KeenCode rollback {} {}",
                    record.input.thread_id, record.input.command_id
                ),
            ) {
                Ok(saved) => messages.push(format!("目标已准备修改保存在额外恢复点 {saved:?}")),
                Err(error) => messages.push(format!("目标回滚暂存失败：{error}")),
            }
        } else {
            messages.push("目标在准备后出现新修改，保留现场，不清理".into());
        }
    }
    if record.target.exists()
        && clean(&record.target).unwrap_or(false)
        && !record.target_created
        && let Err(error) = switch(
            &record.target,
            record.target_branch.as_deref(),
            &record.target_head,
        )
        .and_then(|()| apply(&record.target, &record.target_stash))
    {
        messages.push(format!("目标恢复失败：{error}"));
    }
    if clean(&record.source).unwrap_or(false)
        && head(&record.source).is_ok_and(|actual| actual == record.source_head)
    {
        // 目标可能仍占用源分支，detach 源也可恢复文件；不会强行挪动他人正在使用的分支。
        if let Err(error) = switch(
            &record.source,
            record.source_branch.as_deref(),
            &record.source_head,
        ) {
            messages.push(format!("源分支恢复失败：{error}"));
        }
        if let Err(error) = apply(&record.source, &record.source_stash) {
            messages.push(format!("源文件恢复失败：{error}"));
        }
    }
    messages.push(format!(
        "恢复点均已保留（源 {:?}，目标 {:?}），不会自动清理存在修改的目录",
        record.source_stash, record.target_stash
    ));
    messages.join("；")
}

pub(crate) fn finish(
    record: &mut Transfer,
    receipt: &Path,
    remove_source: bool,
) -> Result<Value, String> {
    let mut result = record.result.clone().ok_or("Git 交接尚未准备完成")?;
    let mut warnings = Vec::new();
    if record.input.target_mode == "local"
        && remove_source
        && record.source.exists()
        && let Err(error) = remove_clean_worktree(&record.target, &record.source)
    {
        warnings.push(format!("会话已切回本地，原工作树保留：{error}"));
    }
    if !warnings.is_empty() {
        let existing = result["message"].as_str().unwrap_or_default();
        result["message"] = json!(format!("{existing} {}", warnings.join("；")).trim());
        warnings.clear();
    }
    // 失败恢复点与冲突恢复点留存；成功且包含清理结果的回执落盘后才可删除本次对象。
    record.result = Some(result.clone());
    record.completed = true;
    save_receipt(receipt, record)?;
    if result["message"].is_null() && result["conflictsDetected"] == false {
        for hash in [&record.source_stash, &record.target_stash]
            .into_iter()
            .flatten()
        {
            if let Err(error) = drop_hash(&record.target, hash) {
                warnings.push(format!("恢复点 {hash} 已保留：{error}"));
            }
        }
    }
    if !warnings.is_empty() {
        let existing = result["message"].as_str().unwrap_or_default();
        result["message"] = json!(format!("{existing} {}", warnings.join("；")).trim());
    }
    record.result = Some(result.clone());
    save_receipt(receipt, record)?;
    Ok(result)
}

/// Git worktree remove 默认也会删除 ignored 文件，必须在调用前显式检查，不能依赖未传 --force。
pub(crate) fn remove_clean_worktree(root: &Path, candidate: &Path) -> Result<(), String> {
    if !crate::ui_worktrees::is_linked_worktree(root, candidate) {
        return Err("清理目标不是该项目的关联工作树".into());
    }
    if !clean(candidate)?
        || !git(
            candidate,
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
            ],
        )?
        .is_empty()
    {
        return Err("工作树包含未提交修改或忽略文件，未删除".into());
    }
    let path = crate::path_utils::path_to_frontend(candidate);
    git(root, &["worktree", "remove", "--", &path])?;
    Ok(())
}

#[tauri::command]
pub async fn ui_git_handoff(app: AppHandle, input: HandoffInput) -> Result<Value, String> {
    let host = app
        .try_state::<Arc<crate::acp_host::AcpHost>>()
        .ok_or("工作树交接需要本进程持有本地 Host")?;
    host.handoff_workspace(input).await
}

/// 原交接前置步骤关闭该会话拥有的执行资源；保留历史与项目文件。
#[tauri::command]
pub async fn ui_thread_session_stop(app: AppHandle, session_id: String) -> Result<(), String> {
    let host = app
        .try_state::<Arc<crate::acp_host::AcpHost>>()
        .ok_or("停止会话需要本进程持有本地 Host")?;
    host.stop_workspace_session(&session_id).await
}

#[cfg(test)]
mod tests;
