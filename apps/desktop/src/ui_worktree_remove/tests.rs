use super::*;

fn fixture(branch: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repo");
    fs::create_dir(&root).unwrap();
    let root = fs::canonicalize(root).unwrap();
    git(&root, &["init", "--initial-branch=main"]).unwrap();
    git(&root, &["config", "core.autocrlf", "false"]).unwrap();
    git(&root, &["config", "user.name", "Fixture"]).unwrap();
    git(&root, &["config", "user.email", "fixture@example.invalid"]).unwrap();
    fs::write(root.join("file.txt"), b"base\n").unwrap();
    fs::write(root.join(".gitignore"), b"ignored.txt\n").unwrap();
    git(&root, &["add", "--all"]).unwrap();
    git(
        &root,
        &[
            "commit",
            "-m",
            "测试：移除工作树基线 / test: worktree removal baseline",
        ],
    )
    .unwrap();
    let target = temporary.path().join("checkout");
    let text = crate::path_utils::path_to_frontend(&target);
    match branch {
        Some(branch) => {
            git(
                &root,
                &["worktree", "add", "-b", branch, "--", &text, "HEAD"],
            )
            .unwrap();
        }
        None => {
            git(&root, &["worktree", "add", "--detach", "--", &text, "HEAD"]).unwrap();
        }
    }
    let target = fs::canonicalize(target).unwrap();
    (temporary, root, target)
}

#[test]
fn default_removal_preserves_dirty_and_ignored_files() {
    let (_temporary, root, target) = fixture(Some("feat/user-name"));
    fs::write(target.join("ignored.txt"), b"private ignored content").unwrap();
    assert!(
        remove_with_ref(&root, &target, false)
            .unwrap_err()
            .contains("忽略文件")
    );
    assert_eq!(
        fs::read(target.join("ignored.txt")).unwrap(),
        b"private ignored content"
    );
    fs::remove_file(target.join("ignored.txt")).unwrap();
    fs::write(target.join("file.txt"), b"changed\n").unwrap();
    fs::write(target.join("未跟踪[1].txt"), b"untracked\n").unwrap();
    assert!(remove_with_ref(&root, &target, false).is_err());
    assert_eq!(fs::read(target.join("file.txt")).unwrap(), b"changed\n");
    assert!(target.join("未跟踪[1].txt").exists());
}

#[test]
fn force_removes_confirmed_dirty_checkout_but_keeps_user_branch() {
    let (_temporary, root, target) = fixture(Some("feat/user-feature"));
    fs::write(target.join("ignored.txt"), b"ignored\n").unwrap();
    fs::write(target.join("file.txt"), b"changed\n").unwrap();
    let candidate = validate_target(&root, &crate::path_utils::path_to_frontend(&target)).unwrap();
    remove_with_ref(&root, &candidate, true).unwrap();
    assert!(!target.exists());
    assert!(
        git(
            &root,
            &["show-ref", "--verify", "refs/heads/feat/user-feature"]
        )
        .is_ok()
    );
    assert!(root.join("file.txt").exists());
}

#[test]
fn ordinary_removal_preserves_user_branch() {
    let (_temporary, root, target) = fixture(Some("feat/user-feature"));
    remove_with_ref(&root, &target, false).unwrap();
    assert!(!target.exists());
    assert!(
        git(
            &root,
            &["show-ref", "--verify", "refs/heads/feat/user-feature"]
        )
        .is_ok()
    );
}

#[test]
fn legacy_reclaim_request_is_rejected_before_touching_checkout() {
    let (_temporary, root, target) = fixture(Some("feat/legacy-feature"));
    let error = reject_unsupported_temporary_branch_reclaim(true).unwrap_err();
    assert!(error.contains("不支持自动回收"));
    assert!(target.exists());
    assert!(
        git(
            &root,
            &["show-ref", "--verify", "refs/heads/feat/legacy-feature"]
        )
        .is_ok()
    );
}

#[test]
fn detached_checkout_can_be_removed_without_reclaiming_refs() {
    let (_temporary, root, target) = fixture(None);
    remove_with_ref(&root, &target, false).unwrap();
    assert!(!target.exists());
    assert!(git(&root, &["show-ref", "--verify", "refs/heads/main"]).is_ok());
}

#[test]
fn main_unrelated_and_relative_paths_are_rejected() {
    let (temporary, root, target) = fixture(Some("feat/user-name"));
    assert!(validate_target(&root, &crate::path_utils::path_to_frontend(&root)).is_err());
    assert!(validate_target(&target, &crate::path_utils::path_to_frontend(&root)).is_err());
    assert!(validate_target(&root, "checkout").is_err());
    assert!(
        validate_target(
            &root,
            &crate::path_utils::path_to_frontend(temporary.path())
        )
        .is_err()
    );
    let (_unrelated, unrelated, _) = fixture(None);
    assert!(validate_target(&unrelated, &crate::path_utils::path_to_frontend(&target)).is_err());
    assert!(target.exists() && root.exists());
}

#[test]
fn locked_checkout_is_preserved_even_when_force_is_requested() {
    let (_temporary, root, target) = fixture(Some("feat/locked-feature"));
    let text = crate::path_utils::path_to_frontend(&target);
    git(
        &root,
        &["worktree", "lock", "--reason", "fixture owner", "--", &text],
    )
    .unwrap();
    assert!(remove_with_ref(&root, &target, true).is_err());
    assert!(target.exists());
    assert!(
        git(
            &root,
            &["show-ref", "--verify", "refs/heads/feat/locked-feature"]
        )
        .is_ok()
    );
    git(&root, &["worktree", "unlock", "--", &text]).unwrap();
}

