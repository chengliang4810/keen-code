//! MCP Bridge、延迟工具目录与 Agent Runtime 的组合集成测试。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use keencode_agent::{
    AgentId, AgentRunner, PlanGuard, RunLimits, SessionId, ToolRegistry, TurnRequest,
};
use keencode_mcp::{
    McpClient, McpClientOptions, McpServerConfig, McpToolEffect, StreamableHttpConfig,
};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities, ResponseMetadata,
    ScriptedProvider, ScriptedReply, StopReason, ToolResultContent,
};
use keencode_tools::{
    DeferredToolCatalog, build_mcp_deferred_tools_best_effort, portable_mcp_tool_name,
    register_deferred_tools,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[path = "mcp_agent_runtime/resources.rs"]
mod resources;

/// MCP 夹具在单个请求上返回的动作。
enum FixtureAction {
    /// 返回一个完整的 HTTP 响应。
    Respond(TestResponse),
    /// 读完请求后直接关闭连接，模拟远端断连。
    Disconnect,
    /// 保持请求在途，直到夹具收到对应取消通知。
    Hold(Arc<tokio::sync::Notify>),
}

/// MCP 夹具请求处理器；处理器本身不持有套接字。
type FixtureHandler = Arc<dyn Fn(TestRequest) -> FixtureAction + Send + Sync>;

/// 本机回环 HTTP MCP Server 及其捕获的请求。
struct TestServer {
    /// MCP 客户端用于连接的随机回环端点。
    endpoint: String,
    /// 按服务接受顺序保存的请求快照。
    captured: Arc<StdMutex<Vec<TestRequest>>>,
    /// 接受固定数量请求并处理的后台任务。
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// 绑定随机回环端口并接受指定数量的 HTTP 请求。
    async fn spawn(expected_requests: usize, handler: FixtureHandler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应绑定随机回环端口");
        let endpoint = format!(
            "http://{}/mcp",
            listener.local_addr().expect("应获得测试服务地址")
        );
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let captured_for_task = Arc::clone(&captured);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            for _ in 0..expected_requests {
                let (mut socket, _) = listener.accept().await.expect("应接受 MCP HTTP 连接");
                let captured_for_task = Arc::clone(&captured_for_task);
                let handler = Arc::clone(&handler);
                connections.spawn(async move {
                    let request = read_http_request(&mut socket).await;
                    captured_for_task
                        .lock()
                        .expect("请求记录锁不应中毒")
                        .push(request.clone());
                    match handler(request) {
                        FixtureAction::Respond(response) => {
                            let bytes = response.to_http_bytes();
                            let _ = socket.write_all(&bytes).await;
                        }
                        FixtureAction::Disconnect => {}
                        FixtureAction::Hold(signal) => {
                            tokio::time::timeout(Duration::from_secs(3), signal.notified())
                                .await
                                .expect("在途请求应收到取消通知");
                        }
                    }
                    let _ = socket.shutdown().await;
                });
            }
            while let Some(result) = connections.join_next().await {
                result.expect("测试连接任务不应 panic");
            }
        });
        Self {
            endpoint,
            captured,
            task,
        }
    }

    /// 等待服务处理固定请求并返回捕获快照。
    async fn finish(mut self) -> Vec<TestRequest> {
        tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await
            .expect("测试服务应在有界时间内结束")
            .expect("测试服务任务不应 panic");
        self.captured.lock().expect("请求记录锁不应中毒").clone()
    }
}

impl Drop for TestServer {
    /// 失败断言或超时时回收监听与连接任务，避免测试失败遗留后台工作。
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// MCP 夹具捕获的一个 HTTP 请求。
#[derive(Clone)]
struct TestRequest {
    /// HTTP 方法，例如 POST 或 DELETE。
    method: String,
    /// 归一化为小写名称的请求头。
    headers: BTreeMap<String, String>,
    /// 按 Content-Length 读取的请求体。
    body: Vec<u8>,
}

/// MCP 夹具返回的有限 HTTP 响应。
struct TestResponse {
    /// HTTP 状态码。
    status: u16,
    /// HTTP 状态原因短语。
    reason: &'static str,
    /// 可选响应 Content-Type。
    content_type: Option<&'static str>,
    /// 可选的额外响应头。
    extra_header: Option<(&'static str, &'static str)>,
    /// 响应正文。
    body: Vec<u8>,
}

impl TestResponse {
    /// 构造带 JSON 正文的 200 响应。
    fn json(value: Value, extra_header: Option<(&'static str, &'static str)>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: Some("application/json"),
            extra_header,
            body: serde_json::to_vec(&value).expect("测试 JSON 应可序列化"),
        }
    }

