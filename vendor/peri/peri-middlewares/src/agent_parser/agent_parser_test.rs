use super::*;

#[test]
fn parses_current_agent_contract() {
    let content = r#"---
name: code-reviewer
description: Reviews code for quality
tools: [Read, Grep, Glob]
disallowedTools: [Write, Edit]
maxTurns: 10
skills: [goal]
promptMode: full
allowedWriteDirs: [.keencode/plans]
---

You are a code reviewer.
"#;

    let agent = parse_agent_file(content).unwrap();

    assert_eq!(agent.frontmatter.name, "code-reviewer");
    assert_eq!(agent.frontmatter.description, "Reviews code for quality");
    assert_eq!(agent.tools(), vec!["Read", "Grep", "Glob"]);
    assert_eq!(agent.disallowed_tools(), vec!["Write", "Edit"]);
    assert_eq!(agent.frontmatter.max_turns, Some(10));
    assert_eq!(agent.frontmatter.skills, vec!["goal"]);
    assert_eq!(agent.frontmatter.prompt_mode.as_deref(), Some("full"));
    assert_eq!(
        agent.frontmatter.allowed_write_dirs,
        vec![".keencode/plans"]
    );
    assert_eq!(agent.system_prompt, "You are a code reviewer.");
}

#[test]
fn parses_minimal_agent_and_preserves_missing_vs_empty_tools() {
    let inherited =
        parse_agent_file("---\nname: inherited\ndescription: inherit tools\n---\nprompt").unwrap();
    let empty = parse_agent_file("---\nname: empty\ndescription: no tools\ntools: []\n---\nprompt")
        .unwrap();

    assert_eq!(inherited.frontmatter.tools, ToolsValue::Inherit);
    assert_eq!(empty.frontmatter.tools, ToolsValue::NoTools);
}

#[test]
fn rejects_non_array_or_invalid_tool_values() {
    for tools in ["Read", "null", "{}", "[Read, 42]", "[Read, '']"] {
        let content = format!("---\nname: test\ndescription: test\ntools: {tools}\n---\nprompt");
        assert!(parse_agent_file(&content).is_err(), "{tools}");
    }
}

#[test]
fn parses_model_override_field() {
    // A8 起 model 字段合法：`"{provider_id}::{model}"` 覆盖编码
    for model in ["sonnet", "provider-a::opus-4", ""] {
        let content = format!("---\nname: test\ndescription: test\nmodel: {model}\n---\nprompt");
        let agent = parse_agent_file(&content).unwrap();
        assert_eq!(
            agent.frontmatter.model.as_deref(),
            if model.is_empty() { None } else { Some(model) },
            "model 覆盖 {model:?} 应被解析"
        );
    }
}

#[test]
fn rejects_unknown_fields_and_invalid_prompt_mode() {
    for extra in ["background: true", "promptMode: old", "maxTurns: 0"] {
        let content = format!("---\nname: test\ndescription: test\n{extra}\n---\nprompt");
        assert!(parse_agent_file(&content).is_err(), "{extra}");
    }
}

#[test]
fn rejects_invalid_names_and_empty_description() {
    for (name, description) in [
        ("", "test"),
        ("UpperCase", "test"),
        ("-leading", "test"),
        ("trailing-", "test"),
        ("two_words", "test"),
        ("valid", "   "),
    ] {
        let content = format!("---\nname: {name}\ndescription: '{description}'\n---\nprompt");
        assert!(parse_agent_file(&content).is_err(), "{name:?}");
    }
}

#[test]
fn rejects_repeated_list_values() {
    for repeated in [
        "tools: [Read, Read]",
        "disallowedTools: [Write, Write]",
        "skills: [goal, goal]",
        "allowedWriteDirs: [.keencode/plans, .keencode/plans]",
    ] {
        let content = format!("---\nname: repeated\ndescription: test\n{repeated}\n---\nprompt");
        assert!(parse_agent_file(&content).is_err(), "{repeated}");
    }
}

#[test]
fn frontmatter_delimiters_are_exact() {
    for content in [
        "plain markdown",
        "\n---\nname: test\ndescription: test\n---\nprompt",
        " ---\nname: test\ndescription: test\n---\nprompt",
        "---\nname: test\ndescription: test\n ---\nprompt",
        "---\nname: test\ndescription: test",
    ] {
        assert!(parse_agent_file(content).is_err(), "{content:?}");
    }
}

#[test]
fn supports_crlf_and_inline_dashes_in_yaml_values() {
    let content = "---\r\nname: test-agent\r\ndescription: Use --- as text\r\n---\r\nprompt";
    let agent = parse_agent_file(content).unwrap();

    assert_eq!(agent.frontmatter.description, "Use --- as text");
    assert_eq!(agent.system_prompt, "prompt");
}

#[test]
fn malformed_yaml_returns_specific_error() {
    let error = parse_agent_file("---\ninvalid: [yaml: broken\n---\nprompt").unwrap_err();

    assert!(error.contains("YAML frontmatter 解析失败"));
}

#[test]
fn format_agent_ids_for_display() {
    assert_eq!(format_agent_id("code-reviewer"), "Code Reviewer");
    assert_eq!(format_agent_id("security_auditor"), "Security Auditor");
    assert_eq!(format_agent_id("researcher"), "Researcher");
    assert_eq!(format_agent_id(""), "");
}
