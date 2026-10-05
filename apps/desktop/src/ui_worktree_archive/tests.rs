use super::*;

fn fixture(branch: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repo");
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let data = temp.path().join("data");
    git(&root, &["init", "--initial-branch=main"]).unwrap();
    git(&root, &["config", "core.autocrlf", "false"]).unwrap();
    git(&root, &["config", "user.name", "Fixture"]).unwrap();
    git(&root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::write(root.join("file.txt"), b"original\n").unwrap();
    fs::write(root.join(".gitignore"), b"ignored.txt\n").unwrap();
    git(&root, &["add", "--all"]).unwrap();
    git(
        &root,
        &["commit", "-m", "测试：归档恢复 / test: archive recovery"],
    )
    .unwrap();
    let target = temp.path().join("checkout");
    let text = crate::path_utils::path_to_frontend(&target);
    if let Some(branch) = branch {
        git(
            &root,
            &["worktree", "add", "-b", branch, "--", &text, "HEAD"],
        )
        .unwrap();
    } else {
        git(&root, &["worktree", "add", "--detach", "--", &text, "HEAD"]).unwrap();
    }
    let target = fs::canonicalize(target).unwrap();
    mark_created(&data, &root, &target).unwrap();
    (temp, root, target, data)
}

fn prepare(data: &Path, root: &Path, target: &Path) -> CleanupReceipt {
    prepare_receipt(
        data,
        &CleanupInput {
            cwd: crate::path_utils::path_to_frontend(root),
            path: crate::path_utils::path_to_frontend(target),
            thread_id: "fixture-session".into(),
            operation_id: "archive-a".into(),
            journal_sequence: 7,
        },
        managed_checkout(data, root, target).unwrap(),
    )
    .unwrap()
}

#[test]
fn handoff_checkout_uses_recorded_owner_root_for_archive_validation() {
    let (temp, root, source, data) = fixture(Some("feat/handoff-archive"));
    let target = temp.path().join("handoff-target");
    let target_text = crate::path_utils::path_to_frontend(&target);
    git(
        &source,
        &[
            "worktree",
            "add",
            "-b",
            "feat/handoff-archive-child",
            "--",
            &target_text,
            "HEAD",
        ],
    )
    .unwrap();
    let target = fs::canonicalize(target).unwrap();
    mark_created(&data, &source, &target).unwrap();
    let owner = managed_checkout_owner(&data, &target).unwrap();
    assert_eq!(owner, source);
    assert_eq!(
        crate::ui_worktree_remove::validate_target(
            &owner,
            &crate::path_utils::path_to_frontend(&target),
        )
        .unwrap(),
        target
    );
    assert!(managed_checkout_owner(&data, &root).is_err());
}

#[test]
fn clean_cleanup_cold_roundtrip_preserves_named_branch_and_exact_commit() {
    let (_temp, root, target, data) = fixture(Some("synara/user-name"));
    let mut record = prepare(&data, &root, &target);
    remove_checkout(&data, &mut record).unwrap();
    assert!(!target.exists());
    assert!(
        git(
            &root,
            &["show-ref", "--verify", "refs/heads/synara/user-name"]
        )
        .is_ok()
    );
    let mut cold = read_receipt(&data, "fixture-session").unwrap().unwrap();
    assert!(recover_checkout(&data, &mut cold).unwrap());
    assert_eq!(fs::read(target.join("file.txt")).unwrap(), b"original\n");
    assert_eq!(
        git(&target, &["branch", "--show-current"]).unwrap().trim(),
        "synara/user-name"
    );
    assert_eq!(
        git(&target, &["rev-parse", "HEAD"]).unwrap().trim(),
        cold.head
    );
    assert!(!recover_checkout(&data, &mut cold).unwrap());
    // 新一次归档用恢复后的宿主身份，旧 marker 不阻断合法的第二轮。
    let mut next = prepare(&data, &root, &target);
    next.operation_id = "archive-b".into();
    remove_checkout(&data, &mut next).unwrap();
    recover_checkout(&data, &mut next).unwrap();
}

#[test]
fn dirty_and_ignored_files_are_preserved_and_detached_is_not_cleaned() {
    let (_temp, root, target, data) = fixture(Some("feat/dirty"));
    fs::write(target.join("ignored.txt"), b"private\n").unwrap();
    let mut record = prepare(&data, &root, &target);
    assert!(
        remove_checkout(&data, &mut record)
            .unwrap_err()
            .contains("忽略文件")
    );
    assert_eq!(fs::read(target.join("ignored.txt")).unwrap(), b"private\n");
    fs::remove_file(target.join("ignored.txt")).unwrap();
    fs::write(target.join("file.txt"), b"changes\n").unwrap();
    assert!(remove_checkout(&data, &mut record).is_err());
    assert_eq!(fs::read(target.join("file.txt")).unwrap(), b"changes\n");
    let (_temp2, root2, target2, data2) = fixture(None);
    let input = CleanupInput {
        cwd: root2.to_string_lossy().into(),
        path: target2.to_string_lossy().into(),
        thread_id: "detached".into(),
        operation_id: "archive".into(),
        journal_sequence: 7,
    };
    assert!(
        prepare_receipt(
            &data2,
            &input,
            managed_checkout(&data2, &root2, &target2).unwrap()
        )
        .unwrap_err()
        .contains("detached")
    );
    assert!(target2.exists());
}

#[test]
fn external_branch_update_is_preserved_and_saved_commit_recovers_detached() {
    let (_temp, root, target, data) = fixture(Some("feat/external"));
    let mut record = prepare(&data, &root, &target);
    remove_checkout(&data, &mut record).unwrap();
    fs::write(root.join("file.txt"), b"external new commit\n").unwrap();
    git(&root, &["add", "--all"]).unwrap();
    git(&root, &["commit", "-m", "外部修改 / external update"]).unwrap();
    let external = git(&root, &["rev-parse", "HEAD"]).unwrap();
    git(
        &root,
        &["update-ref", "refs/heads/feat/external", external.trim()],
    )
    .unwrap();
    git(&root, &["gc", "--prune=now"]).unwrap();
    recover_checkout(&data, &mut record).unwrap();
    assert_eq!(
        git(&target, &["rev-parse", "HEAD"]).unwrap().trim(),
        record.head
    );
    assert!(
        git(&target, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .is_empty()
    );
    assert_eq!(
        git(&root, &["rev-parse", "refs/heads/feat/external"]).unwrap(),
        external
    );
}

#[test]
fn branch_in_another_checkout_recovers_detached_without_stealing_ref() {
    let (temp, root, target, data) = fixture(Some("feat/occupied"));
    let mut record = prepare(&data, &root, &target);
    remove_checkout(&data, &mut record).unwrap();
    let other = temp.path().join("other");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--",
            &other.to_string_lossy(),
            "feat/occupied",
        ],
    )
    .unwrap();
    recover_checkout(&data, &mut record).unwrap();
    assert!(
        git(&target, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .is_empty()
    );
    assert_eq!(
        git(&other, &["branch", "--show-current"]).unwrap().trim(),
        "feat/occupied"
    );
}

#[test]
fn crash_after_git_removal_with_prepared_receipt_restores_original_directory() {
    let (_temp, root, target, data) = fixture(Some("feat/crash"));
    let record = prepare(&data, &root, &target);
    crate::ui_worktree_handoff::remove_clean_worktree(&root, &target).unwrap();
    let mut cold = read_receipt(&data, "fixture-session").unwrap().unwrap();
    assert_eq!(cold.phase, "prepared");
    recover_checkout(&data, &mut cold).unwrap();
    assert_eq!(
        git(&target, &["rev-parse", "HEAD"]).unwrap().trim(),
        record.head
    );
}

#[test]
fn unknown_directory_and_recreated_git_checkout_are_not_overwritten() {
    let (_temp, root, target, data) = fixture(Some("feat/collision"));
    let mut record = prepare(&data, &root, &target);
    remove_checkout(&data, &mut record).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep.txt"), b"unrelated\n").unwrap();
    assert!(recover_checkout(&data, &mut record).is_err());
    assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"unrelated\n");
    fs::remove_file(target.join("keep.txt")).unwrap();
    fs::remove_dir(&target).unwrap();
    git(
        &root,
        &[
            "worktree",
            "add",
            "--",
            &crate::path_utils::path_to_frontend(&target),
            "feat/collision",
        ],
    )
    .unwrap();
    assert!(recover_checkout(&data, &mut record).is_err());
    assert!(managed_checkout(&data, &root, &target).is_err());
    assert!(target.join("file.txt").exists());
}

