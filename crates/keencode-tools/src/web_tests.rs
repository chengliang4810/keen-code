//! 网络工具的本地协议模拟与可选真实服务验证。

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use keencode_agent::{
    AgentId, AgentRunner, AgentTool, PlanGuard, RunLimits, SessionId, ToolCallId, ToolConcurrency,
    ToolContext, ToolEffect, ToolRegistry, TurnCancellation, TurnId, TurnRequest,
};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities, ResponseMetadata,
    ScriptedProvider, ScriptedReply, StopReason, ToolResultContent,
};
use serde_json::{Value, json};
use tempfile::tempdir;

use super::{
    WebFetchTool, WebSearchTool, WebServiceConfig, bounded_content, register_web_tools,
    safe_display_url,
};
use crate::ToolEnvironment;

/// 本地 HTTP 模拟服务捕获到的单次请求。
struct CapturedRequest {
    /// 请求行中的路径与查询部分。
    target: String,
    /// Content-Length 指定的完整请求正文。
    body: Vec<u8>,
}

/// 创建每次测试独立使用的工具上下文。
fn tool_context() -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-web").expect("测试 Session ID 应有效"),
        turn_id: TurnId::new("turn-web").expect("测试 Turn ID 应有效"),
        source_agent_id: AgentId::new("agent-web").expect("测试 Agent ID 应有效"),
        tool_call_id: ToolCallId::new("call-web").expect("测试 ToolCall ID 应有效"),
        cancellation: TurnCancellation::new(),
    }
}

/// 提取只含一个文本块的工具输出。
fn output_text(output: &keencode_agent::ToolOutput) -> &str {
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("测试输出应只包含一个文本块");
    };
    text
}

/// 创建一段包含一个或多个工具调用的脚本化模型响应。
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

/// 创建一段让 Agent Runner 进入成功终态的文本响应。
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

/// 启动只处理一次请求的本地 HTTP 服务并返回带路径前缀的基础地址。
fn spawn_json_server(
    status: u16,
    response_body: impl Into<Vec<u8>>,
    response_delay: Duration,
) -> (String, Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("应绑定本地测试端口");
    let address = listener.local_addr().expect("应读取本地测试地址");
    let body = response_body.into();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("应接受网络工具请求");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("应设置读取超时");
        let request = read_http_request(&mut stream).expect("应读取完整 HTTP 请求");
        // 不关心请求正文的测试会直接丢弃接收端，但模拟服务仍必须继续返回响应。
        let _ = sender.send(request);
        thread::sleep(response_delay);
        let reason = if (200..300).contains(&status) {
            "OK"
        } else {
            "ERROR"
        };
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    });
    (format!("http://{address}/api"), receiver)
}

/// 启动同时响应搜索和提取端点的本地 HTTP 服务，用于 Agent Runner 并发工具测试。
fn spawn_routing_json_server(
    search_body: impl Into<Vec<u8>>,
    fetch_body: impl Into<Vec<u8>>,
) -> (String, Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("应绑定本地测试端口");
    let address = listener.local_addr().expect("应读取本地测试地址");
    let search_body = search_body.into();
    let fetch_body = fetch_body.into();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("应接受网络工具请求");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("应设置读取超时");
            let request = read_http_request(&mut stream).expect("应读取完整 HTTP 请求");
            let body = match request.target.as_str() {
                "/api/search" => &search_body,
                "/api/extract" => &fetch_body,
                target => panic!("网络工具请求了未预期的测试端点：{target}"),
            };
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sender.send(request);
            stream
                .write_all(headers.as_bytes())
                .expect("应写入测试响应头");
            stream.write_all(body).expect("应写入测试响应体");
            stream.flush().expect("应刷新测试响应");
        }
    });
    (format!("http://{address}/api"), receiver)
}

