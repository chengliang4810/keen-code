//! 大型扩展工具目录的延迟搜索与受控执行入口。

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use keencode_agent::{
    AgentTool as RuntimeAgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture,
    ToolOutput, ToolRegistry, ToolRegistryError,
};
use keencode_model::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};

/// 单个延迟工具定义编码后的最大字节数。
const MAX_DEFERRED_DEFINITION_BYTES: usize = 48 * 1024;
/// 单个 Session 延迟目录允许保存的最大工具数量。
const MAX_DEFERRED_TOOLS: usize = 512;
/// 单次关键词查询允许占用的最大 UTF-8 字节数。
const MAX_SEARCH_QUERY_BYTES: usize = 512;
/// 单次查询允许返回的最大完整工具定义数量。
const MAX_SEARCH_RESULTS: usize = 8;
/// 精确选择查询使用的固定前缀。
const EXACT_SELECTION_PREFIX: &str = "select:";
/// 延迟执行入口自身的稳定名称。
const EXECUTE_EXTRA_TOOL_NAME: &str = "ExecuteExtraTool";
/// 延迟搜索入口自身的稳定名称。
const TOOL_SEARCH_NAME: &str = "ToolSearch";

/// 延迟目录内冻结的一个工具定义和执行实现。
struct DeferredToolEntry {
    /// 提供给搜索结果且在目录代次内不可变的定义。
    definition: ToolDefinition,
    /// 收到受控执行请求后实际调用的工具实现。
    implementation: Arc<dyn RuntimeAgentTool>,
}

/// 可被 Runtime 原子替换、供搜索和执行入口共享的延迟工具目录。
#[derive(Default)]
pub struct DeferredToolCatalog {
    /// 保存单调代次与按可移植工具名称排序的当前完整目录。
    state: RwLock<DeferredCatalogState>,
}

/// 延迟目录需要在副作用分类与执行之间保持一致的冻结状态。
#[derive(Default)]
struct DeferredCatalogState {
    /// 每次成功完整替换后递增且不复用的目录代次。
    generation: u64,
    /// 当前代次内按名称排序的冻结工具表。
    entries: BTreeMap<String, DeferredToolEntry>,
}

impl fmt::Debug for DeferredToolCatalog {
    /// 调试输出只展示条目数量，不泄露扩展说明或 Schema。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.read_state();
        formatter
            .debug_struct("DeferredToolCatalog")
            .field("generation", &state.generation)
            .field("len", &state.entries.len())
            .finish()
    }
}

impl DeferredToolCatalog {
    /// 创建一个空的延迟工具目录。
    pub fn new() -> Self {
        Self::default()
    }

    /// 原子校验并替换当前完整目录；任一工具无效时保留旧代次。
    pub fn replace_all(
        &self,
        tools: Vec<Arc<dyn RuntimeAgentTool>>,
    ) -> Result<usize, DeferredToolCatalogError> {
        if tools.len() > MAX_DEFERRED_TOOLS {
            return Err(DeferredToolCatalogError::CapacityExceeded);
        }
        let mut next = BTreeMap::new();
        for tool in tools {
            let definition = tool.definition();
            definition
                .validate()
                .map_err(|_| DeferredToolCatalogError::InvalidDefinition)?;
            if matches!(
                definition.name.as_str(),
                EXECUTE_EXTRA_TOOL_NAME | TOOL_SEARCH_NAME
            ) {
                return Err(DeferredToolCatalogError::ReservedName);
            }
            let encoded_bytes = serde_json::to_vec(&definition)
                .map_err(|_| DeferredToolCatalogError::InvalidDefinition)?
                .len();
            if encoded_bytes > MAX_DEFERRED_DEFINITION_BYTES {
                return Err(DeferredToolCatalogError::DefinitionTooLarge);
            }
            let name = definition.name.clone();
            if next
                .insert(
                    name,
                    DeferredToolEntry {
                        definition,
                        implementation: tool,
                    },
                )
                .is_some()
            {
                return Err(DeferredToolCatalogError::DuplicateName);
            }
        }
        let count = next.len();
        let mut state = self.write_state();
        let generation = state
            .generation
            .checked_add(1)
            .ok_or(DeferredToolCatalogError::GenerationExhausted)?;
        *state = DeferredCatalogState {
            generation,
            entries: next,
        };
        Ok(count)
    }

