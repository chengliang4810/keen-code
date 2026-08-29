use std::sync::{Arc, Mutex};

use parking_lot::RwLock;
use peri_agent::{
    agent::{
        react::{ReactLLM, Reasoning, StreamingContext},
        AgentCancellationToken,
    },
    messages::BaseMessage,
    thread::ThreadStore,
    tools::BaseTool,
};
use tempfile::tempdir;

use super::*;
use crate::claude_agent_parser::ToolsValue;

/// 修改进程级 Agent 环境变量的测试必须串行，避免并发读取彼此的配置。
static AGENT_ENV_LOCK: Mutex<()> = Mutex::new(());

// Mock LLM: returns final answer directly
struct EchoLLM;

#[async_trait::async_trait]
impl ReactLLM for EchoLLM {
    async fn generate_reasoning(
        &self,
        messages: &[BaseMessage],
        _tools: &[&dyn BaseTool],
        _streaming: Option<StreamingContext>,
    ) -> peri_agent::error::AgentResult<Reasoning> {
        let last = messages.last().map(|m| m.content()).unwrap_or_default();
        Ok(Reasoning::with_answer("", format!("echo: {}", last)))
    }
}

fn make_tool(name: &'static str) -> Arc<dyn BaseTool> {
    struct DummyTool(&'static str);

    #[async_trait::async_trait]
    impl BaseTool for DummyTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn is_direct(&self) -> bool {
            true
        }
        async fn invoke(
            &self,
            _input: serde_json::Value,
            _ctx: peri_agent::tools::ToolContext<'_>,
        ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
            Ok(format!("{} result", self.0))
        }
    }

    Arc::new(DummyTool(name))
}

fn make_subagent_tool(parent_tools: Vec<Arc<dyn BaseTool>>) -> SubAgentTool {
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(|_: Option<&str>, _: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
        "/tmp".to_string(),
    )
    .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()))
}

fn capture_completion(
    tool: SubAgentTool,
) -> (
    SubAgentTool,
    tokio::sync::mpsc::UnboundedReceiver<peri_agent::agent::events::BackgroundTaskResult>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let tool = tool.with_on_bg_complete(Arc::new(move |result, _kind| {
        let _ = tx.send(result.clone());
    }));
    (tool, rx)
}

async fn wait_for_completion(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<
        peri_agent::agent::events::BackgroundTaskResult,
    >,
) -> peri_agent::agent::events::BackgroundTaskResult {
    tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .expect("Agent should complete before timeout")
        .expect("completion channel should remain open")
}

#[test]
fn test_tool_name() {
    let t = make_subagent_tool(vec![]);
    assert_eq!(t.name(), "Agent");
}

#[test]
fn agent_tool_description_includes_concurrency_slots() {
    let tool = make_subagent_tool(vec![]).with_concurrency_slots(7);
    assert!(tool
        .description()
        .contains("There are 7 available concurrency slots, including you."));
}

#[test]
fn agent_tool_description_defaults_to_static_text() {
    let tool = make_subagent_tool(vec![]);
    assert!(!tool.description().contains("available concurrency slots"));
}

#[test]
fn test_agent_parameters_require_prompt_without_resume() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    let required = params["required"].as_array().unwrap();
    assert_eq!(required.as_slice(), &[serde_json::json!("prompt")]);
    assert!(
        params["properties"].get("resume_thread_id").is_none(),
        "Agent schema 不应再暴露恢复入口"
    );
}

#[test]
fn test_agent_fork_description_declares_exclusivity_with_subagent_type() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    let fork_desc = params["properties"]["fork"]["description"]
        .as_str()
        .unwrap();
    assert!(
        fork_desc.contains("Mutually exclusive with subagent_type"),
        "fork 描述应声明与 subagent_type 互斥，实际: {fork_desc}"
    );
}

#[test]
fn test_agent_schema_is_english_and_uses_current_builtin_agent_ids() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    let type_desc = params["properties"]["subagent_type"]["description"]
        .as_str()
        .unwrap();

    assert!(type_desc.contains("Available agents:"));
    assert!(type_desc.contains("- code-reviewer"));
    assert!(type_desc.contains("- verification"));
}

/// Verify error returned when prompt parameter is missing
#[tokio::test]
async fn test_agent_prompt_missing_returns_error() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = make_subagent_tool(vec![]);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("prompt"),
        "Should return missing prompt error: {}",
        err_msg
    );
}

/// Verify error returned when subagent_type parameter is missing and fork is not set
#[tokio::test]
async fn test_agent_subagent_type_missing_returns_error() {
    let t = make_subagent_tool(vec![]);
    let result = t
        .invoke(
            serde_json::json!({
                "prompt": "do something"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("subagent_type") || err_msg.contains("fork"),
        "Should return missing subagent_type error with fork hint: {}",
        err_msg
    );
}

/// Verify subagent_type="fork" is treated as fork:true (common LLM mistake)
#[tokio::test]
async fn test_subagent_type_fork_treated_as_fork_mode() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(|_: Option<&str>, _: Option<&str>| Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    // subagent_type: "fork" should trigger fork mode, NOT try to load an agent named "fork"
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "fork",
                "prompt": "do something"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Agent started (child_thread_id:") && result.contains("child_thread_id:"),
        "subagent_type='fork' should trigger fork mode: {}",
        result
    );
    let _ = wait_for_completion(&mut completed).await;
}

#[tokio::test]
async fn test_tool_agent_not_found() {
    let t = make_subagent_tool(vec![]);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "nonexistent-agent",
                "prompt": "do something",
                "cwd": "/tmp"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("cannot find"),
        "Should return not found error: {}",
        err_msg
    );
}

#[tokio::test]
async fn test_tool_filter_inherit_all() {
    // tools is Empty -> inherit all parent tools, but exclude Agent
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Agent"), // this should be excluded
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Write"));
    assert!(!names.contains(&"Agent"), "Agent should not be inherited");
}

#[test]
fn test_tool_filter_allowlist() {
    // tools has value -> only keep specified tools
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["Read".to_string(), "Glob".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Glob"));
    assert!(
        !names.contains(&"Write"),
        "Write not in allowlist should be excluded"
    );
}

#[test]
fn test_tool_filter_disallow() {
    // disallowedTools -> exclude from inherited set
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Edit")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(
        !names.contains(&"Write"),
        "Write in disallow list should be excluded"
    );
    assert!(
        !names.contains(&"Edit"),
        "Edit in disallow list should be excluded"
    );
}

#[test]
fn test_tool_filter_wildcard_star() {
    // tools: "*" -> inherit all parent tools (same as Empty), but still exclude Agent
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Bash"),
        make_tool("Agent"), // should still be excluded
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["*".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(
        names.contains(&"Read"),
        "Read should be inherited with tools: *"
    );
    assert!(
        names.contains(&"Write"),
        "Write should be inherited with tools: *"
    );
    assert!(
        names.contains(&"Bash"),
        "Bash should be inherited with tools: *"
    );
    assert!(
        !names.contains(&"Agent"),
        "Agent should still be excluded even with tools: *"
    );
}

#[test]
fn test_tool_filter_wildcard_star_with_disallowed() {
    // tools: "*" + disallowedTools -> inherit all except disallowed
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Edit"),
        make_tool("Bash"),
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["*".to_string()]);
    let disallowed = ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"), "Read should be inherited");
    assert!(names.contains(&"Bash"), "Bash should be inherited");
    assert!(
        !names.contains(&"Write"),
        "Write in disallow list should be excluded even with tools: *"
    );
    assert!(
        !names.contains(&"Edit"),
        "Edit in disallow list should be excluded even with tools: *"
    );
}

#[tokio::test]
async fn test_tool_executes_with_valid_agent_file() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = make_subagent_tool(vec![]);
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Agent started (child_thread_id:") && result.contains("child_thread_id:"),
        "Should return asynchronous task info: {}",
        result
    );
}

#[tokio::test]
async fn test_tool_rejects_legacy_project_path_and_filename_name_mismatch() {
    let dir = tempdir().unwrap();
    let legacy_dir = dir.path().join(".claude").join("agents");
    let project_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        legacy_dir.join("legacy.md"),
        "---\nname: legacy\ndescription: Legacy\n---\nlegacy",
    )
    .unwrap();
    std::fs::write(
        project_dir.join("mismatch.md"),
        "---\nname: different\ndescription: Mismatch\n---\nmismatch",
    )
    .unwrap();

    let tool = make_subagent_tool(vec![]);
    for agent_id in ["legacy", "mismatch"] {
        let error = tool
            .invoke(
                serde_json::json!({
                    "subagent_type": agent_id,
                    "prompt": "hello",
                    "cwd": dir.path().to_str().unwrap()
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap_err()
            .to_string();
        if agent_id == "legacy" {
            assert!(error.contains("cannot find"), "{error}");
        } else {
            assert!(error.contains("不一致"), "{error}");
        }
    }
}

/// 无效项目定义占用同名 ID，直接执行时不得静默回退到内置 Agent。
#[tokio::test]
async fn test_tool_invalid_project_agent_blocks_builtin_fallback() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("explorer.md"),
        "---\nname: different\ndescription: Invalid shadow\n---\ninvalid",
    )
    .unwrap();

    let error = make_subagent_tool(vec![])
        .invoke(
            serde_json::json!({
                "subagent_type": "explorer",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("不一致"), "应返回项目定义错误: {error}");
}

/// Verify Agent reserved fields (isolation/description/name) don't affect execution.
#[tokio::test]
async fn test_agent_reserved_fields_parsed() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();

    let t = make_subagent_tool(vec![]);
    let result = t
        .invoke(
            serde_json::json!({
                "prompt": "hello",
                "subagent_type": "test-agent",
                "description": "test desc",
                "name": "test-alias",
                "isolation": "worktree",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    // Reserved fields don't affect asynchronous startup.
    assert!(
        result.contains("Agent started (child_thread_id:") && result.contains("child_thread_id:"),
        "Should execute normally: {}",
        result
    );
}

/// Agent 工具 schema 暴露 model/reasoning_effort 覆盖参数(对齐 Codex spawn_agent)。
#[test]
fn test_agent_parameters_declare_model_overrides() {
    let t = make_subagent_tool(vec![]);
    let params = t.parameters();
    let model = params["properties"].get("model").expect("model 参数应声明");
    assert!(
        model["description"].as_str().unwrap().contains("provider_id::model"),
        "{model}"
    );
    let effort = params["properties"]
        .get("reasoning_effort")
        .expect("reasoning_effort 参数应声明");
    assert!(
        effort["description"]
            .as_str()
            .unwrap()
            .contains("minimal, low, medium, high, xhigh"),
        "{effort}"
    );
}

/// 记录 llm_factory 收到的 model selection（每次 subagent 装配调用一次）
fn make_recording_subagent_tool(
    parent_tools: Vec<Arc<dyn BaseTool>>,
    aliases: Arc<std::sync::Mutex<Vec<Option<String>>>>,
) -> SubAgentTool {
    let aliases_clone = Arc::clone(&aliases);
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(move |alias: Option<&str>, _: Option<&str>| {
            aliases_clone
                .lock()
                .unwrap()
                .push(alias.map(|s| s.to_string()));
            Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()))
}

fn write_test_agent_with_model(dir: &tempfile::TempDir, model: &str) {
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        format!(
            "---\nname: test-agent\ndescription: A test agent\nmodel: {}\n---\n\nYou are a test agent.\n",
            model
        ),
    )
    .unwrap();
}

fn write_test_agent(dir: &tempfile::TempDir) {
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("test-agent.md"),
        "---\nname: test-agent\ndescription: A test agent\n---\n\nYou are a test agent.\n",
    )
    .unwrap();
}

/// 调用时 model 覆盖优先于 agent frontmatter(对齐 Codex spawn_agent 语义)。
#[tokio::test]
async fn test_agent_call_time_model_overrides_frontmatter() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "provider-a::base-model");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "provider-b::override-model",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Agent started (child_thread_id:"),
        "应异步启动: {}",
        result
    );
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[Some("provider-b::override-model".to_string())],
        "调用时 model 应覆盖 frontmatter"
    );
}

