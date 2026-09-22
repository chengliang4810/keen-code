//! keencode-mcp 真实 stdio 子进程与本机 Streamable HTTP 集成测试。

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use keencode_mcp::{
    AuthToken, CancellationToken, McpAuthProvider, McpClient, McpClientOptions, McpError,
    McpServerConfig, McpToolEffect, StdioServerConfig, StreamableHttpConfig,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn stdio_runs_tools_resources_notifications_and_cleanup() {
    let executable = env!("CARGO_BIN_EXE_keencode-mcp-mock-stdio");
    let sentinel = std::env::temp_dir().join(format!(
        "keencode-mcp-tree-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应晚于 Unix epoch")
            .as_nanos()
    ));
    let mut config = StdioServerConfig::new(executable);
    config.environment.insert(
        "KEENCODE_MCP_TREE_SENTINEL".to_owned(),
        sentinel.to_string_lossy().into_owned(),
    );
    let options = McpClientOptions {
        request_timeout: Duration::from_secs(3),
        shutdown_timeout: Duration::from_secs(1),
        ..McpClientOptions::default()
    };
    let client = McpClient::connect(McpServerConfig::Stdio(config), options)
        .await
        .expect("stdio MCP 应完成握手");
    assert_eq!(client.session().server_info.name, "keencode-mcp-mock");

    let mut notifications = client
        .subscribe_notifications()
        .await
        .expect("通知监听应启动");
    let tools = client.list_tools().await.expect("应读取两页工具");
    assert_eq!(tools.tools().len(), 2);
    assert!(tools.get("task_only").is_none());
    assert_eq!(tools.effect_for("echo"), McpToolEffect::ChangesState);
    assert_eq!(tools.effect_for("write_mock"), McpToolEffect::ChangesState);
    assert_eq!(tools.effect_for("not-listed"), McpToolEffect::ChangesState);
    let notification = tokio::time::timeout(Duration::from_secs(1), notifications.recv())
        .await
        .expect("应及时收到通知")
        .expect("通知通道应保持可用");
    assert_eq!(notification.method, "notifications/message");

    let call = client
        .call_tool("echo", json!({ "value": 42 }))
        .await
        .expect("工具调用应成功");
    assert!(!call.is_error);
    assert_eq!(call.structured_content, Some(json!({ "value": 42 })));
    assert!(
        client
            .call_tool("task_only", json!({}))
            .await
            .expect_err("required-task 工具不得普通调用")
            .to_string()
            .contains("Tasks")
    );
    assert_eq!(
        client.list_resources().await.expect("资源列表应成功")[0].uri,
        "mock://readme"
    );
    assert_eq!(
        client
            .list_resource_templates()
            .await
            .expect("资源模板应成功")[0]
            .uri_template,
        "mock://file/{name}"
    );
    assert_eq!(
        client
            .read_resource("mock://readme")
            .await
            .expect("资源读取应成功")[0]
            .text
            .as_deref(),
        Some("mock resource")
    );
    let remote_error = client
        .read_resource("mock://remote-error")
        .await
        .expect_err("服务端构造的 RPC 错误必须返回调用方");
    assert!(matches!(&remote_error, McpError::Rpc { .. }));
    for rendered in [remote_error.to_string(), format!("{remote_error:?}")] {
        assert_eq!(rendered, "MCP RPC 错误 -32000：服务端返回错误");
        assert!(!rendered.contains("sk-fake-integration-test-only"));
        assert!(!rendered.contains('\r'));
        assert!(!rendered.contains('\n'));
    }
    client.close().await.expect("stdio 子进程应被回收");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!sentinel.exists(), "stdio 子孙进程必须随客户端一起终止");
}

#[tokio::test]
async fn stdio_close_timeout_is_a_hard_deadline() {
    let executable = env!("CARGO_BIN_EXE_keencode-mcp-mock-stdio");
    let mut config = StdioServerConfig::new(executable);
    config
        .environment
        .insert("KEENCODE_MCP_HOLD_AFTER_EOF".to_owned(), "1".to_owned());
    let shutdown_timeout = Duration::from_millis(60);
    let client = McpClient::connect(
        McpServerConfig::Stdio(config),
        McpClientOptions {
            request_timeout: Duration::from_secs(3),
            shutdown_timeout,
            ..McpClientOptions::default()
        },
    )
    .await
    .expect("stdio MCP 应完成握手");

    let started = tokio::time::Instant::now();
    let error = client
        .close()
        .await
        .expect_err("忽略 EOF 的子进程必须触发关闭超时");
    assert!(matches!(error, McpError::Timeout { .. }));
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "关闭不能继续等待无界 child.wait"
    );
}

#[tokio::test]
async fn streamable_http_handles_json_sse_session_and_headers() {
    let handler: Handler = Arc::new(|request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        if request.method == "GET" {
            return TestResponse::sse(vec![json!({
                "jsonrpc": "2.0",
                "method": "notifications/message",
                "params": { "source": "standalone-get" }
            })])
            .with_hold_open(Duration::from_millis(500));
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        let method = message["method"].as_str().expect("请求应包含 method");
        match method {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("http-integration")),
                Some(("MCP-Session-Id", "session-123")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => {
                let notification = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/tools/list_changed",
                    "params": {}
                });
                let response = json_rpc_result(
                    message["id"].clone(),
                    json!({
                        "tools": [{
                            "name": "http_echo",
                            "inputSchema": { "type": "object" },
                            "annotations": { "readOnlyHint": true }
                        }]
                    }),
                );
                TestResponse::sse(vec![notification, response])
                    .with_hold_open(Duration::from_millis(500))
            }
            other => panic!("未预期的 MCP 方法：{other}"),
        }
    });
    let server = TestServer::spawn(5, handler).await;
    let mut config = StreamableHttpConfig::new(server.endpoint.clone());
    config
        .headers
        .insert("Authorization".to_owned(), "Bearer test-secret".to_owned());
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions::default(),
    )
    .await
    .expect("HTTP MCP 应完成握手");
    let mut notifications = client
        .subscribe_notifications()
        .await
        .expect("HTTP GET SSE 监听应启动");
    assert_eq!(
        notifications
            .recv()
            .await
            .expect("独立 GET SSE 通知应进入广播通道")
            .method,
        "notifications/message"
    );
    let started = tokio::time::Instant::now();
    let tools = client.list_tools().await.expect("SSE 工具响应应成功");
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "客户端应在匹配响应事件到达后返回，而不是等待 SSE 连接关闭"
    );
    assert_eq!(tools.tools()[0].name, "http_echo");
    assert_eq!(
        notifications
            .recv()
            .await
            .expect("SSE 通知应进入广播通道")
            .method,
        "notifications/tools/list_changed"
    );
    client.close().await.expect("HTTP 会话应关闭");
    let requests = server.finish().await;
    assert_eq!(requests.len(), 5);
    for request in &requests {
        assert_eq!(
            request
                .headers
                .get("mcp-protocol-version")
                .map(String::as_str),
            Some(keencode_mcp::DEFAULT_PROTOCOL_VERSION)
        );
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-secret")
        );
    }
    for request in &requests {
        let is_initialize = serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|message| message["method"].as_str().map(str::to_owned))
            .as_deref()
            == Some("initialize");
        if !is_initialize {
            assert_eq!(
                request.headers.get("mcp-session-id").map(String::as_str),
                Some("session-123")
            );
        }
    }
}