/// 从阻塞流中读取请求头和 Content-Length 指定的正文。
fn read_http_request(stream: &mut impl Read) -> std::io::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 2_048];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "请求头尚未完整时连接关闭",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = find_bytes(&bytes, b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            break (header_end + 4, content_length);
        }
    };
    while bytes.len() < header_end.saturating_add(content_length) {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    if bytes.len() < header_end.saturating_add(content_length) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "请求正文短于 Content-Length",
        ));
    }
    let request_line = String::from_utf8_lossy(&bytes[..header_end])
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned();
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    Ok(CapturedRequest {
        target,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

/// 在字节切片中查找第一个完整子切片位置。
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 服务配置必须拒绝危险地址并保留显式路径前缀。
#[tokio::test]
async fn web_config_is_strict_and_preserves_path_prefix() {
    assert!(WebServiceConfig::new("ftp://example.com").is_err());
    assert!(WebServiceConfig::new("https://user:pass@example.com").is_err());
    assert!(WebServiceConfig::new("https://example.com?token=value").is_err());

    let (base_url, captured) =
        spawn_json_server(200, br#"{"results":[]}"#.to_vec(), Duration::ZERO);
    let config = WebServiceConfig::new(&base_url).expect("本地服务配置应有效");
    assert!(config.base_url().as_str().ends_with("/api/"));
    let tool = WebSearchTool::new(config).expect("搜索工具应创建");
    let output = tool
        .execute(tool_context(), json!({ "query": "  Rust async  " }))
        .await
        .expect("空搜索结果仍应成功");
    assert!(output_text(&output).contains("有效结果：0"));

    let request = captured
        .recv_timeout(Duration::from_secs(2))
        .expect("应捕获搜索请求");
    assert_eq!(request.target, "/api/search");
    let body: Value = serde_json::from_slice(&request.body).expect("请求正文应为 JSON");
    assert_eq!(body["query"], "Rust async");
    assert_eq!(body["max_results"], 10);
}

/// 网络工具注册必须提供稳定且互不冲突的两个名称。
#[test]
fn web_tool_registration_is_stable() {
    let directory = tempdir().expect("应创建临时目录");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let config = WebServiceConfig::new("http://127.0.0.1:1/api").expect("无需连接即可创建配置");
    let mut registry = ToolRegistry::new();
    register_web_tools(&mut registry, environment, config).expect("网络工具应注册");
    let names = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["WebFetch", "WebSearch"]);
}

/// WebSearch 必须严格校验输入、标记外部内容并清理敏感网址字段。
#[tokio::test]
async fn web_search_filters_and_redacts_results() {
    let response = json!({
        "results": [
            {
                "title": "  First\nResult  ",
                "url": "https://example.com/page?token=secret&view=full#section",
                "content": "line one\nline two and more"
            },
            {
                "title": "Local file",
                "url": "file:///private.txt",
                "content": "must not appear"
            }
        ]
    });
    let (base_url, captured) = spawn_json_server(
        200,
        serde_json::to_vec(&response).expect("响应应序列化"),
        Duration::ZERO,
    );
    let config = WebServiceConfig::new(base_url)
        .expect("本地服务配置应有效")
        .with_output_limits(100_000, 2_000, 3, 12)
        .expect("输出上限应有效");
    let tool = WebSearchTool::new(config).expect("搜索工具应创建");
    assert_eq!(
        tool.effect(&json!({ "query": "test", "num_results": 2 })),
        Ok(ToolEffect::ReadOnly)
    );
    assert_eq!(tool.concurrency(), ToolConcurrency::ParallelReadOnly);
    assert!(
        tool.effect(&json!({ "query": "test", "num_results": 0 }))
            .is_err()
    );
    assert!(
        tool.effect(&json!({ "query": "test", "num_results": 1.5 }))
            .is_err()
    );

    let output = tool
        .execute(tool_context(), json!({ "query": "test", "num_results": 2 }))
        .await
        .expect("搜索应成功");
    let text = output_text(&output);
    assert!(text.contains("以下内容来自外部网络"));
    assert!(text.contains("有效结果：1"));
    assert!(text.contains("First Result"));
    assert!(text.contains("token=%5BREDACTED%5D"));
    assert!(text.contains("view=full"));
    assert!(!text.contains("secret"));
    assert!(!text.contains("private.txt"));
    assert!(text.contains("摘要已截断"));

    let request = captured
        .recv_timeout(Duration::from_secs(2))
        .expect("应捕获搜索请求");
    let body: Value = serde_json::from_slice(&request.body).expect("请求正文应为 JSON");
    assert_eq!(body["max_results"], 2);
}

/// 搜索来源展示必须移除用户信息、片段以及常见变体的凭据查询参数。
#[test]
fn web_display_url_redacts_credentials_and_sensitive_variants() {
    let url = safe_display_url(
        "https://user:password@example.com/docs?api-key=one&X-API-Key=two&access-token=three&authToken=four&client_secret=five&monkey=six#fragment",
    )
    .expect("HTTP 来源应可展示");
    assert!(url.starts_with("https://example.com/docs?"));
    assert!(!url.contains("user"));
    assert!(!url.contains("password"));
    assert!(!url.contains("one"));
    assert!(!url.contains("two"));
    assert!(!url.contains("three"));
    assert!(!url.contains("four"));
    assert!(!url.contains("five"));
    assert!(url.contains("monkey=six"));
    assert!(!url.contains("fragment"));
    assert!(url.matches("%5BREDACTED%5D").count() >= 5);
}

/// 搜索结果的长标题与长网址必须在固定输出预算内截断。
#[tokio::test]
async fn web_search_bounds_untrusted_title_and_url() {
    let long_title = "标题".repeat(600);
    let long_path = "p".repeat(3_000);
    let response = json!({
        "results": [{
            "title": long_title,
            "url": format!("https://example.com/{long_path}"),
            "content": "摘要"
        }]
    });
    let (base_url, _) = spawn_json_server(
        200,
        serde_json::to_vec(&response).expect("响应应序列化"),
        Duration::ZERO,
    );
    let config = WebServiceConfig::new(base_url).expect("本地服务配置应有效");
    let tool = WebSearchTool::new(config).expect("搜索工具应创建");
    let output = tool
        .execute(tool_context(), json!({ "query": "bounded" }))
        .await
        .expect("搜索应成功");
    let text = output_text(&output);
    assert!(text.contains("文本已截断"));
    assert!(text.len() < 4_000);
}

/// WebFetch 必须无损落盘超大正文，并在模型输出中隐藏敏感查询值。
#[tokio::test]
async fn web_fetch_spills_full_content_without_loss() {
    let directory = tempdir().expect("应创建临时目录");
    let artifact_directory = directory.path().join("artifacts");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_artifact_directory(&artifact_directory)
            .expect("输出目录应有效"),
    );
    let content = "第一行正文\nsecond line\nthird line\n";
    let response = json!({
        "results": [{ "raw_content": content }],
        "failed_results": []
    });
    let (base_url, captured) = spawn_json_server(
        200,
        serde_json::to_vec(&response).expect("响应应序列化"),
        Duration::ZERO,
    );
    let config = WebServiceConfig::new(base_url)
        .expect("本地服务配置应有效")
        .with_output_limits(16, 2, 20, 500)
        .expect("输出上限应有效");
    let tool = WebFetchTool::new(environment, config).expect("提取工具应创建");
    let url = "https://example.com/docs?api_key=private&lang=zh#intro";
    let output = tool
        .execute(
            tool_context(),
            json!({ "url": url, "prompt": "  提取\n要点  " }),
        )
        .await
        .expect("网页提取应成功");
    let text = output_text(&output);
    assert!(text.contains("正文预览已截断"));
    assert!(text.contains("完整正文"));
    assert!(text.contains("api_key=%5BREDACTED%5D"));
    assert!(text.contains("lang=zh"));
    assert!(text.contains("本次关注点：提取 要点"));
    assert!(!text.contains("private"));
    let artifacts = fs::read_dir(&artifact_directory)
        .expect("应读取网页正文目录")
        .map(|entry| entry.expect("目录项应有效").path())
        .collect::<Vec<_>>();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        fs::read_to_string(&artifacts[0]).expect("应读取完整网页正文"),
        content
    );

    let request = captured
        .recv_timeout(Duration::from_secs(2))
        .expect("应捕获提取请求");
    assert_eq!(request.target, "/api/extract");
    let body: Value = serde_json::from_slice(&request.body).expect("请求正文应为 JSON");
    assert_eq!(body["urls"][0], url);
}

