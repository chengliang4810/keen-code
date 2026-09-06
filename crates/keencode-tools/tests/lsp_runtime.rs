//! 原生 LSP 工具的真实 stdio 进程边界集成测试。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolContext, ToolEffect, TurnCancellation, TurnId,
};
use keencode_model::ToolResultContent;
use keencode_tools::{LspRuntime, LspServerConfig, LspTool};
use serde_json::{Value, json};
use tempfile::tempdir;

/// 真正启动一个仅依赖标准库的测试 Language Server，并完成 initialize、didOpen 与 hover。
#[tokio::test]
async fn native_process_completes_initialize_and_hover() {
    let directory = tempdir().expect("创建 LSP 进程测试目录");
    let executable = compile_fake_lsp(directory.path());
    fs::write(directory.path().join("main.rs"), "fn main() {}\n").expect("写入测试源文件");
    let runtime = Arc::new(
        LspRuntime::new(
            directory.path(),
            vec![LspServerConfig {
                name: "test-rust".to_owned(),
                command: executable.to_string_lossy().into_owned(),
                args: Vec::new(),
                current_dir: directory.path().to_path_buf(),
                environment: BTreeMap::new(),
                extension_to_language: BTreeMap::from([("rs".to_owned(), "rust".to_owned())]),
                initialization_options: Some(json!({ "test": true })),
                max_restarts: 1,
                startup_timeout_ms: 5_000,
            }],
        )
        .expect("测试 Runtime 应有效"),
    );
    runtime.start_all().await.expect("候选发布前应预启动 LSP");
    let tool = LspTool::new(runtime);

    assert_eq!(
        tool.effect(&json!({
            "operation": "hover",
            "file": "main.rs",
            "line": 1,
            "character": 1
        }))
        .expect("已预启动的 LSP 查询应通过校验"),
        ToolEffect::ReadOnly
    );

    let output = tool
        .execute(
            tool_context(),
            json!({
                "operation": "hover",
                "file": "main.rs",
                "line": 1,
                "character": 1
            }),
        )
        .await
        .expect("真实 LSP hover 应完成");
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("LSP 结果必须是唯一文本块");
    };
    let result: Value = serde_json::from_str(text).expect("结果应为 JSON");
    assert_eq!(result["server"], "test-rust");
    assert_eq!(result["result"]["contents"], "fake hover");
}

/// 创建一次独立工具调用上下文。
fn tool_context() -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-lsp-integration").expect("测试 Session ID 有效"),
        turn_id: TurnId::new("turn-lsp-integration").expect("测试 Turn ID 有效"),
        source_agent_id: AgentId::new("agent-lsp-integration").expect("测试 Agent ID 有效"),
        tool_call_id: ToolCallId::new("call-lsp-integration").expect("测试 ToolCall ID 有效"),
        cancellation: TurnCancellation::new(),
    }
}

/// 使用当前 Rust 工具链编译一个无外部依赖的 stdio JSON-RPC 测试 Server。
fn compile_fake_lsp(directory: &Path) -> PathBuf {
    let source = directory.join("fake_lsp.rs");
    let executable = directory.join(if cfg!(windows) {
        "fake-lsp.exe"
    } else {
        "fake-lsp"
    });
    fs::write(
        &source,
        r###"
use std::io::{self, BufRead, Read, Write};

fn main() {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut length = None;
        loop {
            let mut line = String::new();
            if input.read_line(&mut line).unwrap() == 0 { return; }
            if line == "\r\n" || line == "\n" { break; }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0_u8; length.unwrap()];
        input.read_exact(&mut body).unwrap();
        let body = String::from_utf8(body).unwrap();
        let Some(id) = extract_id(&body) else { continue; };
        let response = if body.contains("\"method\":\"initialize\"") {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"capabilities":{{}}}}}}"#)
        } else if body.contains("\"method\":\"textDocument/hover\"") {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"contents":"fake hover"}}}}"#)
        } else {
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#)
        };
        write!(output, "Content-Length: {}\r\n\r\n{}", response.len(), response).unwrap();
        output.flush().unwrap();
    }
}

fn extract_id(body: &str) -> Option<u64> {
    let tail = body.split("\"id\":").nth(1)?;
    let digits = tail.chars().take_while(char::is_ascii_digit).collect::<String>();
    digits.parse().ok()
}
"###,
    )
    .expect("写入测试 Language Server 源码");
    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("测试环境必须可以调用 rustc");
    assert!(
        output.status.success(),
        "测试 Language Server 编译失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}