#[tokio::test]
async fn streamable_http_dynamic_auth_retries_initial_initialize_once_and_carries_bearer() {
    let auth = TestAuthProvider::new("expired-token", 1);
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "GET" {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer refreshed-token")
            );
            return TestResponse::empty(405, "Method Not Allowed");
        }
        if request.method == "DELETE" {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer refreshed-token")
            );
            return TestResponse::empty(200, "OK");
        }
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some(
                if initialize_count_for_handler.load(Ordering::SeqCst) == 0 {
                    "Bearer expired-token"
                } else {
                    "Bearer refreshed-token"
                }
            )
        );
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt = initialize_count_for_handler.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    return TestResponse::unauthorized("Bearer realm=\"mcp\"");
                }
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), initialize_result("dynamic-auth")),
                    Some(("MCP-Session-Id", "dynamic-auth-session")),
                )
            }
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => TestResponse::json(
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                None,
            ),
            other => panic!("未预期的 MCP 方法：{other}"),
        }
    });
    let server = TestServer::spawn(6, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(auth.clone()));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions::default(),
    )
    .await
    .expect("动态认证应在首次 initialize 401 后只重试一次");
    client.list_tools().await.expect("刷新令牌后的请求应成功");
    client
        .subscribe_notifications()
        .await
        .expect("GET SSE 也应携带动态 Bearer");
    client.close().await.expect("DELETE 也应携带动态 Bearer");

    let requests = server.finish().await;
    assert_eq!(requests.len(), 6);
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2);
    let state = auth.snapshot();
    assert_eq!(state.unauthorized_calls, 1, "单次 401 只能触发一次刷新");
    assert_eq!(state.refreshes, 1);
    assert_eq!(
        state.last_challenge.as_deref(),
        Some("Bearer realm=\"mcp\"")
    );
    assert_eq!(state.generation, 2);
    for request in requests.iter().skip(1) {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer refreshed-token")
        );
    }
}

#[tokio::test]
async fn dynamic_auth_generation_change_expires_session_without_replaying_tool_call() {
    let auth = TestAuthProvider::new("old-token", 1);
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer refreshed-token")
            );
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt = initialize_count_for_handler.fetch_add(1, Ordering::SeqCst);
                let session = if attempt == 0 {
                    "old-auth-session"
                } else {
                    "new-auth-session"
                };
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some(if attempt == 0 {
                        "Bearer old-token"
                    } else {
                        "Bearer refreshed-token"
                    })
                );
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), initialize_result("generation-auth")),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => TestResponse::json(
                json_rpc_result(
                    message["id"].clone(),
                    json!({
                        "tools": [{
                            "name": "write_once",
                            "inputSchema": { "type": "object" }
                        }]
                    }),
                ),
                None,
            ),
            "tools/call" => {
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some("Bearer old-token")
                );
                TestResponse::unauthorized("Bearer realm=\"mcp\", error=\"invalid_token\"")
            }
            other => panic!("未预期的 MCP 方法：{other}"),
        }
    });
    let server = TestServer::spawn(7, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(auth.clone()));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions::default(),
    )
    .await
    .expect("初始动态认证会话应建立");
    let error = client
        .call_tool("write_once", json!({ "value": 1 }))
        .await
        .expect_err("认证代次变化后有副作用的 tools/call 不得自动重放");
    assert!(matches!(error, McpError::SessionExpired));
    client.close().await.expect("重新初始化后的会话应关闭");

    let requests = server.finish().await;
    assert_eq!(requests.len(), 7);
    assert_eq!(
        requests
            .iter()
            .filter(|request| {
                serde_json::from_slice::<Value>(&request.body)
                    .ok()
                    .and_then(|message| message["method"].as_str().map(str::to_owned))
                    .as_deref()
                    == Some("tools/call")
            })
            .count(),
        1,
        "tools/call 401 后不得再次发送同一副作用请求"
    );
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2);
    let state = auth.snapshot();
    assert_eq!(state.unauthorized_calls, 1);
    assert_eq!(state.refreshes, 1);
    assert_eq!(state.generation, 2);
}

#[tokio::test]
async fn proactive_auth_rotation_reinitializes_before_reusing_old_http_session() {
    let auth = TestAuthProvider::new("old-token", 1);
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer rotated-token")
            );
            assert_eq!(
                request.headers.get("mcp-session-id").map(String::as_str),
                Some("rotated-session")
            );
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt = initialize_count_for_handler.fetch_add(1, Ordering::SeqCst);
                assert!(!request.headers.contains_key("mcp-session-id"));
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some(if attempt == 0 {
                        "Bearer old-token"
                    } else {
                        "Bearer rotated-token"
                    })
                );
                let session = if attempt == 0 {
                    "old-session"
                } else {
                    "rotated-session"
                };
                TestResponse::json(
                    json_rpc_result(
                        message["id"].clone(),
                        initialize_result("proactive-rotation"),
                    ),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => {
                let session = request
                    .headers
                    .get("mcp-session-id")
                    .map(String::as_str)
                    .expect("initialized 必须携带会话 ID");
                let authorization = request.headers.get("authorization").map(String::as_str);
                match session {
                    "old-session" => assert_eq!(authorization, Some("Bearer old-token")),
                    "rotated-session" => {
                        assert_eq!(authorization, Some("Bearer rotated-token"))
                    }
                    other => panic!("未预期的会话 ID：{other}"),
                }
                TestResponse::empty(202, "Accepted")
            }
            "tools/list" => {
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some("Bearer rotated-token")
                );
                assert_eq!(
                    request.headers.get("mcp-session-id").map(String::as_str),
                    Some("rotated-session")
                );
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                    None,
                )
            }
            other => panic!("未预期的 MCP 方法：{other}"),
        }
    });
    let server = TestServer::spawn(6, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(auth.clone()));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions::default(),
    )
    .await
    .expect("初始动态认证会话应建立");

    auth.rotate("rotated-token", 2);
    client
        .list_tools()
        .await
        .expect("认证代次主动变化后安全的 list_tools 应重新初始化");
    client.close().await.expect("轮换后的会话应关闭");

    let requests = server.finish().await;
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2);
    assert!(!requests.iter().any(|request| {
        request.headers.get("mcp-session-id").map(String::as_str) == Some("old-session")
            && serde_json::from_slice::<Value>(&request.body)
                .ok()
                .and_then(|message| message["method"].as_str().map(str::to_owned))
                .as_deref()
                == Some("tools/list")
    }));
}

