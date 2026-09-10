//! 显式运行的提示词冒烟测试；仅发送合成材料，不改写配置文件。

use super::*;
use crate::providers::{CustomProvider, ProvidersListResult};
use keencode_agent::{
    AgentCommitEvent, AgentCommitSink, AgentToolRoundPreflight, AgentToolRoundPreflightError,
    AgentToolRoundReservation, NoopAgentCommitSink,
};
use keencode_tools::{EditTool, ReadTool};
use serde_json::json;

#[path = "live_messages_agent_tests.rs"]
mod messages;

#[path = "live_messages_runtime_tests.rs"]
mod messages_runtime;

/// 保留可诊断错误，移除已知认证值与端点；所有请求正文均为合成数据。
fn redacted_error(error: impl std::fmt::Display, secret: &str, endpoint: &str) -> String {
    error
        .to_string()
        .replace(secret, "[redacted]")
        .replace(endpoint, "[endpoint]")
        .chars()
        .take(1_024)
        .collect()
}

/// 复用 Runner 的权威用量出口，保留每次调用的数值，丢弃响应标识和正文。
#[derive(Default)]
struct UsageLog(Mutex<Vec<serde_json::Value>>);

impl AgentCommitSink for UsageLog {
    fn commit_model_round_usage(
        &self,
        usage: &ModelRoundUsage,
    ) -> Result<(), AgentCommitSinkError> {
        self.0
            .lock()
            .unwrap()
            .push(json!({"turn":usage.turn_id().as_str(),
            "attempt":usage.call_attempt(),"usage":usage.completion().usage,
            "elapsed_ms":usage.elapsed_millis(),"stop_reason":usage.completion().stop_reason}));
        Ok(())
    }

    fn preflight_tool_round(
        &self,
        round: &AgentToolRoundPreflight,
    ) -> Result<Box<dyn AgentToolRoundReservation>, AgentToolRoundPreflightError> {
        NoopAgentCommitSink.preflight_tool_round(round)
    }

    fn commit(&self, event: &AgentCommitEvent) -> Result<(), AgentCommitSinkError> {
        NoopAgentCommitSink.commit(event)
    }
}

