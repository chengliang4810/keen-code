//! keencode-mcp 真实子进程集成测试使用的最小 stdio MCP 服务。

use std::io::{self, BufRead, Write};
use std::process::Command;
use std::time::Duration;

use keencode_mcp::DEFAULT_PROTOCOL_VERSION;
use serde_json::{Value, json};

/// 从标准输入读取换行分帧请求，并向标准输出写入对应 MCP 响应。
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.get(1).map(String::as_str) == Some("--tree-child") {
        std::thread::sleep(Duration::from_millis(600));
        if let Some(path) = arguments.get(2) {
            std::fs::write(path, b"descendant survived")?;
        }
        return Ok(());
    }
    if let Ok(sentinel) = std::env::var("KEENCODE_MCP_TREE_SENTINEL") {
        Command::new(&arguments[0])
            .arg("--tree-child")
            .arg(sentinel)
            .spawn()?;
    }
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line)?;
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        if method == "tools/list" {
            write_json(
                &mut stdout,
                &json!({ "jsonrpc": "2.0", "id": "server-ping", "method": "ping" }),
            )?;
            write_json(
                &mut stdout,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/message",
                    "params": { "level": "info", "data": "mock notification" }
                }),
            )?;
        }
        let response = if method == "resources/read"
            && message.pointer("/params/uri").and_then(Value::as_str) == Some("mock://remote-error")
        {
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32000,
                    "message": format!(
                        "sk-fake-integration-test-only\r\n{}",
                        "remote-vendor-body".repeat(1024)
                    )
                }
            })
        } else {
            match mock_result(method, message.get("params")) {
                Some(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                None => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("unknown method: {method}") }
                }),
            }
        };
        write_json(&mut stdout, &response)?;
    }
    if std::env::var_os("KEENCODE_MCP_HOLD_AFTER_EOF").is_some() {
        std::thread::sleep(Duration::from_secs(5));
    }
    Ok(())
}

/// 为集成测试覆盖的方法构造确定性结果。
fn mock_result(method: &str, params: Option<&Value>) -> Option<Value> {
    match method {
        "initialize" => Some(json!({
            "protocolVersion": DEFAULT_PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": true },
                "resources": { "subscribe": false, "listChanged": false }
            },
            "serverInfo": { "name": "keencode-mcp-mock", "version": "1.0.0" },
            "instructions": "stdio integration mock"
        })),
        "tools/list" => {
            if params
                .and_then(|params| params.get("cursor"))
                .and_then(Value::as_str)
                == Some("page-2")
            {
                Some(json!({
                    "tools": [{
                        "name": "write_mock",
                        "description": "测试保守副作用分类",
                        "inputSchema": { "type": "object" }
                    }, {
                        "name": "task_only",
                        "description": "只能通过 Tasks 协议调用",
                        "inputSchema": { "type": "object" },
                        "execution": { "taskSupport": "required" }
                    }]
                }))
            } else {
                Some(json!({
                    "tools": [{
                        "name": "echo",
                        "description": "回显参数",
                        "inputSchema": { "type": "object" },
                        "annotations": { "readOnlyHint": true }
                    }],
                    "nextCursor": "page-2"
                }))
            }
        }
        "tools/call" => Some(json!({
            "content": [{
                "type": "text",
                "text": params
                    .and_then(|params| params.get("arguments"))
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string()
            }],
            "structuredContent": params
                .and_then(|params| params.get("arguments"))
                .cloned()
                .unwrap_or_else(|| json!({})),
            "isError": false
        })),
        "resources/list" => Some(json!({
            "resources": [{
                "uri": "mock://readme",
                "name": "readme",
                "mimeType": "text/plain"
            }]
        })),
        "resources/templates/list" => Some(json!({
            "resourceTemplates": [{
                "uriTemplate": "mock://file/{name}",
                "name": "file",
                "mimeType": "text/plain"
            }]
        })),
        "resources/read" => Some(json!({
            "contents": [{
                "uri": params
                    .and_then(|params| params.get("uri"))
                    .and_then(Value::as_str)
                    .unwrap_or("mock://unknown"),
                "mimeType": "text/plain",
                "text": "mock resource"
            }]
        })),
        _ => None,
    }
}

/// 写入单行 JSON 并立即刷新，模拟长时间运行的 MCP 子进程。
fn write_json(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
