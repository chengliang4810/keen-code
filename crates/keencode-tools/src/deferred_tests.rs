use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolConcurrency, ToolContext, ToolEffect, ToolError,
    ToolFuture, ToolOutput, ToolRegistry, TurnCancellation, TurnId,
};
use keencode_model::ToolDefinition;
use serde_json::{Value, json};

use crate::{
    DeferredToolCatalog, DeferredToolCatalogError, ExecuteExtraTool, ToolSearchTool,
    register_deferred_tools,
};

/// 可记录执行次数并按构造参数返回副作用分类的测试工具。
struct RecordedTool {
    /// 测试工具的冻结定义。
    definition: ToolDefinition,
    /// 工具调用应返回的副作用分类。
    effect: ToolEffect,
    /// 实际进入 execute 的次数。
    executions: Arc<AtomicUsize>,
}

impl RecordedTool {
    /// 创建一个要求 `value` 字符串参数的测试工具。
    fn new(name: &str, description: &str, effect: ToolEffect) -> (Arc<Self>, Arc<AtomicUsize>) {
        let executions = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                definition: ToolDefinition::new(
                    name,
                    description,
                    json!({
                        "type": "object",
                        "properties": {
                            "value": { "type": "string", "minLength": 1 }
                        },
                        "required": ["value"],
                        "additionalProperties": false
                    }),
                ),
                effect,
                executions: Arc::clone(&executions),
            }),
            executions,
        )
    }
}

impl AgentTool for RecordedTool {
    /// 返回测试使用的冻结定义。
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    /// 校验输入并返回构造时指定的影响分类。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        self.definition
            .validate_input(input)
            .map_err(|_| ToolError::permanent("invalid", "测试输入无效"))?;
        Ok(self.effect)
    }

    /// 测试工具可以由包装入口决定最终屏障。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 记录调用并回传输入值。
    fn execute(&self, _context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            self.executions.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text(
                input["value"].as_str().unwrap_or_default().to_owned(),
            ))
        })
    }
}

/// 创建不包含真实文件或凭据的测试工具上下文。
fn context() -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-deferred").unwrap(),
        turn_id: TurnId::new("turn-deferred").unwrap(),
        source_agent_id: AgentId::new("agent-deferred").unwrap(),
        tool_call_id: ToolCallId::new("call-deferred").unwrap(),
        cancellation: TurnCancellation::new(),
    }
}

#[test]
fn catalog_replacement_is_atomic_and_rejects_reserved_or_duplicate_names() {
    let catalog = DeferredToolCatalog::new();
    let (original, _) = RecordedTool::new("mcp__one", "原始工具", ToolEffect::ReadOnly);
    catalog.replace_all(vec![original]).unwrap();

    let (first, _) = RecordedTool::new("mcp__same", "重复一", ToolEffect::ReadOnly);
    let (second, _) = RecordedTool::new("mcp__same", "重复二", ToolEffect::ReadOnly);
    assert_eq!(
        catalog.replace_all(vec![first, second]).unwrap_err(),
        DeferredToolCatalogError::DuplicateName
    );
    assert_eq!(catalog.definitions()[0].name, "mcp__one");

    let (reserved, _) = RecordedTool::new("ToolSearch", "保留工具", ToolEffect::ReadOnly);
    assert_eq!(
        catalog.replace_all(vec![reserved]).unwrap_err(),
        DeferredToolCatalogError::ReservedName
    );
    assert_eq!(catalog.len(), 1);
}

