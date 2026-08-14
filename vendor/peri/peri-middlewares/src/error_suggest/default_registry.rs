use crate::error_suggest::context::ToolRegistrySnapshot;
use crate::error_suggest::registry::{ErrorSuggestRegistry, ErrorSuggester};
use crate::error_suggest::suggesters::{
    bash_command_suggester::BashCommandSuggester, glob_pattern_suggester::GlobPatternSuggester,
    json_schema_suggester::JsonSchemaSuggester, path_suggester::PathSuggester,
    range_suggester::RangeSuggester, regex_suggester::RegexSuggester,
    subagent_suggester::SubagentSuggester,
};
use std::sync::Arc;

/// 构造默认 registry，按短路顺序注册
/// 顺序：参数语法类（廉价）-> 范围 -> 路径 -> 命令 -> subagent（需 registry 查询）
pub fn build_default_registry() -> Arc<ErrorSuggestRegistry> {
    let suggesters: Vec<Box<dyn ErrorSuggester>> = vec![
        Box::new(JsonSchemaSuggester),  // B5 最先：参数级错误最廉价
        Box::new(GlobPatternSuggester), // B3
        Box::new(RegexSuggester),       // B4
        Box::new(RangeSuggester),       // B2
        Box::new(PathSuggester),        // A1-A4（需 IO）
        Box::new(BashCommandSuggester), // C1（需 PATH 扫描）
        Box::new(SubagentSuggester),    // C3（registry 查询）
    ];
    Arc::new(ErrorSuggestRegistry::new(suggesters))
}

/// 从 collect_tools 结果与严格项目 Agent 目录构建 snapshot。
pub fn build_tool_registry_snapshot(
    tool_names: impl IntoIterator<Item = String>,
    cwd: Option<&str>,
) -> ToolRegistrySnapshot {
    let mut all_tool_names: std::collections::HashSet<String> = tool_names.into_iter().collect();

    let mut subagent_types: std::collections::HashSet<String> =
        crate::subagent::built_in_agent_types()
            .iter()
            .map(|s| s.to_string())
            .collect();

    // 项目文件只要占用合法 ID 就先移除同名内置定义；严格解析成功后再加入，
    // 与实际调用的 fail-closed 优先级保持一致。
    if let Some(cwd) = cwd {
        for (agent_id, valid) in crate::subagent::project_agent_statuses(cwd) {
            subagent_types.remove(&agent_id);
            if valid {
                subagent_types.insert(agent_id);
            }
        }
    }

    // subagent_type 也是有效"工具名"候补
    for t in &subagent_types {
        all_tool_names.insert(t.clone());
    }

    ToolRegistrySnapshot {
        all_tool_names,
        subagent_types,
    }
}

#[cfg(test)]
#[path = "default_registry_test.rs"]
mod tests;