#[test]
fn branch_head_change_after_receipt_prevents_removal() {
    let (_temp, root, target, data) = fixture(Some("feat/race"));
    let mut record = prepare(&data, &root, &target);
    fs::write(target.join("file.txt"), b"new commit\n").unwrap();
    git(&target, &["add", "--all"]).unwrap();
    git(&target, &["commit", "-m", "并发提交 / concurrent commit"]).unwrap();
    assert!(
        remove_checkout(&data, &mut record)
            .unwrap_err()
            .contains("HEAD")
    );
    assert!(target.exists());
}

#[test]
fn interrupted_recovery_after_managed_marker_only_completes_receipt() {
    let (_temp, root, target, data) = fixture(Some("feat/recovery-crash"));
    let mut record = prepare(&data, &root, &target);
    remove_checkout(&data, &mut record).unwrap();
    git(
        &root,
        &[
            "worktree",
            "add",
            "--",
            &crate::path_utils::path_to_frontend(&target),
            "feat/recovery-crash",
        ],
    )
    .unwrap();
    mark_created(&data, &root, &target).unwrap();
    // 磁盘仍是 removed 的旧 token，新 Git marker 证明中断发生在宿主恢复之后。
    let mut cold = read_receipt(&data, "fixture-session").unwrap().unwrap();
    let before = fs::read(target.join("file.txt")).unwrap();
    assert!(recover_checkout(&data, &mut cold).unwrap());
    assert_eq!(cold.phase, "recovered");
    assert_eq!(fs::read(target.join("file.txt")).unwrap(), before);
}

