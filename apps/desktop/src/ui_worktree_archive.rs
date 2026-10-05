//! 原页面归档清理的 Git 回执。会话事实仍由 ACP Journal 持有，回执只恢复物理 checkout。
use crate::ui_git_stash::git;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::{AppHandle, Manager};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CleanupInput {
    pub cwd: String,
    pub path: String,
    pub thread_id: String,
    pub operation_id: String,
    /// 由适配层从准确归档响应取得；不是原页面的投影序号。
    pub journal_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ManagedCheckout {
    pub root: PathBuf,
    pub path: PathBuf,
    git_dir: PathBuf,
    token: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CleanupReceipt {
    pub session_id: String,
    pub operation_id: String,
    pub archive_sequence: u64,
    pub checkout: ManagedCheckout,
    pub branch: String,
    pub head: String,
    /// prepared 在删除前落盘；removed 在 Git 成功后落盘；recovered 证明同路径已恢复。
    pub phase: String,
}

fn keyed_path(data: &Path, directory: &str, key: &str) -> PathBuf {
    data.join(directory)
        .join(format!("{:x}.json", Sha256::digest(key.as_bytes())))
}

fn managed_path(data: &Path, checkout: &Path) -> PathBuf {
    keyed_path(data, "ui-managed-worktrees", &checkout.to_string_lossy())
}

pub(crate) fn receipt_path(data: &Path, session_id: &str) -> PathBuf {
    keyed_path(data, "ui-archive-worktrees", session_id)
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let bytes = crate::storage::read_private_bytes_bounded(path, 64 * 1024, "工作树恢复回执")
        .map_err(|error| error.to_string())?;
    bytes
        .map(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()))
        .transpose()
}

pub(crate) fn save(path: &Path, record: &impl Serialize) -> Result<(), String> {
    crate::storage::atomic_write_private(
        path,
        &serde_json::to_vec(record).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn git_dir(checkout: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(
        git(
            checkout,
            &["rev-parse", "--path-format=absolute", "--git-dir"],
        )?
        .trim(),
    )
    .map_err(|error| error.to_string())
}

/// 只在宿主确实创建 checkout 后登记；Git admin 中的随机标记避免同路径重建冒充旧工作树。
pub(crate) fn mark_created(data: &Path, root: &Path, checkout: &Path) -> Result<(), String> {
    if !crate::ui_worktrees::is_linked_worktree(root, checkout) {
        return Err("无法登记非关联工作树".into());
    }
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).map_err(|_| "无法生成工作树创建标记")?;
    let record = ManagedCheckout {
        root: root.to_owned(),
        path: checkout.to_owned(),
        git_dir: git_dir(checkout)?,
        token: format!("{:x}", Sha256::digest(nonce)),
    };
    crate::storage::atomic_write_private(
        &record.git_dir.join("keencode-ui-managed"),
        record.token.as_bytes(),
    )
    .map_err(|error| error.to_string())?;
    save(&managed_path(data, checkout), &record)
}

pub(crate) fn managed_checkout(
    data: &Path,
    root: &Path,
    checkout: &Path,
) -> Result<ManagedCheckout, String> {
    let record: ManagedCheckout =
        read(&managed_path(data, checkout))?.ok_or("工作树没有宿主创建记录，已保留")?;
    validate_identity(&record, root, checkout)?;
    Ok(record)
}

/// 归档入口先按当前 checkout 查找登记的拥有根，再做完整 token/Git 身份校验。
///
/// 当前 workspace 可能本身就是 handoff 后的 linked worktree；此时不能把
/// workspace 路径误当成 `ManagedCheckout.root`。这里仅返回持久登记的根，调用方
/// 仍必须用 `managed_checkout` 完成授权校验后才可执行删除。
pub(crate) fn managed_checkout_owner(data: &Path, checkout: &Path) -> Result<PathBuf, String> {
    let record: ManagedCheckout =
        read(&managed_path(data, checkout))?.ok_or("工作树没有宿主创建记录，已保留")?;
    if !record.root.is_absolute() || record.path != checkout {
        return Err("工作树登记身份无效，已保留".into());
    }
    Ok(record.root)
}

fn validate_identity(record: &ManagedCheckout, root: &Path, checkout: &Path) -> Result<(), String> {
    if record.root != root
        || record.path != checkout
        || !crate::ui_worktrees::is_linked_worktree(root, checkout)
        || git_dir(checkout)? != record.git_dir
    {
        return Err("工作树身份已改变，未操作目录".into());
    }
    let token = crate::storage::read_private_bytes_bounded(
        &record.git_dir.join("keencode-ui-managed"),
        128,
        "工作树标记",
    )
    .map_err(|error| error.to_string())?;
    if token.as_deref() != Some(record.token.as_bytes()) {
        return Err("工作树创建标记已改变，未操作目录".into());
    }
    Ok(())
}

pub(crate) fn read_receipt(data: &Path, id: &str) -> Result<Option<CleanupReceipt>, String> {
    let record: Option<CleanupReceipt> = read(&receipt_path(data, id))?;
    if let Some(record) = &record
        && (record.session_id != id
            || !matches!(record.phase.as_str(), "prepared" | "removed" | "recovered")
            || !record.checkout.path.is_absolute()
            || !record.checkout.root.is_absolute())
    {
        return Err("工作树恢复回执身份无效".into());
    }
    Ok(record)
}

/// 删除前保存 commit 的私有 ref，防止分支外部更新及 GC 后失去准确恢复点。
fn recovery_ref(record: &CleanupReceipt) -> String {
    format!(
        "refs/keencode/archive-worktrees/{:x}",
        Sha256::digest(record.session_id.as_bytes())
    )
}

pub(crate) fn prepare_receipt(
    data: &Path,
    input: &CleanupInput,
    checkout: ManagedCheckout,
) -> Result<CleanupReceipt, String> {
    let branch = git(
        &checkout.path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )
    .map_err(|_| "detached 工作树不参与自动清理，已保留")?
    .trim()
    .to_owned();
    let head = git(&checkout.path, &["rev-parse", "--verify", "HEAD^{commit}"])?
        .trim()
        .to_owned();
    let record = CleanupReceipt {
        session_id: input.thread_id.clone(),
        operation_id: input.operation_id.clone(),
        archive_sequence: input.journal_sequence,
        checkout,
        branch,
        head,
        phase: "prepared".into(),
    };
    git(
        &record.checkout.root,
        &["update-ref", &recovery_ref(&record), &record.head],
    )?;
    save(&receipt_path(data, &record.session_id), &record)?;
    Ok(record)
}

pub(crate) fn remove_checkout(data: &Path, record: &mut CleanupReceipt) -> Result<(), String> {
    validate_identity(
        &record.checkout,
        &record.checkout.root,
        &record.checkout.path,
    )?;
    if git(
        &record.checkout.path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?
    .trim()
        != record.branch
        || git(
            &record.checkout.path,
            &["rev-parse", "--verify", "HEAD^{commit}"],
        )?
        .trim()
            != record.head
    {
        return Err("工作树分支或 HEAD 已改变，已保留".into());
    }
    crate::ui_worktree_handoff::remove_clean_worktree(
        &record.checkout.root,
        &record.checkout.path,
    )?;
    record.phase = "removed".into();
    save(&receipt_path(data, &record.session_id), record)
}

/// 不覆盖既有未知目录。外部分支更新或被占用时从保存的 commit detached 恢复，保留用户 ref。
pub(crate) fn recover_checkout(data: &Path, record: &mut CleanupReceipt) -> Result<bool, String> {
    match fs::symlink_metadata(&record.checkout.path) {
        Ok(_) => {
            if let Err(error) = validate_identity(
                &record.checkout,
                &record.checkout.root,
                &record.checkout.path,
            ) {
                // 恢复已创建并登记，但最终回执落盘被中断：只接受宿主新 marker 与保存 commit 同时匹配。
                // 没有新创建记录的外部目录始终保留现场，不猜测属于本次恢复。
                if record.phase == "recovered" {
                    return Err(error);
                }
                let current = managed_checkout(data, &record.checkout.root, &record.checkout.path)?;
                if git(&current.path, &["rev-parse", "--verify", "HEAD^{commit}"])?.trim()
                    != record.head
                {
                    return Err("恢复目录中的 commit 已改变，未修改目录".into());
                }
                record.checkout = current;
            }
            let changed = record.phase != "recovered";
            record.phase = "recovered".into();
            save(&receipt_path(data, &record.session_id), record)?;
            return Ok(changed);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    // parent 的规范路径必须等于原记录，不能跟随后来添加的 junction/symlink 去其他位置。
    let parent = record.checkout.path.parent().ok_or("恢复目录没有父目录")?;
    if fs::canonicalize(parent).map_err(|error| error.to_string())? != parent {
        return Err("工作树父目录身份已改变，未恢复".into());
    }
    let pinned = git(
        &record.checkout.root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", recovery_ref(record)),
        ],
    )?;
    if pinned.trim() != record.head {
        return Err("工作树恢复 commit 身份已改变".into());
    }
    let reference = format!("refs/heads/{}", record.branch);
    let unchanged = git(
        &record.checkout.root,
        &["rev-parse", "--verify", &reference],
    )
    .is_ok_and(|head| head.trim() == record.head);
    let occupied = git(
        &record.checkout.root,
        &["worktree", "list", "--porcelain", "-z"],
    )?
    .split('\0')
    .any(|field| field == format!("branch {reference}"));
    let target = crate::path_utils::path_to_frontend(&record.checkout.path);
    // prepared + 目录缺失意味着 Git 删除成功但回执落盘被中断；同样只恢复，不重复清理。
    let mut args = vec!["worktree", "add"];
    if !unchanged || occupied {
        args.push("--detach");
    }
    args.extend([
        "--",
        &target,
        if unchanged && !occupied {
            &record.branch
        } else {
            &record.head
        },
    ]);
    git(&record.checkout.root, &args)?;
    mark_created(data, &record.checkout.root, &record.checkout.path)?;
    record.checkout = managed_checkout(data, &record.checkout.root, &record.checkout.path)?;
    record.phase = "recovered".into();
    save(&receipt_path(data, &record.session_id), record)?;
    Ok(true)
}

#[tauri::command]
pub async fn ui_git_archive_cleanup(app: AppHandle, input: CleanupInput) -> Result<(), String> {
    let host = app
        .try_state::<Arc<crate::acp_host::AcpHost>>()
        .ok_or("归档清理需要本地 ACP Host")?;
    host.cleanup_archived_worktree(input).await
}

#[tauri::command]
pub async fn ui_git_archive_recover(app: AppHandle, session_id: String) -> Result<bool, String> {
    let host = app
        .try_state::<Arc<crate::acp_host::AcpHost>>()
        .ok_or("工作树恢复需要本地 ACP Host")?;
    host.recover_archived_worktree(&session_id).await
}

/// 只返回仍存在权威 Session 的恢复归属；物理工作树列表始终由 Git 单独返回。
#[tauri::command]
pub fn ui_git_archive_records(app: AppHandle) -> Result<serde_json::Value, String> {
    let data = crate::storage::root_dir(&app).map_err(|error| error.to_string())?;
    let runtime = app
        .try_state::<Arc<crate::agent_runtime::AgentRuntime>>()
        .ok_or("工作树恢复需要本地 Runtime")?;
    let mut result = Vec::new();
    for session in runtime
        .stored_sessions()
        .map_err(|error| error.to_string())?
    {
        if let Some(record) = read_receipt(&data, session.session_id.as_str())? {
            if crate::path_utils::path_to_frontend(Path::new(&session.project_root))
                != crate::path_utils::path_to_frontend(&record.checkout.path)
            {
                continue;
            }
            if !crate::workspace::registered_project_root(
                &app,
                &crate::path_utils::path_to_frontend(&record.checkout.root),
            )
            .is_ok_and(|root| root == record.checkout.root)
            {
                continue;
            }
            result.push(serde_json::json!({"sessionId":record.session_id,
                "root":crate::path_utils::path_to_frontend(&record.checkout.root),
                "path":crate::path_utils::path_to_frontend(&record.checkout.path), "branch":record.branch,"head":record.head}));
        }
    }
    Ok(serde_json::Value::Array(result))
}

#[cfg(test)]
mod tests;
