//! 复用父模块 HTTP 夹具，验证 MCP 资源进入真实延迟工具与 Agent Loop。

use super::*;
use keencode_agent::{
    AgentTool, TOOL_OUTPUT_LIMITS, ToolCallId, ToolConcurrency, ToolContext, ToolEffect,
    TurnCancellation, TurnId,
};
use keencode_tools::{
    ExecuteExtraTool, McpDiagnosticCode, McpToolBuildReport, prepare_mcp_server_tools,
};

/// 创建固定身份、不使用文件系统或真实模型凭据的工具上下文。
fn context() -> ToolContext {
    ToolContext {
        session_id: SessionId::new("resource-session").unwrap(),
        turn_id: TurnId::new("resource-turn").unwrap(),
        source_agent_id: AgentId::new("resource-agent").unwrap(),
        tool_call_id: ToolCallId::new("resource-call").unwrap(),
        cancellation: TurnCancellation::new(),
    }
}

/// 构造有界超时的 MCP 连接，真实客户端仍负责握手、分页与取消。
async fn prepare(server: &TestServer) -> McpToolBuildReport {
    prepare_mcp_server_tools(
        "resource-server",
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(&server.endpoint)),
        McpClientOptions {
            request_timeout: Duration::from_secs(2),
            shutdown_timeout: Duration::from_secs(1),
            ..McpClientOptions::default()
        },
    )
    .await
}

/// 包装握手与通知，业务处理器只接收资源/工具 JSON-RPC 请求。
fn resource_handler(capabilities: Value, extra: FixtureHandler) -> FixtureHandler {
    Arc::new(move |request| {
        if request.method == "DELETE" {
            return FixtureAction::Respond(TestResponse::empty(200, "OK"));
        }
        let message: Value = serde_json::from_slice(&request.body).unwrap();
        match message["method"].as_str().unwrap() {
            "initialize" => FixtureAction::Respond(TestResponse::json(
                json!({
                    "jsonrpc": "2.0", "id": message["id"],
                    "result": {
                        "protocolVersion": keencode_mcp::DEFAULT_PROTOCOL_VERSION,
                        "capabilities": capabilities,
                        "serverInfo": {"name": "resources-fixture", "version": "1"}
                    }
                }),
                Some(("MCP-Session-Id", "resources-session")),
            )),
            "notifications/initialized" => {
                FixtureAction::Respond(TestResponse::empty(202, "Accepted"))
            }
            _ => extra(request),
        }
    })
}

/// 从实际生产准备报告中选择一个资源操作，不在测试中重造生产名称算法。
fn resource_tool(report: &McpToolBuildReport, operation: &str) -> Arc<dyn AgentTool> {
    let prefix = format!("mcp_resource__{operation}__");
    Arc::clone(
        report
            .tools()
            .iter()
            .find(|tool| tool.definition().name.starts_with(&prefix))
            .unwrap(),
    )
}

/// 为对应请求构造成功的 JSON-RPC 结果。
fn result(message: &Value, value: Value) -> FixtureAction {
    FixtureAction::Respond(TestResponse::json(
        json!({"jsonrpc": "2.0", "id": message["id"], "result": value}),
        None,
    ))
}

/// 在模型请求历史中按调用 ID 找配对结果，避免依赖消息位置。
fn recorded_result<'a>(
    request: &'a keencode_model::ModelRequest,
    id: &str,
) -> &'a keencode_model::ToolResult {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|content| match content {
            ContentBlock::ToolResult { tool_result } if tool_result.tool_call_id == id => {
                Some(tool_result)
            }
            _ => None,
        })
        .expect("后续模型请求必须包含配对工具结果")
}

