use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use keencode_mcp::{
    McpClient, McpClientOptions, McpContent, McpServerConfig, McpTaskSupport, McpTool,
    McpToolExecution, McpToolSet, StdioServerConfig, ToolCallResult,
};
use keencode_model::{MAX_TOOL_NAME_BYTES, ToolResultContent};
use serde_json::json;

use crate::mcp::{
    McpDiagnosticCode, McpToolBridgeError, bounded_description, mcp_tool_definition,
    normalize_mcp_content, normalize_mcp_result, portable_mcp_tool_name,
};
use crate::{build_mcp_deferred_tools_best_effort, prepare_mcp_server_tools};

/// 创建不要求 Tasks 协议的标准 MCP 测试工具。
fn tool(name: &str) -> McpTool {
    McpTool {
        name: name.to_owned(),
        title: Some("测试标题".to_owned()),
        description: Some("读取测试数据".to_owned()),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
        output_schema: None,
        annotations: None,
        execution: None,
        icons: Vec::new(),
        meta: None,
    }
}

#[test]
fn portable_names_are_stable_distinct_and_bounded() {
    let first = portable_mcp_tool_name("server.with punctuation", "工具/lookup").unwrap();
    let repeated = portable_mcp_tool_name("server.with punctuation", "工具/lookup").unwrap();
    let second = portable_mcp_tool_name("server_with punctuation", "工具/lookup").unwrap();
    assert_eq!(first, repeated);
    assert_ne!(first, second);
    assert!(first.len() <= MAX_TOOL_NAME_BYTES);
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    );
    assert_eq!(
        portable_mcp_tool_name("", "tool").unwrap_err(),
        McpToolBridgeError::InvalidIdentity
    );
}

#[test]
fn tool_definition_preserves_schema_and_bounds_untrusted_description() {
    let mut remote = tool("lookup");
    remote.description = Some(format!("{}\nsecret", "界".repeat(4_000)));
    let definition = mcp_tool_definition("docs", &remote).unwrap();
    definition.validate().unwrap();
    assert_eq!(definition.input_schema, remote.input_schema);
    assert!(definition.description.len() <= 8 * 1024);
    assert!(!definition.description.contains('\n'));

    remote.input_schema = json!({ "type": "object", "$ref": "#/$defs/input" });
    assert_eq!(
        mcp_tool_definition("docs", &remote).unwrap_err(),
        McpToolBridgeError::InvalidDefinition
    );
}

#[test]
fn task_required_tool_is_explicitly_identifiable_before_bridge_creation() {
    let mut remote = tool("async-only");
    remote.execution = Some(McpToolExecution {
        task_support: McpTaskSupport::Required,
        extensions: Default::default(),
    });
    let set = McpToolSet::new(vec![remote.clone()]);
    assert!(set.tools()[0].requires_task());
    assert_eq!(
        set.effect_for(&remote.name),
        keencode_mcp::McpToolEffect::ChangesState
    );
}

#[test]
fn mcp_content_and_structured_results_preserve_order_and_shape() {
    let text = normalize_mcp_content(McpContent {
        content_type: "text".to_owned(),
        text: Some("hello".to_owned()),
        data: None,
        mime_type: None,
        uri: None,
        name: None,
        title: None,
        description: None,
        size: None,
        resource: None,
        annotations: None,
        meta: None,
        extensions: Default::default(),
    })
    .unwrap();
    assert_eq!(
        text,
        ToolResultContent::Text {
            text: "hello".to_owned()
        }
    );

    let result = normalize_mcp_result(ToolCallResult {
        content: vec![McpContent {
            content_type: "resource_link".to_owned(),
            text: None,
            data: None,
            mime_type: Some("text/plain".to_owned()),
            uri: Some("file:///synthetic".to_owned()),
            name: Some("synthetic".to_owned()),
            title: None,
            description: None,
            size: Some(10),
            resource: None,
            annotations: None,
            meta: None,
            extensions: Default::default(),
        }],
        structured_content: Some(json!({ "count": 1 })),
        is_error: false,
        meta: None,
    })
    .unwrap();
    assert_eq!(result.content.len(), 2);
    assert!(matches!(result.content[0], ToolResultContent::Text { .. }));
    assert!(matches!(result.content[1], ToolResultContent::Text { .. }));
}

#[test]
fn business_error_does_not_leak_remote_content_into_error_message() {
    let error = normalize_mcp_result(ToolCallResult {
        content: vec![McpContent {
            content_type: "text".to_owned(),
            text: Some("REMOTE_PRIVATE_CONTENT".to_owned()),
            data: None,
            mime_type: None,
            uri: None,
            name: None,
            title: None,
            description: None,
            size: None,
            resource: None,
            annotations: None,
            meta: None,
            extensions: Default::default(),
        }],
        structured_content: None,
        is_error: true,
        meta: None,
    })
    .unwrap_err();
    assert_eq!(error.code, "mcp_tool_failed");
    assert!(!error.message.contains("REMOTE_PRIVATE_CONTENT"));
}

#[test]
fn description_truncation_never_splits_utf8() {
    let description = bounded_description(&"界".repeat(4_000));
    assert!(description.len() <= 8 * 1024);
    assert!(description.is_char_boundary(description.len()));
}