#[test]
fn branch_repointed_by_external_git_is_preserved() {
    let (_temporary, root, target) = fixture(Some("feat/repoint-feature"));
    remove_with_ref(&root, &target, false).unwrap();
    fs::write(root.join("file.txt"), b"new commit\n").unwrap();
    git(&root, &["add", "--all"]).unwrap();
    git(
        &root,
        &["commit", "-m", "测试：外部提交 / test: external commit"],
    )
    .unwrap();
    let new = git(&root, &["rev-parse", "HEAD"]).unwrap();
    git(
        &root,
        &["update-ref", "refs/heads/feat/repoint-feature", new.trim()],
    )
    .unwrap();
    assert_eq!(
        git(&root, &["rev-parse", "refs/heads/feat/repoint-feature"]).unwrap(),
        new
    );
}

#[test]
fn another_checkout_using_user_branch_is_preserved() {
    let (temporary, root, target) = fixture(Some("feat/shared-feature"));
    let previous = git(&target, &["rev-parse", "HEAD"]).unwrap();
    let other = temporary.path().join("other");
    git(
        &root,
        &[
            "worktree",
            "add",
            "--force",
            "--",
            &crate::path_utils::path_to_frontend(&other),
            "feat/shared-feature",
        ],
    )
    .unwrap();
    remove_with_ref(&root, &target, false).unwrap();
    assert!(other.join("file.txt").exists());
    assert_eq!(
        git(&root, &["rev-parse", "refs/heads/feat/shared-feature"]).unwrap(),
        previous
    );
}

#[test]
fn nested_checkout_references_are_detected_without_matching_siblings() {
    let (temporary, _root, target) = fixture(None);
    fs::create_dir(target.join("nested")).unwrap();
    assert!(path_references_checkout(
        &crate::path_utils::path_to_frontend(&target),
        &target
    ));
    assert!(path_references_checkout(
        &crate::path_utils::path_to_frontend(&target.join("nested")),
        &target
    ));
    assert!(
        path_references_checkout(
            &crate::path_utils::path_to_frontend(&target.join("nested/missing")),
            &target
        ),
        "已消失的 Session 子目录仍应保护父工作树"
    );
    assert!(!path_references_checkout(
        &crate::path_utils::path_to_frontend(temporary.path()),
        &target
    ));
}

fn metadata(
    session_id: &str,
    project_root: &Path,
    status: keencode_resources::SessionStatus,
    corrupt: bool,
) -> keencode_resources::StoredSessionMetadata {
    keencode_resources::StoredSessionMetadata {
        session_id: keencode_resources::SessionId::new(session_id).unwrap(),
        title: session_id.to_owned(),
        project_root: project_root.to_string_lossy().into_owned(),
        pinned: false,
        archived: false,
        title_source: keencode_resources::TitleSource::Unspecified,
        status,
        created_at_unix_ms: 0,
        updated_at_unix_ms: 0,
        last_user_message_at_unix_ms: 0,
        last_sequence: 0,
        corrupt,
    }
}

#[test]
fn deleted_session_tombstone_is_ignored_for_worktree_reference_scan() {
    let (_temporary, _root, target) = fixture(None);
    let deleted = keencode_resources::SessionId::new("deleted-session").unwrap();
    let storage = tempfile::tempdir().unwrap();
    let target_text = target.to_string_lossy();
    keencode_resources::record_deleted_session(storage.path(), &deleted, &target_text).unwrap();
    let deleted_ids =
        keencode_resources::list_deleted_session_ids(storage.path(), &target_text).unwrap();
    let sessions = vec![metadata(
        deleted.as_str(),
        &target,
        keencode_resources::SessionStatus::Closed,
        false,
    )];
    assert!(filter_session_reference_candidates(sessions, &deleted_ids, &[], &[]).is_empty());
}

#[test]
fn undeleted_session_still_blocks_worktree_removal() {
    let (_temporary, _root, target) = fixture(None);
    let live = keencode_resources::SessionId::new("live-session").unwrap();
    let sessions = filter_session_reference_candidates(
        vec![metadata(
            live.as_str(),
            &target,
            keencode_resources::SessionStatus::Closed,
            false,
        )],
        &[],
        &[],
        &[],
    );
    assert_eq!(sessions.len(), 1);
    assert!(path_references_checkout(&sessions[0].project_root, &target));
}

#[test]
fn associated_worktree_path_still_blocks_after_session_filter() {
    let (_temporary, _root, target) = fixture(None);
    let associated = crate::path_utils::path_to_frontend(&target);
    assert!(any_associated_path_references_checkout(
        [associated.as_str()],
        &target
    ));
    let unrelated = target.parent().unwrap().join("other");
    let unrelated = crate::path_utils::path_to_frontend(&unrelated);
    assert!(!any_associated_path_references_checkout(
        [unrelated.as_str()],
        &target
    ));
}

#[test]
fn tombstoned_running_active_or_closing_session_still_blocks_worktree_removal() {
    let (_temporary, _root, target) = fixture(None);
    let running = keencode_resources::SessionId::new("running-session").unwrap();
    let active = keencode_resources::SessionId::new("active-session").unwrap();
    let closing = keencode_resources::SessionId::new("closing-session").unwrap();
    let sessions = vec![
        metadata(
            running.as_str(),
            &target,
            keencode_resources::SessionStatus::Running,
            false,
        ),
        metadata(
            active.as_str(),
            &target,
            keencode_resources::SessionStatus::Closed,
            false,
        ),
        metadata(
            closing.as_str(),
            &target,
            keencode_resources::SessionStatus::Closed,
            false,
        ),
    ];
    let retained = filter_session_reference_candidates(
        sessions,
        &[running, active.clone(), closing],
        &[active.as_str().to_owned()],
        &["closing-session".to_owned()],
    );
    assert_eq!(retained.len(), 3);
}