#[tokio::test]
async fn cold_history_retains_authoritative_cwd_archive_and_title_after_cleanup_and_undo() {
    let (_temp, root, target, data) = fixture(Some("feat/history"));
    let runtime = crate::agent_runtime::AgentRuntime::new_for_control_test(&data).unwrap();
    let session = runtime
        .open_or_create_session(&target, None, "history-a")
        .unwrap();
    let id = session.session_id().as_str().to_owned();
    session.rename("title-a", "历史必须保留", None).unwrap();
    let archived = session
        .set_preference("archive-a", None, Some(true))
        .unwrap();
    let input = CleanupInput {
        cwd: root.to_string_lossy().into(),
        path: target.to_string_lossy().into(),
        thread_id: id.clone(),
        operation_id: "archive-a".into(),
        journal_sequence: archived.last_sequence,
    };
    drop(session);
    runtime.close_session(&id).await.unwrap();
    let mut record = prepare_receipt(
        &data,
        &input,
        managed_checkout(&data, &root, &target).unwrap(),
    )
    .unwrap();
    remove_checkout(&data, &mut record).unwrap();
    let cold = runtime
        .runtime_manager()
        .stored_session_metadata(&id)
        .unwrap();
    assert!(cold.archived);
    assert_eq!(cold.title, "历史必须保留");
    assert_eq!(Path::new(&cold.project_root), target);
    recover_checkout(&data, &mut record).unwrap();
    let reopened = runtime
        .open_or_create_session(&target, Some(&id), "unused")
        .unwrap();
    assert_eq!(reopened.snapshot().unwrap().state.title, "历史必须保留");
    reopened
        .set_preference("undo-a", None, Some(false))
        .unwrap();
    assert!(!reopened.snapshot().unwrap().state.archived);
    drop(reopened);
    runtime.close_session(&id).await.unwrap();
}