/// WebFetch 必须区分服务失败记录与没有正文的成功响应，且不把厂商详情回显给模型。
#[tokio::test]
async fn web_fetch_handles_failed_and_empty_results() {
    let failed_directory = tempdir().expect("应创建失败响应临时目录");
    let (failed_base_url, _) = spawn_json_server(
        200,
        br#"{"results":[],"failed_results":[{}]}"#.to_vec(),
        Duration::ZERO,
    );
    let failed = WebFetchTool::new(
        Arc::new(ToolEnvironment::new(failed_directory.path()).expect("工具环境应有效")),
        WebServiceConfig::new(failed_base_url).expect("本地服务配置应有效"),
    )
    .expect("提取工具应创建");
    let error = failed
        .execute(
            tool_context(),
            json!({ "url": "https://example.com/failed" }),
        )
        .await
        .expect_err("失败记录必须转换成工具错误");
    assert_eq!(error.code, "web_fetch_failed");
    assert!(error.message.contains("1 条失败记录"));

    let empty_directory = tempdir().expect("应创建空响应临时目录");
    let (empty_base_url, _) = spawn_json_server(
        200,
        br#"{"results":[{"raw_content":" \n\t"}],"failed_results":[]}"#.to_vec(),
        Duration::ZERO,
    );
    let empty = WebFetchTool::new(
        Arc::new(ToolEnvironment::new(empty_directory.path()).expect("工具环境应有效")),
        WebServiceConfig::new(empty_base_url).expect("本地服务配置应有效"),
    )
    .expect("提取工具应创建");
    let output = empty
        .execute(
            tool_context(),
            json!({ "url": "https://example.com/empty" }),
        )
        .await
        .expect("空正文响应应返回可消费提示");
    assert!(output_text(&output).contains("未返回正文"));
}