#[tokio::test]
async fn revoked_dynamic_auth_does_not_send_anonymous_tools_call_on_old_session() {
    let auth = TestAuthProvider::new("old-token", 0);
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let unexpected_tools_call = Arc::new(AtomicUsize::new(0));
    let unexpected_tools_call_for_handler = Arc::clone(&unexpected_tools_call);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            assert_eq!(
                request.headers.get("mcp-session-id").map(String::as_str),
                Some("revoked-reinitialized-session")
            );
            assert!(!request.headers.contains_key("authorization"));
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt = initialize_count_for_handler.fetch_add(1, Ordering::SeqCst);
                assert!(!request.headers.contains_key("mcp-session-id"));
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    (attempt == 0).then_some("Bearer old-token")
                );
                let session = if attempt == 0 {
                    "revoked-old-session"
                } else {
                    "revoked-reinitialized-session"
                };
                TestResponse::json(
                    json_rpc_result(
                        message["id"].clone(),
                        initialize_result_with_tool_list_changed("revoked-auth", false),
                    ),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => {
                let session = request
                    .headers
                    .get("mcp-session-id")
                    .map(String::as_str)
                    .expect("initialized 必须携带会话 ID");
                let authorization = request.headers.get("authorization").map(String::as_str);
                match session {
                    "revoked-old-session" => {
                        assert_eq!(authorization, Some("Bearer old-token"))
                    }
                    "revoked-reinitialized-session" => assert!(authorization.is_none()),
                    other => panic!("未预期的会话 ID：{other}"),
                }
                TestResponse::empty(202, "Accepted")
            }
            "tools/list" => {
                match request.headers.get("mcp-session-id").map(String::as_str) {
                    Some("revoked-old-session") => assert_eq!(
                        request.headers.get("authorization").map(String::as_str),
                        Some("Bearer old-token")
                    ),
                    Some("revoked-reinitialized-session") => {
                        assert!(!request.headers.contains_key("authorization"))
                    }
                    other => panic!("未预期的 tools/list 会话 ID：{other:?}"),
                }
                TestResponse::json(
                    json_rpc_result(
                        message["id"].clone(),
                        json!({
                            "tools": [{
                                "name": "write_once",
                                "inputSchema": { "type": "object" }
                            }]
                        }),
                    ),
                    None,
                )
            }
            "tools/call" => {
                unexpected_tools_call_for_handler.fetch_add(1, Ordering::SeqCst);
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), json!({ "content": [] })),
                    None,
                )
            }
            other => panic!("未预期的 MCP 方法：{other}"),
        }
    });
    let server = TestServer::spawn(7, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(auth.clone()));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions::default(),
    )
    .await
    .expect("初始动态认证会话应建立");
    client.list_tools().await.expect("撤销前应缓存工具列表");

    auth.revoke(1);
    let call_result = client.call_tool("write_once", json!({})).await;
    assert!(matches!(&call_result, Err(McpError::SessionExpired)));

    // 正常路径已经在 SessionExpired 后重新初始化；额外请求用于让错误路径也
    // 走完同样数量的本地 HTTP 连接，便于收集服务端记录。
    if call_result.is_ok() {
        client
            .list_tools()
            .await
            .expect("异常路径应仍可读取工具列表");
        client
            .list_tools()
            .await
            .expect("异常路径应仍可读取工具列表");
    } else {
        client
            .list_tools()
            .await
            .expect("重新初始化后的工具列表应可读取");
    }
    let _ = client.close().await;

    let requests = server.finish().await;
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2);
    assert_eq!(unexpected_tools_call.load(Ordering::SeqCst), 0);
    assert!(!requests.iter().any(|request| {
        serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|message| message["method"].as_str().map(str::to_owned))
            .as_deref()
            == Some("tools/call")
    }));
}

#[tokio::test]
async fn proactive_auth_rotation_expires_before_starting_old_http_get_listener() {
    let auth = TestAuthProvider::new("old-token", 1);
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let old_get_count = Arc::new(AtomicUsize::new(0));
    let old_get_count_for_handler = Arc::clone(&old_get_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "GET" {
            if request.headers.get("mcp-session-id").map(String::as_str) == Some("get-old-session")
            {
                old_get_count_for_handler.fetch_add(1, Ordering::SeqCst);
            }
            return TestResponse::empty(405, "Method Not Allowed");
        }
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt = initialize_count_for_handler.fetch_add(1, Ordering::SeqCst);
                assert!(!request.headers.contains_key("mcp-session-id"));
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some(if attempt == 0 {
                        "Bearer old-token"
                    } else {
                        "Bearer rotated-token"
                    })
                );
                let session = if attempt == 0 {
                    "get-old-session"
                } else {
                    "get-rotated-session"
                };
                TestResponse::json(
                    json_rpc_result(
                        message["id"].clone(),
                        initialize_result("proactive-get-rotation"),
                    ),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => {
                let session = request
                    .headers
                    .get("mcp-session-id")
                    .map(String::as_str)
                    .expect("initialized 必须携带会话 ID");
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some(match session {
                        "get-old-session" => "Bearer old-token",
                        "get-rotated-session" => "Bearer rotated-token",
                        other => panic!("未预期的会话 ID：{other}"),
                    })
                );
                TestResponse::empty(202, "Accepted")
            }
            "tools/list" => {
                match request.headers.get("mcp-session-id").map(String::as_str) {
                    Some("get-old-session") => assert_eq!(
                        request.headers.get("authorization").map(String::as_str),
                        Some("Bearer old-token")
                    ),
                    Some("get-rotated-session") => assert_eq!(
                        request.headers.get("authorization").map(String::as_str),
                        Some("Bearer rotated-token")
                    ),
                    other => panic!("未预期的 tools/list 会话 ID：{other:?}"),
                }
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                    None,
                )
            }
            other => panic!("未预期的 MCP 方法：{other}"),
        }
    });
    let server = TestServer::spawn(9, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(auth.clone()));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions::default(),
    )
    .await
    .expect("初始动态认证会话应建立");

    auth.rotate("rotated-token", 2);
    let subscribe_result = client.subscribe_notifications().await;
    assert!(matches!(&subscribe_result, Err(McpError::SessionExpired)));

    // 错误实现会让 GET 伪装成功；按结果补齐连接数，保证两条路径都能
    // 等待本地服务完整收集请求并断言是否真的发出了旧会话 GET。
    if subscribe_result.is_err() {
        client
            .list_tools()
            .await
            .expect("代次变化后应重新初始化并读取工具列表");
        client
            .list_tools()
            .await
            .expect("重新初始化后的工具列表应可再次读取");
        client
            .list_tools()
            .await
            .expect("重新初始化后的工具列表应可再次读取");
    } else {
        client
            .list_tools()
            .await
            .expect("异常 GET 路径仍应读取工具列表");
        client
            .list_tools()
            .await
            .expect("异常 GET 路径仍应读取工具列表");
        client
            .list_tools()
            .await
            .expect("异常 GET 路径仍应读取工具列表");
        client
            .list_tools()
            .await
            .expect("异常 GET 路径仍应读取工具列表");
        client
            .list_tools()
            .await
            .expect("异常 GET 路径仍应读取工具列表");
    }
    client.close().await.expect("轮换后的会话应关闭");

    let requests = server.finish().await;
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2);
    assert_eq!(old_get_count.load(Ordering::SeqCst), 0);
    assert!(!requests.iter().any(|request| {
        request.method == "GET"
            && request.headers.get("mcp-session-id").map(String::as_str) == Some("get-old-session")
    }));
}