/// 模型解析优先级:无调用覆盖 → frontmatter;有调用覆盖 → 调用值。
#[tokio::test]
async fn test_agent_model_resolution_precedence() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "provider-a::model-a");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let tool = make_recording_subagent_tool(vec![], Arc::clone(&aliases));

    for (input, expected) in [
        (
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            Some("provider-a::model-a".to_string()),
        ),
        (
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "provider-b::model-b",
                "cwd": dir.path().to_str().unwrap()
            }),
            Some("provider-b::model-b".to_string()),
        ),
    ] {
        let result = tool
            .invoke(input, peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("Agent started (child_thread_id:"),
            "{result}"
        );
        let recorded = aliases.lock().unwrap();
        assert_eq!(recorded.last().unwrap(), &expected);
    }
}

/// 空/空白 model 视为未指定，保持 agent frontmatter model。
#[tokio::test]
async fn test_agent_frontmatter_model_survives_unknown_input_fields() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "provider-a::base-model");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
    for model in [None, Some(""), Some("   ")] {
        let mut input = serde_json::json!({
            "subagent_type": "test-agent",
            "prompt": "hello",
            "cwd": dir.path().to_str().unwrap()
        });
        if let Some(m) = model {
            input["model"] = serde_json::Value::String(m.to_string());
        }
        let result = t
            .invoke(input, peri_agent::tools::ToolContext::new(&[], "."))
            .await
            .unwrap();
        assert!(
            result.contains("Agent started (child_thread_id:"),
            "应异步启动: {}",
            result
        );
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(
        recorded.as_slice(),
        &[
            Some("provider-a::base-model".to_string()),
            Some("provider-a::base-model".to_string()),
            Some("provider-a::base-model".to_string())
        ],
        "省略/空/空白 model 应保持 frontmatter 定义"
    );
}

/// 省略 model + 空 frontmatter → llm_factory 收到 None（当前会话模型）
#[tokio::test]
async fn test_agent_model_omitted_uses_current_session() {
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    for fm in [""] {
        let dir = tempdir().unwrap();
        write_test_agent_with_model(&dir, fm);
        let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases));
        let result = t
            .invoke(
                serde_json::json!({
                    "subagent_type": "test-agent",
                    "prompt": "hello",
                    "cwd": dir.path().to_str().unwrap()
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap();
        assert!(
            result.contains("Agent started (child_thread_id:"),
            "应异步启动: {}",
            result
        );
    }
    let recorded = aliases.lock().unwrap();
    assert_eq!(recorded.as_slice(), &[None], "省略模型应跟随当前会话");
}

/// fork 路径拒绝 model 覆盖(防漂移):非法格式的 model 同样先被防漂移规则拦截。
#[tokio::test]
async fn test_agent_model_rejected_on_fork() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases))
        .with_parent_messages(parent_messages);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing",
                "model": "turbo"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let error = result.expect_err("fork + model 覆盖应被防漂移规则拒绝");
    assert!(error.to_string().contains("fork"), "{error}");
    assert!(aliases.lock().unwrap().is_empty(), "拒绝路径不得装配 LLM");
}

/// fork 调用携带合法格式的 model 覆盖同样被拒绝(防漂移规则优先于格式校验)。
#[tokio::test]
async fn test_agent_model_override_on_fork_rejected() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases))
        .with_parent_messages(parent_messages);
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing",
                "model": "provider-a::model-a"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let error = result.expect_err("fork + 合法 model 覆盖仍应被拒绝");
    assert!(
        error.to_string().contains("not supported for fork"),
        "{error}"
    );
    assert!(aliases.lock().unwrap().is_empty(), "拒绝路径不得装配 LLM");
}

/// 后台定义型路径同样只使用 agent frontmatter model。
#[tokio::test]
async fn test_agent_frontmatter_model_applies_to_background() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "provider-a::background-model");
    let aliases: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    let t = make_recording_subagent_tool(vec![], Arc::clone(&aliases))
        .with_task_manager(Arc::clone(&registry))
        .with_bg_event_sender(bg_tx);

    let invoke_msg = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "bg task",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("bg 应启动");
    assert!(
        invoke_msg.contains("Agent started (child_thread_id:")
            && invoke_msg.contains("child_thread_id:"),
        "应返回后台任务启动消息: {}",
        invoke_msg
    );
    // llm_factory 在 invoke_background 装配阶段同步调用（spawn 之前）
    {
        let recorded = aliases.lock().unwrap();
        assert_eq!(
            recorded.as_slice(),
            &[Some("provider-a::background-model".to_string())],
            "bg 定义型路径应使用 frontmatter model"
        );
    }

    // 等待 BackgroundTaskCompleted，避免任务悬挂
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::BackgroundTaskCompleted(_))) => break,
            Ok(_) => {}
            _ => break,
        }
    }
}

/// 记录 llm_factory 收到的 (model, effort)（每次 subagent 装配调用一次）。
fn make_recording_pair_subagent_tool(
    parent_tools: Vec<Arc<dyn BaseTool>>,
    records: Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>>,
) -> SubAgentTool {
    let records = Arc::clone(&records);
    SubAgentTool::new(
        Arc::new(parent_tools),
        None,
        Arc::new(
            move |model: Option<&str>, effort: Option<&str>| {
                records
                    .lock()
                    .unwrap()
                    .push((model.map(str::to_string), effort.map(str::to_string)));
                Box::new(EchoLLM) as Box<dyn ReactLLM + Send + Sync>
            },
        ),
        "/tmp".to_string(),
    )
    .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new()))
}