    /// 返回当前目录按名称排序的不可变定义快照。
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.read_state()
            .entries
            .values()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    /// 返回当前目录包含的工具数量。
    pub fn len(&self) -> usize {
        self.read_state().entries.len()
    }

    /// 返回当前目录是否为空。
    pub fn is_empty(&self) -> bool {
        self.read_state().entries.is_empty()
    }

    /// 按精确名称复制冻结定义与执行实现。
    fn resolve(
        &self,
        generation: u64,
        name: &str,
    ) -> Option<(ToolDefinition, Arc<dyn RuntimeAgentTool>)> {
        let state = self.read_state();
        if state.generation != generation {
            return None;
        }
        state
            .entries
            .get(name)
            .map(|entry| (entry.definition.clone(), Arc::clone(&entry.implementation)))
    }

    /// 按精确选择或关键词评分返回有界定义快照。
    fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<(u64, Vec<ToolDefinition>), DeferredToolCatalogError> {
        let query = query.trim();
        if query.is_empty() || query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(DeferredToolCatalogError::InvalidQuery);
        }
        let state = self.read_state();
        let generation = state.generation;
        let entries = &state.entries;
        if let Some(selection) = query.strip_prefix(EXACT_SELECTION_PREFIX) {
            let mut seen = BTreeSet::new();
            let mut selected = Vec::new();
            for name in selection.split(',').map(str::trim) {
                if name.is_empty() || !seen.insert(name) {
                    return Err(DeferredToolCatalogError::InvalidQuery);
                }
                if let Some(entry) = entries.get(name) {
                    selected.push(entry.definition.clone());
                }
                if selected.len() == limit {
                    break;
                }
            }
            return Ok((generation, selected));
        }

        let normalized_query = query.to_lowercase();
        let tokens = normalized_query
            .split_whitespace()
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        if tokens.is_empty() {
            return Err(DeferredToolCatalogError::InvalidQuery);
        }
        let mut matches = entries
            .values()
            .filter_map(|entry| {
                let name = entry.definition.name.to_lowercase();
                let description = entry.definition.description.to_lowercase();
                let mut score = 0_u32;
                for token in &tokens {
                    if name == *token {
                        score = score.saturating_add(1_000);
                    } else if name.contains(token) {
                        score = score.saturating_add(100);
                    } else if description.contains(token) {
                        score = score.saturating_add(10);
                    } else {
                        return None;
                    }
                }
                Some((score, entry.definition.clone()))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.name.cmp(&right.1.name))
        });
        Ok((
            generation,
            matches
                .into_iter()
                .take(limit)
                .map(|(_, definition)| definition)
                .collect(),
        ))
    }

    /// 即使先前调用 panic 导致锁中毒，也只恢复仍然完整的当前目录代次。
    fn read_state(&self) -> RwLockReadGuard<'_, DeferredCatalogState> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 即使先前调用 panic 导致锁中毒，也允许下一次原子替换恢复目录。
    fn write_state(&self) -> RwLockWriteGuard<'_, DeferredCatalogState> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 延迟工具目录拒绝新代次或查询的稳定原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeferredToolCatalogError {
    /// 新代次超过单个 Session 的固定工具数量上限。
    CapacityExceeded,
    /// 工具名称、说明或输入 Schema 不满足统一模型边界。
    InvalidDefinition,
    /// 单个完整定义超过可进入搜索结果的固定大小上限。
    DefinitionTooLarge,
    /// 新代次包含重复工具名称。
    DuplicateName,
    /// 扩展试图占用搜索或延迟执行入口的保留名称。
    ReservedName,
    /// 查询为空、过长或精确选择格式无效。
    InvalidQuery,
    /// 目录代次计数耗尽，不能安全复用旧代次。
    GenerationExhausted,
}

