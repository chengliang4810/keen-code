//! Tests for mod_attrib

use super::*;

#[test]
fn test_git_attribution_reset_clears_pending() {
    let mw = GitAttributionMiddleware::new("test-model");
    // 插入一些待处理内容
    mw.pending_old_content
        .lock()
        .unwrap()
        .insert("file1.rs".to_string(), "old content".to_string());
    mw.pending_old_content
        .lock()
        .unwrap()
        .insert("file2.rs".to_string(), "more content".to_string());
    assert_eq!(mw.pending_old_content.lock().unwrap().len(), 2);

    // reset 后应清空
    mw.reset();
    assert!(mw.pending_old_content.lock().unwrap().is_empty());
}

#[test]
fn test_branch_drift_reports_each_change_once() {
    let mw = GitAttributionMiddleware::new("test-model");

    assert_eq!(mw.observe_branch("main".to_string()), None);
    assert_eq!(mw.observe_branch("main".to_string()), None);
    assert_eq!(
        mw.observe_branch("feature".to_string()),
        Some(("main".to_string(), "feature".to_string()))
    );
    assert_eq!(mw.observe_branch("feature".to_string()), None);
}

#[test]
fn test_prompt_contribution_has_no_outer_blank_lines() {
    let mw = GitAttributionMiddleware::new("test-model");
    let contribution = Middleware::prompt_contribution(&mw).expect("应有 attribution 提示");

    assert_eq!(contribution.trim(), contribution);
    assert!(contribution.starts_with("## Git Attribution"));
    assert!(!contribution.ends_with('\n'));
}

/// Git 状态查询失败时仍应沿用原有的“无分支则跳过”错误契约。
#[tokio::test]
async fn test_current_branch_failure_returns_none() {
    let directory = tempfile::tempdir().expect("创建临时目录失败");

    assert!(
        GitAttributionMiddleware::current_branch(directory.path().to_str().unwrap())
            .await
            .is_none()
    );
}
