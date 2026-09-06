//! 原生 LSP framing、配置、安全边界与真实进程调用测试。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolConcurrency, ToolContext, ToolEffect,
    ToolRegistry, TurnCancellation, TurnId,
};
use keencode_model::ToolResultContent;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::io::{AsyncWriteExt, duplex};

use super::lsp::{
    DiagnosticsState, LspDiagnosticCode, LspRuntime, LspServerConfig, LspTool,
    MAX_LSP_DIAGNOSTIC_DOCUMENTS, MAX_LSP_HEADER_BYTES, bounded_diagnostics,
    bounded_server_error_message, read_lsp_message, update_diagnostics_cache,
};
use crate::register_lsp_tool;

/// 创建最小有效的测试 Server 配置。
fn server_config(root: &Path, command: impl Into<String>) -> LspServerConfig {
    LspServerConfig {
        name: "test-rust".to_owned(),
        command: command.into(),
        args: Vec::new(),
        current_dir: root.to_path_buf(),
        environment: BTreeMap::new(),
        extension_to_language: BTreeMap::from([(".RS".to_owned(), "rust".to_owned())]),
        initialization_options: Some(json!({ "test": true })),
        max_restarts: 1,
        startup_timeout_ms: 5_000,
    }
}

/// 创建一次独立工具调用上下文。
fn tool_context() -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-lsp").expect("测试 Session ID 有效"),
        turn_id: TurnId::new("turn-lsp").expect("测试 Turn ID 有效"),
        source_agent_id: AgentId::new("agent-lsp").expect("测试 Agent ID 有效"),
        tool_call_id: ToolCallId::new("call-lsp").expect("测试 ToolCall ID 有效"),
        cancellation: TurnCancellation::new(),
    }
}

/// 提取工具唯一文本结果。
fn output_text(output: &keencode_agent::ToolOutput) -> &str {
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("LSP 测试结果必须是单个文本块");
    };
    text
}

/// 配置冻结必须归一扩展名，并拒绝重复 Server 名称。
#[test]
fn runtime_normalizes_extensions_and_rejects_duplicate_names() {
    let directory = tempdir().expect("创建 LSP 测试目录");
    let config = server_config(directory.path(), "missing-lsp-command");
    let runtime = LspRuntime::new(directory.path(), vec![config.clone()]).expect("配置应有效");
    assert_eq!(runtime.len(), 1);
    assert_eq!(
        runtime.project_root(),
        fs::canonicalize(directory.path()).unwrap()
    );

    let error = LspRuntime::new(directory.path(), vec![config.clone(), config])
        .err()
        .expect("重复 Server 必须失败");
    assert!(error.to_string().contains("名称重复"));
}

/// best-effort 配置冻结必须跳过坏 Server，并保留可用候选与明确诊断。
#[test]
fn best_effort_constructor_reports_invalid_server_without_blocking_valid_one() {
    let directory = tempdir().expect("创建 LSP 测试目录");
    let valid = server_config(directory.path(), "missing-lsp-command");
    let mut invalid = server_config(directory.path(), "another-missing-lsp-command");
    invalid.name = "bad\nserver".to_owned();

    let (runtime, report) = LspRuntime::new_best_effort(directory.path(), vec![valid, invalid])
        .expect("项目根有效时 best-effort 构造不应失败");
    assert_eq!(runtime.len(), 1);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code,
        LspDiagnosticCode::InvalidConfiguration
    );
    assert_eq!(report.diagnostics()[0].server, "<invalid>");
    assert!(report.diagnostics()[0].message.len() <= 1_024);
}

