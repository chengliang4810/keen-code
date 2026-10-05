use super::*;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repo");
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    git(&root, &["init", "--initial-branch=main"]).unwrap();
    git(&root, &["config", "core.autocrlf", "false"]).unwrap();
    git(&root, &["config", "user.name", "Fixture"]).unwrap();
    git(&root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::write(root.join("file.txt"), b"base\n").unwrap();
    fs::write(root.join("local.txt"), b"local base\n").unwrap();
    fs::write(root.join(".gitignore"), b"ignored.txt\n").unwrap();
    git(&root, &["add", "--all"]).unwrap();
    git(
        &root,
        &["commit", "-m", "测试：交接基线 / test: handoff baseline"],
    )
    .unwrap();
    (temporary, root)
}

fn input(root: &Path, command: &str) -> HandoffInput {
    HandoffInput {
        command_id: command.into(),
        thread_id: "fixture-session".into(),
        cwd: crate::path_utils::path_to_frontend(root),
        target_mode: "worktree".into(),
        current_branch: Some("main".into()),
        worktree_path: None,
        associated_worktree_path: None,
        associated_worktree_branch: None,
        associated_worktree_ref: None,
        preferred_local_branch: Some("main".into()),
        preferred_worktree_base_branch: Some("main".into()),
        preferred_new_worktree_name: Some("handoff-proof".into()),
    }
}

fn dirty(root: &Path) {
    fs::write(root.join("file.txt"), b"base\nstaged\n").unwrap();
    git(root, &["add", "--", "file.txt"]).unwrap();
    fs::write(root.join("file.txt"), b"base\nstaged\nworking\n").unwrap();
    fs::write(root.join("未跟踪[1].bin"), [0, 255, 1]).unwrap();
}

fn assert_changes(root: &Path) {
    assert_eq!(
        fs::read(root.join("file.txt")).unwrap(),
        b"base\nstaged\nworking\n"
    );
    assert_eq!(git(root, &["show", ":file.txt"]).unwrap(), "base\nstaged\n");
    assert_eq!(fs::read(root.join("未跟踪[1].bin")).unwrap(), [0, 255, 1]);
}

fn move_to_worktree(root: &Path, data: &Path) -> (Transfer, PathBuf) {
    let request = input(root, "to-worktree");
    let receipt = receipt_path(data, &request);
    let transfer = begin(root, root, request, &receipt).unwrap();
    let mut transfer = prepare(root, transfer, &receipt).unwrap();
    finish(&mut transfer, &receipt, true).unwrap();
    (transfer, receipt)
}

#[test]
fn roundtrip_and_associated_reuse_preserve_index_untracked_and_local_changes() {
    let (temporary, root) = fixture();
    fs::write(root.join("prior.txt"), b"prior\n").unwrap();
    git(&root, &["stash", "push", "-u", "-m", "prior user stash"]).unwrap();
    let prior = entries(&root).unwrap();
    dirty(&root);
    let (first, _) = move_to_worktree(&root, temporary.path());
    assert!(clean(&root).unwrap());
    assert_changes(&first.target);
    assert_eq!(branch(&root).unwrap().as_deref(), Some("main"));
    assert_eq!(
        branch(&first.target).unwrap().as_deref(),
        Some("feat/handoff-proof")
    );
    assert_eq!(entries(&root).unwrap(), prior);
    fs::write(root.join("local.txt"), b"local edits\n").unwrap();
    let mut back = input(&root, "to-local");
    back.target_mode = "local".into();
    back.worktree_path = Some(crate::path_utils::path_to_frontend(&first.target));
    let receipt = receipt_path(temporary.path(), &back);
    let record = begin(&root, &first.target, back, &receipt).unwrap();
    let mut record = prepare(&root, record, &receipt).unwrap();
    assert_changes(&root);
    assert_eq!(fs::read(root.join("local.txt")).unwrap(), b"local edits\n");
    let result = finish(&mut record, &receipt, true).unwrap();
    assert!(!first.target.exists());
    assert_eq!(
        branch(&root).unwrap().as_deref(),
        Some("feat/handoff-proof")
    );
    assert_eq!(result["changesTransferred"], true);
    assert_eq!(result["conflictsDetected"], false);
    assert_eq!(entries(&root).unwrap(), prior);
    let mut again = input(&root, "reuse-worktree");
    again.associated_worktree_path = result["associatedWorktreePath"].as_str().map(str::to_owned);
    again.associated_worktree_branch = result["associatedWorktreeBranch"]
        .as_str()
        .map(str::to_owned);
    again.associated_worktree_ref = result["associatedWorktreeRef"].as_str().map(str::to_owned);
    let receipt = receipt_path(temporary.path(), &again);
    let record = begin(&root, &root, again, &receipt).unwrap();
    let mut record = prepare(&root, record, &receipt).unwrap();
    finish(&mut record, &receipt, true).unwrap();
    assert_eq!(record.target, first.target);
    assert_changes(&record.target);
    assert_eq!(
        fs::read(record.target.join("local.txt")).unwrap(),
        b"local edits\n"
    );
    assert!(clean(&root).unwrap());
    assert_eq!(
        branch(&record.target).unwrap().as_deref(),
        Some("feat/handoff-proof")
    );
    assert_eq!(entries(&root).unwrap(), prior);
}

