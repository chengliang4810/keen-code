use tempfile::tempdir;

use super::*;

#[test]
fn test_load_overrides_persona_only() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".claude").join("agents");
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
    let agents_dir = dir.path().join(".claude").join("agents");
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
fn test_load_overrides_nested_dir() {
    let dir = tempdir().unwrap();
    let agent_dir = dir
        .path()
        .join(".claude")
        .join("agents")
        .join("security-auditor");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("agent.md"),
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
    // 正常 agent_id 应产生 4 条候选路径
    assert_eq!(
        AgentDefineMiddleware::candidate_paths("/tmp", "my-agent").len(),
        4
    );
}

#[test]
fn test_load_overrides_plain_markdown() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(agents_dir.join("plain.md"), "Just a plain persona.").unwrap();

    let ov = AgentDefineMiddleware::load_overrides(dir.path().to_str().unwrap(), "plain").unwrap();
    assert_eq!(ov.persona.as_deref().unwrap(), "Just a plain persona.");
    assert!(ov.tone.is_none());
}