/// HTTP 限流和服务端错误可重试，普通客户端错误不可重试且错误正文不能掩盖状态。
#[tokio::test]
async fn web_http_status_retryability_is_conservative() {
    for (status, retryable, response_limit) in [
        (429, true, 64 * 1024),
        (503, true, 4),
        (400, false, 64 * 1024),
    ] {
        let (base_url, _) = spawn_json_server(
            status,
            br#"{"error":"vendor detail"}"#.to_vec(),
            Duration::ZERO,
        );
        let config = WebServiceConfig::new(base_url)
            .expect("本地服务配置应有效")
            .with_response_limits(8 * 1024 * 1024, response_limit)
            .expect("响应上限应有效");
        let tool = WebSearchTool::new(config).expect("搜索工具应创建");
        let error = tool
            .execute(tool_context(), json!({ "query": "status" }))
            .await
            .expect_err("非成功状态必须失败");
        assert_eq!(error.code, "web_http_status");
        assert_eq!(error.retryable, retryable);
        assert!(error.message.contains(&status.to_string()));
        assert!(!error.message.contains("vendor detail"));
    }
}

/// 成功状态下的非法 JSON 与网络连接失败必须有稳定、可重试分类。
#[tokio::test]
async fn web_protocol_and_transport_errors_are_stable() {
    let (base_url, _) = spawn_json_server(200, b"not-json".to_vec(), Duration::ZERO);
    let tool = WebSearchTool::new(WebServiceConfig::new(base_url).expect("本地服务配置应有效"))
        .expect("搜索工具应创建");
    let error = tool
        .execute(tool_context(), json!({ "query": "invalid-json" }))
        .await
        .expect_err("非法 JSON 必须失败");
    assert_eq!(error.code, "invalid_web_response");
    assert!(!error.retryable);

    let address = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("应绑定临时网络端口");
        listener.local_addr().expect("应读取临时网络地址")
    };
    let config = WebServiceConfig::new(format!("http://{address}/api"))
        .expect("闭合端口地址仍应是有效配置")
        .with_timeouts(Duration::from_millis(100), Duration::from_millis(500))
        .expect("超时配置应有效");
    let tool = WebSearchTool::new(config).expect("搜索工具应创建");
    let error = tool
        .execute(tool_context(), json!({ "query": "connection" }))
        .await
        .expect_err("连接关闭必须失败");
    assert!(
        matches!(
            error.code.as_str(),
            "web_connection_failed" | "web_request_timeout"
        ),
        "连接故障应归类为传输错误，实际为 {}",
        error.code
    );
    assert!(error.retryable, "传输故障应允许有限重试");
}