    /// 构造指定状态且没有正文的响应。
    fn empty(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            content_type: None,
            extra_header: None,
            body: Vec::new(),
        }
    }

    /// 编码为 HTTP/1.1 且主动关闭连接的响应。
    fn to_http_bytes(&self) -> Vec<u8> {
        let mut headers = format!(
            "HTTP/1.1 {} {}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.status,
            self.reason,
            self.body.len()
        );
        if let Some(content_type) = self.content_type {
            headers.push_str(&format!("Content-Type: {content_type}\r\n"));
        }
        if let Some((name, value)) = self.extra_header {
            headers.push_str(&format!("{name}: {value}\r\n"));
        }
        headers.push_str("\r\n");
        let mut bytes = headers.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

/// 创建 MCP initialize 成功响应。
fn initialize_response(id: Value) -> TestResponse {
    TestResponse::json(
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": keencode_mcp::DEFAULT_PROTOCOL_VERSION,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "runtime-fixture", "version": "1.0.0"}
            }
        }),
        Some(("MCP-Session-Id", "runtime-fixture-session")),
    )
}

/// 创建 MCP tools/list 成功响应；服务端注解不直接成为本地副作用信任。
fn tools_list_response(id: Value) -> TestResponse {
    let text_schema = json!({
        "type": "object",
        "properties": {
            "value": {"type": "string", "minLength": 1}
        },
        "required": ["value"],
        "additionalProperties": false
    });
    TestResponse::json(
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Return the supplied value",
                        "inputSchema": text_schema,
                        "annotations": {"readOnlyHint": true}
                    },
                    {
                        "name": "write_once",
                        "description": "A state-changing fixture tool",
                        "inputSchema": text_schema
                    }
                ]
            }
        }),
        None,
    )
}

/// 创建一个包含指定模型工具调用的脚本响应。
fn tool_reply(calls: &[(&str, &str, Value)]) -> ScriptedReply {
    let mut events = vec![ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata::default(),
    }];
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let index = u32::try_from(index).expect("测试工具调用数量应在 u32 范围内");
        events.push(ModelStreamEvent::ToolCallStart {
            index,
            id: (*id).to_owned(),
            name: (*name).to_owned(),
        });
        events.push(ModelStreamEvent::ToolCallArgumentsDelta {
            index,
            id: (*id).to_owned(),
            delta: arguments.to_string(),
        });
        events.push(ModelStreamEvent::ToolCallEnd {
            index,
            id: (*id).to_owned(),
        });
    }
    events.push(ModelStreamEvent::MessageEnd {
        stop_reason: StopReason::ToolUse,
    });
    ScriptedReply::events(events)
}

/// 创建一个包含最终文本的脚本响应。
fn text_reply(text: &str) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: text.to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ])
}

/// 构造一次使用独立身份的 Agent Turn。
fn turn_request(turn: &str, prompt: &str, plan_guard: PlanGuard) -> TurnRequest {
    TurnRequest::new(
        SessionId::new("mcp-runtime-session").expect("测试 Session ID 应有效"),
        keencode_agent::TurnId::new(turn).expect("测试 Turn ID 应有效"),
        AgentId::new("mcp-runtime-agent").expect("测试 Agent ID 应有效"),
        "fixture-model",
        vec![Message::text(MessageRole::User, prompt)],
        plan_guard,
    )
}