#[tokio::test]
async fn proactive_auth_rotation_expires_before_terminating_old_http_session() {
    let auth = TestAuthProvider::new("old-token", 1);
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let old_session_delete_count = Arc::new(AtomicUsize::new(0));
    let old_session_delete_count_for_handler = Arc::clone(&old_session_delete_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            if request.headers.get("mcp-session-id").map(String::as_str)
                == Some("delete-old-session")
            {
                old_session_delete_count_for_handler.fetch_add(1, Ordering::SeqCst);
            }
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt = initialize_count_for_handler.fetch_add(1, Ordering::SeqCst);
                assert!(!request.headers.contains_key("mcp-session-id"));
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some(if attempt == 0 {
                        "Bearer old-token"
                    } else {
                        "Bearer rotated-token"
                    })
                );
                let session = if attempt == 0 {
                    "delete-old-session"
                } else {
                    "delete-second-session"
                };
                TestResponse::json(
                    json_rpc_result(
                        message["id"].clone(),
                        initialize_result("proactive-delete-rotation"),
                    ),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => {
                let session = request
                    .headers
                    .get("mcp-session-id")
                    .map(String::as_str)
                    .expect("initialized 必须携带会话 ID");
                assert_eq!(
                    request.headers.get("authorization").map(String::as_str),
                    Some(match session {
                        "delete-old-session" => "Bearer old-token",
                        "delete-second-session" => "Bearer rotated-token",
                        other => panic!("未预期的会话 ID：{other}"),
                    })
                );
                TestResponse::empty(202, "Accepted")
            }
            other => panic!("未预期的 MCP 方法：{other}"),
        }
    });
    let server = TestServer::spawn(5, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(auth.clone()));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config.clone()),
        McpClientOptions::default(),
    )
    .await
    .expect("初始动态认证会话应建立");

    auth.rotate("rotated-token", 2);
    let close_result = client.close().await;
    assert!(matches!(&close_result, Err(McpError::SessionExpired)));

    // 正确路径不会发送第一个 DELETE；错误路径会发送它。通过第二个本地
    // 客户端补齐服务端连接数，最终仍可检查两条路径的请求记录。
    if close_result.is_err() {
        let second_client = McpClient::connect(
            McpServerConfig::StreamableHttp(config),
            McpClientOptions::default(),
        )
        .await
        .expect("第二个动态认证会话应建立");
        second_client.close().await.expect("第二个会话应正常关闭");
    } else {
        let _second_client = McpClient::connect(
            McpServerConfig::StreamableHttp(config),
            McpClientOptions::default(),
        )
        .await
        .expect("第二个动态认证会话应建立");
    }

    let requests = server.finish().await;
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2);
    assert_eq!(old_session_delete_count.load(Ordering::SeqCst), 0);
    assert!(!requests.iter().any(|request| {
        request.method == "DELETE"
            && request.headers.get("mcp-session-id").map(String::as_str)
                == Some("delete-old-session")
    }));
}

#[tokio::test]
async fn pagination_rejects_repeated_cursor_and_maximum_pages() {
    let repeated_handler: Handler = Arc::new(|request| pagination_response(request, "same"));
    let repeated_server = TestServer::spawn(5, repeated_handler).await;
    let repeated_client = http_client(&repeated_server.endpoint, 10, Duration::from_secs(2)).await;
    let error = repeated_client
        .list_tools()
        .await
        .expect_err("重复游标必须触发熔断");
    assert!(matches!(error, McpError::Pagination { .. }));
    assert!(error.to_string().contains("重复返回游标"));
    repeated_client.close().await.expect("会话应关闭");
    repeated_server.finish().await;

    let maximum_handler: Handler = Arc::new(|request| pagination_response(request, "next"));
    let maximum_server = TestServer::spawn(4, maximum_handler).await;
    let maximum_client = http_client(&maximum_server.endpoint, 1, Duration::from_secs(2)).await;
    let error = maximum_client
        .list_tools()
        .await
        .expect_err("达到最大页数且仍有游标时必须失败");
    assert!(matches!(error, McpError::Pagination { .. }));
    assert!(error.to_string().contains("最大页数 1"));
    maximum_client.close().await.expect("会话应关闭");
    maximum_server.finish().await;
}

#[tokio::test]
async fn timeout_sends_cancel_notification_and_cancelled_token_stops_request() {
    let handler: Handler = Arc::new(|request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("timeout")),
                Some(("MCP-Session-Id", "timeout-session")),
            ),
            "notifications/initialized" | "notifications/cancelled" => {
                TestResponse::empty(202, "Accepted")
            }
            "tools/list" => TestResponse::json(
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                None,
            )
            .with_delay(Duration::from_millis(150)),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(5, handler).await;
    let client = http_client(&server.endpoint, 10, Duration::from_millis(40)).await;
    let error = client.list_tools().await.expect_err("慢响应必须超时");
    assert!(matches!(error, McpError::Timeout { .. }));

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let error = client
        .list_tools_with_cancellation(&cancelled)
        .await
        .expect_err("预先取消的请求不得发往服务端");
    assert!(matches!(error, McpError::Cancelled { .. }));
    client.close().await.expect("会话应关闭");
    let requests = server.finish().await;
    assert!(requests.iter().any(|request| {
        serde_json::from_slice::<Value>(&request.body)
            .ok()
            .and_then(|message| message["method"].as_str().map(str::to_owned))
            .as_deref()
            == Some("notifications/cancelled")
    }));
}