/// 响应体超过配置上限时必须在解析前失败且不返回部分 JSON。
#[tokio::test]
async fn web_response_limit_is_enforced() {
    let (base_url, _) = spawn_json_server(
        200,
        br#"{"results":[{"title":"oversized","url":"https://example.com"}]}"#.to_vec(),
        Duration::ZERO,
    );
    let config = WebServiceConfig::new(base_url)
        .expect("本地服务配置应有效")
        .with_response_limits(16, 16)
        .expect("响应上限应有效");
    let tool = WebSearchTool::new(config).expect("搜索工具应创建");
    let error = tool
        .execute(tool_context(), json!({ "query": "large" }))
        .await
        .expect_err("超大响应必须失败");
    assert_eq!(error.code, "web_response_too_large");
    assert!(!error.retryable);
}

/// 取消令牌必须中止正在等待的真实 HTTP 响应。
#[tokio::test]
async fn web_request_observes_turn_cancellation() {
    let (base_url, _) =
        spawn_json_server(200, br#"{"results":[]}"#.to_vec(), Duration::from_secs(2));
    let config = WebServiceConfig::new(base_url)
        .expect("本地服务配置应有效")
        .with_timeouts(Duration::from_secs(1), Duration::from_secs(5))
        .expect("超时配置应有效");
    let tool = WebSearchTool::new(config).expect("搜索工具应创建");
    let context = tool_context();
    let cancellation = context.cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancellation.cancel();
    });
    let started = Instant::now();
    let error = tool
        .execute(context, json!({ "query": "cancel" }))
        .await
        .expect_err("取消中的网络请求必须失败");
    assert_eq!(error.code, "cancelled");
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// 正文预览必须同时遵守行数、字节数和 UTF-8 字符边界。
#[test]
fn bounded_web_content_preserves_utf8_boundary() {
    let content = "甲乙丙\nsecond\nthird";
    let bounded = bounded_content(content, 2, 7);
    assert_eq!(bounded.preview, "甲乙");
    assert!(bounded.truncated);

    let full = bounded_content(content, 10, 1_000);
    assert_eq!(full.preview, content);
    assert!(!full.truncated);
}