#[test]
fn creation_failure_restores_source_without_erasing_recovery_points() {
    let (temporary, root) = fixture();
    dirty(&root);
    let original = git(&root, &["status", "--porcelain", "-z"]).unwrap();
    let mut request = input(&root, "missing-reference");
    request.preferred_worktree_base_branch = Some("missing-reference".into());
    let receipt = receipt_path(temporary.path(), &request);
    let record = begin(&root, &root, request.clone(), &receipt).unwrap();
    let error = prepare(&root, record, &receipt).unwrap_err();
    assert!(error.contains("恢复点均已保留"));
    assert_changes(&root);
    assert_eq!(
        git(&root, &["status", "--porcelain", "-z"]).unwrap(),
        original
    );
    assert_eq!(branch(&root).unwrap().as_deref(), Some("main"));
    assert_eq!(entries(&root).unwrap().len(), 1);
    let record = read_receipt(&receipt, &request).unwrap().unwrap();
    assert!(record.result.is_none());
    assert!(!record.completed);
}

#[test]
fn conflicting_target_keeps_source_and_both_saved_modifications() {
    let (temporary, root) = fixture();
    let target = temporary.path().join("conflict-tree");
    let target_text = crate::path_utils::path_to_frontend(&target);
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            "feat/conflict",
            "--",
            &target_text,
            "HEAD",
        ],
    )
    .unwrap();
    fs::write(target.join("file.txt"), b"target commit\n").unwrap();
    git(&target, &["add", "--all"]).unwrap();
    git(
        &target,
        &["commit", "-m", "测试：目标冲突 / test: target conflict"],
    )
    .unwrap();
    fs::write(target.join("local.txt"), b"target own edits\n").unwrap();
    // index 保持基线，令 stash apply 进入真实三方合并，而不是提前报告无法恢复 index。
    fs::write(root.join("file.txt"), b"source conflict\n").unwrap();
    fs::write(root.join("未跟踪[1].bin"), [0, 255, 1]).unwrap();
    let mut request = input(&root, "conflict");
    request.associated_worktree_path = Some(target_text);
    request.associated_worktree_branch = Some("feat/conflict".into());
    request.associated_worktree_ref = Some(head(&target).unwrap());
    let receipt = receipt_path(temporary.path(), &request);
    let record = begin(&root, &root, request, &receipt).unwrap();
    let error = prepare(&root, record, &receipt).unwrap_err();
    assert!(error.contains("交接失败"));
    assert_eq!(
        fs::read(root.join("file.txt")).unwrap(),
        b"source conflict\n"
    );
    assert_eq!(fs::read(root.join("未跟踪[1].bin")).unwrap(), [0, 255, 1]);
    assert!(target.exists());
    assert!(
        !git(&target, &["diff", "--name-only", "--diff-filter=U"])
            .unwrap()
            .is_empty()
    );
    assert_eq!(entries(&root).unwrap().len(), 2);
    assert_eq!(branch(&root).unwrap().as_deref(), Some("main"));
}

#[test]
fn overlapping_local_edits_are_reported_without_inventing_merge_conflicts() {
    let (temporary, root) = fixture();
    fs::write(root.join("file.txt"), b"source conflict\n").unwrap();
    let (first, _) = move_to_worktree(&root, temporary.path());
    fs::write(root.join("file.txt"), b"original local conflict\n").unwrap();
    let mut request = input(&root, "restore-local-conflict");
    request.target_mode = "local".into();
    request.worktree_path = Some(crate::path_utils::path_to_frontend(&first.target));
    let receipt = receipt_path(temporary.path(), &request);
    let record = begin(&root, &first.target, request, &receipt).unwrap();
    let mut record = prepare(&root, record, &receipt).unwrap();
    let result = finish(&mut record, &receipt, true).unwrap();
    assert_eq!(result["changesTransferred"], true);
    assert!(result["message"].is_string());
    assert_eq!(entries(&root).unwrap().len(), 2);
    // 重叠的脏工作文件会在三方合并前被 Git 拒绝，不能把此错误伪报为已产生 merge conflict。
    assert_eq!(result["conflictsDetected"], false);
    assert!(
        git(&root, &["diff", "--name-only", "--diff-filter=U"])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fs::read(root.join("file.txt")).unwrap(),
        b"source conflict\n"
    );
    let target_stash = record.target_stash.as_ref().unwrap();
    assert_eq!(
        git(&root, &["show", &format!("{target_stash}:file.txt")]).unwrap(),
        "original local conflict\n"
    );
    assert!(record.completed);
}