/// 有界读取 HTTP 请求头和 Content-Length 指定的请求体。
async fn read_http_request(socket: &mut TcpStream) -> TestRequest {
    const LIMIT: usize = 1024 * 1024;
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = find_bytes(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
        assert!(bytes.len() < LIMIT, "测试请求头超过安全上限");
        let mut chunk = [0_u8; 4096];
        let read = socket.read(&mut chunk).await.expect("应读取 HTTP 请求头");
        assert!(read > 0, "HTTP 请求头提前结束");
        bytes.extend_from_slice(&chunk[..read]);
    };
    let header_text = std::str::from_utf8(&bytes[..header_end]).expect("请求头应为 UTF-8");
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().expect("应存在 HTTP 请求行");
    let method = request_line
        .split_whitespace()
        .next()
        .expect("请求行应包含 HTTP 方法")
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').expect("请求头应包含冒号");
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>().expect("Content-Length 应为数字"))
        .unwrap_or(0);
    assert!(content_length <= LIMIT, "测试请求体超过安全上限");
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let read = socket.read(&mut chunk).await.expect("应读取 HTTP 请求体");
        assert!(read > 0, "HTTP 请求体提前结束");
        bytes.extend_from_slice(&chunk[..read]);
    }
    TestRequest {
        method,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

/// 查找字节子串首次出现的位置。
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 贯通 MCP Client、Bridge、DeferredToolCatalog、ToolRegistry 和 AgentRunner。
#[tokio::test]
async fn mcp_bridge_deferred_catalog_and_agent_runner_complete_workflow() {
    let remote_calls = Arc::new(AtomicUsize::new(0));
    let remote_calls_for_handler = Arc::clone(&remote_calls);
    let handler: FixtureHandler = Arc::new(move |request| {
        if request.method == "DELETE" {
            return FixtureAction::Respond(TestResponse::empty(200, "OK"));
        }
        let message: Value = serde_json::from_slice(&request.body).expect("MCP 请求体应为 JSON");
        match message["method"].as_str().expect("MCP 请求应包含 method") {
            "initialize" => FixtureAction::Respond(initialize_response(message["id"].clone())),
            "notifications/initialized" => {
                FixtureAction::Respond(TestResponse::empty(202, "Accepted"))
            }
            "tools/list" => FixtureAction::Respond(tools_list_response(message["id"].clone())),
            "tools/call" => {
                let call_number = remote_calls_for_handler.fetch_add(1, Ordering::SeqCst);
                if call_number == 0 {
                    assert_eq!(message["params"]["name"], "echo");
                    assert_eq!(message["params"]["arguments"], json!({"value": "hello"}));
                    FixtureAction::Respond(TestResponse::json(
                        json!({
                            "jsonrpc": "2.0",
                            "id": message["id"].clone(),
                            "result": {
                                "content": [{"type": "text", "text": "echo:hello"}],
                                "structuredContent": {"value": "hello"},
                                "isError": false
                            }
                        }),
                        None,
                    ))
                } else if call_number == 1 {
                    assert_eq!(message["params"]["name"], "echo");
                    FixtureAction::Disconnect
                } else {
                    panic!("MCP 夹具不应收到第三次 tools/call")
                }
            }
            other => panic!("MCP 夹具收到未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(6, handler).await;
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(server.endpoint.clone())),
        McpClientOptions {
            request_timeout: Duration::from_millis(500),
            shutdown_timeout: Duration::from_secs(1),
            ..McpClientOptions::default()
        },
    )
    .await
    .expect("MCP HTTP 客户端应完成 initialize 握手");

    let mut tool_set = client.list_tools().await.expect("MCP tools/list 应成功");
    assert_eq!(tool_set.tools().len(), 2);
    assert_eq!(tool_set.effect_for("echo"), McpToolEffect::ChangesState);
    assert!(tool_set.set_local_effect("echo", McpToolEffect::ReadOnly));
    assert_eq!(tool_set.effect_for("echo"), McpToolEffect::ReadOnly);
    assert_eq!(
        tool_set.effect_for("write_once"),
        McpToolEffect::ChangesState
    );

    let echo_name =
        portable_mcp_tool_name("runtime-http", "echo").expect("echo 应生成稳定的中立工具名称");
    let write_name = portable_mcp_tool_name("runtime-http", "write_once")
        .expect("write_once 应生成稳定的中立工具名称");
    let report = build_mcp_deferred_tools_best_effort("runtime-http", client.clone(), &tool_set);
    assert_eq!(report.tool_count(), 2);
    assert!(report.diagnostics().is_empty());

    let catalog = Arc::new(DeferredToolCatalog::new());
    assert_eq!(
        catalog
            .replace_all(report.into_tools())
            .expect("MCP 工具应原子写入延迟目录"),
        2
    );
    assert_eq!(catalog.len(), 2);
    assert!(
        catalog
            .definitions()
            .iter()
            .any(|tool| tool.name == echo_name)
    );
    assert!(
        catalog
            .definitions()
            .iter()
            .any(|tool| tool.name == write_name)
    );

    let mut registry = ToolRegistry::new();
    register_deferred_tools(&mut registry, Arc::clone(&catalog)).expect("延迟搜索和执行入口应注册");
    assert_eq!(
        registry
            .definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>(),
        vec!["ExecuteExtraTool", "ToolSearch"]
    );

    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[("search-echo", "ToolSearch", json!({"query": "echo"}))]),
            tool_reply(&[(
                "execute-echo",
                "ExecuteExtraTool",
                json!({
                    "catalog_generation": 1,
                    "tool_name": echo_name,
                    "params": {"value": "hello"}
                }),
            )]),
            text_reply("echo workflow complete"),
            tool_reply(&[(
                "invalid-echo",
                "ExecuteExtraTool",
                json!({
                    "catalog_generation": 1,
                    "tool_name": echo_name,
                    "params": {"value": 7}
                }),
            )]),
            text_reply("invalid input handled"),
            tool_reply(&[(
                "plan-write",
                "ExecuteExtraTool",
                json!({
                    "catalog_generation": 1,
                    "tool_name": write_name,
                    "params": {"value": "blocked"}
                }),
            )]),
            text_reply("plan guard handled"),
            tool_reply(&[(
                "disconnect-echo",
                "ExecuteExtraTool",
                json!({
                    "catalog_generation": 1,
                    "tool_name": echo_name,
                    "params": {"value": "after-disconnect"}
                }),
            )]),
            text_reply("disconnect handled"),
        ],
    ));
    let runner = AgentRunner::new(provider.clone(), registry, RunLimits::default());

    let complete = runner
        .run_turn(turn_request(
            "mcp-runtime-turn",
            "搜索 echo 并执行它",
            PlanGuard::inactive(),
        ))
        .await;
    assert!(
        complete.is_success(),
        "正常 MCP Tool Loop 不应失败：{:?}",
        complete.error
    );
    assert_eq!(complete.state.round_count(), 3);
    assert_eq!(complete.state.step_count(), 2);
    assert_eq!(
        complete
            .final_response
            .as_ref()
            .map(|response| &response.content),
        Some(&vec![ContentBlock::text("echo workflow complete")])
    );

    let invalid = runner
        .run_turn(turn_request(
            "mcp-runtime-invalid",
            "使用错误参数调用 echo",
            PlanGuard::inactive(),
        ))
        .await;
    assert!(
        invalid.is_success(),
        "非法参数应作为配对工具结果交给模型：{:?}",
        invalid.error
    );
    assert_eq!(invalid.state.round_count(), 2);
    assert_eq!(invalid.state.step_count(), 0);

    let plan = runner
        .run_turn(turn_request(
            "mcp-runtime-plan",
            "计划模式下调用写工具",
            PlanGuard::read_only(),
        ))
        .await;
    assert!(
        plan.is_success(),
        "计划守卫拒绝应不终止后续模型回合：{:?}",
        plan.error
    );
    assert_eq!(plan.state.round_count(), 2);
    assert_eq!(plan.state.step_count(), 0);

    let disconnected = runner
        .run_turn(turn_request(
            "mcp-runtime-disconnect",
            "在远端断连后安全结束",
            PlanGuard::inactive(),
        ))
        .await;
    assert!(
        disconnected.is_success(),
        "MCP 断连应形成安全工具错误并完成后续模型回合：{:?}",
        disconnected.error
    );
    assert_eq!(disconnected.state.round_count(), 2);
    assert_eq!(disconnected.state.step_count(), 1);

    let requests = provider.requests().expect("Provider 请求快照应可读取");
    assert_eq!(requests.len(), 9);
    for request in &requests {
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            vec!["ExecuteExtraTool", "ToolSearch"]
        );
        assert!(
            request
                .tools
                .iter()
                .all(|tool| !tool.name.starts_with("mcp__"))
        );
    }

    let ContentBlock::ToolResult {
        tool_result: search_result,
    } = &requests[1].messages[2].content[0]
    else {
        panic!("ToolSearch 第二轮应包含工具结果");
    };
    assert_eq!(search_result.tool_call_id, "search-echo");
    assert!(!search_result.is_error);
    let [ToolResultContent::Text { text: search_text }] = search_result.content.as_slice() else {
        panic!("ToolSearch 结果应只包含一个 JSON 文本块");
    };
    let search_value: Value = serde_json::from_str(search_text).expect("ToolSearch 结果应为 JSON");
    assert_eq!(search_value["catalog_generation"], 1);
    assert_eq!(search_value["tools"][0]["name"], echo_name);

    let ContentBlock::ToolResult {
        tool_result: echo_result,
    } = &requests[2].messages[4].content[0]
    else {
        panic!("ExecuteExtraTool 第二轮应包含 echo 工具结果");
    };
    assert_eq!(echo_result.tool_call_id, "execute-echo");
    assert!(!echo_result.is_error);
    assert_eq!(echo_result.content.len(), 2);
    assert!(matches!(
        &echo_result.content[0],
        ToolResultContent::Text { text } if text == "echo:hello"
    ));
    assert!(matches!(
        &echo_result.content[1],
        ToolResultContent::Text { text } if text == r#"{"structured_content":{"value":"hello"}}"#
    ));

    let ContentBlock::ToolResult {
        tool_result: invalid_result,
    } = &requests[4].messages[2].content[0]
    else {
        panic!("非法参数第二轮应包含配对工具结果");
    };
    assert_eq!(invalid_result.tool_call_id, "invalid-echo");
    assert!(invalid_result.is_error);
    assert!(tool_result_text(invalid_result).contains("deferred_tool_input_invalid"));

    let ContentBlock::ToolResult {
        tool_result: plan_result,
    } = &requests[6].messages[2].content[0]
    else {
        panic!("计划守卫第二轮应包含配对工具结果");
    };
    assert_eq!(plan_result.tool_call_id, "plan-write");
    assert!(plan_result.is_error);
    assert!(tool_result_text(plan_result).contains("计划模式禁止执行"));

    let ContentBlock::ToolResult {
        tool_result: disconnected_result,
    } = &requests[8].messages[2].content[0]
    else {
        panic!("断连第二轮应包含配对工具结果");
    };
    assert_eq!(disconnected_result.tool_call_id, "disconnect-echo");
    assert!(disconnected_result.is_error);
    assert!(tool_result_text(disconnected_result).contains("mcp_unavailable"));

    assert_eq!(remote_calls.load(Ordering::SeqCst), 2);
    assert_eq!(client.close().await, Ok(()));
    let http_requests = server.finish().await;
    assert_eq!(http_requests.len(), 6);
    assert_eq!(http_requests[0].method, "POST");
    assert_eq!(http_requests[1].method, "POST");
    assert_eq!(http_requests[2].method, "POST");
    assert_eq!(http_requests[3].method, "POST");
    assert_eq!(http_requests[4].method, "POST");
    assert_eq!(http_requests[5].method, "DELETE");
    for request in &http_requests {
        assert_eq!(
            request
                .headers
                .get("mcp-protocol-version")
                .map(String::as_str),
            Some(keencode_mcp::DEFAULT_PROTOCOL_VERSION)
        );
    }
    let methods = http_requests[..5]
        .iter()
        .map(|request| {
            serde_json::from_slice::<Value>(&request.body)
                .expect("MCP POST 正文应为 JSON")["method"]
                .as_str()
                .expect("MCP JSON-RPC 请求应包含 method")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        vec![
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/call",
            "tools/call"
        ]
    );
}

/// 提取单个模型工具结果的文本正文。
fn tool_result_text(result: &keencode_model::ToolResult) -> &str {
    let [ToolResultContent::Text { text }] = result.content.as_slice() else {
        panic!("测试工具错误结果应只包含一个文本块");
    };
    text
}
