//! 扩展贡献器注册的 Skill/PluginCommand 工具链回归测试。

use super::*;
use crate::agent_runtime::RuntimeToolContext;
use crate::plugins::{
    ComponentFile, PluginCommandCatalog, PluginId, PluginRuntimeSnapshot, RuntimePlugin,
};
use keencode_agent::{
    AgentId, AgentRunner, PlanGuard, RunLimits, SessionId, ToolRegistry, ToolRegistryError, TurnId,
    TurnRequest,
};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities, ResponseMetadata,
    ScriptedProvider, ScriptedReply, StopReason, ToolResultContent,
};
use keencode_skills::{SkillDiscoveryConfig, discover_skills};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// 创建脚本 Provider 使用的完整工具调用响应。
fn tool_reply(calls: &[(&str, &str, Value)]) -> ScriptedReply {
    let mut events = vec![ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata::default(),
    }];
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let index = u32::try_from(index).expect("测试工具调用数量应在 u32 范围内");
        events.push(ModelStreamEvent::ToolCallStart {
            index,
            id: (*id).to_owned(),
            name: (*name).to_owned(),
        });
        events.push(ModelStreamEvent::ToolCallArgumentsDelta {
            index,
            id: (*id).to_owned(),
            delta: arguments.to_string(),
        });
        events.push(ModelStreamEvent::ToolCallEnd {
            index,
            id: (*id).to_owned(),
        });
    }
    events.push(ModelStreamEvent::MessageEnd {
        stop_reason: StopReason::ToolUse,
    });
    ScriptedReply::events(events)
}

/// 创建脚本 Provider 使用的完整文本响应。
fn text_reply(text: &str) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: text.to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ])
}

/// 创建包含项目 Skill、插件 command 和无副作用标记文本的隔离扩展候选。
fn populated_contributor(
    directory: &TempDir,
) -> (PathBuf, PathBuf, PathBuf, NativeExtensionContributor) {
    let data_root = directory.path().join("data");
    let project_root = directory.path().join("project");
    let plugin_root = directory.path().join("plugin");
    let skill_root = project_root.join(".agents").join("skills").join("review");
    let command_root = plugin_root.join("commands");
    fs::create_dir_all(&data_root).expect("应创建扩展数据目录");
    fs::create_dir_all(&skill_root).expect("应创建项目 Skill 目录");
    fs::create_dir_all(&command_root).expect("应创建插件 command 目录");

    fs::write(
        skill_root.join("SKILL.md"),
        "---\nname: review-guide\ndescription: 项目审查指导\n---\n\nSkill 正文会进入下一轮模型上下文。\n",
    )
    .expect("应写入 Skill 文档");

    let marker = plugin_root.join("extension-command-marker.txt");
    let command_path = command_root.join("review.md");
    fs::write(
        &command_path,
        format!(
            "---\ndescription: 插件审查命令\n---\n请检查 $1。下列文本不得执行：\n!`echo COMMAND_SHOULD_NOT_RUN > \"{}\"`\n",
            marker.display()
        ),
    )
    .expect("应写入插件 command 模板");

    let project_root = fs::canonicalize(project_root).expect("项目根应可规范化");
    let data_root = fs::canonicalize(data_root).expect("数据根应可规范化");
    let plugin_root = fs::canonicalize(plugin_root).expect("插件根应可规范化");
    let command_path = fs::canonicalize(command_path).expect("command 文件应可规范化");
    let skill_catalog = Arc::new(
        discover_skills(&SkillDiscoveryConfig::new(
            data_root.clone(),
            project_root.clone(),
        ))
        .expect("有效 Skill 目录应完成发现"),
    );
    let snapshot = PluginRuntimeSnapshot {
        plugins: vec![RuntimePlugin {
            id: PluginId::parse("demo@official").expect("插件 ID 应有效"),
            root: plugin_root,
            commands: vec![ComponentFile {
                path: command_path,
                relative_path: PathBuf::from("commands/review.md"),
            }],
            skills: Vec::new(),
            agents: Vec::new(),
            hooks: None,
            unsupported_hooks: Vec::new(),
            mcp_servers: Default::default(),
            lsp_servers: Vec::new(),
        }],
    };
    let command_catalog = Arc::new(
        PluginCommandCatalog::from_snapshot(&snapshot).expect("有效 command 目录应冻结成功"),
    );
    let contributor = NativeExtensionContributor {
        project_root: project_root.clone(),
        skills: skill_catalog,
        deferred_tools: None,
        mcp_servers: Vec::new(),
        hooks: Vec::new(),
        agents: AgentCatalog::default(),
        commands: command_catalog,
        lsp_runtime: None,
        diagnostics: Vec::new(),
    };
    (
        project_root,
        data_root,
        directory
            .path()
            .join("plugin")
            .join("extension-command-marker.txt"),
        contributor,
    )
}