#[test]
fn failed_cwd_commit_restores_both_checkouts_without_discarding_recovery_points() {
    let (temporary, root) = fixture();
    dirty(&root);
    let (first, _) = move_to_worktree(&root, temporary.path());
    fs::write(root.join("local.txt"), b"local before failed commit\n").unwrap();
    let mut request = input(&root, "failed-cwd-commit");
    request.target_mode = "local".into();
    request.worktree_path = Some(crate::path_utils::path_to_frontend(&first.target));
    let receipt = receipt_path(temporary.path(), &request);
    let record = begin(&root, &first.target, request, &receipt).unwrap();
    let record = prepare(&root, record, &receipt).unwrap();
    let recovery = rollback(&record);
    assert!(!recovery.contains("恢复失败"), "{recovery}");
    assert_eq!(branch(&root).unwrap().as_deref(), Some("main"));
    assert_eq!(
        branch(&first.target).unwrap().as_deref(),
        Some("feat/handoff-proof")
    );
    assert_eq!(fs::read(root.join("file.txt")).unwrap(), b"base\n");
    assert_eq!(
        fs::read(root.join("local.txt")).unwrap(),
        b"local before failed commit\n"
    );
    assert!(!root.join("未跟踪[1].bin").exists());
    assert_changes(&first.target);
    assert_eq!(entries(&root).unwrap().len(), 3);
}

#[test]
fn rollback_preserves_external_changes_written_after_git_preparation() {
    let (temporary, root) = fixture();
    dirty(&root);
    let request = input(&root, "external-modification");
    let receipt = receipt_path(temporary.path(), &request);
    let record = begin(&root, &root, request, &receipt).unwrap();
    let record = prepare(&root, record, &receipt).unwrap();
    fs::write(record.target.join("external.txt"), b"external write\n").unwrap();
    let recovery = rollback(&record);
    assert!(recovery.contains("出现新修改"));
    assert_eq!(
        fs::read(record.target.join("external.txt")).unwrap(),
        b"external write\n"
    );
    assert_changes(&root);
    assert_changes(&record.target);
    assert_eq!(entries(&root).unwrap().len(), 1);
}

#[test]
fn cleanup_does_not_force_delete_ignored_files_or_rehome_another_branch() {
    let (temporary, root) = fixture();
    let (first, _) = move_to_worktree(&root, temporary.path());
    fs::write(first.target.join("ignored.txt"), b"keep ignored data\n").unwrap();
    let mut request = input(&root, "keep-ignored");
    request.target_mode = "local".into();
    request.worktree_path = Some(crate::path_utils::path_to_frontend(&first.target));
    let receipt = receipt_path(temporary.path(), &request);
    let record = begin(&root, &first.target, request.clone(), &receipt).unwrap();
    let mut record = prepare(&root, record, &receipt).unwrap();
    let result = finish(&mut record, &receipt, true).unwrap();
    assert_eq!(
        fs::read(first.target.join("ignored.txt")).unwrap(),
        b"keep ignored data\n"
    );
    assert!(result["message"].as_str().unwrap().contains("原工作树保留"));
    assert!(crate::ui_worktrees::is_linked_worktree(
        &root,
        &first.target
    ));
    assert_eq!(
        branch(&root).unwrap().as_deref(),
        Some("feat/handoff-proof")
    );
    assert!(read_receipt(&receipt, &request).unwrap().unwrap().completed);
    request.preferred_new_worktree_name = Some("changed-payload".into());
    assert!(
        read_receipt(&receipt, &request)
            .unwrap_err()
            .contains("其他输入")
    );
}

#[test]
fn unrelated_repository_and_oversized_changes_are_rejected_before_stashing() {
    let (temporary, root) = fixture();
    let (_unrelated_temporary, unrelated) = fixture();
    let mut request = input(&root, "unrelated");
    request.associated_worktree_path = Some(crate::path_utils::path_to_frontend(&unrelated));
    let receipt = receipt_path(temporary.path(), &request);
    assert!(
        begin(&root, &root, request, &receipt)
            .unwrap_err()
            .contains("不属于")
    );
    fs::write(root.join("oversized.bin"), vec![0; 16 * 1024 * 1024 + 1]).unwrap();
    let request = input(&root, "oversized");
    let receipt = receipt_path(temporary.path(), &request);
    assert!(
        begin(&root, &root, request, &receipt)
            .unwrap_err()
            .contains("16 MiB")
    );
    assert!(entries(&root).unwrap().is_empty());
    assert!(root.join("oversized.bin").exists());
}