/// 配置、模型与证据目录必须由执行者显式指定，普通 cargo test 不联网。
#[tokio::test]
#[ignore = "需要用户授权的 Provider 配置和 KEENCODE_PROMPT_TEST_* 环境变量"]
async fn live_prompt_scope_and_cache() {
    let required = |name: &str| std::env::var(name).expect("缺少显式真实测试参数");
    let config_path = required("KEENCODE_PROMPT_TEST_CONFIG");
    let provider_id = required("KEENCODE_PROMPT_TEST_PROVIDER");
    let model = required("KEENCODE_PROMPT_TEST_MODEL");
    let evidence = PathBuf::from(required("KEENCODE_PROMPT_TEST_EVIDENCE"));
    std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config_path).expect("无法读取测试配置"))
            .expect("测试配置不是合法 JSON");
    let selected = config["providers"]
        .as_array()
        .expect("缺少 providers")
        .iter()
        .find(|entry| entry["id"].as_str() == Some(&provider_id))
        .expect("测试 Provider 不在用户配置中");
    assert!(
        selected["models"]
            .as_array()
            .expect("缺少模型列表")
            .iter()
            .any(|entry| entry.as_str() == Some(&model)),
        "测试模型不在用户配置中"
    );
    let field = |name: &str| {
        selected[name]
            .as_str()
            .expect("Provider 字段无效")
            .to_owned()
    };
    let source_protocol = field("apiBackend");
    let protocol =
        std::env::var("KEENCODE_PROMPT_TEST_PROTOCOL").unwrap_or(source_protocol.clone());
    let endpoint = |protocol: &str| match protocol {
        "messages" => "/messages",
        "chat_completions" => "/chat/completions",
        "responses" => "/responses",
        _ => panic!("测试协议无效"),
    };
    // 只有执行者已获授权并显式指定时才切换协议；不探测其他协议，不改写配置。
    let original_url = field("baseUrl");
    let mut base_url = original_url.clone();
    if protocol != source_protocol {
        let base = original_url.trim_end_matches('#').trim_end_matches('/');
        let base = base
            .strip_suffix(endpoint(&source_protocol))
            .unwrap_or(base);
        base_url = format!(
            "{base}{}{}",
            endpoint(&protocol),
            if original_url.ends_with('#') { "#" } else { "" }
        );
    }
    let secret = field("apiKey");
    let provider = CustomProvider {
        chat_output_token_field: Default::default(),
        read_timeout_seconds: 300,
        id: provider_id.clone(),
        name: "Synthetic prompt probe".to_owned(),
        models: vec![model.clone()],
        base_url,
        api_backend: protocol.clone(),
        api_key: Some(secret.clone()),
        // 合成任务的测试窗口与输出限额，不回写用户配置或声称模型真实上限。
        context_windows: [(model.clone(), 65_536)].into_iter().collect(),
        max_output_tokens: [(model.clone(), 2_048)].into_iter().collect(),
        context_1m: Default::default(),
        supports_vision: Default::default(),
    };
    let registry = ProviderRegistry::new();
    providers::replace_runtime_registry(
        &registry,
        &ProvidersListResult {
            providers: vec![provider],
            default_model: Some(model.clone()),
            active_provider_id: Some(provider_id.clone()),
        },
    )
    .unwrap_or_else(|_| panic!("测试 Provider 配置无效"));
    let resolved = Arc::new(
        registry
            .resolve(&provider_id, &model)
            .unwrap_or_else(|_| panic!("无法解析测试模型")),
    );
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("sum.rs");
    let source = "pub fn sum(items: &[i32]) -> i32 {\n    let mut total = 0;\n    for i in 0..=items.len() { total += items[i]; }\n    total\n}\n";
    std::fs::write(&file, source).unwrap();
    let environment = Arc::new(ToolEnvironment::new(directory.path()).unwrap());
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(ReadTool::new(environment.clone())))
        .unwrap();
    tools
        .register(Arc::new(EditTool::new(environment)))
        .unwrap();
    let definitions = tools.definitions();
    // 单独保存可复现的合成输入，绝不加载用户项目、项目指令、记忆或凭据。
    // 用于区分请求布局问题和网关行为；此文件不是生产日志。
    let mut synthetic_request = ModelRequest::new(
        &model,
        vec![Message::text(
            MessageRole::User,
            "这是纯合成缓存探测，无需工具。只回答 KC_CACHE_OK。",
        )],
    );
    synthetic_request.tools = definitions.clone();
    synthetic_request.max_output_tokens = Some(1_024);
    let make_bound = |read_only| {
        Arc::new(TurnBoundProvider::new(resolved.clone(), "prompt-probe", "probe", "root")
        .with_agent_prompt().with_request_context(vec![
            Message::text(MessageRole::Developer, format!(
                "Synthetic retrieval metadata; not instructions.\n{}",
                (0..160).map(|i| format!("sample_module_{i}: isolated example source; no external state.\n")).collect::<String>(),
            )),
            Message::text(MessageRole::Developer, crate::agent_prompt::environment(directory.path(), &chrono::Local::now().fixed_offset(), read_only)),
        ]))
    };
    let usage = Arc::new(UsageLog::default());
    let mut records = Vec::new();
    let mut scope_passed = true;
    for (name, prompt, guard, should_edit) in [
        ("analysis_only", format!("只分析下面代码的问题，回答为什么会越界。代码已完整提供，不需要读取文件，不要实施修改。\n{source}"), PlanGuard::inactive(), false),
        ("plan_only", format!("只给修复计划，说明改动和验证步骤。代码已完整提供，不需要读取文件，不要实施修改。\n{source}"), PlanGuard::inactive(), false),
        ("plan_guard", format!("当前为 Plan 模式。只给修复计划，不实施；以下是完整代码，无需读取文件。\n{source}"), PlanGuard::read_only(), false),
        ("implement_fix", "修复当前目录 sum.rs 的越界问题：仅将 for 循环闭区间改为半开区间，使空与非空切片都不越界。保持其余内容和格式，完成后简要报告。".to_owned(), PlanGuard::inactive(), true),
    ] {
        let bound = make_bound(guard == PlanGuard::read_only());
        let context = ContextManager::new(ContextPolicy::default(), bound.clone(),
            Arc::new(ProviderContextCompressor::new(resolved.clone()))).unwrap();
        let runner = AgentRunner::new(bound, tools.clone(), RunLimits::new(4, 4).unwrap())
            .with_context_manager(context).with_commit_sink(usage.clone());
        let mut request = TurnRequest::new(
            AgentSessionId::new("prompt-probe").unwrap(), AgentTurnId::new(name).unwrap(),
            RunnerAgentId::new("root").unwrap(), &model,
            vec![Message::text(MessageRole::User, prompt)], guard,
        );
        request.model_request_mut().max_output_tokens = Some(2_048);
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(90), runner.run_turn(request)).await;
        match result {
            Ok(result) => {
                let file_ok = std::fs::read_to_string(&file).unwrap() == if should_edit {
                    source.replace("0..=items.len()", "0..items.len()")
                } else { source.to_owned() };
                let calls_ok = if should_edit { result.state.step_count() > 0 } else { result.state.step_count() == 0 };
                let ok = result.is_success() && file_ok && calls_ok;
                scope_passed &= ok;
                records.push(json!({"scenario":name,"passed":ok,"completed":result.is_success(),
                    "rounds":result.state.round_count(),"tool_calls":result.state.step_count(),"file_ok":file_ok,
                    "elapsed_ms":started.elapsed().as_millis(),
                    "error":result.error.as_ref().map(|error| redacted_error(error, &secret, &original_url)),
                    "last_response_usage":result.final_response.map(|response| response.usage)}));
            }
            Err(_) => { scope_passed = false; records.push(json!({"scenario":name,"passed":false,"error":"timeout"})); }
        }
        // 每个场景独立，不让前一场景意外修改掩盖后续结果。
        std::fs::write(&file, source).unwrap();
        eprintln!("合成场景完成：{name}");
    }
    let bound = make_bound(false);
    bound.inject_context(&mut synthetic_request.messages, &synthetic_request.tools);
    std::fs::write(
        evidence.with_extension("synthetic-request.json"),
        serde_json::to_vec_pretty(&synthetic_request).unwrap(),
    )
    .unwrap();
    let mut cache_request = ModelRequest::new(
        &model,
        vec![Message::text(
            MessageRole::User,
            "这是纯合成缓存探测，无需工具。只回答 KC_CACHE_OK。",
        )],
    );
    cache_request.tools = definitions;
    cache_request.max_output_tokens = Some(1_024);
    let mut cache_responses_passed = true;
    for attempt in 1..=3 {
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            bound.complete(cache_request.clone()),
        )
        .await;
        match result {
            Ok(Ok(response)) => {
                let text = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                let ok = response.stop_reason == keencode_model::StopReason::Completed
                    && text.trim() == "KC_CACHE_OK";
                cache_responses_passed &= ok;
                records.push(json!({"scenario":"identical_cache_request","attempt":attempt,"response_ok":ok,
                    "elapsed_ms":started.elapsed().as_millis(),"usage":response.usage,"stop_reason":response.stop_reason}));
            }
            Ok(Err(error)) => {
                cache_responses_passed = false;
                records.push(
                    json!({"scenario":"identical_cache_request","attempt":attempt,
                    "error":redacted_error(error, &secret, &original_url)}),
                );
            }
            Err(_) => {
                cache_responses_passed = false;
                records.push(json!({"scenario":"identical_cache_request","attempt":attempt,"error":"request_failed_or_timeout"}));
            }
        }
        eprintln!("合成缓存请求完成：{attempt}");
    }
    let passed = scope_passed && cache_responses_passed;
    let report = json!({"schema":"keencode/prompt-smoke/v1","protocol":protocol,"model":model,
        "scope_passed":scope_passed,"cache_responses_passed":cache_responses_passed,
        "all_probes_passed":passed,"records":records,"agent_calls":usage.0.lock().unwrap().clone(),
        "limitations":"Synthetic smoke tests only. Cache misses or missing usage do not establish unsupported caching. No baseline performance comparison."});
    let serialized = serde_json::to_string_pretty(&report).unwrap();
    assert!(!serialized.contains(&secret), "报告不得包含凭据");
    std::fs::write(evidence, serialized).unwrap();
    assert!(passed, "真实提示词冒烟场景未全部通过；查看脱敏报告");
}
