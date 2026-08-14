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
model: provider-a::model-x
---

You are a code reviewer.
"#;

    let agent = parse_project_agent("code-reviewer", content).unwrap();

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
    assert_eq!(
        agent.frontmatter.model.as_deref(),
        Some("provider-a::model-x")
    );
    assert_eq!(agent.system_prompt, "You are a code reviewer.");
}

#[test]
fn preserves_missing_vs_empty_tools() {
    let inherited =
        parse_agent_file("---\nname: inherited\ndescription: inherit tools\n---\nprompt").unwrap();
    let empty = parse_agent_file("---\nname: empty\ndescription: no tools\ntools: []\n---\nprompt")
        .unwrap();

    assert_eq!(inherited.frontmatter.tools, ToolsValue::Inherit);
    assert_eq!(empty.frontmatter.tools, ToolsValue::NoTools);
}

#[test]
fn converts_to_subagent_contract_without_widening_features() {
    let agent = parse_agent_file(
        "---\nname: test\ndescription: test\ntools: []\nmodel: provider::model\n---\nprompt",
    )
    .unwrap()
    .into_claude_agent();

    assert_eq!(agent.frontmatter.tools, ClaudeToolsValue::NoTools);
    assert_eq!(agent.frontmatter.model.as_deref(), Some("provider::model"));
    assert!(agent.frontmatter.permission_mode.is_none());
    assert!(agent.frontmatter.mcp_servers.is_empty());
    assert!(!agent.frontmatter.background);
}

#[test]
fn rejects_non_array_or_invalid_tool_values() {
    for tools in ["Read", "null", "{}", "[Read, 42]", "[Read, '']"] {
        let content = format!("---\nname: test\ndescription: test\ntools: {tools}\n---\nprompt");
        assert!(parse_agent_file(&content).is_err(), "{tools}");
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
fn rejects_invalid_names_empty_description_and_filename_mismatch() {
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

    let error = parse_project_agent(
        "file-name",
        "---\nname: different-name\ndescription: test\n---\nprompt",
    )
    .unwrap_err();
    assert!(error.contains("不一致"));
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

/// 沙箱目录只能位于项目根下，不能借助空路径、绝对路径或父目录扩大写范围。
#[test]
fn rejects_unsafe_allowed_write_directories() {
    for directories in [
        "['']",
        "[.]",
        "[..]",
        "[../outside]",
        "[/outside]",
        "['C:\\outside']",
        "[plans/../outside]",
        "[plans//nested]",
    ] {
        let content = format!(
            "---\nname: sandbox\ndescription: test\nallowedWriteDirs: {directories}\n---\nprompt"
        );
        assert!(parse_agent_file(&content).is_err(), "{directories}");
    }
}

/// 模型标识不得包含换行等控制字符，避免污染日志和运行时选择参数。
#[test]
fn rejects_invalid_or_control_character_model_selection() {
    for model in ["'::model'", "'provider::'", "'provider::   '"] {
        let content = format!("---\nname: test\ndescription: test\nmodel: {model}\n---\nprompt");
        assert!(parse_agent_file(&content).is_err(), "{model}");
    }

    let multiline =
        "---\nname: test\ndescription: test\nmodel: |\n  sonnet\n  injected\n---\nprompt";
    assert!(parse_agent_file(multiline).is_err());
}

/// 项目 Agent 兼容上游四档/inherit，并统一归一化档位大小写。
#[test]
fn preserves_upstream_model_tiers() {
    for (raw, expected) in [
        ("InHerit", None),
        ("HAIKU", Some("haiku")),
        ("Sonnet", Some("sonnet")),
        ("OPUS", Some("opus")),
        ("Fable", Some("fable")),
    ] {
        let content = format!("---\nname: test\ndescription: test\nmodel: {raw}\n---\nprompt");
        let definition = parse_agent_file(&content).unwrap();
        assert_eq!(definition.frontmatter.model.as_deref(), expected, "{raw}");
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