#[tokio::test]
async fn search_returns_bounded_full_schemas_for_keywords_and_exact_selection() {
    let catalog = Arc::new(DeferredToolCatalog::new());
    let (read, _) = RecordedTool::new(
        "mcp__files__read",
        "读取 workspace 文件",
        ToolEffect::ReadOnly,
    );
    let (send, _) = RecordedTool::new(
        "mcp__chat__send",
        "发送 channel 消息",
        ToolEffect::ChangesState,
    );
    catalog.replace_all(vec![read, send]).unwrap();
    let search = ToolSearchTool::new(catalog);

    let keyword = search
        .execute(context(), json!({ "query": "workspace read" }))
        .await
        .unwrap();
    let keyword: Value = match &keyword.content[0] {
        keencode_model::ToolResultContent::Text { text } => serde_json::from_str(text).unwrap(),
        _ => panic!("搜索结果应为 JSON 文本"),
    };
    assert_eq!(keyword["catalog_generation"], 1);
    assert_eq!(keyword["tools"].as_array().unwrap().len(), 1);
    assert_eq!(keyword["tools"][0]["name"], "mcp__files__read");
    assert!(keyword["tools"][0]["inputSchema"].is_object());

    let exact = search
        .execute(
            context(),
            json!({ "query": "select:mcp__chat__send,mcp__missing" }),
        )
        .await
        .unwrap();
    let exact: Value = match &exact.content[0] {
        keencode_model::ToolResultContent::Text { text } => serde_json::from_str(text).unwrap(),
        _ => panic!("搜索结果应为 JSON 文本"),
    };
    assert_eq!(exact["tools"].as_array().unwrap().len(), 1);
    assert_eq!(exact["tools"][0]["name"], "mcp__chat__send");
}

#[tokio::test]
async fn execute_entry_delegates_schema_effect_context_and_single_execution() {
    let catalog = Arc::new(DeferredToolCatalog::new());
    let (target, executions) =
        RecordedTool::new("mcp__chat__send", "发送消息", ToolEffect::ChangesState);
    catalog.replace_all(vec![target]).unwrap();
    let execute = ExecuteExtraTool::new(catalog);
    let input = json!({
        "catalog_generation": 1,
        "tool_name": "mcp__chat__send",
        "params": { "value": "hello" }
    });

    assert_eq!(execute.effect(&input).unwrap(), ToolEffect::ChangesState);
    let output = execute.execute(context(), input).await.unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        output.content,
        vec![keencode_model::ToolResultContent::Text {
            text: "hello".to_owned()
        }]
    );
    assert!(
        execute
            .effect(&json!({
                "catalog_generation": 1,
                "tool_name": "mcp__chat__send",
                "params": { "value": "" }
            }))
            .is_err()
    );
}

#[test]
fn direct_registry_exposes_only_search_and_execute_entrypoints() {
    let catalog = Arc::new(DeferredToolCatalog::new());
    let (target, _) = RecordedTool::new("mcp__hidden__tool", "隐藏工具", ToolEffect::ReadOnly);
    catalog.replace_all(vec![target]).unwrap();
    let mut registry = ToolRegistry::new();
    register_deferred_tools(&mut registry, catalog).unwrap();

    let names = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["ExecuteExtraTool", "ToolSearch"]);
    assert!(!names.iter().any(|name| name == "mcp__hidden__tool"));
}

#[test]
fn cancelled_search_and_missing_execution_fail_without_target_side_effect() {
    let catalog = Arc::new(DeferredToolCatalog::new());
    let search = ToolSearchTool::new(Arc::clone(&catalog));
    let cancellation = TurnCancellation::new();
    cancellation.cancel();
    let cancelled_context = ToolContext {
        cancellation,
        ..context()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(
        runtime
            .block_on(search.execute(cancelled_context, json!({ "query": "anything" })))
            .is_err()
    );

    let execute = ExecuteExtraTool::new(catalog);
    assert!(
        execute
            .effect(&json!({
                "catalog_generation": 1,
                "tool_name": "mcp__missing",
                "params": {}
            }))
            .is_err()
    );
}

#[test]
fn catalog_generation_fences_replacement_between_effect_and_execution() {
    let catalog = Arc::new(DeferredToolCatalog::new());
    let (read_only, _) = RecordedTool::new("mcp__stable", "初始只读工具", ToolEffect::ReadOnly);
    catalog.replace_all(vec![read_only]).unwrap();
    let execute = ExecuteExtraTool::new(Arc::clone(&catalog));
    let approved_input = json!({
        "catalog_generation": 1,
        "tool_name": "mcp__stable",
        "params": { "value": "before" }
    });
    assert_eq!(
        execute.effect(&approved_input).unwrap(),
        ToolEffect::ReadOnly
    );

    let (replacement, executions) = RecordedTool::new(
        "mcp__stable",
        "替换后的副作用工具",
        ToolEffect::ChangesState,
    );
    catalog.replace_all(vec![replacement]).unwrap();
    assert!(execute.effect(&approved_input).is_err());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert!(
        runtime
            .block_on(execute.execute(context(), approved_input))
            .is_err()
    );
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}