#[tokio::test]
async fn close_cancels_active_request_before_lifecycle_deadline() {
    let handler: Handler = Arc::new(|request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("close-deadline")),
                Some(("MCP-Session-Id", "close-deadline-session")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => TestResponse::json(
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                None,
            )
            .with_delay(Duration::from_millis(600)),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(4, handler).await;
    let shutdown_timeout = Duration::from_millis(150);
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(&server.endpoint)),
        McpClientOptions {
            request_timeout: Duration::from_secs(5),
            shutdown_timeout,
            ..McpClientOptions::default()
        },
    )
    .await
    .expect("HTTP MCP 应完成握手");
    let requesting_client = client.clone();
    let request_task = tokio::spawn(async move { requesting_client.list_tools().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if server.captured.lock().expect("请求锁不应中毒").len() >= 3 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("慢请求应已经到达服务端");

    let started = tokio::time::Instant::now();
    client.close().await.expect("关闭应取消活跃请求并终止会话");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "关闭不能等待完整 request_timeout"
    );
    let request_result = tokio::time::timeout(Duration::from_secs(1), request_task)
        .await
        .expect("活跃请求应随关闭及时结束")
        .expect("请求任务不应 panic");
    assert!(request_result.is_err());
    server.finish().await;
}

#[tokio::test]
async fn expired_http_session_reinitializes_once_and_retries_request() {
    let initialize_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt =
                    initialize_count_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let session = if attempt == 0 {
                    "session-old"
                } else {
                    "session-new"
                };
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), initialize_result("session-recovery")),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list"
                if request.headers.get("mcp-session-id").map(String::as_str)
                    == Some("session-old") =>
            {
                TestResponse::empty(404, "Not Found")
            }
            "tools/list" => TestResponse::json(
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                None,
            ),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(7, handler).await;
    let client = http_client(&server.endpoint, 10, Duration::from_secs(2)).await;
    assert!(
        client
            .list_tools()
            .await
            .expect("过期会话应自动恢复")
            .tools()
            .is_empty()
    );
    assert_eq!(
        initialize_count.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    client.close().await.expect("恢复后的会话应关闭");
    let requests = server.finish().await;
    assert!(requests.iter().any(|request| {
        request.headers.get("mcp-session-id").map(String::as_str) == Some("session-new")
    }));
}

#[tokio::test]
async fn initialize_timeout_never_sends_cancel_notification() {
    let handler: Handler = Arc::new(|request| {
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        assert_eq!(message["method"], "initialize");
        TestResponse::json(
            json_rpc_result(message["id"].clone(), initialize_result("slow-initialize")),
            None,
        )
        .with_delay(Duration::from_millis(120))
    });
    let server = TestServer::spawn(1, handler).await;
    let options = McpClientOptions {
        request_timeout: Duration::from_millis(30),
        ..McpClientOptions::default()
    };
    let error = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(&server.endpoint)),
        options,
    )
    .await
    .expect_err("慢 initialize 必须超时");
    assert!(matches!(error, McpError::Timeout { .. }));
    let requests = server.finish().await;
    assert_eq!(requests.len(), 1, "initialize 超时不得额外发送取消通知");
}

#[tokio::test]
async fn pagination_enforces_total_item_limit() {
    let handler: Handler = Arc::new(|request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("bounded-list")),
                Some(("MCP-Session-Id", "bounded-session")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => TestResponse::json(
                json_rpc_result(
                    message["id"].clone(),
                    json!({
                        "tools": [
                            { "name": "one", "inputSchema": { "type": "object" } },
                            { "name": "two", "inputSchema": { "type": "object" } }
                        ]
                    }),
                ),
                None,
            ),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(4, handler).await;
    let options = McpClientOptions {
        max_total_items: 1,
        ..McpClientOptions::default()
    };
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(&server.endpoint)),
        options,
    )
    .await
    .expect("客户端应初始化");
    let error = client.list_tools().await.expect_err("累计条目上限必须生效");
    assert!(error.to_string().contains("累计条目数超过上限 1"));
    client.close().await.expect("会话应关闭");
    server.finish().await;
}

#[tokio::test]
async fn post_sse_ignores_priming_data_and_resumes_with_last_event_id() {
    let response_id = Arc::new(StdMutex::new(None::<Value>));
    let response_id_for_handler = Arc::clone(&response_id);
    let handler: Handler = Arc::new(move |request| {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer sse-token")
        );
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        if request.method == "GET" {
            assert_eq!(
                request.headers.get("last-event-id").map(String::as_str),
                Some("resume-1")
            );
            let id = response_id_for_handler
                .lock()
                .expect("响应 ID 锁不应中毒")
                .clone()
                .expect("POST 应先记录请求 ID");
            return TestResponse::raw_sse(
                format!(
                    "id: resume-2\r\ndata: {}\r\n\r\n",
                    json_rpc_result(id, json!({ "tools": [] }))
                )
                .into_bytes(),
            );
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("sse-resume")),
                Some(("MCP-Session-Id", "resume-session")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => {
                *response_id_for_handler.lock().expect("响应 ID 锁不应中毒") =
                    Some(message["id"].clone());
                TestResponse::raw_sse(b"id: resume-1\r\nretry: 1\r\ndata:\r\n\r\n".to_vec())
            }
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(5, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(TestAuthProvider::new("sse-token", 1)));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions {
            max_pages: 10,
            request_timeout: Duration::from_secs(2),
            ..McpClientOptions::default()
        },
    )
    .await
    .expect("动态认证 HTTP MCP 应完成握手");
    assert!(
        client
            .list_tools()
            .await
            .expect("断开的 POST SSE 应通过 Last-Event-ID 恢复")
            .tools()
            .is_empty()
    );
    client.close().await.expect("会话应关闭");
    server.finish().await;
}

#[tokio::test]
async fn http_sse_answers_server_ping_before_completing_request() {
    let ping_responses = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ping_responses_for_handler = Arc::clone(&ping_responses);
    let handler: Handler = Arc::new(move |request| {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer ping-token")
        );
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            assert_eq!(message["id"], "server-ping");
            assert_eq!(message["result"], json!({}));
            ping_responses_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return TestResponse::empty(202, "Accepted");
        };
        match method {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("server-ping")),
                Some(("MCP-Session-Id", "ping-session")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => TestResponse::sse(vec![
                json!({ "jsonrpc": "2.0", "id": "server-ping", "method": "ping" }),
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
            ]),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(5, handler).await;
    let mut config = StreamableHttpConfig::new(&server.endpoint);
    config.auth_provider = Some(Arc::new(TestAuthProvider::new("ping-token", 1)));
    let client = McpClient::connect(
        McpServerConfig::StreamableHttp(config),
        McpClientOptions {
            max_pages: 10,
            request_timeout: Duration::from_secs(2),
            ..McpClientOptions::default()
        },
    )
    .await
    .expect("动态认证 HTTP MCP 应完成握手");
    client.list_tools().await.expect("ping 后的工具响应应成功");
    assert_eq!(ping_responses.load(std::sync::atomic::Ordering::SeqCst), 1);
    client.close().await.expect("会话应关闭");
    server.finish().await;
}

#[tokio::test]
async fn pagination_bounds_individual_and_cumulative_cursor_bytes_without_echoing_values() {
    let single_handler: Handler = Arc::new(|request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("cursor-single")),
                Some(("MCP-Session-Id", "cursor-single-session")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => TestResponse::json(
                json_rpc_result(
                    message["id"].clone(),
                    json!({ "tools": [], "nextCursor": "single-secret-cursor" }),
                ),
                None,
            ),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let single_server = TestServer::spawn(4, single_handler).await;
    let single_options = McpClientOptions {
        max_cursor_bytes: 8,
        ..McpClientOptions::default()
    };
    let single_client = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(&single_server.endpoint)),
        single_options,
    )
    .await
    .expect("客户端应初始化");
    let error = single_client
        .list_tools()
        .await
        .expect_err("单个超长游标必须拒绝");
    assert!(error.to_string().contains("单个游标超过"));
    assert!(!error.to_string().contains("single-secret-cursor"));
    single_client.close().await.expect("会话应关闭");
    single_server.finish().await;

    let cumulative_handler: Handler = Arc::new(|request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("cursor-total")),
                Some(("MCP-Session-Id", "cursor-total-session")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list" => {
                let cursor = if message["params"].get("cursor").is_some() {
                    "bbbbbb"
                } else {
                    "aaaaaa"
                };
                TestResponse::json(
                    json_rpc_result(
                        message["id"].clone(),
                        json!({ "tools": [], "nextCursor": cursor }),
                    ),
                    None,
                )
            }
            other => panic!("未预期的方法：{other}"),
        }
    });
    let cumulative_server = TestServer::spawn(5, cumulative_handler).await;
    let cumulative_options = McpClientOptions {
        max_cursor_bytes: 8,
        max_total_cursor_bytes: 10,
        ..McpClientOptions::default()
    };
    let cumulative_client = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(&cumulative_server.endpoint)),
        cumulative_options,
    )
    .await
    .expect("客户端应初始化");
    let error = cumulative_client
        .list_tools()
        .await
        .expect_err("累计游标字节上限必须生效");
    assert!(error.to_string().contains("累计游标超过"));
    assert!(!error.to_string().contains("aaaaaa"));
    assert!(!error.to_string().contains("bbbbbb"));
    cumulative_client.close().await.expect("会话应关闭");
    cumulative_server.finish().await;
}

