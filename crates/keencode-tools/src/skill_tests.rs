//! Skill 工具按需加载和安全边界的确定性测试。

use std::fs;
use std::sync::Arc;

use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolConcurrency, ToolContext, ToolEffect,
    TurnCancellation, TurnId,
};
use keencode_model::ToolResultContent;
use keencode_skills::{SkillDiscoveryConfig, discover_skills};
use serde_json::{Value, json};

use crate::SkillTool;

/// 创建独立 Skill 工具执行上下文。
fn context(cancellation: TurnCancellation) -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-skill").unwrap(),
        turn_id: TurnId::new("turn-skill").unwrap(),
        source_agent_id: AgentId::new("agent-skill").unwrap(),
        tool_call_id: ToolCallId::new("call-skill").unwrap(),
        cancellation,
    }
}

/// 提取 Skill 工具的唯一文本结果并解析 JSON。
fn output_json(output: keencode_agent::ToolOutput) -> Value {
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("Skill 必须返回唯一文本结果");
    };
    serde_json::from_str(text).expect("Skill 输出必须是 JSON")
}

/// 在临时项目中创建一个可被发现的 Skill 目录。
fn test_catalog() -> (tempfile::TempDir, Arc<keencode_skills::SkillCatalog>) {
    let root = tempfile::tempdir().expect("应创建临时目录");
    let data = root.path().join("data");
    let project = root.path().join("project");
    let skill_directory = project.join(".agents").join("skills").join("review");
    fs::create_dir_all(&data).expect("应创建数据目录");
    fs::create_dir_all(&skill_directory).expect("应创建项目 Skill 目录");
    fs::write(
        skill_directory.join("SKILL.md"),
        "---\nname: code-review\ndescription: 审查代码边界\n---\n\n先检查测试，再报告风险。\n",
    )
    .expect("应写入 Skill 文档");
    let catalog =
        discover_skills(&SkillDiscoveryConfig::new(data, project)).expect("有效目录应完成发现");
    (root, Arc::new(catalog))
}

/// 工具必须按名称懒加载正文并保留规范来源。
#[tokio::test]
async fn skill_loads_enabled_document_on_demand() {
    let (_root, catalog) = test_catalog();
    let tool = SkillTool::new(catalog);
    let definition = tool.definition();
    assert_eq!(definition.name, "Skill");
    assert_eq!(tool.effect(&json!({})).unwrap(), ToolEffect::ReadOnly);
    assert_eq!(tool.concurrency(), ToolConcurrency::ParallelReadOnly);

    let output = tool
        .execute(
            context(TurnCancellation::new()),
            json!({"name": "CODE-REVIEW"}),
        )
        .await
        .expect("ASCII 大小写不敏感查找应成功");
    let value = output_json(output);
    assert_eq!(value["name"], "code-review");
    assert_eq!(value["description"], "审查代码边界");
    assert_eq!(value["source"], "project");
    assert_eq!(value["markdown"], "\n先检查测试，再报告风险。\n");
}

/// 路径式名称、未知名称和额外输入字段都不能进入文件系统加载。
#[tokio::test]
async fn skill_rejects_invalid_and_unknown_names() {
    let (_root, catalog) = test_catalog();
    let tool = SkillTool::new(catalog);
    for input in [
        json!({"name": "../secret"}),
        json!({"name": "missing"}),
        json!({"name": "code-review", "path": "elsewhere"}),
    ] {
        let error = tool
            .execute(context(TurnCancellation::new()), input)
            .await
            .unwrap_err();
        assert!(matches!(
            error.code.as_str(),
            "invalid_input" | "skill_not_found"
        ));
    }
}

/// 已取消 Turn 不得启动 Skill 读取工作。
#[tokio::test]
async fn skill_observes_pre_cancelled_turn() {
    let (_root, catalog) = test_catalog();
    let tool = SkillTool::new(catalog);
    let cancellation = TurnCancellation::new();
    cancellation.cancel();
    let error = tool
        .execute(context(cancellation), json!({"name": "code-review"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "skill_cancelled");
}