/// 创建空 Skill/PluginCommand 目录对应的贡献器。
fn empty_contributor(directory: &TempDir) -> (PathBuf, NativeExtensionContributor) {
    let data_root = directory.path().join("data");
    let project_root = directory.path().join("project");
    fs::create_dir_all(&data_root).expect("应创建空扩展数据目录");
    fs::create_dir_all(&project_root).expect("应创建空项目目录");
    let project_root = fs::canonicalize(project_root).expect("空项目根应可规范化");
    let data_root = fs::canonicalize(data_root).expect("空数据根应可规范化");
    let skills = Arc::new(
        discover_skills(&SkillDiscoveryConfig::new(data_root, project_root.clone()))
            .expect("空 Skill 目录应完成发现"),
    );
    let commands = Arc::new(
        PluginCommandCatalog::from_snapshot(&PluginRuntimeSnapshot::default())
            .expect("空 command 目录应冻结成功"),
    );
    (
        project_root.clone(),
        NativeExtensionContributor {
            project_root,
            skills,
            deferred_tools: None,
            mcp_servers: Vec::new(),
            hooks: Vec::new(),
            agents: AgentCatalog::default(),
            commands,
            lsp_runtime: None,
            diagnostics: Vec::new(),
        },
    )
}

/// 返回 Provider 请求中所有工具定义的稳定名称。
fn request_tool_names(request: &keencode_model::ModelRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

/// 从一条 Tool 消息中提取 JSON 文本，并校验调用 ID 已正确配对。
fn tool_result_json(message: &Message, call_id: &str) -> Value {
    let [ContentBlock::ToolResult { tool_result }] = message.content.as_slice() else {
        panic!("模型下一轮必须包含唯一 ToolResult 消息");
    };
    assert_eq!(tool_result.tool_call_id, call_id);
    assert!(!tool_result.is_error, "有效扩展工具结果不应被标记为错误");
    let [ToolResultContent::Text { text }] = tool_result.content.as_slice() else {
        panic!("扩展工具结果必须是唯一 JSON 文本块");
    };
    serde_json::from_str(text).expect("扩展工具结果必须是 JSON")
}

/// Skill 与 PluginCommand 必须由真实贡献器注册，并能连续进入 Runner 的后续轮次。
#[tokio::test]
async fn contributor_registered_tools_complete_skill_then_plugin_command_chain() {
    let directory = tempfile::tempdir().expect("应创建隔离扩展测试目录");
    let (project_root, _data_root, marker, contributor) = populated_contributor(&directory);
    let context = RuntimeToolContext::for_extension_test(project_root, PlanGuard::read_only());

    let mut all_tools = ToolRegistry::new();
    contributor
        .register_tools(&mut all_tools, &context)
        .expect("真实扩展贡献器应注册 Skill 与 PluginCommand");
    assert_eq!(
        all_tools
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>(),
        vec!["PluginCommand", "Skill"]
    );
    let registry = all_tools
        .select_exact(&["Skill".to_owned(), "PluginCommand".to_owned()])
        .expect("精确工具快照应接受当前两个扩展入口");

    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[("skill-call", "Skill", json!({"name": "review-guide"}))]),
            tool_reply(&[(
                "command-call",
                "PluginCommand",
                json!({
                    "name": "plugin:official:demo:review",
                    "arguments": "src/lib.rs"
                }),
            )]),
            text_reply("extension chain complete"),
        ],
    ));
    let request = TurnRequest::new(
        SessionId::new("extension-chain-session").expect("Session ID 应有效"),
        TurnId::new("extension-chain-turn").expect("Turn ID 应有效"),
        AgentId::new("extension-chain-agent").expect("Agent ID 应有效"),
        "extension-chain-model",
        vec![Message::text(
            MessageRole::User,
            "先加载审查 Skill，再展开插件 command",
        )],
        PlanGuard::read_only(),
    );
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        AgentRunner::new(provider.clone(), registry, RunLimits::default()).run_turn(request),
    )
    .await
    .expect("扩展工具链必须在有界时间内完成");

    assert!(
        result.is_success(),
        "Skill/PluginCommand 工具链应正常完成：{:?}",
        result.error
    );
    assert_eq!(result.state.round_count(), 3);
    assert_eq!(result.state.step_count(), 2);
    assert_eq!(
        result
            .final_response
            .as_ref()
            .map(|response| &response.content),
        Some(&vec![ContentBlock::text("extension chain complete")])
    );

    let requests = provider.requests().expect("应读取脚本 Provider 请求快照");
    assert_eq!(requests.len(), 3, "应恰好完成两轮工具调用和一轮最终文本");
    for request in &requests {
        assert_eq!(request_tool_names(request), ["PluginCommand", "Skill"]);
    }

    let skill_value = tool_result_json(&requests[1].messages[2], "skill-call");
    assert_eq!(skill_value["name"], "review-guide");
    assert_eq!(skill_value["description"], "项目审查指导");
    assert_eq!(skill_value["source"], "project");
    assert_eq!(
        skill_value["markdown"],
        "\nSkill 正文会进入下一轮模型上下文。\n"
    );
    let command_value = tool_result_json(&requests[2].messages[4], "command-call");
    assert_eq!(command_value["name"], "plugin:official:demo:review");
    assert_eq!(command_value["description"], "插件审查命令");
    assert!(
        command_value["markdown"]
            .as_str()
            .expect("command markdown 应为字符串")
            .contains("请检查 src/lib.rs")
    );
    assert!(
        command_value["markdown"]
            .as_str()
            .expect("command markdown 应为字符串")
            .contains("COMMAND_SHOULD_NOT_RUN")
    );
    assert!(!marker.exists(), "PluginCommand 模板正文不得直接执行 shell");
}

/// 空扩展目录不得暴露工具，也不得接受旧的 Skill/command 工具名称。
#[test]
fn empty_contributor_rejects_current_and_legacy_extension_tool_names() {
    let directory = tempfile::tempdir().expect("应创建隔离空扩展目录");
    let (project_root, contributor) = empty_contributor(&directory);
    let context = RuntimeToolContext::for_extension_test(project_root, PlanGuard::read_only());
    let mut registry = ToolRegistry::new();
    contributor
        .register_tools(&mut registry, &context)
        .expect("空扩展贡献器注册应成功");
    assert!(registry.is_empty(), "空目录不得注册 Skill 或 PluginCommand");

    for old_name in [
        "Skill",
        "PluginCommand",
        "SkillTool",
        "DiscoverSkillsTool",
        "PluginCommandTool",
    ] {
        let error = registry
            .select_exact(&[old_name.to_owned()])
            .err()
            .expect("空目录不应接受任何扩展工具名");
        assert_eq!(
            error,
            ToolRegistryError::UnknownName {
                name: old_name.to_owned()
            }
        );
    }
}