impl fmt::Display for DeferredToolCatalogError {
    /// 输出不包含扩展名称、说明或 Schema 的固定错误文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CapacityExceeded => "延迟工具目录超过容量上限",
            Self::InvalidDefinition => "延迟工具定义无效",
            Self::DefinitionTooLarge => "延迟工具定义超过大小上限",
            Self::DuplicateName => "延迟工具名称重复",
            Self::ReservedName => "延迟工具占用了保留名称",
            Self::InvalidQuery => "延迟工具查询无效",
            Self::GenerationExhausted => "延迟工具目录代次已经耗尽",
        })
    }
}

impl Error for DeferredToolCatalogError {}

/// 将 `ToolSearch` 和 `ExecuteExtraTool` 注册到模型直接可见的工具表。
pub fn register_deferred_tools(
    registry: &mut ToolRegistry,
    catalog: Arc<DeferredToolCatalog>,
) -> Result<(), ToolRegistryError> {
    let occupied = registry.definitions().into_iter().find(|definition| {
        matches!(
            definition.name.as_str(),
            TOOL_SEARCH_NAME | EXECUTE_EXTRA_TOOL_NAME
        )
    });
    if let Some(definition) = occupied {
        return Err(ToolRegistryError::DuplicateName {
            name: definition.name,
        });
    }
    registry.register(Arc::new(ToolSearchTool::new(Arc::clone(&catalog))))?;
    registry.register(Arc::new(ExecuteExtraTool::new(catalog)))?;
    Ok(())
}

/// 让模型按关键词或精确名称发现延迟工具完整 Schema 的只读工具。
pub struct ToolSearchTool {
    /// 当前 Session 的共享延迟目录。
    catalog: Arc<DeferredToolCatalog>,
}

impl ToolSearchTool {
    /// 创建绑定指定延迟目录的搜索工具。
    pub fn new(catalog: Arc<DeferredToolCatalog>) -> Self {
        Self { catalog }
    }
}

impl RuntimeAgentTool for ToolSearchTool {
    /// 返回有界关键词查询与精确选择输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            TOOL_SEARCH_NAME,
            "搜索当前 Session 的延迟扩展工具。普通查询要求所有关键词命中名称或说明；select:name1,name2 可精确取得完整 Schema。找到工具后使用 ExecuteExtraTool 执行。",
            json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_SEARCH_QUERY_BYTES
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_SEARCH_RESULTS
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    /// 搜索只读取进程内冻结目录。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_search_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 目录读取可与其他只读工具并发。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 返回完整但数量和总大小均有上界的工具定义数组。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let input = parse_search_input(&input)?;
            let (generation, definitions) = self
                .catalog
                .search(&input.query, input.limit.unwrap_or(MAX_SEARCH_RESULTS))
                .map_err(map_catalog_error)?;
            let encoded = serde_json::to_string(&json!({
                "catalog_generation": generation,
                "tools": definitions
            }))
            .map_err(|_| {
                ToolError::permanent("tool_search_encode_failed", "工具搜索结果无法编码")
            })?;
            Ok(ToolOutput::text(encoded))
        })
    }
}

/// 只通过延迟目录中冻结的精确名称执行一个扩展工具的入口。
pub struct ExecuteExtraTool {
    /// 当前 Session 的共享延迟目录。
    catalog: Arc<DeferredToolCatalog>,
}

impl ExecuteExtraTool {
    /// 创建绑定指定延迟目录的执行入口。
    pub fn new(catalog: Arc<DeferredToolCatalog>) -> Self {
        Self { catalog }
    }
}