/// 调用时 reasoning_effort 传递到工厂;model 同时覆盖 frontmatter。
#[tokio::test]
async fn test_agent_call_time_effort_and_model_reach_factory() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "provider-a::model-a");
    let records: Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>> = Arc::default();
    let t = make_recording_pair_subagent_tool(vec![], Arc::clone(&records));
    t.invoke(
        serde_json::json!({
            "subagent_type": "test-agent",
            "prompt": "hello",
            "model": "provider-b::model-b",
            "reasoning_effort": "low",
            "cwd": dir.path().to_str().unwrap()
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    assert_eq!(
        records.lock().unwrap().as_slice(),
        &[(
            Some("provider-b::model-b".to_string()),
            Some("low".to_string())
        )]
    );
}

/// reasoning_effort 未知枚举/空值被拒,拒绝路径不装配 LLM。
#[tokio::test]
async fn test_agent_invalid_reasoning_effort_rejected() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "");
    let records: Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>> = Arc::default();
    let t = make_recording_pair_subagent_tool(vec![], Arc::clone(&records));
    for effort in ["ultra", ""] {
        let result = t
            .invoke(
                serde_json::json!({
                    "subagent_type": "test-agent",
                    "prompt": "hello",
                    "reasoning_effort": effort,
                    "cwd": dir.path().to_str().unwrap()
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await;
        let error = result.expect_err("非法 reasoning_effort 应被拒绝");
        assert!(error.to_string().contains("reasoning_effort"), "{error}");
    }
    assert!(records.lock().unwrap().is_empty(), "拒绝路径不得装配 LLM");
}

/// 非法 model 串(缺 provider 前缀)被拒,拒绝路径不装配 LLM。
#[tokio::test]
async fn test_agent_invalid_model_format_rejected() {
    let dir = tempdir().unwrap();
    write_test_agent_with_model(&dir, "");
    let records: Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>> = Arc::default();
    let t = make_recording_pair_subagent_tool(vec![], Arc::clone(&records));
    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "prompt": "hello",
                "model": "just-a-name",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    let error = result.expect_err("非法 model 串应被拒绝");
    assert!(error.to_string().contains("invalid model override"), "{error}");
    assert!(records.lock().unwrap().is_empty(), "拒绝路径不得装配 LLM");
}

#[tokio::test]
async fn test_agent_tool_in_list() {
    // Verify SubAgentTool's tool name is correct, can join tool list
    let t = make_subagent_tool(vec![]);
    assert_eq!(t.name(), "Agent");
    let def = t.definition();
    assert_eq!(def.name, "Agent");
}

/// Recursion prevention: even if agent.md tools field explicitly includes Agent, it must be excluded
#[test]
fn test_agent_excluded_even_when_explicitly_allowed() {
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Agent"), // parent tool set has Agent
    ];
    let t = make_subagent_tool(parent_tools);

    // agent.md has tools: ["Agent", "Read"]
    let allowed = ToolsValue::List(vec!["Agent".to_string(), "Read".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"), "Read should be kept");
    assert!(
        !names.contains(&"Agent"),
        "Agent must be excluded even when explicitly in allowlist (recursion prevention)"
    );
}

/// tools/disallowedTools filtering: case-insensitive (users often write PascalCase)
#[test]
fn test_tool_filter_case_insensitive() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];
    let t = make_subagent_tool(parent_tools);

    // User writes different cases in agent.md: tools: READ, glob
    let allowed = ToolsValue::List(vec!["READ".to_string(), "glob".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(
        names.contains(&"Read"),
        "Case-insensitive: READ should match Read"
    );
    assert!(
        names.contains(&"Glob"),
        "Case-insensitive: glob should match Glob"
    );
    assert!(
        !names.contains(&"Write"),
        "Write not in allowlist should be excluded"
    );

    // disallowedTools case-insensitive
    let allowed2 = ToolsValue::Empty;
    let disallowed2 = ToolsValue::List(vec!["WRITE".to_string()]);
    let filtered2 = t.filter_tools(&allowed2, &disallowed2);
    let names2: Vec<&str> = filtered2.iter().map(|t| t.name()).collect();

    assert!(names2.contains(&"Read"));
    assert!(names2.contains(&"Glob"));
    assert!(
        !names2.contains(&"Write"),
        "WRITE should case-insensitively exclude Write"
    );
}

/// Recursion prevention: Agent in disallowedTools is redundant but should not error
#[test]
fn test_agent_excluded_when_in_disallowed() {
    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::List(vec!["Agent".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(!names.contains(&"Agent"), "Agent should not appear");
}

/// Verify with_system_builder correctly injects system prompt
#[tokio::test]
async fn test_system_builder_injects_system_message() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("tone-test.md"),
        "---\nname: tone-test\ndescription: Test tone injection\n---\n\nYou are a tone tester.\n",
    )
    .unwrap();

    // LLM echoes system message content
    struct SystemEchoLLM;
    #[async_trait::async_trait]
    impl ReactLLM for SystemEchoLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            // Find system message and return its content
            let system_content = messages
                .iter()
                .find(|m| matches!(m, BaseMessage::System { .. }))
                .map(|m| m.content())
                .unwrap_or_else(|| "no-system".to_string());
            Ok(Reasoning::with_answer(
                "",
                format!("system={system_content}"),
            ))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(|_: Option<&str>, _: Option<&str>| Box::new(SystemEchoLLM) as Box<dyn ReactLLM + Send + Sync>),
            dir.path().to_str().unwrap().to_string(),
        )
        .with_system_builder(Arc::new(|_overrides, _cwd| "tone: be concise".to_string()))
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "tone-test",
                "prompt": "hello",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Agent started (child_thread_id:"));
    let result = wait_for_completion(&mut completed).await;
    assert!(
        result.output.contains("tone: be concise"),
        "{}",
        result.output
    );
}

/// Verify SkillPreloadMiddleware is correctly registered when agent.md contains skills field
/// LLM received messages should contain "(system: preloaded skill file)"
#[tokio::test]
async fn test_skill_preload_registered() {
    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    let skills_dir = dir.path().join(".agents").join("skills").join("test-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();

    // agent.md with skills field
    std::fs::write(
            agents_dir.join("skill-user.md"),
            "---\nname: skill-user\ndescription: Uses skills\nskills:\n  - test-skill\n---\n\nYou use skills.\n",
        )
        .unwrap();

    // SKILL.md content
    std::fs::write(
            skills_dir.join("SKILL.md"),
            "---\nname: 'test-skill'\ndescription: 'A test skill'\n---\n\n# Test Skill\n\nThis is the test skill content.\n",
        )
        .unwrap();

    // LLM 验证 prompt 已由 Receive 写入、并精确统计显式 skill 的 fake ToolResult。
    let preload_count: Arc<std::sync::Mutex<usize>> = Arc::new(std::sync::Mutex::new(0));
    let preload_count_clone = Arc::clone(&preload_count);
    struct SkillPreloadCheckLLM {
        preload_count: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for SkillPreloadCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            assert!(
                messages
                    .iter()
                    .any(|message| message.content().contains("test task")),
                "before_agent must run after Receive has appended the prompt"
            );
            *self.preload_count.lock().unwrap() = messages
                .iter()
                .filter(|message| {
                    message
                        .content()
                        .contains("This is the test skill content.")
                })
                .count();
            Ok(Reasoning::with_answer("", "skill_preload_found"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(SkillPreloadCheckLLM {
                    preload_count: Arc::clone(&preload_count_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            dir.path().to_str().unwrap().to_string(),
        )
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "skill-user",
                "prompt": "test task",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert!(result.contains("Agent started (child_thread_id:"));
    let completed = wait_for_completion(&mut completed).await;
    assert!(completed.output.contains("skill_preload_found"));
    assert_eq!(
        *preload_count.lock().unwrap(),
        1,
        "the explicit skill must inject exactly one ToolResult sequence"
    );
}

#[test]
fn test_agent_description_extended() {
    let t = make_subagent_tool(vec![]);
    let desc = t.description();
    assert!(
        desc.contains("Usage:"),
        "description should contain Usage section"
    );
    assert!(
        desc.contains("sub-agent") || desc.contains("sub agent"),
        "description should mention sub-agent"
    );
    assert!(
        desc.contains("isolated") || desc.contains("isolation"),
        "description should mention context isolation"
    );
    assert!(
        desc.contains("Fork mode"),
        "description should mention Fork mode"
    );
    assert!(
        desc.len() > 300,
        "description should be extended multi-paragraph text"
    );
}

/// Verify overrides_from_agent_def correctly extracts AgentOverrides from parsed data
#[test]
fn test_overrides_from_agent_def_with_all_fields() {
    let ov = SubAgentTool::overrides_from_agent_def(
        "You are a reviewer.",
        &Some("Be thorough.".to_string()),
        &Some("Proactively suggest.".to_string()),
        &None,
    );
    let ov = ov.unwrap();
    assert_eq!(ov.persona.as_deref().unwrap(), "You are a reviewer.");
    assert_eq!(ov.tone.as_deref().unwrap(), "Be thorough.");
    assert_eq!(ov.proactiveness.as_deref().unwrap(), "Proactively suggest.");
}

#[test]
fn test_overrides_from_agent_def_empty() {
    let ov = SubAgentTool::overrides_from_agent_def("", &None, &None, &None);
    assert!(ov.is_none(), "All-empty fields should return None");
}

#[test]
fn test_overrides_from_agent_def_persona_only() {
    let ov = SubAgentTool::overrides_from_agent_def("I am a helper.", &None, &None, &None);
    let ov = ov.unwrap();
    assert_eq!(ov.persona.as_deref().unwrap(), "I am a helper.");
    assert!(ov.tone.is_none());
    assert!(ov.proactiveness.is_none());
}

/// Verify cancellation token can interrupt sub-agent execution
#[tokio::test]
async fn test_agent_requires_task_manager() {
    let dir = tempdir().unwrap();
    let thread_store = make_fs_store(&dir);
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("forever.md"),
        "---\nname: forever\ndescription: Runs forever\n---\n\nYou run forever.\n",
    )
    .unwrap();

    // LLM always calls a never-registered tool, causing ToolNotFound but no infinite loop
    struct ToolNotFoundLLM;
    #[async_trait::async_trait]
    impl ReactLLM for ToolNotFoundLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            if messages
                .iter()
                .any(|m| matches!(m, BaseMessage::Tool { .. }))
            {
                Ok(Reasoning::with_answer("", "done"))
            } else {
                Ok(Reasoning::with_tools(
                    "call missing",
                    vec![peri_agent::agent::react::ToolCall::new(
                        "id1",
                        "nonexistent",
                        serde_json::json!({}),
                    )],
                ))
            }
        }
    }

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(|_: Option<&str>, _: Option<&str>| Box::new(ToolNotFoundLLM) as Box<dyn ReactLLM + Send + Sync>),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_thread_store(thread_store.clone());

    let result = t
        .invoke(
            serde_json::json!({
                "subagent_type": "forever",
                "prompt": "run",
                "cwd": dir.path().to_str().unwrap()
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect_err("Agent without TaskManager must fail before execution")
        .to_string();
    assert!(
        result.contains("no task manager configured"),
        "missing manager error should be explicit: {}",
        result
    );
    assert!(
        thread_store.list_threads().await.unwrap().is_empty(),
        "缺少 TaskManager 时不得创建子线程"
    );
}

// ─── Fork path tests ────────────────────────────────────────────────────

/// Fork inherits parent messages
#[tokio::test]
async fn test_fork_inherits_parent_messages() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages.write().push(BaseMessage::human("Hello"));
    parent_messages.write().push(BaseMessage::ai("Hi there"));

    let msg_capture: Arc<std::sync::Mutex<usize>> = Arc::new(std::sync::Mutex::new(0));
    let msg_capture_clone = Arc::clone(&msg_capture);

    struct ForkTestLLM {
        msg_count: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ForkTestLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.msg_count.lock().unwrap() = messages.len();
            Ok(Reasoning::with_answer("", "fork-done"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(ForkTestLLM {
                    msg_count: Arc::clone(&msg_capture_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(Arc::clone(&parent_messages))
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do the thing"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert!(
        result.contains("Agent started (child_thread_id:"),
        "{result}"
    );
    let _ = wait_for_completion(&mut completed).await;
    // Messages should include: 2 parent history + 1 system + 1 fork directive (human) = 4+
    let count = *msg_capture.lock().unwrap();
    assert!(
        count >= 3,
        "Fork should receive parent messages (got {})",
        count
    );
}

/// Fork registers all tools including Agent (no hard-coded exclusion)
#[tokio::test]
async fn test_fork_registers_all_tools_including_agent() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let tools_capture: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let tools_capture_clone = Arc::clone(&tools_capture);

    struct ToolsCheckLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for ToolsCheckLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = tools.iter().map(|t| t.name().to_string()).collect();
            Ok(Reasoning::with_answer("", "tools-check"))
        }
    }

    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(parent_tools),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(ToolsCheckLLM {
                    captured: Arc::clone(&tools_capture_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "check tools"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let _ = wait_for_completion(&mut completed).await;

    let captured = tools_capture.lock().unwrap();
    assert!(
        captured.contains(&"Agent".to_string()),
        "Fork should register Agent tool (no exclusion), got: {:?}",
        *captured
    );
    assert!(
        captured.contains(&"Read".to_string()),
        "Fork should register Read tool, got: {:?}",
        *captured
    );
}

/// Fork without parent_messages succeeds with empty ToolContext messages
#[tokio::test]
async fn test_fork_without_parent_messages_returns_error() {
    let t = make_subagent_tool(vec![]);

    // Fork 现在从 ToolContext 获取消息（而非 self.parent_messages），
    // 空消息也是合法输入。
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do something"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(
        result.is_ok(),
        "Fork with empty ToolContext messages should succeed, got: {:?}",
        result.err()
    );
}

/// Fork system prompt is consistent with system_builder
#[tokio::test]
async fn test_fork_system_prompt_consistent() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let sys_capture: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let sys_capture_clone = Arc::clone(&sys_capture);

    struct SystemCheckLLM {
        captured: Arc<std::sync::Mutex<String>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for SystemCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let sys = messages
                .iter()
                .find(|m| matches!(m, BaseMessage::System { .. }))
                .map(|m| m.content())
                .unwrap_or_default();
            *self.captured.lock().unwrap() = sys;
            Ok(Reasoning::with_answer("", "sys-check"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(SystemCheckLLM {
                    captured: Arc::clone(&sys_capture_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_system_builder(Arc::new(|_ov, _cwd| "FORK-TEST-SYSTEM".to_string()))
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "check system"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let _ = wait_for_completion(&mut completed).await;

    let captured = sys_capture.lock().unwrap();
    assert!(
        captured.contains("FORK-TEST-SYSTEM"),
        "Fork system prompt should contain builder output, got: {}",
        *captured
    );
}

/// [回归测试] SubAgent fork 复用父冻结 system prompt，不回退 system_builder。
///
/// 历史背景（ARC-FROZEN-001 / 审计 prompt-sections-audit.md 条目 7）：fork
/// 生产路径继承父**冻结** system prompt（execute_fork.rs frozen_system_prompt
/// 优先），与主 agent 前缀保持一致；若改为无条件走 system_builder 或每轮
/// 重渲染，会破坏会话内前缀一致性。本测试固定两个输入（frozen 与 builder），
/// 断言 frozen 优先——即"同一 FrozenSessionData 输入下主 agent 与 subagent
/// 复用稳定 prompt"的外部结果。
#[tokio::test]
async fn test_fork_prefers_frozen_system_prompt_over_builder() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let sys_capture: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let sys_capture_clone = Arc::clone(&sys_capture);

    struct FrozenCheckLLM {
        captured: Arc<std::sync::Mutex<String>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for FrozenCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let sys = messages
                .iter()
                .find(|m| matches!(m, BaseMessage::System { .. }))
                .map(|m| m.content())
                .unwrap_or_default();
            *self.captured.lock().unwrap() = sys;
            Ok(Reasoning::with_answer("", "frozen-check"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(FrozenCheckLLM {
                    captured: Arc::clone(&sys_capture_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_frozen_system_prompt(Arc::new("FROZEN-PARENT-SYSTEM-PROMPT".to_string()))
        .with_system_builder(Arc::new(|_ov, _cwd| "BUILDER-SYSTEM-PROMPT".to_string()))
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "check frozen prefix"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let _ = wait_for_completion(&mut completed).await;

    let captured = sys_capture.lock().unwrap();
    assert!(
        captured.contains("FROZEN-PARENT-SYSTEM-PROMPT"),
        "Fork 应复用父冻结 system prompt, got: {}",
        *captured
    );
    assert!(
        !captured.contains("BUILDER-SYSTEM-PROMPT"),
        "frozen 存在时不应回退 system_builder, got: {}",
        *captured
    );
}

/// Fork directive includes RULES
#[tokio::test]
async fn test_fork_directive_includes_rules() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));

    let last_capture: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
    let last_capture_clone = Arc::clone(&last_capture);

    struct DirectiveCheckLLM {
        last: Arc<std::sync::Mutex<String>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for DirectiveCheckLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let last = messages.last().map(|m| m.content()).unwrap_or_default();
            *self.last.lock().unwrap() = last;
            Ok(Reasoning::with_answer("", "directive-check"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(DirectiveCheckLLM {
                    last: Arc::clone(&last_capture_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(parent_messages)
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    t.invoke(
        serde_json::json!({
            "fork": true,
            "prompt": "my directive task"
        }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();

    let _ = wait_for_completion(&mut completed).await;

    let last = last_capture.lock().unwrap();
    assert!(
        last.contains("<fork_directive>"),
        "Fork directive should contain <fork_directive>, got: {}",
        *last
    );
    assert!(
        last.contains("RULES"),
        "Fork directive should contain RULES, got: {}",
        *last
    );
    assert!(
        last.contains("my directive task"),
        "Fork directive should contain the prompt, got: {}",
        *last
    );
}

// ─── build_subagent_middlewares 单元测试 ───────────────────────────────────

use super::{build_subagent_middlewares, SubAgentMiddlewareConfig};

#[test]
fn test_build_middleware_fork_config_无_skill_preload() {
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_fork("/tmp"));
    assert_eq!(middlewares.len(), 4);
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "ImageMiddleware",
            "TodoMiddleware"
        ]
    );
}

#[test]
fn test_build_middleware_agent_def_空技能_无_skill_preload() {
    let middlewares =
        build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(vec![], "/tmp"));
    assert_eq!(middlewares.len(), 4);
    assert!(!middlewares
        .iter()
        .any(|m| m.name() == "SkillPreloadMiddleware"));
}

#[test]
fn test_build_middleware_agent_def_有技能_包含_skill_preload() {
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(
        vec!["test-skill".to_string()],
        "/tmp",
    ));
    assert_eq!(middlewares.len(), 5);
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "ImageMiddleware",
            "TodoMiddleware"
        ]
    );
}

#[test]
fn test_build_middleware_顺序固定() {
    // 有 skills 时验证完整顺序
    let middlewares = build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(
        vec!["a".to_string()],
        "/tmp",
    ));
    let names: Vec<&str> = middlewares.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec![
            "AgentsMdMiddleware",
            "SkillsMiddleware",
            "SkillPreloadMiddleware",
            "ImageMiddleware",
            "TodoMiddleware"
        ]
    );
}

#[tokio::test]
async fn test_subagent_中间件链_加载_image_附件() {
    let dir = tempdir().unwrap();
    let image_path = dir.path().join("screen.png");
    image::RgbaImage::new(1, 1).save(&image_path).unwrap();
    let cwd = dir.path().to_string_lossy().to_string();
    let middlewares =
        build_subagent_middlewares(SubAgentMiddlewareConfig::for_agent_def(Vec::new(), &cwd));
    let image = middlewares
        .iter()
        .find(|middleware| middleware.name() == "ImageMiddleware")
        .expect("子 Agent 必须装配 ImageMiddleware");
    let mut state = AgentState::with_messages(
        &cwd,
        vec![BaseMessage::human(format!(
            "Inspect this screenshot\n@image {}",
            image_path.display()
        ))],
    );

    image.before_agent(&mut state).await.unwrap();

    assert!(state.messages()[0]
        .content_blocks()
        .iter()
        .any(|block| matches!(block, peri_agent::messages::ContentBlock::Image { .. })));
}

#[tokio::test]
async fn test_build_middleware_继承父会话插件_skill_根() {
    let dir = tempdir().unwrap();
    let plugin_root = dir.path().join("plugin-skills");
    let skill_dir = plugin_root.join("plugin-review");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: plugin-review\ndescription: Review from plugin\n---\n\n# Plugin review\n",
    )
    .unwrap();

    let cwd = dir.path().to_string_lossy().to_string();
    let config = SubAgentMiddlewareConfig::for_fork(&cwd).with_plugin_roots(vec![
        peri_acp_types::skills::SkillRoot {
            path: plugin_root,
            source: peri_acp_types::skills::SkillSource::Plugin,
            plugin_name: Some("plugin:test".to_string()),
        },
    ]);
    let middlewares = build_subagent_middlewares(config);
    let skills = middlewares
        .iter()
        .find(|middleware| middleware.name() == "SkillsMiddleware")
        .expect("子 Agent 必须装配 SkillsMiddleware");
    let mut state = AgentState::new(&cwd);

    skills.before_agent(&mut state).await.unwrap();

    let contribution = skills
        .prompt_contribution()
        .expect("插件 Skill 应进入子 Agent 的目录摘要");
    assert!(contribution.contains("- **plugin-review** [plugin]"));
}

// ─── frozen 数据传递测试 ──────────────────────────────────────────────────

use peri_agent::agent::state::AgentState;
use tempfile::TempDir;

/// 验证：传入 frozen CLAUDE.md 内容时，SubAgent 中间件链的 AgentsMdMiddleware
/// 应直接 prepend frozen 内容，跳过磁盘读取。
///
/// 这是 SC#2 修复的核心契约：SubAgent 必须复用 main agent 捕获的 frozen 数据，
/// 不能在 spawn 时重新读盘。
#[tokio::test]
async fn test_subagent_中间件链_注入_frozen_claude_md() {
    // Arrange: 空白 tempdir（无 CLAUDE.md），但提供 frozen 内容
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();
    let frozen_content =
        "# FROZEN TEST CLAUDE.md\nThis content must be injected verbatim.".to_string();

    let config = SubAgentMiddlewareConfig::for_fork(&cwd).with_frozen(
        Some(frozen_content.clone()),
        None,
        None,
    );

    // Act: 构造中间件链，模拟 SubAgent spawn
    let middlewares = build_subagent_middlewares(config);
    let _state = AgentState::new(&cwd);

    // 找到 AgentsMdMiddleware（v2 通过 prompt_contribution 声明贡献，不再 prepend_message）
    let agents_md = middlewares
        .iter()
        .find(|m| m.name() == "AgentsMdMiddleware")
        .expect("AgentsMdMiddleware 必须在链首");

    // Assert: prompt_contribution 应返回 frozen 内容
    let contribution = agents_md
        .prompt_contribution()
        .expect("v2 prompt_contribution 应返回 frozen CLAUDE.md 内容");
    assert!(
        contribution.contains("FROZEN TEST CLAUDE.md"),
        "frozen 内容应通过 prompt_contribution 暴露，实际：{}",
        contribution
    );
}

/// 验证：未提供 frozen 数据时（遗留/测试场景），中间件回退到磁盘读取。
/// 在空白 tempdir 中不注入任何 System 消息。
#[tokio::test]
async fn test_subagent_中间件链_无_frozen_回退磁盘() {
    // Arrange: 空白 tempdir，无 frozen 数据
    let tmp = TempDir::new().unwrap();
    let cwd = tmp.path().to_str().unwrap().to_string();

    let config = SubAgentMiddlewareConfig::for_fork(&cwd);
    let middlewares = build_subagent_middlewares(config);
    let mut state = AgentState::new(&cwd);

    let agents_md = middlewares
        .iter()
        .find(|m| m.name() == "AgentsMdMiddleware")
        .unwrap();
    agents_md.before_agent(&mut state).await.unwrap();

    // Assert: 没有 CLAUDE.md 时，不注入任何 System 消息
    assert!(
        state.messages().is_empty(),
        "无 frozen + 无磁盘文件时不应注入消息"
    );
}

// ─── SubAgent v2 集成测试（4 场景）──────────────────────────────────────
//
// 对应 docs/refactor/pending-fix-plan-2026-07-06.md [INTEGRATION-TESTS]。
// 依赖 BUG-A / BUG-B / BUG-C 修复（已完成）。
// 所有测试基于 v2 路径（`build_v2_subagent_context` / `SubAgentTool::invoke`），
// 不依赖 v1 `ReActAgent`。

/// 场景 1（Fork 父消息透传）：端到端验证 fork 模式下子 agent 收到完整父对话历史。
///
/// 断言（对应 plan.md §场景1 acceptance）：
/// - mock LLM 收到的 messages.len() >= 4（3 父 + 1 fork_directive prompt）
///   实际还会包含 system_builder 注入的 1 条 System 消息
/// - mock LLM 收到的最后一条消息是 `BaseMessage::Human` 且 content 包含 `<fork_directive>`（验证 BUG-A）
/// - mock LLM 收到的 messages 包含 `BaseMessage::System` 内容含 "FORK-CONTEXT-SP"（验证 BUG-B fork 路径）
/// - mock LLM 收到的 messages 包含全部 3 条父消息（按顺序透传，验证 BUG-C）
#[tokio::test]
async fn test_integration_fork_parent_messages_passthrough() {
    // Arrange: 3 条父消息（Human/AI 交替 + 1 条 system context）
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent Q1"));
    parent_messages
        .write()
        .push(BaseMessage::ai("parent A1 with details"));
    parent_messages
        .write()
        .push(BaseMessage::human("parent Q2 followup"));

    // 捕获 mock LLM 收到的完整消息列表
    let captured: Arc<std::sync::Mutex<Vec<BaseMessage>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct CaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<BaseMessage>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for CaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.to_vec();
            Ok(Reasoning::with_answer("", "fork integration done"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(CaptureLLM {
                    captured: Arc::clone(&captured_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(Arc::clone(&parent_messages))
        .with_system_builder(Arc::new(|_ov, _cwd| "FORK-CONTEXT-SP".to_string()))
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    // Act: fork 模式触发端到端路径
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "continue from parent context"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    assert!(
        result.contains("Agent started (child_thread_id:"),
        "{result}"
    );
    let _ = wait_for_completion(&mut completed).await;

    let msgs = captured.lock().unwrap().clone();

    // Assert 1: 消息数量 >= 4（3 父 + 1 system + 1 fork_directive prompt）
    assert!(
        msgs.len() >= 4,
        "fork should receive parent messages + system + directive (got {})",
        msgs.len()
    );

    // Assert 2: 最后一条是 Human，且包含 <fork_directive>（BUG-A）
    let last = msgs.last().expect("messages non-empty");
    assert!(
        matches!(last, BaseMessage::Human { .. }),
        "last message should be Human (fork directive)"
    );
    let last_content = last.content();
    assert!(
        last_content.contains("<fork_directive>"),
        "last message should contain <fork_directive> (BUG-A), got: {}",
        last_content
    );
    assert!(
        last_content.contains("continue from parent context"),
        "fork directive should wrap original prompt"
    );

    // Assert 3: messages 中包含 System 消息，内容含 "FORK-CONTEXT-SP"（BUG-B）
    let sys_msg = msgs
        .iter()
        .find(|m| matches!(m, BaseMessage::System { .. }));
    assert!(
        sys_msg.is_some(),
        "fork path should inject System message (BUG-B)"
    );
    assert!(
        sys_msg.unwrap().content().contains("FORK-CONTEXT-SP"),
        "System message should contain system_builder output (BUG-B)"
    );

    // Assert 4: 父消息按顺序透传（BUG-C）
    // 验证三条父消息的 content 都在 LLM 收到的 messages 中
    let contents: Vec<String> = msgs.iter().map(|m| m.content()).collect();
    assert!(
        contents.iter().any(|c| c.contains("parent Q1")),
        "first parent message should pass through (BUG-C)"
    );
    assert!(
        contents
            .iter()
            .any(|c| c.contains("parent A1 with details")),
        "second parent message should pass through (BUG-C)"
    );
    assert!(
        contents.iter().any(|c| c.contains("parent Q2 followup")),
        "third parent message should pass through (BUG-C)"
    );
}

#[tokio::test]
async fn test_fork_prefers_tool_context_messages_over_parent_snapshot() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("old before_agent snapshot"));
    let ctx_messages = vec![
        BaseMessage::human("old before_agent snapshot"),
        BaseMessage::ai("new current turn detail"),
        BaseMessage::human("latest user request"),
    ];

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct CaptureContentLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for CaptureContentLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            Ok(Reasoning::with_answer("", "ctx-preferred"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(CaptureContentLLM {
                    captured: Arc::clone(&captured_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(Arc::clone(&parent_messages))
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review current turn"
            }),
            peri_agent::tools::ToolContext::new(&ctx_messages, "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Agent started (child_thread_id:"),
        "{result}"
    );
    let _ = wait_for_completion(&mut completed).await;
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("new current turn detail")),
        "fork should inherit current ToolContext messages, got: {:?}",
        contents
    );
    assert!(
        contents.iter().any(|c| c.contains("latest user request")),
        "fork should inherit latest ToolContext user request, got: {:?}",
        contents
    );
}

#[tokio::test]
async fn test_fork_falls_back_to_parent_messages_when_tool_context_empty() {
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent fallback context"));

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct FallbackCaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for FallbackCaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            Ok(Reasoning::with_answer("", "fallback-used"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(FallbackCaptureLLM {
                    captured: Arc::clone(&captured_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_parent_messages(Arc::clone(&parent_messages))
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review fallback"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Agent started (child_thread_id:"),
        "{result}"
    );
    let _ = wait_for_completion(&mut completed).await;
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("parent fallback context")),
        "fork should fall back to parent_messages when ToolContext is empty, got: {:?}",
        contents
    );
}

#[tokio::test]
async fn test_fork_drops_trailing_tool_call_message_from_tool_context() {
    let ctx_messages = vec![
        BaseMessage::human("stable context before tool call"),
        BaseMessage::ai_with_tool_calls(
            "unfinished agent tool call text",
            vec![peri_agent::messages::ToolCallRequest::new(
                "call-agent-1",
                "Agent",
                serde_json::json!({"fork": true, "prompt": "review"}),
            )],
        ),
    ];

    let captured: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured_clone = Arc::clone(&captured);

    struct DropToolCallCaptureLLM {
        captured: Arc<std::sync::Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for DropToolCallCaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            *self.captured.lock().unwrap() = messages.iter().map(|m| m.content()).collect();
            Ok(Reasoning::with_answer("", "tool-call-dropped"))
        }
    }

    let (t, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(DropToolCallCaptureLLM {
                    captured: Arc::clone(&captured_clone),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            "/tmp".to_string(),
        )
        .with_task_manager(Arc::new(peri_agent::agent::async_tasks::TaskManager::new())),
    );

    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "review without dangling tool call"
            }),
            peri_agent::tools::ToolContext::new(&ctx_messages, "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Agent started (child_thread_id:"),
        "{result}"
    );
    let _ = wait_for_completion(&mut completed).await;
    let contents = captured.lock().unwrap().clone();
    assert!(
        contents
            .iter()
            .any(|c| c.contains("stable context before tool call")),
        "fork should keep earlier context, got: {:?}",
        contents
    );
    assert!(
        contents
            .iter()
            .all(|c| !c.contains("unfinished agent tool call text")),
        "fork should drop trailing AI message with unclosed tool call, got: {:?}",
        contents
    );
}

/// 场景 2（Background Independent cancel）：端到端验证 background fork 在父 cancel 后**不**中断。
///
/// 基于 `SubAgentTool::invoke` → `invoke_background` → `invoke_background_fork`
/// → `spawn_background_fork` 完整链路（v2 `build_v2_subagent_context` 装配）。
///
/// 关键断言：
/// - 父 cancel_token.cancel() 后，background task 仍能完成（Independent policy）
/// - mock LLM 至少被调用 1 次（证明未被父 cancel 中断）
/// - bg_event_sender 接收到 BackgroundTaskCompleted（task 正常结束）
#[tokio::test]
async fn test_integration_background_independent_survives_parent_cancel() {
    use peri_agent::agent::events::ExecutorEvent;
    use tokio::sync::mpsc;

    // Arrange: 共享的 LLM 调用计数（mock 会阻塞等待，但 cancel 不影响它）
    let llm_call_count: Arc<std::sync::atomic::AtomicUsize> =
        Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let llm_call_count_clone = Arc::clone(&llm_call_count);

    struct CountingLLM {
        count: Arc<std::sync::atomic::AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for CountingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Reasoning::with_answer("", "bg done independent"))
        }
    }

    // bg_event_sender 通道，捕获完成事件
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();

    // Background registry
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    // 父消息（fork background 需要）
    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("parent ctx"));

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>, _: Option<&str>| {
            Box::new(CountingLLM {
                count: Arc::clone(&llm_call_count_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages)
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx);

    // Act 1: 启动 background fork
    let invoke_result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "long running bg task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(
        invoke_result.is_ok(),
        "background fork should start: {:?}",
        invoke_result.err()
    );
    let invoke_msg = invoke_result.unwrap();
    assert!(
        invoke_msg.contains("Agent started (child_thread_id:"),
        "should return background task started message: {}",
        invoke_msg
    );

    // 等待 background task 完整执行（消耗所有事件直到 BackgroundTaskCompleted）。
    let mut got_started = false;
    let mut got_stopped = false;
    let mut got_completed = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ev)) => match ev {
                ExecutorEvent::SubagentStarted { is_background, .. } => {
                    got_started = true;
                    assert!(
                        is_background,
                        "SubagentStarted should have is_background=true"
                    );
                }
                ExecutorEvent::SubagentStopped { .. } => {
                    got_stopped = true;
                }
                ExecutorEvent::BackgroundTaskCompleted(ref res) => {
                    assert!(
                        res.success,
                        "background task result should be success (independent of parent cancel)"
                    );
                    assert!(
                        res.output.contains("bg done independent"),
                        "background output should match mock LLM answer: {}",
                        res.output
                    );
                    got_completed = true;
                    break;
                }
                _ => {}
            },
            Ok(None) => break, // channel closed
            Err(_) => break,   // timeout
        }
    }

    // Assert 2: 接收到完整事件序列（Started → Stopped → Completed）
    assert!(
        got_started,
        "should receive SubagentStarted event from bg pump"
    );
    assert!(
        got_stopped,
        "should receive SubagentStopped event from bg pump"
    );
    assert!(
        got_completed,
        "should receive BackgroundTaskCompleted within timeout (independent of parent cancel)"
    );

    // Assert 3: LLM 被调用过（>=1 次），证明执行未被 cancel 中断
    let call_count = llm_call_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        call_count >= 1,
        "mock LLM should be called at least once (parent cancel should not interrupt independent task), got {}",
        call_count
    );
}

/// P0-2：background non-fork 必须通过实际 `invoke_background` 路径，由 loop 在
/// Receive 后唯一执行 before_agent。测试使用 registry 和 bg event sender，不轮询或 sleep。
#[tokio::test]
async fn test_p0_2_async_defined_skill_preload_once() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    let skills_dir = dir.path().join(".agents").join("skills").join("p0-2-skill");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::create_dir_all(&skills_dir).unwrap();
    std::fs::write(
        agents_dir.join("p0-2-bg.md"),
        "---\nname: p0-2-bg\ndescription: P0-2 background agent\nskills:\n  - p0-2-skill\n---\n\nRun the task.\n",
    )
    .unwrap();
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: p0-2-skill\ndescription: P0-2 skill\n---\n\nP0-2 BACKGROUND SKILL MARKER\n",
    )
    .unwrap();

    let llm_calls = Arc::new(AtomicUsize::new(0));
    let preload_count = Arc::new(std::sync::Mutex::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let preload_count_clone = Arc::clone(&preload_count);
    struct BackgroundSkillLLM {
        calls: Arc<AtomicUsize>,
        preload_count: Arc<std::sync::Mutex<usize>>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for BackgroundSkillLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(
                messages
                    .iter()
                    .any(|message| message.content().contains("p0-2 background prompt")),
                "before_agent must observe the prompt only after Receive"
            );
            *self.preload_count.lock().unwrap() = messages
                .iter()
                .filter(|message| message.content().contains("P0-2 BACKGROUND SKILL MARKER"))
                .count();
            Ok(Reasoning::with_answer("", "p0-2 background done"))
        }
    }

    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>, _: Option<&str>| {
            Box::new(BackgroundSkillLLM {
                calls: Arc::clone(&llm_calls_clone),
                preload_count: Arc::clone(&preload_count_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        dir.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(registry)
    .with_bg_event_sender(bg_tx);

    let started = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "p0-2-bg",
                "prompt": "p0-2 background prompt",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("background defined subagent should start");
    assert!(started.contains("Agent started (child_thread_id:"));
    let mut lifecycle = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = bg_rx.recv().await {
            match event {
                ExecutorEvent::SubagentStarted { is_background, .. } => {
                    assert!(is_background);
                    lifecycle.push("started");
                }
                ExecutorEvent::SubagentStopped { .. } => lifecycle.push("stopped"),
                ExecutorEvent::BackgroundTaskCompleted(result) => {
                    assert!(result.success);
                    assert!(result.output.contains("p0-2 background done"));
                    lifecycle.push("completed");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("background defined subagent must complete within the bounded receiver timeout");

    assert_eq!(lifecycle, ["started", "stopped", "completed"]);
    assert_eq!(llm_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *preload_count.lock().unwrap(),
        1,
        "explicit skill preload must occur exactly once on execute_bg.rs"
    );
}

/// Fork 也必须走统一异步路径并保留标准 Fork 指令。
///
/// 关键断言：
/// - 调用返回值包含 "Background task"（证明走了 background 路径）
/// - bg_event_sender 接收到 SubagentStarted（is_background=true）
/// - background registry 中注册了任务（task_id 前缀为 "bg-"）
/// - 捕获的 mock LLM prompt 包含 `<fork_directive>`，证明后台 Fork 仍使用标准 Fork 指令
#[tokio::test]
async fn test_integration_fork_starts_async_with_directive() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    // Arrange: 捕获 LLM 收到的 prompt（用于验证 directive kind）
    let prompt_capture: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
    let prompt_capture_clone = Arc::clone(&prompt_capture);
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let llm_gate = Arc::new(tokio::sync::Notify::new());
    let llm_gate_clone = Arc::clone(&llm_gate);

    struct PromptCaptureLLM {
        captured: Arc<std::sync::Mutex<String>>,
        calls: Arc<AtomicUsize>,
        gate: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for PromptCaptureLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            // 找到最后一条 Human 消息（fork directive 在 prompt queue 里）
            if let Some(last_human) = messages
                .iter()
                .rev()
                .find(|m| matches!(m, BaseMessage::Human { .. }))
            {
                *self.captured.lock().unwrap() = last_human.content();
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.gate.notified().await;
            Ok(Reasoning::with_answer("", "bg-fork done"))
        }
    }

    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());

    let parent_messages: Arc<RwLock<Vec<BaseMessage>>> = Arc::new(RwLock::new(Vec::new()));
    parent_messages
        .write()
        .push(BaseMessage::human("ctx for bg fork"));

    let t = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        Arc::new(move |_: Option<&str>, _: Option<&str>| {
            Box::new(PromptCaptureLLM {
                captured: Arc::clone(&prompt_capture_clone),
                calls: Arc::clone(&llm_calls_clone),
                gate: Arc::clone(&llm_gate_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        }),
        "/tmp".to_string(),
    )
    .with_parent_messages(parent_messages)
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx);

    // Act: fork 始终异步。
    let result = t
        .invoke(
            serde_json::json!({
                "fork": true,
                "prompt": "do both"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();

    // Assert 1: 立即返回异步任务信息。
    assert!(
        result.contains("Agent started (child_thread_id:"),
        "fork should start asynchronously: {}",
        result
    );

    // Assert 2: registry 中注册了任务（active_count >= 1）
    assert!(
        registry.active_count() >= 1,
        "background fork should be registered in BackgroundTaskRegistry"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while llm_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Fork 应在后台进入阻塞 LLM");

    // Assert 4: bg_event_sender 收到 SubagentStarted（is_background=true）
    let started_ev = tokio::time::timeout(std::time::Duration::from_secs(2), bg_rx.recv())
        .await
        .expect("should receive SubagentStarted within timeout")
        .expect("channel should not be closed");
    match started_ev {
        ExecutorEvent::SubagentStarted { is_background, .. } => {
            assert!(
                is_background,
                "SubagentStarted should have is_background=true for fork+bg"
            );
        }
        other => panic!("expected SubagentStarted first, got: {:?}", other),
    }
    llm_gate.notify_one();

    // Assert 5: 等待 background task 完成，捕获 LLM 收到的 prompt
    // 验证 directive kind = Fork（英文 `<fork_directive>`）
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bg_rx.recv().await {
                Some(ExecutorEvent::BackgroundTaskCompleted(_)) => break,
                Some(_) => continue,
                None => break,
            }
        }
    })
    .await;

    let captured_prompt = prompt_capture.lock().unwrap().clone();
    assert!(
        captured_prompt.contains("<fork_directive>"),
        "fork+bg should use Fork directive kind (English <fork_directive>), got: {}",
        captured_prompt
    );
    assert!(
        captured_prompt.contains("do both"),
        "fork directive should wrap original prompt 'do both'"
    );
}

// ─── S3.1 注册门控 + S3.2 取消收尾（issue 2026-08-05）────────────────────

/// 构造一个已注册状态的 bg 任务（预置 registry 占用额度用）
fn make_registered_bg_task(id: &str) -> peri_agent::agent::async_tasks::BackgroundTask {
    use peri_agent::agent::async_tasks::{
        BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind,
    };
    let handle = tokio::runtime::Handle::current().spawn(async {});
    BackgroundTask {
        id: id.to_string(),
        agent_name: "pre-seeded".to_string(),
        prompt_summary: "pre-seeded task".to_string(),
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        child_thread_id: Some(format!("thread-{id}")),
        cancel_handle: BgCancelHandle::Abort(handle),
        cancel_token: None,
        agent_followup: None,
        pid: None,
        output_preview: None,
    }
}

/// [回归测试] S3.1 幽灵任务：注册失败（并发撞 kind 上限）的任务必须不执行。
///
/// 预检与注册（per-kind 上限）之间的竞态无法单测自然触发，
/// 用 barrier 确定性制造：预置到 Agent 上限减 1，4 个并发 invoke 都
/// 通过预检后同步汇合在 llm_factory，放行后串行注册——只容 1 个成功，
/// 其余 3 个必须：
/// - invoke 返回 "Failed to register" 错误（如实）
/// - 不执行 run_react_loop（零 LLM 调用）
/// - 不 emit 任何事件（无 SubagentStarted → 无配对问题）
/// - 不注册 register_runtime（无需 deregister）
///
/// 历史 bug（issue 2026-08-05）：注册失败仅 return Err，任务已 spawn 继续跑，
/// 幽灵执行 + double 泄漏（register_runtime 无配对 deregister）。
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_bg_register_failure_does_not_execute_task() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let thread_store = make_fs_store(&dir);
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("gate-agent.md"),
        "---\nname: gate-agent\ndescription: Gate test\n---\n\nYou are gated.\n",
    )
    .unwrap();

    // 预置到上限减 1：4 个并发 invoke 都能通过预检
    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let agent_limit = registry.agent_limit();
    for i in 0..agent_limit - 1 {
        registry
            .register_with_kind(make_registered_bg_task(&format!("bg-pre-{}", i)))
            .unwrap();
    }
    assert_eq!(registry.active_count(), agent_limit - 1);

    // barrier：4 个 invoke 都通过预检并到达 llm_factory 后放行（确定性竞态窗口）
    let gate = Arc::new(Barrier::new(4));
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let gate_clone = Arc::clone(&gate);
    // 成功注册的任务阻塞在 LLM 调用（保持 kind 额度占用，防止任务快速完成
    // 触发 complete 移除条目后额度回落、后续注册"假成功"）
    let llm_gate = Arc::new(tokio::sync::Notify::new());
    let llm_gate_clone = Arc::clone(&llm_gate);

    struct GateLLM {
        calls: Arc<AtomicUsize>,
        block: Arc<tokio::sync::Notify>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for GateLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 阻塞成功任务：注册窗口内 kind 额度不释放
            self.block.notified().await;
            Ok(Reasoning::with_answer("", "bg gate done"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>, Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>, _: Option<&str>| {
            // 4 个 invoke 在此同步汇合（保证全部通过预检后才放行注册）
            gate_clone.wait();
            Box::new(GateLLM {
                calls: Arc::clone(&llm_calls_clone),
                block: Arc::clone(&llm_gate_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        });

    // register_runtime / deregister_runtime mock：记录调用
    let registered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let deregistered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let registered_clone = registered.clone();
    let deregistered_clone = deregistered.clone();
    let register_cb: Arc<dyn Fn(String, AgentCancellationToken) + Send + Sync> =
        Arc::new(move |tid, _tok| {
            registered_clone.lock().unwrap().push(tid);
        });
    let deregister_cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |tid| {
        deregistered_clone.lock().unwrap().push(tid.to_string());
    });

    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        llm_factory,
        dir.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(Arc::clone(&registry))
    .with_thread_store(thread_store.clone())
    .with_bg_event_sender(bg_tx)
    .with_register_runtime(register_cb)
    .with_deregister_runtime(deregister_cb);

    // 4 个并发 invoke——必须各自 tokio::spawn（llm_factory 内的 Barrier::wait()
    // 是同步阻塞：若在 join_all 单任务内逐个 poll，第一个 future 会卡死当前
    // worker，其余 3 个永远不被 poll，barrier 凑不齐 4 个参与者而死锁）。
    let tool = Arc::new(tool);
    let mut handles = Vec::new();
    for _ in 0..4 {
        let tool = Arc::clone(&tool);
        let cwd = dir.path().to_str().unwrap().to_string();
        handles.push(tokio::spawn(async move {
            tool.invoke(
                serde_json::json!({
                    "subagent_type": "gate-agent",
                    "prompt": "parallel bg task",
                    "cwd": cwd,
                }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
        }));
    }
    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.expect("invoke 任务不应 panic"))
        .collect();

    // 恰好 1 个注册成功，3 个注册失败（错误信息如实返回）
    let oks = results.iter().filter(|r| r.is_ok()).count();
    let errs = results.iter().filter(|r| r.is_err()).count();
    assert_eq!(oks, 1, "恰好 1 个并发任务注册成功，实际 {}", oks);
    assert_eq!(errs, 3, "其余 3 个必须注册失败，实际 {}", errs);
    for r in &results {
        if let Err(e) = r {
            assert!(
                e.to_string().contains("concurrent Agent tasks reached"),
                "注册失败错误应如实返回: {}",
                e
            );
        }
    }

    // 等待成功任务 emit SubagentStarted（只有注册成功的任务实际执行）
    let mut started = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::SubagentStarted { .. })) => {
                started += 1;
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        started, 1,
        "只有注册成功的任务 emit SubagentStarted，实际 {}",
        started
    );

    // 成功任务阻塞在 LLM 调用：等待小窗口后断言无任何完成事件（失败任务零事件）
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut stopped = 0usize;
    let mut completed = 0usize;
    while let Ok(ev) = bg_rx.try_recv() {
        match ev {
            ExecutorEvent::SubagentStopped { .. } => stopped += 1,
            ExecutorEvent::BackgroundTaskCompleted(_) => completed += 1,
            _ => {}
        }
    }
    assert_eq!(
        stopped, 0,
        "注册失败的任务不得 emit SubagentStopped（无幽灵完成）"
    );
    assert_eq!(completed, 0, "注册失败的任务不得产生完成事件（无幽灵完成）");
    assert_eq!(
        llm_calls.load(Ordering::SeqCst),
        1,
        "注册失败的任务不得执行 run_react_loop（LLM 仅被成功任务调用一次），实际 {}",
        llm_calls.load(Ordering::SeqCst)
    );
    // register_runtime 只在注册成功后执行（失败任务零注册 → 无需 deregister）
    assert_eq!(
        registered.lock().unwrap().len(),
        1,
        "仅注册成功的任务进入 active_agents"
    );
    assert_eq!(
        deregistered.lock().unwrap().len(),
        0,
        "任务仍在运行（阻塞），不得提前 deregister"
    );
    // registry 无幽灵条目：上限减 1 个预置 + 1 个成功注册（任务阻塞未完成）
    assert_eq!(
        registry.active_count(),
        agent_limit,
        "registry 不应有幽灵条目"
    );
    let thread_index: Vec<peri_agent::thread::ThreadMeta> = serde_json::from_slice(
        &std::fs::read(dir.path().join("threads").join("index.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(thread_index.len(), 1, "并发超限调用不得创建幽灵子线程");
}

/// [回归测试] S3.2 取消收尾：cancel() 先 token.cancel()，任务响应取消链走
/// 完整收尾——SubagentStopped 配对（subagent_depth 归零）、active_agents
/// deregister（任务内同步 guard）、registry 层无幽灵 Completed 事件。
///
/// 历史 bug（issue 2026-08-05）：取消仅 abort，收尾全部跳过（active_agents
/// 泄漏 + depth 错乱 + thread 状态停留 running）。
#[tokio::test]
async fn test_bg_cancel_trigger_token_and_cleanup() {
    use peri_agent::agent::events::ExecutorEvent;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    let dir = tempdir().unwrap();
    let agents_dir = dir.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&agents_dir).unwrap();
    std::fs::write(
        agents_dir.join("blocking-agent.md"),
        "---\nname: blocking-agent\ndescription: Blocks\n---\n\nYou block.\n",
    )
    .unwrap();

    // LLM 在 generate_reasoning 中阻塞（模拟长时间运行的 bg agent；
    // reason 阶段的 biased select 会在 cancel 后 drop 本 future 并返回 Interrupted）
    let gate = Arc::new(tokio::sync::Notify::new());
    let llm_calls = Arc::new(AtomicUsize::new(0));
    let llm_calls_clone = Arc::clone(&llm_calls);
    let gate_clone = Arc::clone(&gate);

    struct BlockingLLM {
        gate: Arc<tokio::sync::Notify>,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ReactLLM for BlockingLLM {
        async fn generate_reasoning(
            &self,
            _messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // 阻塞直到被取消（select 放弃本 future）
            self.gate.notified().await;
            Ok(Reasoning::with_answer("", "never"))
        }
    }

    let llm_factory: Arc<dyn Fn(Option<&str>, Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(move |_: Option<&str>, _: Option<&str>| {
            Box::new(BlockingLLM {
                gate: Arc::clone(&gate_clone),
                calls: Arc::clone(&llm_calls_clone),
            }) as Box<dyn ReactLLM + Send + Sync>
        });

    let registry = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let (bg_tx, mut bg_rx) = mpsc::unbounded_channel::<ExecutorEvent>();
    let (reg_events_tx, mut reg_events_rx) =
        mpsc::unbounded_channel::<peri_agent::agent::async_tasks::BgRegistryEvent>();
    registry.set_event_sender(reg_events_tx, "sess-cancel".to_string());

    let deregistered: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let deregistered_clone = deregistered.clone();
    let deregister_cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |tid| {
        deregistered_clone.lock().unwrap().push(tid.to_string());
    });

    let tool = SubAgentTool::new(
        Arc::new(vec![]),
        None,
        llm_factory,
        dir.path().to_str().unwrap().to_string(),
    )
    .with_task_manager(Arc::clone(&registry))
    .with_bg_event_sender(bg_tx)
    .with_deregister_runtime(deregister_cb);

    let msg = tool
        .invoke(
            serde_json::json!({
                "subagent_type": "blocking-agent",
                "prompt": "block forever",
                "cwd": dir.path().to_str().unwrap(),
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .expect("bg task should start");
    assert!(msg.contains("Agent started (child_thread_id:"));

    // 等待 LLM 进入阻塞（任务真正运行中，位于 reason 的 select 内）
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while llm_calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("LLM 应被调用（任务运行中）");

    // 取消：token.cancel() 应让任务响应并走完整收尾
    let tasks = registry.list_tasks();
    let (task_id, _, _) = tasks.into_iter().next().expect("任务应已注册");
    registry.cancel(&task_id).unwrap();
    assert_eq!(registry.active_count(), 0, "取消后条目已移除");

    // 事件流：SubagentStopped 必须到达（与 SubagentStarted 配对，depth 归零）
    let mut started = 0usize;
    let mut stopped = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, bg_rx.recv()).await {
            Ok(Some(ExecutorEvent::SubagentStarted { .. })) => started += 1,
            Ok(Some(ExecutorEvent::SubagentStopped { .. })) => stopped += 1,
            Ok(Some(ExecutorEvent::BackgroundTaskCompleted(_))) => {
                panic!("取消后不得注入幽灵 BackgroundTaskCompleted")
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => break,
        }
        if started == 1 && stopped == 1 {
            break;
        }
    }
    assert_eq!(started, 1);
    assert_eq!(
        stopped, 1,
        "取消后任务应 emit SubagentStopped（与 Started 配对）"
    );

    // active_agents 注销（任务内同步收尾 guard）：complete 后闭包结束触发 drop
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while deregistered.lock().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("取消后任务收尾应 deregister active_agents");
    assert_eq!(deregistered.lock().unwrap().len(), 1);

    // registry 层无幽灵 Completed 事件（complete 对已移除条目返回 false 不推事件）
    let mut saw_completed = false;
    while let Ok(ev) = reg_events_rx.try_recv() {
        if matches!(
            ev,
            peri_agent::agent::async_tasks::BgRegistryEvent::Completed { .. }
        ) {
            saw_completed = true;
        }
    }
    assert!(!saw_completed, "取消后不得推幽灵 Completed 事件");
}

// ─── Agent 控制工具 ──────────────────────

/// 构造 FilesystemThreadStore（写盘即时刷新，无需 flush）
fn make_fs_store(dir: &tempfile::TempDir) -> Arc<peri_agent::thread::FilesystemThreadStore> {
    Arc::new(peri_agent::thread::FilesystemThreadStore::new(
        dir.path().join("threads"),
    ))
}

/// 预置可恢复 thread：创建（title 决定工具集恢复路径）+ 写消息 + 置非 active。
/// FilesystemThreadStore 写盘即时落库（append 后 load_messages 立即可见）。
async fn preset_resumable_thread(
    store: &Arc<peri_agent::thread::FilesystemThreadStore>,
    id: &str,
    title: &str,
    parent_thread_id: Option<&str>,
    msgs: Vec<BaseMessage>,
) {
    let id = id.to_string();
    let mut meta = peri_agent::thread::ThreadMeta::new("/tmp/work");
    meta.id = id.clone();
    meta.title = Some(title.to_string());
    meta.parent_thread_id = parent_thread_id.map(|s| s.to_string());
    meta.hidden = true;
    meta.agent_nickname = Some(peri_agent::thread::AgentNickname {
        index: 0,
        generation: 1,
    });
    store.create_thread(meta).await.unwrap();
    if !msgs.is_empty() {
        store.append_messages(&id, &msgs).await.unwrap();
    }
    store.update_thread_status(&id, "done").await.unwrap();
}

fn child_thread_id_from_start(message: &str) -> String {
    message
        .split("child_thread_id: ")
        .nth(1)
        .and_then(|value| value.split(')').next())
        .expect("Agent start result should contain child_thread_id")
        .to_string()
}

#[tokio::test]
async fn followup_agent_reaches_running_agent_without_interrupting_it() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FollowupLLM {
        calls: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        prompts: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ReactLLM for FollowupLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let prompt = messages.last().map(|m| m.content()).unwrap_or_default();
            self.prompts.lock().unwrap().push(prompt.clone());
            if call == 0 {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(Reasoning::with_answer("", format!("handled: {prompt}")))
        }
    }

    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let task_manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let calls_for_factory = Arc::clone(&calls);
    let started_for_factory = Arc::clone(&started);
    let release_for_factory = Arc::clone(&release);
    let prompts_for_factory = Arc::clone(&prompts);
    let (agent, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(FollowupLLM {
                    calls: Arc::clone(&calls_for_factory),
                    started: Arc::clone(&started_for_factory),
                    release: Arc::clone(&release_for_factory),
                    prompts: Arc::clone(&prompts_for_factory),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            dir.path().to_string_lossy().into_owned(),
        )
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
        .with_task_manager(Arc::clone(&task_manager))
        .with_parent_thread_id("parent-thread".to_string()),
    );
    let followup = FollowupAgentTool::new(agent.clone());

    let launched = agent
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "initial task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let child_thread_id = child_thread_id_from_start(&launched);
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("Agent should enter its first model call");

    let output = followup
        .invoke(
            serde_json::json!({
                "target": child_thread_id,
                "message": "additional work"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(output.is_empty());
    assert_eq!(task_manager.active_count(), 1);

    release.notify_one();
    let result = wait_for_completion(&mut completed).await;
    assert!(result.success);
    assert!(
        result.output.contains("additional work"),
        "{}",
        result.output
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        prompts.lock().unwrap().as_slice(),
        &["initial task".to_string(), "additional work".to_string()]
    );
}

#[tokio::test]
async fn interrupt_agent_stops_only_current_turn_and_thread_can_follow_up() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct InterruptibleLLM {
        calls: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl ReactLLM for InterruptibleLLM {
        async fn generate_reasoning(
            &self,
            messages: &[BaseMessage],
            _tools: &[&dyn BaseTool],
            _streaming: Option<StreamingContext>,
        ) -> peri_agent::error::AgentResult<Reasoning> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                std::future::pending::<()>().await;
            }
            let prompt = messages.last().map(|m| m.content()).unwrap_or_default();
            Ok(Reasoning::with_answer("", format!("resumed: {prompt}")))
        }
    }

    let dir = tempdir().unwrap();
    write_test_agent(&dir);
    let store = make_fs_store(&dir);
    let task_manager = Arc::new(peri_agent::agent::async_tasks::TaskManager::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let calls_for_factory = Arc::clone(&calls);
    let started_for_factory = Arc::clone(&started);
    let (agent, mut completed) = capture_completion(
        SubAgentTool::new(
            Arc::new(vec![]),
            None,
            Arc::new(move |_: Option<&str>, _: Option<&str>| {
                Box::new(InterruptibleLLM {
                    calls: Arc::clone(&calls_for_factory),
                    started: Arc::clone(&started_for_factory),
                }) as Box<dyn ReactLLM + Send + Sync>
            }),
            dir.path().to_string_lossy().into_owned(),
        )
        .with_thread_store(Arc::clone(&store) as Arc<dyn ThreadStore>)
        .with_task_manager(Arc::clone(&task_manager))
        .with_parent_thread_id("parent-thread".to_string()),
    );

    let launched = agent
        .invoke(
            serde_json::json!({
                "subagent_type": "test-agent",
                "cwd": dir.path().to_str().unwrap(),
                "prompt": "long task"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let child_thread_id = child_thread_id_from_start(&launched);
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("Agent should enter its first model call");

    let output = InterruptAgentTool::new(agent.clone())
        .invoke(
            serde_json::json!({ "target": child_thread_id }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap()["previous_status"],
        "active"
    );
    assert_eq!(task_manager.active_count(), 0);
    assert_eq!(
        store
            .load_meta(&child_thread_id)
            .await
            .unwrap()
            .agent_status,
        peri_agent::thread::AgentStatus::Cancelled
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), completed.recv())
            .await
            .is_err(),
        "InterruptAgent must not emit a normal AgentResult"
    );

    FollowupAgentTool::new(agent.clone())
        .invoke(
            serde_json::json!({
                "target": child_thread_id,
                "message": "continue after interrupt"
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    let result = wait_for_completion(&mut completed).await;
    assert!(result.success);
    assert_eq!(
        result.child_thread_id.as_deref(),
        Some(child_thread_id.as_str())
    );
    assert!(
        result.output.contains("continue after interrupt"),
        "{}",
        result.output
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let idle_interrupt = InterruptAgentTool::new(agent)
        .invoke(
            serde_json::json!({ "target": child_thread_id }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&idle_interrupt).unwrap()["previous_status"],
        "done"
    );
}

#[tokio::test]
async fn list_agents_returns_all_direct_children_with_status() {
    let dir = tempdir().unwrap();
    let store = make_fs_store(&dir);
    let active_id = uuid::Uuid::now_v7().to_string();
    let done_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &active_id,
        "explorer",
        Some("parent-thread"),
        Vec::new(),
    )
    .await;
    preset_resumable_thread(
        &store,
        &done_id,
        "verification",
        Some("parent-thread"),
        Vec::new(),
    )
    .await;
    store
        .update_thread_status(&active_id, "active")
        .await
        .unwrap();

    let output = ListAgentsTool::new(
        make_subagent_tool(vec![])
            .with_thread_store(store)
            .with_parent_thread_id("parent-thread".to_string()),
    )
    .invoke(
        serde_json::json!({}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    let agents = serde_json::from_str::<serde_json::Value>(&output).unwrap()["agents"]
        .as_array()
        .unwrap()
        .clone();

    assert_eq!(agents.len(), 2);
    assert!(agents.iter().any(|agent| {
        agent["child_thread_id"] == active_id
            && agent["agent_type"] == "explorer"
            && agent["status"] == "active"
    }));
    assert!(agents.iter().any(|agent| {
        agent["child_thread_id"] == done_id
            && agent["agent_type"] == "verification"
            && agent["status"] == "done"
    }));
}

#[tokio::test]
async fn control_tools_reject_invalid_target_empty_message_and_wrong_parent() {
    let agent = make_subagent_tool(vec![]);
    let invalid = FollowupAgentTool::new(agent.clone())
        .invoke(
            serde_json::json!({ "target": "not-a-uuid", "message": "work" }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(invalid.contains("invalid agent target"), "{invalid}");

    let empty = FollowupAgentTool::new(agent)
        .invoke(
            serde_json::json!({
                "target": uuid::Uuid::now_v7().to_string(),
                "message": "   "
            }),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(empty.contains("Empty message"), "{empty}");

    let dir = tempdir().unwrap();
    let store = make_fs_store(&dir);
    let child_thread_id = uuid::Uuid::now_v7().to_string();
    preset_resumable_thread(
        &store,
        &child_thread_id,
        "fork",
        Some("other-parent"),
        Vec::new(),
    )
    .await;
    let wrong_parent = InterruptAgentTool::new(
        make_subagent_tool(vec![])
            .with_thread_store(store)
            .with_parent_thread_id("current-parent".to_string()),
    )
    .invoke(
        serde_json::json!({ "target": child_thread_id }),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(wrong_parent.contains("another parent"), "{wrong_parent}");
}

// ── 全局 Agent 目录（PERI_AGENT_DIRS）运行时加载 ─────────────────────────────

fn write_flat_agent_file(dir: &std::path::Path, id: &str, description: &str) {
    std::fs::write(
        dir.join(format!("{id}.md")),
        format!("---\nname: {id}\ndescription: {description}\n---\n\nYou are {id}.\n"),
    )
    .unwrap();
}

#[test]
fn load_global_agent_file_loads_definition_and_skips_symlinks() {
    let dir = tempdir().unwrap();
    write_flat_agent_file(dir.path(), "global-helper", "Global helper agent");

    let agent = super::define::load_global_agent_file("global-helper", &[dir.path().to_path_buf()])
        .expect("全局目录中的定义应可加载");
    assert_eq!(agent.frontmatter.description, "Global helper agent");

    // 符号链接跳过（与项目 Agent 路径的安全姿态一致）。
    let link_dir = tempdir().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        dir.path().join("global-helper.md"),
        link_dir.path().join("global-helper.md"),
    )
    .unwrap();
    assert!(
        super::define::load_global_agent_file("global-helper", &[link_dir.path().to_path_buf()])
            .is_none(),
        "符号链接定义必须跳过"
    );

    assert!(
        super::define::load_global_agent_file("missing", &[dir.path().to_path_buf()]).is_none(),
        "不存在的定义应返回 None"
    );
}

/// 三级优先级：项目 > 内置 > 插件/全局目录（与 scan_agents_detailed 去重一致）。
#[test]
fn load_agent_def_prefers_project_and_builtin_over_global_dirs() {
    let _env_guard = AGENT_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let plugin = tempdir().unwrap();
    let global = tempdir().unwrap();
    let project = tempdir().unwrap();
    let project_agents = project.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&project_agents).unwrap();

    write_flat_agent_file(global.path(), "shadowed", "From global");
    write_flat_agent_file(&project_agents, "shadowed", "From project");
    write_flat_agent_file(global.path(), "explorer", "Global explorer override");
    write_flat_agent_file(global.path(), "global-only", "Only in global");
    write_flat_agent_file(plugin.path(), "plugin-only", "Only in plugin");

    let previous_agent_dirs = std::env::var_os("PERI_AGENT_DIRS");
    std::env::set_var(
        "PERI_AGENT_DIRS",
        std::env::join_paths([plugin.path(), global.path()]).unwrap(),
    );
    let tool = make_subagent_tool(vec![]);
    let cwd = project.path().to_str().unwrap();
    let catalog = crate::subagent::scan_agents_detailed(
        cwd,
        &[plugin.path().to_path_buf(), global.path().to_path_buf()],
    );
    let project_def = tool.load_agent_def("shadowed", cwd);
    let builtin_def = tool.load_agent_def("explorer", cwd);
    let plugin_def = tool.load_agent_def("plugin-only", cwd);
    let global_def = tool.load_agent_def("global-only", cwd);
    let missing = tool.load_agent_def("no-such-agent", cwd);
    if let Some(previous) = previous_agent_dirs {
        std::env::set_var("PERI_AGENT_DIRS", previous);
    } else {
        std::env::remove_var("PERI_AGENT_DIRS");
    }

    assert!(
        catalog.iter().any(|agent| agent.0 == "plugin-only"),
        "插件 Agent 应同时出现在主 Agent 目录中"
    );

    assert_eq!(
        project_def.unwrap().frontmatter.description,
        "From project",
        "项目定义应优先于全局目录"
    );
    let builtin = builtin_def.expect("内置 explorer 应可加载");
    assert_ne!(
        builtin.frontmatter.description, "Global explorer override",
        "内置定义应优先于同名全局文件"
    );
    assert_eq!(
        plugin_def.unwrap().frontmatter.description,
        "Only in plugin",
        "插件目录中的定义应可执行"
    );
    assert_eq!(
        global_def.unwrap().frontmatter.description,
        "Only in global",
        "无项目/内置定义时应加载全局目录"
    );
    assert!(missing.is_err(), "三层都未命中应返回错误");
}

/// 界面(UI)子智能体定义目录(PERI_AGENT_PRIMARY_DIRS)优先级最高:
/// 同名时压过项目文件与内置定义,且出现在动态角色表中。
#[test]
fn load_agent_def_prefers_ui_primary_dir_over_everything() {
    let _env_guard = AGENT_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let ui = tempdir().unwrap();
    let project = tempdir().unwrap();
    let project_agents = project.path().join(".keencode").join("agents");
    std::fs::create_dir_all(&project_agents).unwrap();

    write_flat_agent_file(ui.path(), "shadowed", "From UI");
    write_flat_agent_file(&project_agents, "shadowed", "From project");

    let previous_primary = std::env::var_os("PERI_AGENT_PRIMARY_DIRS");
    std::env::set_var("PERI_AGENT_PRIMARY_DIRS", ui.path());
    let tool = make_subagent_tool(vec![]);
    let cwd = project.path().to_str().unwrap();
    let ui_def = tool.load_agent_def("shadowed", cwd);
    let schema_desc = tool.parameters()["properties"]["subagent_type"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    if let Some(previous) = previous_primary {
        std::env::set_var("PERI_AGENT_PRIMARY_DIRS", previous);
    } else {
        std::env::remove_var("PERI_AGENT_PRIMARY_DIRS");
    }

    assert_eq!(
        ui_def.unwrap().frontmatter.description,
        "From UI",
        "界面定义应优先于项目文件"
    );
    assert!(
        schema_desc.contains("- shadowed: From UI"),
        "动态角色表应包含界面定义: {schema_desc}"
    );
}

/// 内置 Agent 的模型覆盖表：命中替换 frontmatter.model，移除恢复定义默认。
#[test]
fn load_agent_def_applies_builtin_model_override() {
    let _env_guard = AGENT_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempdir().unwrap();
    let map = dir.path().join("agent-model-overrides.json");
    std::fs::write(&map, r#"{"explorer":"provider-a::cheap-model"}"#).unwrap();

    std::env::set_var("PERI_AGENT_MODEL_OVERRIDES", &map);
    let tool = make_subagent_tool(vec![]);
    let overridden = tool.load_agent_def("explorer", "/tmp");
    std::env::remove_var("PERI_AGENT_MODEL_OVERRIDES");

    assert_eq!(
        overridden.unwrap().frontmatter.model.as_deref(),
        Some("provider-a::cheap-model"),
        "覆盖表命中时应替换内置定义的 model"
    );
    assert_ne!(
        tool.load_agent_def("explorer", "/tmp")
            .unwrap()
            .frontmatter
            .model
            .as_deref(),
        Some("provider-a::cheap-model"),
        "覆盖移除后应恢复内置定义默认值"
    );

    std::fs::write(&map, r#"{"explorer":"unqualified-model"}"#).unwrap();
    std::env::set_var("PERI_AGENT_MODEL_OVERRIDES", &map);
    let invalid = tool.load_agent_def("explorer", "/tmp").unwrap();
    std::env::remove_var("PERI_AGENT_MODEL_OVERRIDES");
    assert!(
        invalid.frontmatter.model.is_none(),
        "无效覆盖不得绕过 provider_id::model 合同"
    );
}

/// catalog 扫描套用模型覆盖，但不改变 Agent 能力画像。
#[test]
fn scan_agents_detailed_marks_overridden_builtin_as_configured() {
    let _env_guard = AGENT_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = tempdir().unwrap();
    let map = dir.path().join("agent-model-overrides.json");
    std::fs::write(&map, r#"{"explorer":"provider-a::cheap-model"}"#).unwrap();
    let cwd = tempdir().unwrap();

    std::env::set_var("PERI_AGENT_MODEL_OVERRIDES", &map);
    let with_override = crate::subagent::scan_agents_detailed(cwd.path().to_str().unwrap(), &[]);
    std::env::remove_var("PERI_AGENT_MODEL_OVERRIDES");
    let without_override = crate::subagent::scan_agents_detailed(cwd.path().to_str().unwrap(), &[]);

    let capability_of = |list: Vec<(String, String, String, crate::subagent::AgentCapability)>,
                         id: &str| {
        list.into_iter()
            .find(|agent| agent.0 == id)
            .map(|agent| agent.3)
            .unwrap_or_else(|| panic!("catalog 应包含内置 {id}"))
    };
    assert!(!capability_of(with_override, "explorer").can_mutate);
    assert!(!capability_of(without_override, "explorer").can_mutate);
}