/// best-effort 启动必须保留已初始化 Server，并把单个失败转换为诊断。
#[tokio::test]
async fn start_available_keeps_healthy_server_when_another_fails() {
    let directory = tempdir().expect("创建 LSP 测试目录");
    let executable = compile_fake_lsp(directory.path());
    fs::write(directory.path().join("main.rs"), "fn main() {}\n").expect("写入测试源文件");
    let mut available = server_config(directory.path(), executable.to_string_lossy());
    available.name = "a-available".to_owned();
    let mut missing = server_config(directory.path(), "definitely-missing-lsp-command");
    missing.name = "z-missing".to_owned();

    let (runtime, constructor_report) =
        LspRuntime::new_best_effort(directory.path(), vec![available, missing])
            .expect("配置冻结不应失败");
    assert!(constructor_report.diagnostics().is_empty());
    let runtime = Arc::new(runtime);
    let report = runtime.start_available().await;
    assert_eq!(report.started_servers(), &["a-available".to_owned()]);
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code,
        LspDiagnosticCode::StartupFailed
    );
    assert_eq!(report.diagnostics()[0].server, "z-missing");

    let output = LspTool::new(Arc::clone(&runtime))
        .execute(
            tool_context(),
            json!({
                "operation": "hover",
                "server": "a-available",
                "file": "main.rs",
                "line": 1,
                "character": 1
            }),
        )
        .await
        .expect("健康 Server 不应受其他 Server 失败影响");
    let result: Value = serde_json::from_str(output_text(&output)).expect("结果应为 JSON");
    assert_eq!(result["result"]["contents"], "fake hover");

    let output = LspTool::new(Arc::clone(&runtime))
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
        .expect("自动选择必须忽略已诊断为不可用的 Server");
    let result: Value = serde_json::from_str(output_text(&output)).expect("结果应为 JSON");
    assert_eq!(result["server"], "a-available");
    runtime.shutdown_all().await;
}

/// 非空 Runtime 必须只注册一个只读 LSP 工具，且工具分类不承担进程生命周期。
#[test]
fn register_exposes_one_read_only_lsp_tool() {
    let directory = tempdir().expect("创建 LSP 测试目录");
    let file = directory.path().join("main.rs");
    fs::write(&file, "fn main() {}\n").expect("写入测试源文件");
    let runtime = Arc::new(
        LspRuntime::new(
            directory.path(),
            vec![server_config(directory.path(), "missing-lsp-command")],
        )
        .expect("配置应有效"),
    );
    let tool = LspTool::new(Arc::clone(&runtime));
    assert_eq!(
        tool.effect(&json!({ "operation": "hover", "file": "main.rs", "line": 1, "character": 1 }))
            .expect("hover 输入应有效"),
        ToolEffect::ReadOnly
    );
    assert_eq!(tool.concurrency(), ToolConcurrency::ParallelReadOnly);

    let mut registry = ToolRegistry::new();
    register_lsp_tool(&mut registry, runtime).expect("LSP 工具应注册");
    let definitions = registry.definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].name, "LSP");
}

/// 未由宿主预启动的 Runtime 只能失败，工具执行绝不能借只读分类拉起外部进程。
#[tokio::test]
async fn tool_never_starts_an_unprepared_server() {
    let directory = tempdir().expect("创建 LSP 测试目录");
    let file = directory.path().join("main.rs");
    fs::write(&file, "fn main() {}\n").expect("写入测试源文件");
    let tool = LspTool::new(Arc::new(
        LspRuntime::new(
            directory.path(),
            vec![server_config(
                directory.path(),
                "definitely-missing-lsp-command",
            )],
        )
        .expect("配置应有效"),
    ));

    let error = tool
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
        .expect_err("未准备的 Runtime 必须安全失败");
    assert_eq!(error.code, "lsp_not_started");
}

/// 任一 Server 启动失败时，候选必须清理同代次已经启动的全部前序进程。
#[tokio::test]
async fn failed_start_cleans_up_partially_started_generation() {
    let directory = tempdir().expect("创建 LSP 测试目录");
    let executable = compile_fake_lsp(directory.path());
    fs::write(directory.path().join("main.rs"), "fn main() {}\n").expect("写入测试源文件");
    let mut available = server_config(directory.path(), executable.to_string_lossy());
    available.name = "a-available".to_owned();
    let mut missing = server_config(directory.path(), "definitely-missing-lsp-command");
    missing.name = "z-missing".to_owned();
    let runtime = Arc::new(
        LspRuntime::new(directory.path(), vec![available, missing]).expect("候选配置应有效"),
    );

    runtime
        .start_all()
        .await
        .expect_err("后序 Server 启动失败必须拒绝整个候选");
    let error = LspTool::new(runtime)
        .execute(
            tool_context(),
            json!({
                "operation": "hover",
                "server": "a-available",
                "file": "main.rs",
                "line": 1,
                "character": 1
            }),
        )
        .await
        .expect_err("前序 Server 必须已随失败候选清理");
    assert_eq!(error.code, "lsp_not_started");
}

