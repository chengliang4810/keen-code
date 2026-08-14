use std::path::Path;

use tempfile::tempdir;

use super::*;

/// 创建测试用文件符号链接；Windows 权限不足时调用方跳过该断言。
#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 创建测试用文件符号链接；Windows 权限不足时调用方跳过该断言。
#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// 错误建议目录必须与严格项目解析和无效 ID 占位语义保持一致。
#[test]
fn snapshot_only_includes_valid_flat_project_agents() {
    let project = tempdir().unwrap();
    let external = tempdir().unwrap();
    let agents_dir = project.path().join(".keencode").join("agents");
    std::fs::create_dir_all(agents_dir.join("nested")).unwrap();
    std::fs::write(
        agents_dir.join("custom.md"),
        "---\nname: custom\ndescription: valid\n---\nprompt",
    )
    .unwrap();
    std::fs::write(
        agents_dir.join("explorer.md"),
        "---\nname: different\ndescription: invalid override\n---\nprompt",
    )
    .unwrap();
    std::fs::write(
        agents_dir.join("mismatch.md"),
        "---\nname: different\ndescription: mismatch\n---\nprompt",
    )
    .unwrap();
    std::fs::write(
        agents_dir.join("nested").join("agent.md"),
        "---\nname: nested\ndescription: nested\n---\nprompt",
    )
    .unwrap();

    let target = external.path().join("linked.md");
    std::fs::write(
        &target,
        "---\nname: linked\ndescription: linked\n---\nprompt",
    )
    .unwrap();
    let linked = agents_dir.join("linked.md");
    let symlink_created = create_file_symlink(&target, &linked).is_ok();

    let snapshot = build_tool_registry_snapshot(["Read".to_string()], project.path().to_str());

    assert!(snapshot.subagent_types.contains("custom"));
    assert!(!snapshot.subagent_types.contains("explorer"));
    assert!(!snapshot.subagent_types.contains("mismatch"));
    assert!(!snapshot.subagent_types.contains("nested"));
    if symlink_created {
        assert!(!snapshot.subagent_types.contains("linked"));
    }
    assert!(snapshot.all_tool_names.contains("custom"));
    assert!(!snapshot.all_tool_names.contains("explorer"));
}
