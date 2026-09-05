//! ToolSearchMiddleware — 注册元工具并注入延迟工具列表到 system prompt

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use peri_agent::{
    error::AgentResult,
    middleware::{r#trait::Middleware, state::MiddlewareState},
    tools::BaseTool,
};

use super::{
    artifact_tool::ArtifactTool, declaration::collect_declarations, execute_tool::ExecuteExtraTool,
    search_tool::SearchExtraTools, tool_index::ToolSearchIndex,
};

/// ToolSearch 中间件
///
/// 职责：
/// 1. 注册 SearchExtraTools 和 ExecuteExtraTool 两个元工具
/// 2. 将延迟工具列表和 direct 工具声明注入 system prompt
pub struct ToolSearchMiddleware {
    tool_search_index: Arc<ToolSearchIndex>,
    shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
}

impl ToolSearchMiddleware {
    pub fn new(
        tool_search_index: Arc<ToolSearchIndex>,
        shared_tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    ) -> Self {
        Self {
            tool_search_index,
            shared_tools,
        }
    }
}

#[async_trait]
impl Middleware for ToolSearchMiddleware {
    fn name(&self) -> &str {
        "ToolSearch"
    }

    fn collect_tools(&self, cwd: &str) -> Vec<Box<dyn BaseTool>> {
        vec![
            Box::new(SearchExtraTools::new(Arc::clone(&self.tool_search_index))),
            Box::new(ExecuteExtraTool::new(Arc::clone(&self.shared_tools))),
            Box::new(ArtifactTool::new(cwd.to_string())),
        ]
    }

    fn prompt_contribution(&self) -> Option<String> {
        // `build_stage_context` constructs AgentModelBridge before the ReAct
        // loop can run `before_agent`.  Refresh synchronously from the current
        // shared tool snapshot so the first request gets the same deferred and
        // direct declarations as subsequent turns.  `before_agent` still owns
        // the recall side effect and calls the same helper below.
        self.refresh_prompt_contribution(None)
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        let _ = self.refresh_prompt_contribution(Some(state));
        Ok(())
    }
}

impl ToolSearchMiddleware {
    /// Rebuild the search/prompt snapshot from the current shared registry.
    ///
    /// The stage builder collects middleware tools before constructing the
    /// model, while the ReAct loop invokes `before_agent` only afterwards.
    /// Keeping this operation synchronous lets both paths share one source of
    /// truth without moving the asynchronous middleware lifecycle hook or
    /// constructing `SubAgentTool` before its parent session is available.
    fn refresh_prompt_contribution(
        &self,
        state: Option<&mut dyn MiddlewareState>,
    ) -> Option<String> {
        // 检查 shared_tools 是否有变化（MCP 后续连接等场景）
        // 一次加锁同时收集 deferred（搜索索引面）与 direct（LLM 可见面，
        // 声明段数据源，design v2 §2.5.2）两个集合。
        let tools = self.shared_tools.read();
        let deferred_arcs: Vec<Arc<dyn BaseTool>> = tools
            .iter()
            .filter(|(_, tool)| !tool.is_direct())
            .map(|(_, tool)| Arc::clone(tool))
            .collect();
        let direct_arcs: Vec<Arc<dyn BaseTool>> = tools
            .iter()
            .filter(|(_, tool)| tool.is_direct())
            .map(|(_, tool)| Arc::clone(tool))
            .collect();
        drop(tools);

        // P2-2: 用 content_version 比对取代简单 count 比对
        let current_version = self.tool_search_index.content_version();
        let cached_version = self.tool_search_index.cached_prompt_version();
        let old_count = self.tool_search_index.total_count();
        let should_rebuild =
            cached_version != Some(current_version) || old_count != deferred_arcs.len();

        if should_rebuild {
            self.tool_search_index.build(deferred_arcs);
            let new_count = self.tool_search_index.total_count();
            if old_count > 0 && new_count != old_count {
                if let Some(state) = state {
                    state.push_recall(format!(
                        "[ToolSearch] Deferred tools updated: {} tools available (was {})",
                        new_count, old_count
                    ));
                }
            }
            let list = self.tool_search_index.format_deferred_list();
            if !list.is_empty() {
                self.tool_search_index.set_cached_prompt(list);
            } else {
                // An empty deferred snapshot must not retain a previous
                // turn's prompt when a session registry was reset.
                self.tool_search_index.set_cached_prompt(String::new());
            }
        }

        // 生成 prompt 贡献（由 prompt_contribution 与 before_agent 共同调用）。
        // 合并策略（design v2 §2.5.2）：deferred 列表在前、声明段在后，`\n\n` 分隔；
        // 任一段为空时只保留另一段。声明段不走索引 content_version 失效路径——
        // 每轮 before_agent 独立重渲染，输出仅依赖工具静态字段。
        let list = self
            .tool_search_index
            .cached_prompt()
            .filter(|text| !text.is_empty());
        let declarations = collect_declarations(&direct_arcs);
        match (list, declarations) {
            (Some(l), Some(d)) => Some(format!("{l}\n\n{d}")),
            (Some(l), None) => Some(l),
            (None, Some(d)) => Some(d),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
#[path = "middleware_test.rs"]
mod tests;