/// 文件型操作必须拒绝项目外路径，Plan 模式因此不会借 LSP 读取外部文件。
#[test]
fn tool_rejects_files_outside_project() {
    let project = tempdir().expect("创建项目目录");
    let outside = tempdir().expect("创建外部目录");
    let outside_file = outside.path().join("outside.rs");
    fs::write(&outside_file, "fn outside() {}\n").expect("写入外部源文件");
    let runtime = Arc::new(
        LspRuntime::new(
            project.path(),
            vec![server_config(project.path(), "missing-lsp-command")],
        )
        .expect("配置应有效"),
    );
    let tool = LspTool::new(runtime);
    let error = tool
        .effect(&json!({
            "operation": "document_symbols",
            "file": outside_file.to_string_lossy()
        }))
        .expect_err("项目外文件必须失败");
    assert_eq!(error.code, "path_outside_project");
}

/// Reader 必须处理分片 Header/正文并保留 Unicode JSON。
#[tokio::test]
async fn framing_reads_fragmented_unicode_message() {
    let (mut writer, mut reader) = duplex(1024);
    let body = r#"{"jsonrpc":"2.0","id":1,"result":"诊断"}"#.as_bytes();
    let header = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    let first = header[..7].to_vec();
    let second = header[7..].to_vec();
    let body = body.to_vec();
    tokio::spawn(async move {
        writer.write_all(&first).await.unwrap();
        writer.write_all(&second).await.unwrap();
        writer.write_all(&body).await.unwrap();
    });

    let message = read_lsp_message(&mut reader)
        .await
        .expect("应读取完整 LSP 帧");
    assert_eq!(message["result"], "诊断");
}

