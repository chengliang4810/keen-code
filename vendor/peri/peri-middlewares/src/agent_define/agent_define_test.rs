use tempfile::tempdir;

use super::*;

/// 创建测试用文件符号链接；Windows 未启用开发者模式时由测试自行跳过。
#[cfg(unix)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 创建测试用文件符号链接；Windows 未启用开发者模式时由测试自行跳过。
#[cfg(windows)]
fn create_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

/// 创建测试用目录符号链接；Windows 未启用开发者模式时由测试自行跳过。
#[cfg(unix)]
fn create_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 创建测试用目录符号链接；Windows 未启用开发者模式时由测试自行跳过。
#[cfg(windows)]
fn create_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[test]
fn test_load_overrides_persona_only() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("code-reviewer.md"),
        "---\nname: code-reviewer\ndescription: Reviews code\n---\n\nYou are a code reviewer.\n",
    )
    .unwrap();

    let ov = AgentDefineMiddleware::load_overrides(dir.path().to_str().unwrap(), "code-reviewer")
        .unwrap();
    assert_eq!(
        ov.persona.as_deref().unwrap().trim(),
        "You are a code reviewer."
    );
    assert!(ov.tone.is_none());
    assert!(ov.proactiveness.is_none());
}

#[test]
fn test_load_overrides_with_tone_and_proactiveness() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
            agents_dir.join("analyst.md"),
            "---\nname: analyst\ndescription: Data analyst\ntone: Be thorough and detailed.\nproactiveness: Proactively explore related data.\n---\n\nYou are a data analyst.\n",
        )
        .unwrap();

    let ov =
        AgentDefineMiddleware::load_overrides(dir.path().to_str().unwrap(), "analyst").unwrap();
    assert!(ov.persona.is_some());
    assert_eq!(
        ov.tone.as_deref().unwrap().trim(),
        "Be thorough and detailed."
    );
    assert_eq!(
        ov.proactiveness.as_deref().unwrap().trim(),
        "Proactively explore related data."
    );
}

#[test]
fn test_only_reads_flat_keencode_project_definition() {
    let dir = tempdir().unwrap();
    let agent_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("security-auditor.md"),
        "---\nname: security-auditor\ndescription: Audit\n---\n\nYou are a security auditor.\n",
    )
    .unwrap();

    let ov =
        AgentDefineMiddleware::load_overrides(dir.path().to_str().unwrap(), "security-auditor")
            .unwrap();
    assert_eq!(
        ov.persona.as_deref().unwrap().trim(),
        "You are a security auditor."
    );
}

#[test]
fn test_load_overrides_no_file_returns_none() {
    let ov = AgentDefineMiddleware::load_overrides("/nonexistent", "unknown");
    assert!(ov.is_none());
}

#[test]
fn test_candidate_paths_rejects_traversal() {
    assert!(AgentDefineMiddleware::candidate_paths("/tmp", "../etc/passwd").is_empty());
    assert!(AgentDefineMiddleware::candidate_paths("/tmp", "foo/../../bar").is_empty());
    assert!(AgentDefineMiddleware::candidate_paths("/tmp", "a\\b").is_empty());
    assert!(AgentDefineMiddleware::candidate_paths("/tmp", "").is_empty());
    let paths = AgentDefineMiddleware::candidate_paths("/tmp", "my-agent");
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with(".keencode/agents/my-agent.md"));
}

#[test]
fn test_rejects_plain_markdown_name_mismatch_and_legacy_directories() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("plain.md"), "Just a plain persona.").unwrap();
    std::fs::write(
        agents_dir.join("mismatch.md"),
        "---\nname: another\ndescription: test\n---\nprompt",
    )
    .unwrap();
    let legacy_dir = dir.path().join(".claude").join("agents");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(
        legacy_dir.join("legacy.md"),
        "---\nname: legacy\ndescription: test\n---\nprompt",
    )
    .unwrap();

    assert!(AgentDefineMiddleware::load_overrides(dir.path().to_str().unwrap(), "plain").is_none());
    assert!(
        AgentDefineMiddleware::load_overrides(dir.path().to_str().unwrap(), "mismatch").is_none()
    );
    assert!(
        AgentDefineMiddleware::load_overrides(dir.path().to_str().unwrap(), "legacy").is_none()
    );
}

/// 最终定义文件是符号链接时必须占位报错，不能跟随到项目边界之外。
#[test]
fn test_project_agent_file_rejects_symlink() {
    let project = tempdir().unwrap();
    let external = tempdir().unwrap();
    let agents_dir = project.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    let target = external.path().join("explorer.md");
    std::fs::write(
        &target,
        "---\nname: explorer\ndescription: external\n---\nprompt",
    )
    .unwrap();
    if create_file_symlink(&target, &agents_dir.join("explorer.md")).is_err() {
        return;
    }

    let error =
        AgentDefineMiddleware::project_agent_file(project.path().to_str().unwrap(), "explorer")
            .unwrap_err();
    assert!(error.contains("普通文件"), "{error}");
    assert!(
        AgentDefineMiddleware::load_overrides(project.path().to_str().unwrap(), "explorer")
            .is_none()
    );
}

/// `.keencode` 或 `agents` 目录为符号链接时整个项目目录必须被忽略。
#[test]
fn test_project_agents_dir_rejects_ancestor_symlink() {
    let project = tempdir().unwrap();
    let external = tempdir().unwrap();
    let external_keencode = external.path().join(".keencode");
    std::fs::create_dir_all(external_keencode.join("agents")).unwrap();
    if create_dir_symlink(&external_keencode, &project.path().join(".keencode")).is_err() {
        return;
    }

    assert!(AgentDefineMiddleware::project_agents_dir(project.path().to_str().unwrap()).is_none());
}

/// Windows 大小写不敏感路径也必须按当前精确文件名契约读取。
#[cfg(windows)]
#[test]
fn test_project_agent_file_rejects_case_variant_file_names() {
    for file_name in ["Explorer.md", "explorer.MD"] {
        let project = tempfile::tempdir().unwrap();
        let agents_dir = project.path().join(".keencode").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join(file_name),
            "---\nname: explorer\ndescription: wrong case\n---\nprompt",
        )
        .unwrap();

        let resolved =
            AgentDefineMiddleware::project_agent_file(project.path().to_str().unwrap(), "explorer")
                .unwrap();
        assert!(
            resolved.is_none(),
            "大小写变体不应解析为 explorer: {file_name}"
        );
    }
}