/// WebFetch 与 WebSearch 必须通过真实 AgentRunner 在 Plan 模式下执行，不能被误判为变更工具。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_tools_run_through_agent_runner_in_plan_mode() {
    let project = tempdir().expect("应创建 Plan 测试项目目录");
    let search_response = json!({
        "results": [{
            "title": "Search result",
            "url": "https://example.com/search",
            "content": "search body"
        }]
    });
    let fetch_response = json!({
        "results": [{ "raw_content": "fetched body\n" }],
        "failed_results": []
    });
    let (base_url, captured) = spawn_routing_json_server(
        serde_json::to_vec(&search_response).expect("搜索响应应序列化"),
        serde_json::to_vec(&fetch_response).expect("提取响应应序列化"),
    );
    let environment = Arc::new(ToolEnvironment::new(project.path()).expect("工具环境应有效"));
    let config = WebServiceConfig::new(base_url).expect("本地服务配置应有效");
    let mut registry = ToolRegistry::new();
    register_web_tools(&mut registry, Arc::clone(&environment), config)
        .expect("Web 工具应注册到真实 ToolRegistry");

    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                (
                    "web-search-call",
                    "WebSearch",
                    json!({ "query": "Rust", "num_results": 1 }),
                ),
                (
                    "web-fetch-call",
                    "WebFetch",
                    json!({ "url": "https://example.com/docs" }),
                ),
            ]),
            text_reply("web-plan-complete"),
        ],
    ));
    let request = TurnRequest::new(
        SessionId::new("session-web-plan").expect("Web Plan Session ID 应有效"),
        TurnId::new("turn-web-plan").expect("Web Plan Turn ID 应有效"),
        AgentId::new("agent-web-plan").expect("Web Plan Agent ID 应有效"),
        "test-model",
        vec![Message::text(MessageRole::User, "只读查询网络资料")],
        PlanGuard::read_only(),
    );
    let result = AgentRunner::new(provider.clone(), registry, RunLimits::default())
        .run_turn(request)
        .await;
    assert!(
        result.is_success(),
        "Plan 模式 Web 工具应成功完成：{:?}",
        result.error
    );
    let requests = provider.requests().expect("Provider 请求快照应可读取");
    assert_eq!(requests.len(), 2, "应有一轮 Web 工具请求和一轮最终文本请求");
    let tool_results = requests
        .iter()
        .flat_map(|request| request.messages.iter())
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_result } => Some(tool_result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_results.len(), 2, "两个 Web 工具调用都应有配对结果");
    for call_id in ["web-search-call", "web-fetch-call"] {
        let result = tool_results
            .iter()
            .find(|result| result.tool_call_id == call_id)
            .unwrap_or_else(|| panic!("缺少 Web 工具 {call_id} 的结果"));
        assert!(!result.is_error, "Plan 模式不应拒绝只读 Web 工具 {call_id}");
    }
    let mut targets = [
        captured
            .recv_timeout(Duration::from_secs(2))
            .expect("应捕获第一个 Web 请求")
            .target,
        captured
            .recv_timeout(Duration::from_secs(2))
            .expect("应捕获第二个 Web 请求")
            .target,
    ];
    targets.sort();
    assert_eq!(targets, ["/api/extract", "/api/search"]);
}

/// 显式提供真实服务地址时验证搜索和提取都返回可消费正文。
#[tokio::test]
#[ignore = "需要 KEENCODE_WEB_SERVICE_URL 指向用户授权的真实兼容服务"]
async fn configured_live_web_service_returns_real_results() {
    let base_url = std::env::var("KEENCODE_WEB_SERVICE_URL")
        .expect("真实验证必须提供 KEENCODE_WEB_SERVICE_URL");
    let config = WebServiceConfig::new(base_url).expect("真实服务地址应有效");
    let search = WebSearchTool::new(config.clone()).expect("真实搜索工具应创建");
    let search_output = search
        .execute(
            tool_context(),
            json!({ "query": "Rust programming language official website", "num_results": 3 }),
        )
        .await
        .expect("真实搜索服务应返回成功结果");
    let search_text = output_text(&search_output);
    assert!(search_text.contains("有效结果："));
    assert!(search_text.contains("http"));

    let directory = tempdir().expect("应创建真实验证临时目录");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let fetch = WebFetchTool::new(environment, config).expect("真实提取工具应创建");
    let fetch_output = fetch
        .execute(
            tool_context(),
            json!({ "url": "https://www.rust-lang.org/" }),
        )
        .await
        .expect("真实提取服务应返回成功正文");
    let fetch_text = output_text(&fetch_output);
    assert!(fetch_text.contains("--- 外部网页正文开始 ---"));
    assert!(!fetch_text.contains("未返回正文"));
}
