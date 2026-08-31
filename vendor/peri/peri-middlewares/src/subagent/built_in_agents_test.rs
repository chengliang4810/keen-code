use super::*;
use crate::claude_agent_parser::{parse_agent_file, ToolsValue};
use crate::subagent::{infer_agent_capability, MUTATION_CORE_TOOL_NAMES};
use crate::tool_search::core_tools::{TOOL_AGENT, TOOL_BASH};

#[test]
fn test_all_built_in_agents_parseable() {
    for agent in list_built_in_agents() {
        let parsed = parse_agent_file(agent.content);
        assert!(
            parsed.is_some(),
            "Built-in agent '{}' failed to parse",
            agent.agent_id
        );
    }
}

#[test]
fn test_built_in_agent_ids_unique() {
    let ids: Vec<&str> = list_built_in_agents().iter().map(|a| a.agent_id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "Built-in agent IDs should be sorted");
    assert_eq!(
        ids.len(),
        {
            let mut deduped = ids.clone();
            deduped.dedup();
            deduped.len()
        },
        "Built-in agent IDs should be unique"
    );
}

#[test]
fn test_get_built_in_agent_found() {
    assert!(get_built_in_agent("code-reviewer").is_some());
    assert!(get_built_in_agent("explorer").is_some());
    assert!(get_built_in_agent("plan").is_some());
    assert!(get_built_in_agent("general-purpose").is_some());
    assert!(get_built_in_agent("verification").is_some());
    assert!(get_built_in_agent("vision").is_some());
    assert!(get_built_in_agent("web-researcher").is_some());
    assert!(get_built_in_agent("coder").is_some());
}

#[test]
fn test_code_reviewer_is_read_only() {
    let agent = get_built_in_agent("code-reviewer").unwrap();
    let parsed = parse_agent_file(agent.content).unwrap();
    assert_eq!(
        parsed.tools(),
        vec!["Read", "Glob", "Grep"],
        "Code reviewer should only receive read-only inspection tools"
    );
    assert!(parsed
        .system_prompt
        .contains("diff MUST be provided inline"));
}

#[test]
fn test_get_built_in_agent_not_found() {
    assert!(get_built_in_agent("nonexistent").is_none());
    assert!(get_built_in_agent("").is_none());
}

/// explorer、plan 与 verification 的最终工具能力合同必须同步演进。
#[test]
fn test_report_agents_follow_capability_contract() {
    /// 单个内置 Agent 的工具能力预期。
    struct CapabilityContract {
        /// 内置 Agent 标识符。
        agent_id: &'static str,
        /// 仍需保留的核心变更工具；其余核心变更工具必须显式禁用。
        allowed_mutation_tools: &'static [&'static str],
        /// 运行时能力画像是否应标记为可变更项目。
        expected_can_mutate: bool,
    }

    const SANDBOX_REPORT_DIR: &str = ".peri/plans/";
    const CONTRACTS: &[CapabilityContract] = &[
        CapabilityContract {
            agent_id: "explorer",
            allowed_mutation_tools: &[],
            expected_can_mutate: false,
        },
        CapabilityContract {
            agent_id: "plan",
            allowed_mutation_tools: &[],
            expected_can_mutate: false,
        },
        CapabilityContract {
            agent_id: "verification",
            allowed_mutation_tools: &[TOOL_BASH],
            expected_can_mutate: true,
        },
    ];

    for contract in CONTRACTS {
        let agent = get_built_in_agent(contract.agent_id)
            .unwrap_or_else(|| panic!("缺少内置 Agent {}", contract.agent_id));
        let parsed = parse_agent_file(agent.content)
            .unwrap_or_else(|| panic!("内置 Agent {} 解析失败", contract.agent_id));

        assert!(
            matches!(&parsed.frontmatter.tools, ToolsValue::Empty),
            "{} 应继续通过 disallowedTools 裁剪继承工具",
            contract.agent_id
        );

        let mut expected_disallowed = vec![TOOL_AGENT.to_ascii_lowercase()];
        expected_disallowed.extend(
            MUTATION_CORE_TOOL_NAMES
                .iter()
                .filter(|tool| {
                    !contract
                        .allowed_mutation_tools
                        .iter()
                        .any(|allowed| allowed.eq_ignore_ascii_case(tool))
                })
                .map(|tool| tool.to_ascii_lowercase()),
        );
        expected_disallowed.sort_unstable();

        let mut actual_disallowed = parsed
            .disallowed_tools()
            .into_iter()
            .map(|tool| tool.to_ascii_lowercase())
            .collect::<Vec<_>>();
        actual_disallowed.sort_unstable();
        assert_eq!(
            actual_disallowed, expected_disallowed,
            "{} 的显式工具禁用合同发生漂移",
            contract.agent_id
        );

        assert_eq!(
            parsed.frontmatter.allowed_write_dirs,
            vec![SANDBOX_REPORT_DIR.to_string()],
            "{} 只能通过唯一沙箱目录保存报告",
            contract.agent_id
        );
        assert!(
            parsed.system_prompt.contains("SandboxWrite"),
            "{} 的提示词必须说明唯一受控写入口",
            contract.agent_id
        );
        assert_eq!(
            infer_agent_capability(&parsed.frontmatter).can_mutate,
            contract.expected_can_mutate,
            "{} 的运行时能力画像与合同不一致",
            contract.agent_id
        );
    }
}

#[test]
fn test_general_purpose_has_all_tools() {
    let agent = get_built_in_agent("general-purpose").unwrap();
    let parsed = parse_agent_file(agent.content).unwrap();
    assert!(
        !parsed.tools().is_empty(),
        "General-purpose agent should have tools configured"
    );
}

#[test]
fn test_coder_agent_tools() {
    let agent = get_built_in_agent("coder").unwrap();
    let parsed = parse_agent_file(agent.content).unwrap();
    let tools = parsed.tools();
    assert_eq!(tools.len(), 7, "Coder agent should have exactly 7 tools");
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Edit")),
        "Coder agent should have Edit"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Write")),
        "Coder agent should have Write"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Grep")),
        "Coder agent should have Grep"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Read")),
        "Coder agent should have Read"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Glob")),
        "Coder agent should have Glob"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Bash")),
        "Coder agent should have Bash"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("TodoWrite")),
        "Coder agent should have TodoWrite"
    );
}

#[test]
fn test_vision_agent_has_no_tools() {
    let agent = get_built_in_agent("vision").unwrap();
    let parsed = parse_agent_file(agent.content).unwrap();
    assert!(matches!(parsed.frontmatter.tools, ToolsValue::NoTools));
    assert!(parsed.system_prompt.contains("no image is attached"));
}