impl RuntimeAgentTool for ExecuteExtraTool {
    /// 返回目标名称与原始对象参数的严格包装 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            EXECUTE_EXTRA_TOOL_NAME,
            "执行此前通过 ToolSearch 发现的延迟扩展工具。tool_name 必须精确匹配搜索结果，params 必须满足该工具返回的输入 Schema。",
            json!({
                "type": "object",
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 64
                    },
                    "catalog_generation": {
                        "type": "integer",
                        "minimum": 1
                    },
                    "params": { "type": "object" }
                },
                "required": ["catalog_generation", "tool_name", "params"],
                "additionalProperties": false
            }),
        )
    }

    /// 使用目标工具自身的输入校验和副作用分类执行 Plan 只读约束。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let input = parse_execute_input(input)?;
        let (definition, tool) = self
            .catalog
            .resolve(input.catalog_generation, &input.tool_name)
            .ok_or_else(missing_tool_error)?;
        definition.validate_input(&input.params).map_err(|_| {
            ToolError::permanent(
                "deferred_tool_input_invalid",
                "延迟工具输入不符合已发现的 Schema",
            )
        })?;
        tool.effect(&input.params)
    }

    /// 延迟目标可能产生副作用，因此入口保守地形成顺序屏障。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在再次校验冻结 Schema 后把可信 ToolContext 原样交给目标工具。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(cancelled_error());
            }
            let input = parse_execute_input(&input)?;
            let (definition, tool) = self
                .catalog
                .resolve(input.catalog_generation, &input.tool_name)
                .ok_or_else(missing_tool_error)?;
            definition.validate_input(&input.params).map_err(|_| {
                ToolError::permanent(
                    "deferred_tool_input_invalid",
                    "延迟工具输入不符合已发现的 Schema",
                )
            })?;
            tool.execute(context, input.params).await
        })
    }
}

/// `ToolSearch` 的严格输入对象。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolSearchInput {
    /// 普通关键词或带 `select:` 前缀的精确名称列表。
    query: String,
    /// 本次结果数量上限。
    limit: Option<usize>,
}

/// `ExecuteExtraTool` 的严格输入对象。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecuteExtraToolInput {
    /// `ToolSearch` 返回且参与执行一致性校验的目录代次。
    catalog_generation: u64,
    /// 先前搜索返回的精确延迟工具名称。
    tool_name: String,
    /// 传给目标工具的原始 JSON 对象参数。
    params: Value,
}

/// 解析并校验一次工具搜索输入。
fn parse_search_input(input: &Value) -> Result<ToolSearchInput, ToolError> {
    let parsed: ToolSearchInput = serde_json::from_value(input.clone())
        .map_err(|_| ToolError::permanent("tool_search_input_invalid", "工具搜索输入无效"))?;
    let limit = parsed.limit.unwrap_or(MAX_SEARCH_RESULTS);
    if parsed.query.trim().is_empty()
        || parsed.query.len() > MAX_SEARCH_QUERY_BYTES
        || !(1..=MAX_SEARCH_RESULTS).contains(&limit)
    {
        return Err(ToolError::permanent(
            "tool_search_input_invalid",
            "工具搜索输入无效",
        ));
    }
    Ok(parsed)
}

/// 解析并校验一次延迟执行包装输入。
fn parse_execute_input(input: &Value) -> Result<ExecuteExtraToolInput, ToolError> {
    let parsed: ExecuteExtraToolInput = serde_json::from_value(input.clone())
        .map_err(|_| ToolError::permanent("deferred_tool_input_invalid", "延迟工具包装输入无效"))?;
    if parsed.catalog_generation == 0
        || parsed.tool_name.trim().is_empty()
        || parsed.tool_name.len() > 64
        || !parsed.params.is_object()
    {
        return Err(ToolError::permanent(
            "deferred_tool_input_invalid",
            "延迟工具包装输入无效",
        ));
    }
    Ok(parsed)
}

/// 把目录错误归一为不包含扩展正文的稳定工具错误。
fn map_catalog_error(_error: DeferredToolCatalogError) -> ToolError {
    ToolError::permanent("tool_search_failed", "延迟工具目录无法完成查询")
}

/// 返回目标已经不存在时的稳定错误。
fn missing_tool_error() -> ToolError {
    ToolError::permanent(
        "deferred_tool_not_found",
        "延迟工具不存在或目录已经更新，请重新搜索",
    )
}

/// 返回 Turn 已取消时的稳定错误。
fn cancelled_error() -> ToolError {
    ToolError::permanent("tool_cancelled", "当前 Turn 已取消")
}
