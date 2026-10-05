//! Workflow 运行之间的 workspace 写入协调。
//!
//! 引擎的 `write_gate` 只覆盖单个 run；这里补充桌面进程内、按冻结工作目录共享的
//! 瞬时锁，使不同父 Session 指向同一 workspace 时不会同时进入真实写入路径。锁表
//! 只保存弱引用，不保存运行状态；Journal 仍是唯一事实源。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

static WORKSPACE_WRITE_GATES: OnceLock<Mutex<BTreeMap<String, Weak<AsyncMutex<()>>>>> =
    OnceLock::new();

/// 为同一规范化 workspace 取得跨 run 写锁；等待期间响应 Workflow 取消。
pub(crate) async fn acquire_workspace_write_gate(
    workspace: &Path,
    cancellation: &CancellationToken,
) -> Result<OwnedMutexGuard<()>, String> {
    if cancellation.is_cancelled() {
        return Err("workflow cancelled while waiting for workspace write gate".to_owned());
    }
    let gate = workspace_gate(workspace)?;
    tokio::select! {
        _ = cancellation.cancelled() => {
            Err("workflow cancelled while waiting for workspace write gate".to_owned())
        }
        guard = gate.lock_owned() => Ok(guard),
    }
}

fn workspace_gate(workspace: &Path) -> Result<Arc<AsyncMutex<()>>, String> {
    let key = workspace_key(workspace);
    let registry = WORKSPACE_WRITE_GATES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| "workspace write gate registry is unavailable".to_owned())?;
    registry.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = registry.get(&key).and_then(Weak::upgrade) {
        return Ok(gate);
    }
    let gate = Arc::new(AsyncMutex::new(()));
    registry.insert(key, Arc::downgrade(&gate));
    Ok(gate)
}

fn workspace_key(workspace: &Path) -> String {
    // Workflow cwd 已由宿主校验为绝对目录；canonicalize 还消除相对别名和符号链接
    // 差异。目录暂时不可访问时保留规范化字符串，让失败由真实工具报告。
    let path = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut key = path.to_string_lossy().replace('\\', "/");
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    if cfg!(windows) {
        key.make_ascii_lowercase();
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn same_workspace_serializes_writes() {
        let workspace = tempfile::tempdir().expect("workspace");
        let cancellation = CancellationToken::new();
        let first = acquire_workspace_write_gate(workspace.path(), &cancellation)
            .await
            .expect("first writer should acquire");
        let waiting_workspace = workspace.path().to_path_buf();
        let waiting_cancellation = CancellationToken::new();
        let mut waiter = tokio::spawn(async move {
            let guard = acquire_workspace_write_gate(&waiting_workspace, &waiting_cancellation)
                .await
                .expect("second writer should eventually acquire");
            drop(guard);
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiter)
                .await
                .is_err(),
            "same workspace must not admit a second writer while the first holds the gate"
        );
        drop(first);
        tokio::time::timeout(Duration::from_secs(1), &mut waiter)
            .await
            .expect("released workspace should wake the second writer")
            .expect("second writer task should finish");
    }

    #[tokio::test]
    async fn different_workspaces_do_not_share_gate() {
        let first_workspace = tempfile::tempdir().expect("first workspace");
        let second_workspace = tempfile::tempdir().expect("second workspace");
        let cancellation = CancellationToken::new();
        let first = acquire_workspace_write_gate(first_workspace.path(), &cancellation)
            .await
            .expect("first writer should acquire");
        let second = acquire_workspace_write_gate(second_workspace.path(), &cancellation)
            .await
            .expect("different workspace should acquire independently");
        drop(second);
        drop(first);
    }
}
