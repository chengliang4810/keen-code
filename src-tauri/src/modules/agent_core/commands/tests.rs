use super::*;
use std::fs;

fn config(name: &str, prompt: &str) -> CommandConfig {
    CommandConfig {
        name: name.into(),
        description: "Review code".into(),
        argument_hint: "<file-path>".into(),
        prompt: prompt.into(),
        enabled: true,
    }
}

#[test]
fn missing_directories_are_empty_without_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let catalog = list_at(&home, Some(&home)).unwrap();
    assert!(catalog.commands.is_empty());
    assert!(catalog.diagnostics.is_empty());
    assert!(resolve_at(&home, None, "unknown", "").unwrap().is_none());
    assert!(!home.join(".rcode").exists());
}

#[test]
fn command_files_round_trip_and_mutations_require_the_original_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let directory = Directory::user(&home);
    let mut command = config("review_file", "Review $ARGUMENTS\n");
    command.description = "Quoted \"text\"\n下一行".into();
    let saved = directory.save(&command, None).unwrap();
    assert_eq!(saved.config, command);
    assert!(directory.save(&command, None).is_err());
    let changed = config("review_file", "Changed");
    assert!(directory.save(&changed, Some("stale")).is_err());
    assert!(directory.delete("review_file", "stale").is_err());
    let updated = directory.save(&changed, Some(&saved.content)).unwrap();
    assert_eq!(updated.config.prompt, "Changed");
    fs::write(
        directory.path(Some("review_file")).unwrap(),
        "External edit",
    )
    .unwrap();
    assert!(directory.save(&command, Some(&updated.content)).is_err());
    assert!(directory.delete("review_file", &updated.content).is_err());
    directory.delete("review_file", "External edit").unwrap();
    assert!(directory.list().unwrap().commands.is_empty());
}

#[test]
fn project_commands_override_user_commands_and_disabled_commands_cannot_run() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let project = home.join("project");
    fs::create_dir(&project).unwrap();
    Directory::user(&home)
        .save(&config("review", "User $ARGUMENTS"), None)
        .unwrap();
    let scoped = Directory::project(&project);
    let saved = scoped
        .save(&config("Review", "Project $0 $1 $ARGUMENTS"), None)
        .unwrap();
    assert_eq!(
        resolve_at(&home, Some(&project), "review", "one two")
            .unwrap()
            .unwrap(),
        "Project one two one two"
    );
    assert_eq!(
        resolve_at(&home, None, "review", "file").unwrap().unwrap(),
        "User file"
    );
    let mut disabled = saved.config;
    disabled.enabled = false;
    scoped.save(&disabled, Some(&saved.content)).unwrap();
    assert!(resolve_at(&home, Some(&project), "review", "file").is_err());
    assert!(scoped.save(&config("review", "alias"), None).is_err());
    assert_eq!(list_at(&home, Some(&project)).unwrap().commands.len(), 2);
}

#[test]
fn names_sizes_invalid_roots_and_sensitive_paths_are_rejected() {
    for name in [
        "../review",
        "review.md",
        "C:review",
        "CON",
        "nul",
        "lpt9",
        "com1",
        "plan",
        "init",
        "claude-code",
        "",
    ] {
        assert!(files::validate_name(name).is_err(), "{name}");
    }
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let directory = Directory::user(&home);
    assert!(directory
        .save(&config("review", &"x".repeat(files::MAX_BYTES + 1)), None)
        .is_err());
    assert!(directory
        .save(&config("review", "null\0byte"), None)
        .is_err());
    fs::write(home.join(".rcode"), "invalid root").unwrap();
    assert!(directory.list().is_err());
    fs::remove_file(home.join(".rcode")).unwrap();
    let sensitive = home.join("credentials");
    fs::create_dir(&sensitive).unwrap();
    assert!(Directory::project(&sensitive)
        .save(&config("review", "secret"), None)
        .is_err());
}

#[test]
fn broken_files_do_not_hide_other_commands_and_cannot_be_resolved() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let directory = Directory::user(&home);
    directory.save(&config("review", "Review"), None).unwrap();
    fs::write(
        directory.path(Some("broken")).unwrap(),
        "---\ndescription: unclosed",
    )
    .unwrap();
    let catalog = directory.list().unwrap();
    assert_eq!(catalog.commands.len(), 1);
    assert_eq!(catalog.diagnostics.len(), 1);
    assert!(resolve_at(&home, None, "broken", "").is_err());
}

#[test]
fn mutations_count_invalid_files_without_loading_unrelated_content() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let directory = Directory::user(&home);
    let original = directory.save(&config("review", "Review"), None).unwrap();
    let root = directory.path(None).unwrap();
    for index in 1..files::MAX_COMMANDS {
        fs::write(root.join(format!("invalid-{index}.md")), "---\ninvalid").unwrap();
    }
    assert!(directory.save(&config("extra", "Extra"), None).is_err());
    let updated = directory
        .save(&config("review", "Updated"), Some(&original.content))
        .unwrap();
    assert_eq!(updated.config.prompt, "Updated");
    assert_eq!(
        fs::read_to_string(root.join("invalid-1.md")).unwrap(),
        "---\ninvalid"
    );
}

#[cfg(unix)]
#[test]
fn redirected_roots_and_files_are_rejected() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), home.join(".rcode")).unwrap();
    assert!(Directory::user(&home).list().is_err());
    fs::remove_file(home.join(".rcode")).unwrap();
    fs::create_dir_all(home.join(".rcode/commands")).unwrap();
    fs::write(outside.path().join("review.md"), "Secret").unwrap();
    symlink(
        outside.path().join("review.md"),
        home.join(".rcode/commands/review.md"),
    )
    .unwrap();
    assert!(Directory::user(&home).read("review").is_err());
}

#[cfg(windows)]
#[test]
fn windows_junctions_are_rejected_for_read_write_and_delete() {
    use std::os::windows::process::CommandExt;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(home.join(".rcode"))
        .arg(outside.path())
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(status.status.success());
    let directory = Directory::user(&home);
    assert!(directory.list().is_err());
    assert!(directory.read("review").is_err());
    assert!(directory.save(&config("review", "Review"), None).is_err());
    assert!(directory.delete("review", "Review").is_err());
    fs::remove_dir(home.join(".rcode")).unwrap();
}