/// Resource-only Server 经生产准备、延迟搜索与执行，在 Plan 中完成三种资源操作。
#[tokio::test]
async fn resource_only_server_reaches_agent_loop_in_plan_and_preserves_contents() {
    let handler = resource_handler(
        json!({"resources": {}}),
        Arc::new(|request| {
            let message: Value = serde_json::from_slice(&request.body).unwrap();
            match message["method"].as_str().unwrap() {
                "resources/list" if message["params"]["cursor"].is_null() => result(
                    &message,
                    json!({
                        "resources": [{"uri": "fixture://one", "name": "one", "_meta": {"source": "fixture"}}], "nextCursor": "page-2"
                    }),
                ),
                "resources/list" => {
                    assert_eq!(message["params"]["cursor"], "page-2");
                    result(
                        &message,
                        json!({"resources": [{"uri": "fixture://two", "name": "two"}]}),
                    )
                }
                "resources/templates/list" => result(
                    &message,
                    json!({
                        "resourceTemplates": [{"uriTemplate": "fixture://{name}", "name": "entry"}]
                    }),
                ),
                "resources/read" => {
                    assert_eq!(message["params"], json!({"uri": "fixture://one"}));
                    result(
                        &message,
                        json!({"contents": [
                            {"uri": "fixture://one", "mimeType": "text/plain", "text": "RESOURCE_EVIDENCE", "_meta": {"revision": 7}, "custom": true},
                            {"uri": "fixture://binary", "mimeType": "application/octet-stream", "blob": "AAEC"}
                        ]}),
                    )
                }
                other => panic!("纯资源服务不应收到 {other}"),
            }
        }),
    );
    let server = TestServer::spawn(6, handler).await;
    let report = prepare(&server).await;
    assert_eq!(report.tool_count(), 3);
    assert!(report.diagnostics().is_empty());
    let names = ["list", "templates", "read"].map(|op| {
        let tool = resource_tool(&report, op);
        assert_eq!(tool.concurrency(), ToolConcurrency::ParallelReadOnly);
        let input = if op == "read" {
            json!({"uri": "fixture://one"})
        } else {
            json!({})
        };
        assert_eq!(tool.effect(&input).unwrap(), ToolEffect::ReadOnly);
        tool.definition().name
    });
    let catalog = Arc::new(DeferredToolCatalog::new());
    catalog.replace_all(report.into_tools()).unwrap();
    let mut registry = ToolRegistry::new();
    register_deferred_tools(&mut registry, catalog).unwrap();
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[(
                "search",
                "ToolSearch",
                json!({"query": "mcp_resource", "limit": 8}),
            )]),
            tool_reply(&[(
                "list",
                "ExecuteExtraTool",
                json!({"catalog_generation": 1, "tool_name": names[0], "params": {}}),
            )]),
            tool_reply(&[(
                "templates",
                "ExecuteExtraTool",
                json!({"catalog_generation": 1, "tool_name": names[1], "params": {}}),
            )]),
            tool_reply(&[(
                "read",
                "ExecuteExtraTool",
                json!({"catalog_generation": 1, "tool_name": names[2], "params": {"uri": "fixture://one"}}),
            )]),
            text_reply("资源读取完成"),
        ],
    ));
    let runner = AgentRunner::new(provider.clone(), registry, RunLimits::default());
    let turn = runner
        .run_turn(turn_request(
            "resource-loop",
            "读取 MCP 资源",
            PlanGuard::read_only(),
        ))
        .await;
    assert!(turn.is_success(), "{:?}", turn.error);
    assert_eq!(turn.state.step_count(), 4);
    let requests = provider.requests().unwrap();
    assert_eq!(requests.len(), 5);
    for request in &requests {
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["ExecuteExtraTool", "ToolSearch"]
        );
    }
    let read_json: Value =
        serde_json::from_str(tool_result_text(recorded_result(&requests[4], "read"))).unwrap();
    assert_eq!(read_json["contents"][0]["text"], "RESOURCE_EVIDENCE");
    assert_eq!(read_json["contents"][0]["_meta"]["revision"], 7);
    assert_eq!(read_json["contents"][0]["custom"], true);
    assert_eq!(read_json["contents"][1]["blob"], "AAEC");
    let listed: Value =
        serde_json::from_str(tool_result_text(recorded_result(&requests[4], "list"))).unwrap();
    assert_eq!(listed["resources"].as_array().unwrap().len(), 2);
    assert_eq!(listed["resources"][0]["_meta"]["source"], "fixture");
    let templates: Value =
        serde_json::from_str(tool_result_text(recorded_result(&requests[4], "templates"))).unwrap();
    assert_eq!(
        templates["resourceTemplates"][0]["uriTemplate"],
        "fixture://{name}"
    );
    for id in ["search", "list", "templates", "read"] {
        assert!(!recorded_result(&requests[4], id).is_error);
    }
    let captured = server.finish().await;
    assert_eq!(captured.len(), 6);
    assert!(captured.iter().all(|request| request.method == "POST"));
}

