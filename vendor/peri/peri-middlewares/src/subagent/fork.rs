//! Fork semantics: tool filtering, fork directive construction, agent override extraction.
//!
//! Pure computation functions for sub-agent inheritance from parent agent.
//! No async, no external state mutation — safe for unit testing without mocks.

use std::sync::Arc;

use peri_agent::tools::BaseTool;

use crate::tool_search::core_tools::TOOL_AGENT;
use crate::{agent_define::AgentOverrides, claude_agent_parser::ToolsValue, tools::ArcToolWrapper};

/// Filter tools from parent set based on agent definition's tools/disallowedTools fields.
///
/// Rules:
/// - `tools` is omitted -> inherit all parent tools (but always exclude `Agent` itself to prevent recursion)
/// - `tools: []` -> inherit no parent tools
/// - `tools` has value -> only keep tools in the list (also exclude `Agent`)
/// - then remove tools listed in `disallowed_tools` from the result
///
/// Matching is case-insensitive (users often write PascalCase in agent.md).
/// A trailing `*` is treated as a prefix wildcard so dynamic tool namespaces
/// such as `mcp__*` can be restricted without enumerating server tools.
///
/// 行为约束：先按 `tools` 白名单保留，再按 `disallowedTools` 删除；`Agent`
/// 始终排除以阻止递归。两侧都支持大小写不敏感的精确匹配与尾部 `*` 前缀
/// 通配，因而 `mcp__*` 会覆盖所有动态 MCP 工具，但不会匹配
/// `mcp_read_resource`。
pub fn filter_tools(
    parent_tools: &[Arc<dyn BaseTool>],
    allowed: &ToolsValue,
    disallowed: &ToolsValue,
) -> Vec<Box<dyn BaseTool>> {
    let disallowed_list = disallowed.to_vec();

    parent_tools
        .iter()
        .filter(|tool| {
            let name = tool.name();
            if name.eq_ignore_ascii_case(TOOL_AGENT) {
                return false;
            }
            let is_allowed = match allowed {
                ToolsValue::Empty => true,
                ToolsValue::NoTools => false,
                ToolsValue::List(allowed_list) => allowed_list
                    .iter()
                    .any(|pattern| tool_name_matches(pattern, name)),
            };
            if !is_allowed {
                return false;
            }
            if disallowed_list
                .iter()
                .any(|pattern| tool_name_matches(pattern, name))
            {
                return false;
            }
            true
        })
        .map(|tool| Box::new(ArcToolWrapper(Arc::clone(tool))) as Box<dyn BaseTool>)
        .collect()
}

/// 按工具声明匹配工具名；精确匹配大小写不敏感，末尾 `*` 匹配任意前缀扩展。
///
/// 仅支持尾部通配符，避免把 Agent 定义中的普通字符意外解释为复杂 glob；
/// `*` 本身仍表示全部工具，`mcp__*` 则覆盖所有动态 MCP 工具而不覆盖
/// `mcp_read_resource` 这类独立的资源工具。
pub(crate) fn tool_name_matches(pattern: &str, tool_name: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let tool_name = tool_name.to_lowercase();
    pattern
        .strip_suffix('*')
        .map(|prefix| tool_name.starts_with(prefix))
        .unwrap_or_else(|| tool_name == pattern)
}

/// Whether an agent declaration permits tools injected outside parent-tool inheritance.
/// Explicit `tools: []` is a strict zero-tool boundary.
pub(crate) fn allows_injected_tools(allowed: &ToolsValue) -> bool {
    !matches!(allowed, ToolsValue::NoTools)
}

/// Extract [`AgentOverrides`] from already-parsed agent definition fields.
///
/// Returns `None` when all fields are empty (no overrides needed).
///
/// `mode: "full"` 在下游 `PromptTemplate::with_overrides` 中只替换
/// PersonaDomain 层；不可替换层（安全/工程/能力/运行时边界）始终渲染。
pub fn overrides_from_agent_def(
    system_prompt: &str,
    tone: &Option<String>,
    proactiveness: &Option<String>,
    mode: &Option<String>,
) -> Option<AgentOverrides> {
    let persona = if system_prompt.is_empty() {
        None
    } else {
        Some(system_prompt.to_string())
    };
    let overrides = AgentOverrides {
        persona,
        tone: tone.clone(),
        proactiveness: proactiveness.clone(),
        mode: mode.clone(),
    };
    if overrides.is_empty() {
        None
    } else {
        Some(overrides)
    }
}

// ─── fork / prediction 指令模板（L3 迁至 peri-agent，此处 re-export；
// mod.rs 统一对外 re-export） ───
pub use peri_agent::session::subagent::{build_fork_directive, build_prediction_directive};

#[cfg(test)]
#[path = "fork_test.rs"]
mod tests;