#[tokio::test]
async fn concurrent_session_expiry_reinitializes_once_and_retries_both_requests() {
    let initialize_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let old_request_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let old_request_count_for_handler = Arc::clone(&old_request_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt =
                    initialize_count_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let session = if attempt == 0 {
                    "old-session"
                } else {
                    "new-session"
                };
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), initialize_result("concurrent-404")),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list"
                if request.headers.get("mcp-session-id").map(String::as_str)
                    == Some("old-session") =>
            {
                let attempt =
                    old_request_count_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                TestResponse::empty(404, "Not Found").with_delay(if attempt == 0 {
                    Duration::from_millis(20)
                } else {
                    Duration::from_millis(100)
                })
            }
            "tools/list" => TestResponse::json(
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                None,
            ),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(9, handler).await;
    let client = http_client(&server.endpoint, 10, Duration::from_secs(2)).await;
    let (left, right) = tokio::join!(client.list_tools(), client.list_tools());
    assert!(left.expect("第一个请求应恢复").tools().is_empty());
    assert!(right.expect("第二个请求应恢复").tools().is_empty());
    assert_eq!(
        initialize_count.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    client.close().await.expect("会话应关闭");
    server.finish().await;
}

#[tokio::test]
async fn listener_restart_failure_does_not_repeat_completed_reinitialization() {
    let initialize_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let old_request_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let old_request_count_for_handler = Arc::clone(&old_request_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        if request.method == "GET" {
            return if request.headers.get("mcp-session-id").map(String::as_str)
                == Some("old-session")
            {
                TestResponse::raw_sse("id: old-listener\r\n\r\n")
                    .with_hold_open(Duration::from_millis(500))
            } else {
                TestResponse::empty(500, "Internal Server Error")
            };
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt =
                    initialize_count_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let session = if attempt == 0 {
                    "old-session"
                } else {
                    "new-session"
                };
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), initialize_result("listener-restart")),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list"
                if request.headers.get("mcp-session-id").map(String::as_str)
                    == Some("old-session") =>
            {
                let attempt =
                    old_request_count_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                TestResponse::empty(404, "Not Found").with_delay(if attempt == 0 {
                    Duration::from_millis(20)
                } else {
                    Duration::from_millis(100)
                })
            }
            "tools/list" => TestResponse::json(
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                None,
            ),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(10, handler).await;
    let client = http_client(&server.endpoint, 10, Duration::from_secs(2)).await;
    let _notifications = client
        .subscribe_notifications()
        .await
        .expect("旧会话 GET 监听应启动");
    let (left, right) = tokio::join!(client.list_tools(), client.list_tools());
    assert_eq!(
        [&left, &right]
            .into_iter()
            .filter(|result| result.is_ok())
            .count(),
        1,
        "listener 重启失败不应让等待中的请求重复初始化"
    );
    assert_eq!(
        initialize_count.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    client.close().await.expect("会话应关闭");
    server.finish().await;
}

#[tokio::test]
async fn delayed_old_get_404_cannot_clear_reinitialized_session() {
    let initialize_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let initialize_count_for_handler = Arc::clone(&initialize_count);
    let handler: Handler = Arc::new(move |request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        if request.method == "GET" {
            return if request.headers.get("mcp-session-id").map(String::as_str)
                == Some("old-session")
            {
                TestResponse::empty(404, "Not Found").with_delay(Duration::from_millis(200))
            } else {
                TestResponse::empty(405, "Method Not Allowed")
            };
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => {
                let attempt =
                    initialize_count_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let session = if attempt == 0 {
                    "old-session"
                } else {
                    "new-session"
                };
                TestResponse::json(
                    json_rpc_result(message["id"].clone(), initialize_result("delayed-get-404")),
                    Some(("MCP-Session-Id", session)),
                )
            }
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            "tools/list"
                if request.headers.get("mcp-session-id").map(String::as_str)
                    == Some("old-session") =>
            {
                TestResponse::empty(404, "Not Found")
            }
            "tools/list" => TestResponse::json(
                json_rpc_result(message["id"].clone(), json!({ "tools": [] })),
                None,
            ),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(10, handler).await;
    let client = http_client(&server.endpoint, 10, Duration::from_secs(2)).await;
    let listener_client = client.clone();
    let listener = tokio::spawn(async move { listener_client.subscribe_notifications().await });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if server
                .captured
                .lock()
                .expect("请求记录锁不应中毒")
                .iter()
                .any(|request| request.method == "GET")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("GET 请求应及时发出");
    client.list_tools().await.expect("POST 404 应恢复到新会话");
    assert!(listener.await.expect("监听任务不应 panic").is_err());
    client
        .list_tools()
        .await
        .expect("旧 GET 的迟到 404 不得清除新会话");
    assert_eq!(
        initialize_count.load(std::sync::atomic::Ordering::SeqCst),
        2
    );
    client.close().await.expect("会话应关闭");
    server.finish().await;
}

#[tokio::test]
async fn session_id_must_be_visible_ascii_and_transport_errors_redact_query() {
    let handler: Handler = Arc::new(|request| {
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        TestResponse::json(
            json_rpc_result(message["id"].clone(), initialize_result("bad-session")),
            Some(("MCP-Session-Id", "bad id")),
        )
    });
    let server = TestServer::spawn(1, handler).await;
    let error = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(&server.endpoint)),
        McpClientOptions::default(),
    )
    .await
    .expect_err("包含空格的 Session ID 必须拒绝");
    assert!(error.to_string().contains("可见 ASCII"));
    assert!(!error.to_string().contains("bad id"));
    server.finish().await;

    let secret = "query-secret-value";
    let unused_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("应临时绑定未使用端口");
    let unused_address = unused_listener.local_addr().expect("应读取临时端口");
    drop(unused_listener);
    let error = McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(format!(
            "http://{unused_address}/mcp?token={secret}"
        ))),
        McpClientOptions {
            request_timeout: Duration::from_millis(100),
            ..McpClientOptions::default()
        },
    )
    .await
    .expect_err("未监听端口应连接失败");
    assert!(!error.to_string().contains(secret));
}