/// Schema、URI 字节边界、预取消与旧代次均须在网络请求前拒绝。
#[tokio::test]
async fn invalid_resource_inputs_and_stale_generation_never_reach_server() {
    let server = TestServer::spawn(
        3,
        resource_handler(
            json!({"resources": {}}),
            Arc::new(|request| {
                let message: Value = serde_json::from_slice(&request.body).unwrap();
                assert_eq!(message["method"], "resources/read");
                assert_eq!(message["params"]["uri"].as_str().unwrap().len(), 4096);
                result(&message, json!({"contents": []}))
            }),
        ),
    )
    .await;
    let report = prepare(&server).await;
    let read = resource_tool(&report, "read");
    for input in [
        json!({}),
        json!({"uri": 4}),
        json!({"uri": ""}),
        json!({"uri": "  "}),
        json!({"uri": "x\ny"}),
        json!({"uri": "x", "extra": true}),
        json!({"uri": "界".repeat(1366)}),
        json!({"uri": "x".repeat(4097)}),
    ] {
        let error = read.execute(context(), input.clone()).await.unwrap_err();
        assert_eq!(error.code, "mcp_resource_input_invalid");
        assert_eq!(
            read.effect(&input).unwrap_err().code,
            "mcp_resource_input_invalid"
        );
    }
    for op in ["list", "templates"] {
        assert_eq!(
            resource_tool(&report, op)
                .execute(context(), json!({"unexpected": true}))
                .await
                .unwrap_err()
                .code,
            "mcp_resource_input_invalid"
        );
    }
    let cancelled = context();
    cancelled.cancellation.cancel();
    assert_eq!(
        read.execute(cancelled, json!({"uri": "fixture://cancelled"}))
            .await
            .unwrap_err()
            .code,
        "tool_cancelled"
    );
    let catalog = Arc::new(DeferredToolCatalog::new());
    catalog.replace_all(report.tools().to_vec()).unwrap();
    catalog.replace_all(report.into_tools()).unwrap();
    let execute = ExecuteExtraTool::new(catalog);
    let error = execute.execute(context(), json!({"catalog_generation": 1, "tool_name": read.definition().name, "params": {"uri": "fixture://stale"}})).await.unwrap_err();
    assert_eq!(error.code, "deferred_tool_not_found");
    assert!(
        read.execute(context(), json!({"uri": "x".repeat(4096)}))
            .await
            .is_ok()
    );
    assert_eq!(server.finish().await.len(), 3);
}

/// 普通工具发现失败不应移除可用资源，也不应提前关闭连接。
#[tokio::test]
async fn resources_survive_tools_discovery_failure() {
    let server = TestServer::spawn(4, resource_handler(json!({"tools": {}, "resources": {}}), Arc::new(|request| {
        let message: Value = serde_json::from_slice(&request.body).unwrap();
        if message["method"] == "tools/list" {
            FixtureAction::Respond(TestResponse::json(json!({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32603, "message": "PRIVATE_REMOTE_ERROR"}}), None))
        } else {
            assert_eq!(message["method"], "resources/read");
            result(&message, json!({"contents": []}))
        }
    }))).await;
    let report = prepare(&server).await;
    assert_eq!(report.tool_count(), 3);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code,
        McpDiagnosticCode::ToolDiscoveryFailed
    );
    assert!(
        resource_tool(&report, "read")
            .execute(context(), json!({"uri": "fixture://ok"}))
            .await
            .is_ok()
    );
    assert!(
        server
            .finish()
            .await
            .iter()
            .all(|request| request.method != "DELETE")
    );
}

/// 没有工具或资源能力的服务应显式终止 HTTP 会话，且不发送发现请求。
#[tokio::test]
async fn server_without_callable_capabilities_is_closed() {
    let server = TestServer::spawn(
        3,
        resource_handler(json!({}), Arc::new(|_| panic!("没有能力不应发送发现请求"))),
    )
    .await;
    let report = prepare(&server).await;
    assert_eq!(report.tool_count(), 0);
    assert!(report.diagnostics().is_empty());
    assert_eq!(server.finish().await[2].method, "DELETE");
}

