//! 真实 Messages 模型经过桌面装配、ACP 投递、MCP 和持久化的隔离验证。

use super::*;

#[derive(Default)]
struct LiveEmitter(Mutex<Vec<Value>>);

impl DeliveryEmitter for LiveEmitter {
    fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError> {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_value(delivery).unwrap());
        Ok(())
    }
}

struct LiveExtensions(Arc<keencode_tools::DeferredToolCatalog>);

impl RuntimeExtensionContributor for LiveExtensions {
    fn register_tools(
        &self,
        registry: &mut ToolRegistry,
        _: &RuntimeToolContext,
    ) -> Result<(), String> {
        keencode_tools::register_deferred_tools(registry, self.0.clone()).map_err(|e| e.to_string())
    }
    fn build_hook_runtime(&self, _: &RuntimeToolContext) -> Result<HookRuntime, String> {
        Ok(HookRuntime::default())
    }
    fn prepare_lsp_runtime(&self, _: &RuntimeToolContext) -> Result<(), String> {
        Ok(())
    }
    fn resolve_agent(
        &self,
        _: &str,
        _: &RuntimeAgentTemplateContext,
    ) -> Result<Option<RuntimeAgentTemplate>, String> {
        Ok(None)
    }
}

async fn wait_idle(runtime: &Arc<AgentRuntime>, session: &RuntimeSession) {
    tokio::time::timeout(Duration::from_secs(240), async {
        while runtime
            .session_has_active_work(session.session_id().as_str())
            .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("真实桌面 Runtime 超过 240 秒仍未形成终态");
}

async fn wait_for_script(
    runtime: &Arc<AgentRuntime>,
    session: &RuntimeSession,
    marker: &Path,
    emitter: &LiveEmitter,
    evidence: &Path,
) {
    let started = Instant::now();
    while !marker.exists() {
        if started.elapsed() > Duration::from_secs(120)
            || !runtime
                .session_has_active_work(session.session_id().as_str())
                .unwrap()
        {
            // 保存失败现场并停止所有测试进程，避免等待一个已失败或未执行的命令。
            std::fs::write(
                evidence.join("script-start-failure.json"),
                serde_json::to_vec_pretty(&json!({
                    "marker": marker,
                    "elapsed_ms": started.elapsed().as_millis(),
                    "state": session.snapshot().unwrap().state,
                }))
                .unwrap(),
            )
            .unwrap();
            std::fs::write(
                evidence.join("acp-deliveries.json"),
                serde_json::to_vec_pretty(&*emitter.0.lock().unwrap()).unwrap(),
            )
            .unwrap();
            runtime.shutdown().await.unwrap();
            panic!("真实脚本没有启动，已保存失败现场：{}", marker.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn run(
    runtime: &Arc<AgentRuntime>,
    session: &RuntimeSession,
    name: &str,
    prompt: &str,
    options: RootTurnOptions,
) -> Value {
    let started = Instant::now();
    runtime
        .start_root_turn(session.session_id().as_str(), name, prompt, options)
        .await
        .unwrap();
    wait_idle(runtime, session).await;
    let snapshot = session.snapshot().unwrap();
    let turn = snapshot
        .state
        .turns
        .get(&ResourceTurnId::new(name).unwrap())
        .unwrap();
    let record = json!({"scenario":name,"status":turn.status,"elapsed_ms":started.elapsed().as_millis(),"subagents":snapshot.state.sub_agents.len()});
    if let Ok(directory) = std::env::var("KEENCODE_MESSAGES_TEST_EVIDENCE") {
        std::fs::write(
            PathBuf::from(directory).join(format!("stage-{name}.json")),
            serde_json::to_vec_pretty(&record).unwrap(),
        )
        .unwrap();
    }
    eprintln!("Desktop runtime {name}: {}", record["status"]);
    record
}

/// 使用实际进程内桌面装配与 ACP 序列化边界；不将此证据标成原生 WebView 验收。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要显式 KEENCODE_MESSAGES_TEST_* 配置；MCP 场景另传合成服务路径"]
async fn live_messages_desktop_lifecycle() {
    let required = |name| std::env::var(name).expect("缺少显式真实测试参数");
    let config: Value =
        serde_json::from_slice(&std::fs::read(required("KEENCODE_MESSAGES_TEST_CONFIG")).unwrap())
            .unwrap();
    assert_eq!(config["providers"].as_array().unwrap().len(), 1);
    let p = &config["providers"][0];
    assert_eq!(p["apiBackend"], "messages");
    let model = required("KEENCODE_MESSAGES_TEST_MODEL");
    let mcp_command = std::env::var("KEENCODE_MESSAGES_TEST_MCP").ok();
    let evidence = PathBuf::from(required("KEENCODE_MESSAGES_TEST_EVIDENCE"));
    std::fs::create_dir_all(&evidence).unwrap();
    let root = evidence.join("project");
    let storage = evidence.join("runtime");
    std::fs::create_dir_all(&root).unwrap();
    let facts = "receipt=KC_RUNTIME_J6N8\nbudget=7319\ncurrency=JPY\n";
    std::fs::write(root.join("facts.txt"), facts).unwrap();
    let mut provider = CustomProvider {
        chat_output_token_field: Default::default(),
        read_timeout_seconds: 300,
        id: "messages-runtime-validation".into(),
        name: "Synthetic runtime".into(),
        models: vec![model.clone()],
        api_backend: "messages".into(),
        base_url: p["baseUrl"].as_str().unwrap().into(),
        api_key: Some(p["apiKey"].as_str().unwrap().into()),
        context_windows: Default::default(),
        context_1m: [(model.clone(), true)].into_iter().collect(),
        supports_vision: [(model.clone(), true)].into_iter().collect(),
        max_output_tokens: [(model.clone(), 8192)].into_iter().collect(),
    };
    let registry = ProviderRegistry::new();
    let install = |registry: &ProviderRegistry, provider: CustomProvider| {
        providers::replace_runtime_registry(
            registry,
            &ProvidersListResult {
                active_provider_id: Some(provider.id.clone()),
                default_model: Some(model.clone()),
                providers: vec![provider],
            },
        )
        .unwrap()
    };
    install(&registry, provider.clone());
    let emitter = Arc::new(LiveEmitter::default());
    let runtime =
        Arc::new(AgentRuntime::new_with_registry(&storage, emitter.clone(), registry).unwrap());
    let session = runtime
        .open_or_create_session(&root, None, "live-messages-runtime")
        .unwrap();
    let id = session.session_id().as_str().to_owned();
    runtime
        .set_session_model(&id, "select-model", &provider.id, &model)
        .unwrap();
    let mut records = Vec::new();
    records.push(run(&runtime,&session,"read-facts","读取 facts.txt，报告回执和预算。",RootTurnOptions {
        developer_context:Some("Synthetic memory: currency is JPY; do not add dependencies. Internal request tag KC_EPHEMERAL_ONLY_F3M9 is metadata, omit it from the answer.".into()),plan_enabled:false,
    }).await);
    records.push(
        run(
            &runtime,
            &session,
            "delegation",
            "请创建一个子代理读取 facts.txt，检查预算和币种，等它完成后汇总结果。",
            RootTurnOptions::default(),
        )
        .await,
    );
    let first_children = session.snapshot().unwrap().state.sub_agents.len();
    records.push(
        run(
            &runtime,
            &session,
            "child-followup",
            "让刚才那个子代理继续核对回执与预算，汇报它的复核结果。",
            RootTurnOptions::default(),
        )
        .await,
    );
    let same_child =
        first_children > 0 && first_children == session.snapshot().unwrap().state.sub_agents.len();

    if let Some(command) = &mcp_command {
        let mcp = keencode_tools::prepare_mcp_server_tools(
            "synthetic",
            keencode_mcp::McpServerConfig::Stdio(keencode_mcp::StdioServerConfig::new(command)),
            keencode_mcp::McpClientOptions::default(),
        )
        .await;
        assert!(!mcp.tools().is_empty());
        let catalog = Arc::new(keencode_tools::DeferredToolCatalog::new());
        catalog.replace_all(mcp.into_tools()).unwrap();
        runtime
            .publish_extension_candidate(
                &root,
                RuntimeExtensionCandidate::new(1, Arc::new(LiveExtensions(catalog))).unwrap(),
            )
            .unwrap();
        records.push(
            run(
                &runtime,
                &session,
                "mcp-search-execute",
                "查找可用的 MCP echo 工具，调用它回显 receipt=KC_MCP_C4B7，然后报告工具返回值。",
                RootTurnOptions::default(),
            )
            .await,
        );
    }
    records.push(
        run(
            &runtime,
            &session,
            "plan-guard",
            "把 facts.txt 的 budget 改为 8888。",
            RootTurnOptions {
                developer_context: None,
                plan_enabled: true,
            },
        )
        .await,
    );
    let plan_unchanged = std::fs::read_to_string(root.join("facts.txt")).unwrap() == facts;

    std::fs::write(root.join("child-slow.py"),"from pathlib import Path\nimport time\nPath('child-started').write_text('yes')\ntime.sleep(30)\nPath('child-finished').write_text('yes')\n").unwrap();
    runtime
        .start_root_turn(
            &id,
            "cancel-child",
            "创建一个子代理运行 python3 child-slow.py，等待它完成后汇报。",
            RootTurnOptions::default(),
        )
        .await
        .unwrap();
    wait_for_script(
        &runtime,
        &session,
        &root.join("child-started"),
        &emitter,
        &evidence,
    )
    .await;
    let child_task = runtime
        .background_tasks_list(&id)
        .unwrap()
        .into_iter()
        .find(|t| t.kind == BackgroundTaskKind::Agent)
        .expect("子代理必须真实运行");
    runtime
        .steer_root_turn(
            &id,
            "stop-child-steer",
            "取消刚创建的子任务，保留当前状态并汇报取消结果。",
        )
        .unwrap();
    runtime
        .background_task_cancel(&id, &child_task.task_id)
        .unwrap();
    wait_idle(&runtime, &session).await;
    let child_cancelled = session.snapshot().unwrap().state.turns
        [&ResourceTurnId::new(&child_task.task_id).unwrap()]
        .status
        == TurnStatus::Cancelled;

    std::fs::write(root.join("slow.py"),"from pathlib import Path\nimport time\nPath('slow-started').write_text('yes')\ntime.sleep(30)\nPath('slow-finished').write_text('yes')\n").unwrap();
    runtime
        .start_root_turn(
            &id,
            "cancel-running-command",
            "由你直接运行 python3 slow.py，不委派子代理，完成后报告。",
            RootTurnOptions::default(),
        )
        .await
        .unwrap();
    wait_for_script(
        &runtime,
        &session,
        &root.join("slow-started"),
        &emitter,
        &evidence,
    )
    .await;
    runtime.cancel_turn(&id, "cancel-running-command").unwrap();
    wait_idle(&runtime, &session).await;
    let cancelled = session.snapshot().unwrap().state.turns
        [&ResourceTurnId::new("cancel-running-command").unwrap()]
        .status
        == TurnStatus::Cancelled;

    let previous_provider = session.snapshot().unwrap().state.provider.unwrap();
    provider.max_output_tokens.insert(model.clone(), 6144);
    install(runtime.provider_registry(), provider.clone());
    records.push(
        run(
            &runtime,
            &session,
            "after-cancel-refresh",
            "继续处理当前任务：读取 facts.txt，只报告回执。",
            RootTurnOptions::default(),
        )
        .await,
    );
    let refreshed = session.snapshot().unwrap().state.provider.unwrap();
    let config_refreshed = previous_provider.config_fingerprint != refreshed.config_fingerprint;
    let transcript = session
        .model_transcript_for_agent(&ResourceAgentId::new("root").unwrap())
        .unwrap();
    let calls: Vec<_> = transcript
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::ToolCall { tool_call } => Some(tool_call),
            _ => None,
        })
        .collect();
    let executions: HashSet<_> = calls
        .iter()
        .filter(|c| c.name == "ExecuteExtraTool")
        .map(|c| c.id.as_str())
        .collect();
    let tools_seen = calls.iter().any(|c| c.name == "SearchExtraTools")
        && transcript.iter().flat_map(|m| &m.content).any(|b| match b {
            ContentBlock::ToolResult { tool_result } => {
                executions.contains(tool_result.tool_call_id.as_str())
                    && !tool_result.is_error
                    && serde_json::to_string(&tool_result.content)
                        .unwrap()
                        .contains("KC_MCP_C4B7")
            }
            _ => false,
        });
    // 模型可能在 reasoning 中复述输入；校验运行时没有持久化请求期指令消息本身。
    let request_context_not_persisted = !transcript.iter().any(|m| {
        matches!(m.role, MessageRole::System | MessageRole::Developer)
            && serde_json::to_string(m)
                .unwrap()
                .contains("KC_EPHEMERAL_ONLY_F3M9")
    });
    runtime.shutdown().await.unwrap();
    drop(session);
    drop(runtime);

    let registry = ProviderRegistry::new();
    install(&registry, provider);
    let cold =
        Arc::new(AgentRuntime::new_with_registry(&storage, emitter.clone(), registry).unwrap());
    let session = cold
        .open_or_create_session(&root, Some(&id), "cold-open")
        .unwrap();
    let cold_equal = session
        .model_transcript_for_agent(&ResourceAgentId::new("root").unwrap())
        .unwrap()
        == transcript;
    records.push(
        run(
            &cold,
            &session,
            "cold-recall",
            "回顾此前文件读取结果，预算和币种分别是什么？直接根据会话历史回答。",
            RootTurnOptions::default(),
        )
        .await,
    );
    let transcript = session
        .model_transcript_for_agent(&ResourceAgentId::new("root").unwrap())
        .unwrap();
    let after = transcript
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant)
        .map(|m| serde_json::to_string(m).unwrap())
        .unwrap_or_default();
    cold.replay_session(&id, None, 1000).await.unwrap();
    cold.shutdown().await.unwrap();
    let mut assertions = json!({"single_child_reused":same_child,"child_cancelled":child_cancelled,"child_command_never_finished":!root.join("child-finished").exists(),"plan_unchanged":plan_unchanged,"cancelled":cancelled,"cancelled_command_never_finished":!root.join("slow-finished").exists(),"config_refreshed_next_turn":config_refreshed,"request_context_not_persisted":request_context_not_persisted,"cold_transcript_equal":cold_equal,"recall_contains_facts":after.contains("7319") && after.contains("JPY")});
    if mcp_command.is_some() {
        assertions["mcp_discovered_and_executed"] = tools_seen.into();
    }
    let report = json!({"scope":"real desktop runtime and ACP serialization; no native WebView claim","mcp_enabled":mcp_command.is_some(),"records":records,"assertions":assertions});
    std::fs::write(
        evidence.join("runtime-report.json"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(
        evidence.join("acp-deliveries.json"),
        serde_json::to_vec_pretty(&*emitter.0.lock().unwrap()).unwrap(),
    )
    .unwrap();
    assert!(
        assertions.as_object().unwrap().values().all(|v| v == true),
        "桌面真实链路有未通过断言，查看报告"
    );
    assert!(
        records.iter().all(|r| r["status"] == "completed"),
        "桌面真实 Turn 未全部成功"
    );
}