#[tokio::test]
async fn get_listener_startup_failure_is_reported() {
    let handler: Handler = Arc::new(|request| {
        if request.method == "DELETE" {
            return TestResponse::empty(200, "OK");
        }
        if request.method == "GET" {
            return TestResponse::empty(500, "Internal Server Error");
        }
        let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
        match message["method"].as_str().expect("请求应包含 method") {
            "initialize" => TestResponse::json(
                json_rpc_result(message["id"].clone(), initialize_result("listener-error")),
                Some(("MCP-Session-Id", "listener-error-session")),
            ),
            "notifications/initialized" => TestResponse::empty(202, "Accepted"),
            other => panic!("未预期的方法：{other}"),
        }
    });
    let server = TestServer::spawn(4, handler).await;
    let client = http_client(&server.endpoint, 10, Duration::from_secs(2)).await;
    let error = client
        .subscribe_notifications()
        .await
        .expect_err("GET 监听初始 HTTP 错误必须返回调用方");
    assert!(matches!(error, McpError::Transport(_)));
    client.close().await.expect("会话应关闭");
    server.finish().await;
}

/// 启动使用给定分页游标的 HTTP MCP 客户端。
async fn http_client(endpoint: &str, max_pages: usize, request_timeout: Duration) -> McpClient {
    let options = McpClientOptions {
        max_pages,
        request_timeout,
        ..McpClientOptions::default()
    };
    McpClient::connect(
        McpServerConfig::StreamableHttp(StreamableHttpConfig::new(endpoint)),
        options,
    )
    .await
    .expect("HTTP MCP 应完成握手")
}

/// 为分页保护测试返回初始化、通知、分页与会话关闭响应。
fn pagination_response(request: TestRequest, cursor: &str) -> TestResponse {
    if request.method == "DELETE" {
        return TestResponse::empty(200, "OK");
    }
    let message: Value = serde_json::from_slice(&request.body).expect("请求体应为 JSON");
    match message["method"].as_str().expect("请求应包含 method") {
        "initialize" => TestResponse::json(
            json_rpc_result(message["id"].clone(), initialize_result("pagination")),
            Some(("MCP-Session-Id", "pagination-session")),
        ),
        "notifications/initialized" => TestResponse::empty(202, "Accepted"),
        "tools/list" => TestResponse::json(
            json_rpc_result(
                message["id"].clone(),
                json!({ "tools": [], "nextCursor": cursor }),
            ),
            None,
        ),
        other => panic!("未预期的方法：{other}"),
    }
}

/// 构造支持工具与资源的 initialize 结果。
fn initialize_result(server_name: &str) -> Value {
    initialize_result_with_tool_list_changed(server_name, true)
}

/// 构造可控制 tools/listChanged 标志的 initialize 结果。
fn initialize_result_with_tool_list_changed(server_name: &str, list_changed: bool) -> Value {
    json!({
        "protocolVersion": keencode_mcp::DEFAULT_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": list_changed },
            "resources": { "subscribe": false, "listChanged": false }
        },
        "serverInfo": { "name": server_name, "version": "1.0.0" }
    })
}

/// 构造 JSON-RPC 成功响应。
fn json_rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

type Handler = Arc<dyn Fn(TestRequest) -> TestResponse + Send + Sync>;

/// 本机 TCP 测试服务及其捕获的 HTTP 请求。
struct TestServer {
    endpoint: String,
    captured: Arc<StdMutex<Vec<TestRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    /// 在随机回环端口启动指定请求数量的服务。
    async fn spawn(expected_requests: usize, handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("应绑定回环端口");
        let endpoint = format!(
            "http://{}/mcp",
            listener.local_addr().expect("应获得监听地址")
        );
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let captured_for_task = Arc::clone(&captured);
        let task = tokio::spawn(async move {
            let mut connections = Vec::with_capacity(expected_requests);
            for _ in 0..expected_requests {
                let (mut socket, _) = listener.accept().await.expect("应接受 HTTP 连接");
                let captured = Arc::clone(&captured_for_task);
                let handler = Arc::clone(&handler);
                connections.push(tokio::spawn(async move {
                    let request = read_http_request(&mut socket).await;
                    captured
                        .lock()
                        .expect("请求记录锁不应中毒")
                        .push(request.clone());
                    let response = handler(request);
                    if !response.delay.is_zero() {
                        tokio::time::sleep(response.delay).await;
                    }
                    let bytes = response.to_http_bytes();
                    let _ = socket.write_all(&bytes).await;
                    if !response.hold_open.is_zero() {
                        tokio::time::sleep(response.hold_open).await;
                    }
                    let _ = socket.shutdown().await;
                }));
            }
            for connection in connections {
                connection.await.expect("HTTP 连接任务不应 panic");
            }
        });
        Self {
            endpoint,
            captured,
            task,
        }
    }