/// MCP Server 连接失败必须只产生有界诊断，不得阻断核心工具目录构建。
#[tokio::test]
async fn server_preparation_degrades_on_connection_failure() {
    let report = prepare_mcp_server_tools(
        "unavailable-server",
        McpServerConfig::Stdio(StdioServerConfig::new(
            "definitely-missing-keencode-mcp-command",
        )),
        McpClientOptions::default(),
    )
    .await;

    assert!(report.tools().is_empty());
    assert!(report.is_degraded());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code,
        McpDiagnosticCode::ServerUnavailable
    );
    assert_eq!(report.diagnostics()[0].server_id, "unavailable-server");
    assert!(report.diagnostics()[0].message.len() <= 1_024);
}

/// 非法 Server 身份必须在连接前被隐藏并记录固定诊断，避免启动外部进程。
#[tokio::test]
async fn server_preparation_reports_invalid_identity_without_connecting() {
    let report = prepare_mcp_server_tools(
        "bad\nserver",
        McpServerConfig::Stdio(StdioServerConfig::new(
            "definitely-missing-keencode-mcp-command",
        )),
        McpClientOptions::default(),
    )
    .await;

    assert_eq!(report.tools().len(), 0);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code,
        McpDiagnosticCode::InvalidIdentity
    );
    assert_eq!(report.diagnostics()[0].server_id, "<invalid>");
    assert!(!report.diagnostics()[0].message.contains('\n'));
}

/// 多 Server 准备报告合并时必须同时保留每个 Server 的诊断。
#[tokio::test]
async fn preparation_report_append_preserves_diagnostics() {
    let mut first = prepare_mcp_server_tools(
        "bad\nserver",
        McpServerConfig::Stdio(StdioServerConfig::new(
            "definitely-missing-keencode-mcp-command",
        )),
        McpClientOptions::default(),
    )
    .await;
    let second = prepare_mcp_server_tools(
        "unavailable-server",
        McpServerConfig::Stdio(StdioServerConfig::new(
            "definitely-missing-keencode-mcp-command",
        )),
        McpClientOptions::default(),
    )
    .await;

    first.append(second);
    assert_eq!(first.tools().len(), 0);
    assert_eq!(first.diagnostics().len(), 2);
    assert_eq!(
        first.diagnostics()[1].code,
        McpDiagnosticCode::ServerUnavailable
    );
}

/// best-effort 工具桥必须保留有效工具，并将坏 Schema 单独记录为诊断。
#[tokio::test]
async fn best_effort_bridge_keeps_valid_tools_when_one_definition_is_invalid() {
    let directory = tempfile::tempdir().expect("创建 MCP 测试目录");
    let executable = compile_fake_mcp(directory.path());
    let client = McpClient::connect(
        McpServerConfig::Stdio(StdioServerConfig::new(executable.to_string_lossy())),
        McpClientOptions::default(),
    )
    .await
    .expect("测试 MCP Server 应完成握手");
    let tool_set = client.list_tools().await.expect("测试工具列表应成功");
    let report = build_mcp_deferred_tools_best_effort("test-server", client.clone(), &tool_set);

    assert_eq!(report.tool_count(), 1);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code,
        McpDiagnosticCode::InvalidDefinition
    );
    assert_eq!(
        report.diagnostics()[0].tool_name.as_deref(),
        Some("bad-schema")
    );

    drop(report);
    client.close().await.expect("测试 MCP Server 应可关闭");
}

/// 编译一个只依赖标准库、用于验证工具列表降级的行分隔 MCP Server。
fn compile_fake_mcp(directory: &Path) -> PathBuf {
    let source = directory.join("fake_mcp.rs");
    let executable = directory.join(if cfg!(windows) {
        "fake-mcp.exe"
    } else {
        "fake-mcp"
    });
    fs::write(
        &source,
        r###"
use std::io::{self, BufRead, Write};

fn main() {
    let stdin = io::stdin();
    let mut output = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        if line.contains("\"method\":\"notifications/initialized\"") {
            continue;
        }
        let Some(id) = extract_id(&line) else { continue; };
        let response = if line.contains("\"method\":\"initialize\"") {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"protocolVersion":"2025-11-25","capabilities":{{"tools":{{}}}},"serverInfo":{{"name":"fake-mcp","version":"1"}}}}}}"#)
        } else if line.contains("\"method\":\"tools/list\"") {
            r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"good","description":"valid test tool","inputSchema":{"type":"object","properties":{}}},{"name":"bad-schema","description":"invalid test tool","inputSchema":42}]}}"#.replace("\"id\":2", &format!("\"id\":{id}"))
        } else {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#)
        };
        writeln!(output, "{response}").unwrap();
        output.flush().unwrap();
    }
}

fn extract_id(body: &str) -> Option<&str> {
    let tail = body.split("\"id\":").nth(1)?;
    let end = tail.find(',').unwrap_or(tail.len());
    Some(tail[..end].trim())
}
"###,
    )
    .expect("写入 MCP 测试 Server 源码");
    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("测试环境必须可以调用 rustc");
    assert!(
        output.status.success(),
        "测试 MCP Server 编译失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}