/// Reader 必须在 Header 硬上限处失败，不能继续等待无界输入。
#[tokio::test]
async fn framing_rejects_oversized_header() {
    let (mut writer, mut reader) = duplex(MAX_LSP_HEADER_BYTES * 2);
    tokio::spawn(async move {
        writer
            .write_all(&vec![b'x'; MAX_LSP_HEADER_BYTES + 1])
            .await
            .unwrap();
    });
    let error = read_lsp_message(&mut reader)
        .await
        .expect_err("超大 Header 必须失败");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

/// 诊断缓存必须同时限制条目数与编码字节数。
#[test]
fn diagnostics_cache_is_bounded() {
    let items = (0..1_000)
        .map(|index| json!({ "message": "x".repeat(2_048), "index": index }))
        .collect::<Vec<_>>();
    let bounded = bounded_diagnostics(&items);
    let retained = bounded.as_array().expect("诊断投影应为数组");
    assert!(retained.len() < 500);
    assert!(serde_json::to_vec(&bounded).unwrap().len() <= 256 * 1024);
}

/// 多文件诊断缓存必须固定容量并优先淘汰最久未更新的 URI。
#[test]
fn diagnostics_document_cache_is_lru_bounded() {
    let mut state = DiagnosticsState::default();
    for index in 0..MAX_LSP_DIAGNOSTIC_DOCUMENTS {
        update_diagnostics_cache(
            &mut state,
            &format!("file:///project/{index}.rs"),
            &[json!({ "message": index })],
        )
        .expect("正常 URI 应写入诊断缓存");
    }
    update_diagnostics_cache(
        &mut state,
        "file:///project/0.rs",
        &[json!({ "message": "recent" })],
    )
    .expect("已有 URI 应刷新 LRU 顺序");
    update_diagnostics_cache(
        &mut state,
        "file:///project/new.rs",
        &[json!({ "message": "new" })],
    )
    .expect("新 URI 应触发最旧项淘汰");

    assert_eq!(state.by_uri.len(), MAX_LSP_DIAGNOSTIC_DOCUMENTS);
    assert!(state.by_uri.contains_key("file:///project/0.rs"));
    assert!(!state.by_uri.contains_key("file:///project/1.rs"));
    assert!(state.by_uri.contains_key("file:///project/new.rs"));
    assert_eq!(state.uri_order.len(), MAX_LSP_DIAGNOSTIC_DOCUMENTS);
    let generation = state.generation;
    assert!(
        update_diagnostics_cache(&mut state, &"x".repeat(8 * 1024 + 1), &[]).is_none(),
        "异常大的 URI 不得进入长期缓存"
    );
    assert_eq!(state.generation, generation);
    assert_eq!(state.by_uri.len(), MAX_LSP_DIAGNOSTIC_DOCUMENTS);
}

/// Server 错误说明必须清理控制字符并在 UTF-8 边界截断。
#[test]
fn server_error_message_is_sanitized_and_bounded() {
    let value = format!("前缀\r\n{}", "诊".repeat(4_096));
    let bounded = bounded_server_error_message(&value);

    assert!(!bounded.contains(['\r', '\n']));
    assert!(bounded.len() <= 4 * 1024);
    assert!(bounded.is_char_boundary(bounded.len()));
}

/// 真正启动一个仅依赖标准库的测试 Language Server，并完成 initialize、didOpen 与 hover。
#[tokio::test]
async fn native_process_completes_initialize_and_hover() {
    let directory = tempdir().expect("创建 LSP 进程测试目录");
    let executable = compile_fake_lsp(directory.path());
    let file = directory.path().join("main.rs");
    fs::write(&file, "fn main() {}\n").expect("写入测试源文件");
    let runtime = Arc::new(
        LspRuntime::new(
            directory.path(),
            vec![server_config(
                directory.path(),
                executable.to_string_lossy(),
            )],
        )
        .expect("测试 Runtime 应有效"),
    );
    runtime
        .start_all()
        .await
        .expect("宿主应在注册工具前启动 LSP");
    let tool = LspTool::new(runtime);

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
    let result: Value = serde_json::from_str(output_text(&output)).expect("结果应为 JSON");
    assert_eq!(result["server"], "test-rust");
    assert_eq!(result["result"]["contents"], "fake hover");
}

/// 已启动 Server 的协议连接失败后，查询工具只能报错，不能以只读身份自动重启进程。
#[tokio::test]
async fn read_only_query_never_restarts_a_failed_server() {
    let directory = tempdir().expect("创建 LSP 失败测试目录");
    let executable = compile_exiting_fake_lsp(directory.path());
    let launch_log = directory.path().join("launches.log");
    fs::write(directory.path().join("main.rs"), "fn main() {}\n").expect("写入测试源文件");
    let mut config = server_config(directory.path(), executable.to_string_lossy());
    config.environment.insert(
        "KEENCODE_LSP_LAUNCH_LOG".to_owned(),
        launch_log.to_string_lossy().into_owned(),
    );
    let runtime =
        Arc::new(LspRuntime::new(directory.path(), vec![config]).expect("失败 Server 配置应有效"));
    runtime
        .start_all()
        .await
        .expect("宿主应完成唯一一次受控启动");
    let error = LspTool::new(runtime)
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
        .expect_err("连接关闭必须作为查询错误返回");
    assert!(
        matches!(
            error.code.as_str(),
            "lsp_connection_closed" | "lsp_process_exited" | "lsp_write_failed"
        ),
        "意外错误码：{}",
        error.code
    );
    assert_eq!(
        fs::read_to_string(&launch_log).expect("启动日志应存在"),
        "start\n",
        "只读查询不得触发第二次 LSP 启动"
    );
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

/// 编译一个 initialize 成功、收到 hover 后立即断开的测试 Server，并记录每次启动。
fn compile_exiting_fake_lsp(directory: &Path) -> PathBuf {
    let source = directory.join("exiting_fake_lsp.rs");
    let executable = directory.join(if cfg!(windows) {
        "exiting-fake-lsp.exe"
    } else {
        "exiting-fake-lsp"
    });
    fs::write(
        &source,
        r###"
use std::fs::OpenOptions;
use std::io::{self, BufRead, Read, Write};

fn main() {
    let launch_log = std::env::var_os("KEENCODE_LSP_LAUNCH_LOG").unwrap();
    let mut log = OpenOptions::new().create(true).append(true).open(launch_log).unwrap();
    writeln!(log, "start").unwrap();
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
        if body.contains("\"method\":\"textDocument/hover\"") { return; }
        let Some(id) = extract_id(&body) else { continue; };
        let response = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"capabilities":{{}}}}}}"#);
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
    .expect("写入断线测试 Language Server 源码");
    let output = Command::new("rustc")
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("测试环境必须可以调用 rustc");
    assert!(
        output.status.success(),
        "断线测试 Language Server 编译失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}