    /// 等待服务处理完预期请求并返回捕获记录。
    async fn finish(self) -> Vec<TestRequest> {
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("测试服务应及时结束")
            .expect("测试服务任务不应 panic");
        self.captured.lock().expect("请求记录锁不应中毒").clone()
    }
}

/// 测试服务捕获的单个 HTTP 请求。
#[derive(Clone)]
struct TestRequest {
    method: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

/// 可控的动态认证提供方，用于验证令牌代次和 401 刷新边界。
#[derive(Clone)]
struct TestAuthProvider {
    state: Arc<StdMutex<TestAuthState>>,
}

/// 测试认证提供方的可观察状态；不保存到生产代码。
#[derive(Clone)]
struct TestAuthState {
    token: String,
    generation: u64,
    authorized: bool,
    unauthorized_calls: usize,
    refreshes: usize,
    last_challenge: Option<String>,
}

impl TestAuthProvider {
    /// 创建使用指定初始令牌和代次的测试认证提供方。
    fn new(token: &str, generation: u64) -> Self {
        Self {
            state: Arc::new(StdMutex::new(TestAuthState {
                token: token.to_owned(),
                generation,
                authorized: true,
                unauthorized_calls: 0,
                refreshes: 0,
                last_challenge: None,
            })),
        }
    }

    /// 读取测试认证状态，供断言使用。
    fn snapshot(&self) -> TestAuthState {
        self.state.lock().expect("认证状态锁不应中毒").clone()
    }

    /// 主动轮换测试令牌，模拟无 401 的外部刷新或密钥替换。
    fn rotate(&self, token: &str, generation: u64) {
        let mut state = self.state.lock().expect("认证状态锁不应中毒");
        state.token = token.to_owned();
        state.generation = generation;
        state.authorized = true;
    }

    /// 撤销测试令牌并推进代次，模拟认证提供方返回 None。
    fn revoke(&self, generation: u64) {
        let mut state = self.state.lock().expect("认证状态锁不应中毒");
        state.generation = generation;
        state.authorized = false;
    }
}

#[async_trait]
impl McpAuthProvider for TestAuthProvider {
    /// 返回当前测试令牌及其代次。
    async fn access_token(&self) -> Result<Option<AuthToken>, McpError> {
        let state = self.state.lock().expect("认证状态锁不应中毒");
        Ok(state.authorized.then(|| AuthToken {
            token: state.token.clone(),
            generation: state.generation,
        }))
    }

    /// 模拟只在触发请求仍使用当前代次时执行一次令牌刷新。
    async fn on_unauthorized(
        &self,
        sent_generation: u64,
        www_authenticate: Option<&str>,
    ) -> Result<(), McpError> {
        let mut state = self.state.lock().expect("认证状态锁不应中毒");
        state.unauthorized_calls += 1;
        state.last_challenge = www_authenticate.map(str::to_owned);
        if state.generation == sent_generation {
            state.generation += 1;
            state.token = "refreshed-token".to_owned();
            state.authorized = true;
            state.refreshes += 1;
        }
        Ok(())
    }
}

/// 测试服务返回的原始 HTTP 响应。
struct TestResponse {
    status: u16,
    reason: &'static str,
    content_type: Option<&'static str>,
    extra_header: Option<(&'static str, &'static str)>,
    body: Vec<u8>,
    include_content_length: bool,
    delay: Duration,
    hold_open: Duration,
}

impl TestResponse {
    /// 构造 JSON 200 响应。
    fn json(value: Value, extra_header: Option<(&'static str, &'static str)>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: Some("application/json"),
            extra_header,
            body: serde_json::to_vec(&value).expect("JSON 响应应可序列化"),
            include_content_length: true,
            delay: Duration::ZERO,
            hold_open: Duration::ZERO,
        }
    }

    /// 构造包含多个 data 事件的 SSE 200 响应。
    fn sse(messages: Vec<Value>) -> Self {
        let mut body = String::new();
        for message in messages {
            write!(&mut body, "data: {message}\r\n\r\n").expect("写入 String 不应失败");
        }
        Self {
            status: 200,
            reason: "OK",
            content_type: Some("text/event-stream"),
            extra_header: None,
            body: body.into_bytes(),
            include_content_length: false,
            delay: Duration::ZERO,
            hold_open: Duration::ZERO,
        }
    }

    /// 构造用于事件 ID、retry 与断线恢复测试的原始 SSE 响应。
    fn raw_sse(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            content_type: Some("text/event-stream"),
            extra_header: None,
            body: body.into(),
            include_content_length: false,
            delay: Duration::ZERO,
            hold_open: Duration::ZERO,
        }
    }

    /// 构造没有响应体的 HTTP 响应。
    fn empty(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            content_type: None,
            extra_header: None,
            body: Vec::new(),
            include_content_length: true,
            delay: Duration::ZERO,
            hold_open: Duration::ZERO,
        }
    }

    /// 构造携带 OAuth challenge 的 401 响应。
    fn unauthorized(challenge: &'static str) -> Self {
        Self::empty(401, "Unauthorized").with_extra_header("WWW-Authenticate", challenge)
    }

    /// 为测试响应增加一个额外 HTTP 头。
    fn with_extra_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.extra_header = Some((name, value));
        self
    }

    /// 为响应增加确定性延迟。
    fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// 在响应写完后保持连接开放，用于验证增量 SSE 消费。
    fn with_hold_open(mut self, hold_open: Duration) -> Self {
        self.hold_open = hold_open;
        self
    }

    /// 编码为关闭连接的 HTTP/1.1 响应。
    fn to_http_bytes(&self) -> Vec<u8> {
        let mut headers = format!(
            "HTTP/1.1 {} {}\r\nConnection: close\r\n",
            self.status, self.reason
        );
        if self.include_content_length {
            headers.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
        }
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

/// 有界读取测试 HTTP 请求头与 Content-Length 请求体。
async fn read_http_request(socket: &mut TcpStream) -> TestRequest {
    const LIMIT: usize = 1024 * 1024;
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(position) = find_bytes(&bytes, b"\r\n\r\n") {
            break position + 4;
        }
        assert!(bytes.len() < LIMIT, "测试请求头超过上限");
        let mut chunk = [0_u8; 4096];
        let read = socket.read(&mut chunk).await.expect("应读取 HTTP 请求");
        assert!(read > 0, "HTTP 请求头提前结束");
        bytes.extend_from_slice(&chunk[..read]);
    };
    let header_text = std::str::from_utf8(&bytes[..header_end]).expect("请求头应为 UTF-8");
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next().expect("应存在请求行");
    let method = request_line
        .split_whitespace()
        .next()
        .expect("请求行应包含方法")
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
    assert!(content_length <= LIMIT, "测试请求体超过上限");
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