/// 取消在途资源请求必须真正向服务发送匹配 requestId 的标准取消通知。
#[tokio::test]
async fn inflight_resource_cancellation_sends_matching_notification() {
    let started = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    let request_id = Arc::new(StdMutex::new(Value::Null));
    let server = TestServer::spawn(
        4,
        resource_handler(
            json!({"resources": {}}),
            Arc::new({
                let started = Arc::clone(&started);
                let released = Arc::clone(&released);
                let request_id = Arc::clone(&request_id);
                move |request| {
                    let message: Value = serde_json::from_slice(&request.body).unwrap();
                    match message["method"].as_str().unwrap() {
                        "resources/read" => {
                            *request_id.lock().unwrap() = message["id"].clone();
                            started.notify_one();
                            FixtureAction::Hold(Arc::clone(&released))
                        }
                        "notifications/cancelled" => {
                            assert_eq!(message["params"]["requestId"], *request_id.lock().unwrap());
                            released.notify_one();
                            FixtureAction::Respond(TestResponse::empty(202, "Accepted"))
                        }
                        other => panic!("未预期的取消方法 {other}"),
                    }
                }
            }),
        ),
    )
    .await;
    let report = prepare(&server).await;
    let read = resource_tool(&report, "read");
    let context = context();
    let cancellation = context.cancellation.clone();
    let (output, ()) = tokio::join!(
        read.execute(context, json!({"uri": "fixture://slow"})),
        async {
            tokio::time::timeout(Duration::from_secs(2), started.notified())
                .await
                .unwrap();
            cancellation.cancel();
        }
    );
    assert_eq!(output.unwrap_err().code, "tool_cancelled");
    assert_eq!(server.finish().await.len(), 4);
}

/// 远端资源错误、断连和循环分页应归一为稳定错误，不能泄露正文或返回不完整成功。
#[tokio::test]
async fn resource_errors_and_cyclic_pagination_are_normalized() {
    let server = TestServer::spawn(6, resource_handler(json!({"resources": {}}), Arc::new(|request| {
        let message: Value = serde_json::from_slice(&request.body).unwrap();
        if message["method"] == "resources/list" {
            return result(&message, json!({"resources": [], "nextCursor": "cycle"}));
        }
        assert_eq!(message["method"], "resources/read");
        if message["params"]["uri"] == "fixture://disconnect" {
            FixtureAction::Disconnect
        } else {
            FixtureAction::Respond(TestResponse::json(json!({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32603, "message": "PRIVATE_RESOURCE_ERROR"}}), None))
        }
    }))).await;
    let report = prepare(&server).await;
    for (uri, code) in [
        ("fixture://error", "mcp_tool_failed"),
        ("fixture://disconnect", "mcp_unavailable"),
    ] {
        let error = resource_tool(&report, "read")
            .execute(context(), json!({"uri": uri}))
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.message.contains("PRIVATE_RESOURCE_ERROR"));
    }
    assert_eq!(
        resource_tool(&report, "list")
            .execute(context(), json!({}))
            .await
            .unwrap_err()
            .code,
        "mcp_tool_failed"
    );
    assert_eq!(server.finish().await.len(), 6);
}

/// 资源正文经过真实 Runner 的统一输出预算，超限必须产生配对错误而非进入模型。
#[tokio::test]
async fn oversized_resource_output_is_rejected_by_agent_budget() {
    let server = TestServer::spawn(3, resource_handler(json!({"resources": {}}), Arc::new(|request| {
        let message: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(message["method"], "resources/read");
        result(&message, json!({"contents": [{"uri": "fixture://large", "text": "x".repeat(TOOL_OUTPUT_LIMITS.max_text_bytes + 1)}]}))
    }))).await;
    let report = prepare(&server).await;
    let name = resource_tool(&report, "read").definition().name;
    let catalog = Arc::new(DeferredToolCatalog::new());
    catalog.replace_all(report.into_tools()).unwrap();
    let mut registry = ToolRegistry::new();
    register_deferred_tools(&mut registry, catalog).unwrap();
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[(
                "large",
                "ExecuteExtraTool",
                json!({"catalog_generation": 1, "tool_name": name, "params": {"uri": "fixture://large"}}),
            )]),
            text_reply("超限已处理"),
        ],
    ));
    let runner = AgentRunner::new(provider.clone(), registry, RunLimits::default());
    assert!(
        runner
            .run_turn(turn_request(
                "large-resource",
                "读取资源",
                PlanGuard::read_only()
            ))
            .await
            .is_success()
    );
    let requests = provider.requests().unwrap();
    let output = recorded_result(&requests[1], "large");
    assert!(output.is_error);
    assert!(tool_result_text(output).contains("tool_output_limit_exceeded"));
    assert!(tool_result_text(output).len() < 1024);
    assert_eq!(server.finish().await.len(), 3);
}
