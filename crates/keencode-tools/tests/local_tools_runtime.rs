//! 本地工具在隔离临时项目中的端到端执行集成测试。

use std::fs;
use std::sync::Arc;

use keencode_agent::{
    AgentId, AgentRunner, AgentTool, PlanGuard, RunLimits, SessionId, ToolCallId, ToolContext,
    ToolEffect, ToolRegistry, TurnCancellation, TurnId, TurnRequest,
};
use keencode_model::{
    ContentBlock, ImageSource, Message, MessageRole, ModelStreamEvent, ProviderCapabilities,
    ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason, ToolResultContent,
};
#[cfg(not(windows))]
use keencode_tools::BashTool;
#[cfg(windows)]
use keencode_tools::PowerShellTool;
use keencode_tools::{
    EditTool, GitTool, GlobTool, GrepTool, ReadTool, ToolEnvironment, WriteTool,
    register_local_tools,
};
use serde_json::json;
use tempfile::tempdir;

/// 为单次工具调用构造不含用户项目身份的测试上下文。
fn tool_context(call_id: &str) -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-local-integration").expect("测试 Session ID 有效"),
        turn_id: TurnId::new("turn-local-integration").expect("测试 Turn ID 有效"),
        source_agent_id: AgentId::new("agent-local-integration").expect("测试 Agent ID 有效"),
        tool_call_id: ToolCallId::new(call_id).expect("测试 ToolCall ID 有效"),
        cancellation: TurnCancellation::new(),
    }
}

/// 提取工具输出中的唯一文本块。
fn output_text(output: &keencode_agent::ToolOutput) -> &str {
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("本地工具集成结果应只包含一个文本块");
    };
    text
}

/// 创建一段最终完成的文本模型响应。
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

/// 创建一段包含指定工具调用的模型响应。
fn tool_reply(calls: &[(&str, &str, serde_json::Value)]) -> ScriptedReply {
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

/// 在同一个隔离临时项目中实际执行文件、搜索、Shell 与 Git 工具。
#[tokio::test]
async fn local_tools_execute_complete_workflow_in_isolated_directory() {
    let directory = tempdir().expect("应创建隔离临时项目");
    let artifact_directory = directory.path().join("artifacts");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("临时项目工具环境应有效")
            .with_artifact_directory(&artifact_directory)
            .expect("临时项目输出目录应有效"),
    );

    let mut registry = ToolRegistry::new();
    register_local_tools(&mut registry, Arc::clone(&environment)).expect("本地工具应注册");
    let names = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "Bash",
            "Edit",
            "Git",
            "Glob",
            "Grep",
            "PowerShell",
            "Read",
            "Write"
        ]
    );

    let write = WriteTool::new(Arc::clone(&environment));
    let write_output = write
        .execute(
            tool_context("call-write"),
            json!({
                "file_path": "src/main.rs",
                "content": "fn main() { println!(\"before\"); }\n"
            }),
        )
        .await
        .expect("Write 应创建项目内文件");
    assert!(output_text(&write_output).contains("原子创建"));

    let read = ReadTool::new(Arc::clone(&environment));
    let before = read
        .execute(
            tool_context("call-read-before"),
            json!({ "file_path": "src/main.rs" }),
        )
        .await
        .expect("Read 应读取刚写入的文件");
    assert!(output_text(&before).contains("println!(\"before\")"));

    let edit = EditTool::new(Arc::clone(&environment));
    let edit_output = edit
        .execute(
            tool_context("call-edit"),
            json!({
                "file_path": "src/main.rs",
                "old_string": "before",
                "new_string": "after"
            }),
        )
        .await
        .expect("Edit 应替换项目内文本");
    assert!(output_text(&edit_output).contains("替换"));
    assert!(
        fs::read_to_string(directory.path().join("src/main.rs"))
            .expect("应读取编辑后的文件")
            .contains("after")
    );

    let glob = GlobTool::new(Arc::clone(&environment));
    let glob_output = glob
        .execute(tool_context("call-glob"), json!({ "pattern": "**/*.rs" }))
        .await
        .expect("Glob 应发现项目内源文件");
    assert!(output_text(&glob_output).contains("src/main.rs"));

    let grep = GrepTool::new(Arc::clone(&environment));
    let grep_output = grep
        .execute(
            tool_context("call-grep"),
            json!({ "pattern": "after", "glob": "**/*.rs" }),
        )
        .await
        .expect("Grep 应发现编辑后的文本");
    assert!(output_text(&grep_output).contains("after"));

    #[cfg(windows)]
    let shell_output = PowerShellTool::new(Arc::clone(&environment))
        .execute(
            tool_context("call-powershell"),
            json!({ "command": "[Console]::Out.Write('shell-ok')" }),
        )
        .await
        .expect("PowerShell 应在项目目录中完成命令");
    #[cfg(not(windows))]
    let shell_output = BashTool::new(Arc::clone(&environment))
        .execute(
            tool_context("call-bash"),
            json!({ "command": "printf shell-ok" }),
        )
        .await
        .expect("Bash 应在项目目录中完成命令");
    assert!(output_text(&shell_output).contains("shell-ok"));

    let git = GitTool::new(environment);
    assert_eq!(
        git.effect(&json!({ "args": ["init", "--quiet"] })),
        Ok(ToolEffect::ChangesState)
    );
    git.execute(
        tool_context("call-git-init"),
        json!({ "args": ["init", "--quiet"] }),
    )
    .await
    .expect("Git 应初始化隔离临时仓库");
    let status = git
        .execute(
            tool_context("call-git-status"),
            json!({ "args": ["status", "--short", "--untracked-files=all"] }),
        )
        .await
        .expect("Git status 应读取隔离临时仓库");
    assert!(output_text(&status).contains("?? src/main.rs"));
}

/// 真实 Read 图片结果必须经 Agent Runner 完整进入第二轮 Provider 中立请求。
#[tokio::test]
async fn read_inline_png_is_preserved_in_agent_runner_second_request() {
    // 使用固定的 1x1 合成 PNG，既经过 Read 的签名校验，也便于核对完整 Base64 数据。
    const SYNTHETIC_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xB5,
        0x1C, 0x0C, 0x02, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64,
        0xF8, 0x0F, 0x00, 0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xE3, 0x66, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    const SYNTHETIC_PNG_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

    let directory = tempdir().expect("应创建隔离临时目录");
    let image_path = directory.path().join("pixel.png");
    fs::write(&image_path, SYNTHETIC_PNG).expect("应写入合成 PNG");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));

    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            image_input: true,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(&[("read-image", "Read", json!({ "file_path": "pixel.png" }))]),
            text_reply("image-read-complete"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(ReadTool::new(environment)))
        .expect("真实 Read 工具应注册");

    let request = TurnRequest::new(
        SessionId::new("session-read-image").expect("图片测试 Session ID 应有效"),
        TurnId::new("turn-read-image").expect("图片测试 Turn ID 应有效"),
        AgentId::new("agent-read-image").expect("图片测试 Agent ID 应有效"),
        "test-model",
        vec![Message::text(
            MessageRole::User,
            "读取 pixel.png 并确认图片内容",
        )],
        PlanGuard::inactive(),
    );
    let result = AgentRunner::new(provider.clone(), registry, RunLimits::default())
        .run_turn(request)
        .await;

    assert!(
        result.is_success(),
        "真实 Read 图片 Tool Loop 应正常完成：{:?}",
        result.error
    );
    assert_eq!(result.state.round_count(), 2);
    assert_eq!(result.state.step_count(), 1);
    let final_response = result
        .final_response
        .as_ref()
        .expect("正常完成应保留最终模型响应");
    assert_eq!(final_response.stop_reason, StopReason::Completed);
    assert_eq!(
        final_response.content,
        vec![ContentBlock::text("image-read-complete")]
    );

    let requests = provider.requests().expect("Provider 请求快照应可读取");
    assert_eq!(requests.len(), 2, "应捕获首轮 Read 请求和第二轮最终请求");
    let second_request = &requests[1];
    assert_eq!(second_request.messages.len(), 3);

    let assistant_message = &second_request.messages[1];
    assert_eq!(assistant_message.role, MessageRole::Assistant);
    let [ContentBlock::ToolCall { tool_call }] = assistant_message.content.as_slice() else {
        panic!("第二轮必须保留首轮的唯一 Read 工具调用");
    };
    assert_eq!(tool_call.id, "read-image");
    assert_eq!(tool_call.name, "Read");
    assert_eq!(tool_call.arguments, json!({ "file_path": "pixel.png" }));

    let tool_message = &second_request.messages[2];
    assert_eq!(tool_message.role, MessageRole::Tool);
    let [ContentBlock::ToolResult { tool_result }] = tool_message.content.as_slice() else {
        panic!("第二轮必须保留 Read 的工具结果消息");
    };
    assert_eq!(
        tool_result.tool_call_id, tool_call.id,
        "工具调用与结果 ID 必须配对"
    );
    assert!(!tool_result.is_error, "真实 Read 图片结果不应被归一为错误");

    // 顺序必须是 Read 的说明文本在前、图片块在后，不能丢失任一内容块。
    let [
        ToolResultContent::Text { text },
        ToolResultContent::Image { image },
    ] = tool_result.content.as_slice()
    else {
        panic!("Read 图片结果必须保持文本块在前、图片块在后的顺序");
    };
    let expected_text = format!(
        "图片：{}（{} 字节）",
        fs::canonicalize(&image_path)
            .expect("Read 结果应使用实际文件的规范路径")
            .to_string_lossy()
            .replace('\\', "/"),
        SYNTHETIC_PNG.len()
    );
    assert_eq!(text, &expected_text);
    let ImageSource::Base64 { media_type, data } = &image.source else {
        panic!("Read 图片结果必须使用 Base64 内联来源");
    };
    assert_eq!(media_type, "image/png");
    assert_eq!(data, SYNTHETIC_PNG_BASE64, "PNG Base64 必须完整保留");
}

/// 绝对路径不受项目目录边界限制，并且 Shell 会使用请求给出的外部 cwd。
#[tokio::test]
async fn local_tools_execute_absolute_paths_in_sibling_temp_directory() {
    let project = tempdir().expect("应创建隔离临时项目");
    let external = tempdir().expect("应创建与项目平级的外部临时目录");
    let temp_root = fs::canonicalize(std::env::temp_dir()).expect("系统临时根目录应可解析");
    let project_path = fs::canonicalize(project.path()).expect("项目临时目录应可解析");
    let external_path = fs::canonicalize(external.path()).expect("外部临时目录应可解析");
    assert_eq!(
        project_path.parent(),
        external_path.parent(),
        "两个夹具必须位于同一个安全临时目录下"
    );
    assert!(
        project_path.starts_with(&temp_root) && external_path.starts_with(&temp_root),
        "绝对路径测试必须限制在系统临时目录内"
    );

    let environment = Arc::new(ToolEnvironment::new(&project_path).expect("项目工具环境应有效"));
    let external_file = external_path.join("outside.txt");
    let external_file_text = external_file.to_string_lossy().into_owned();

    WriteTool::new(Arc::clone(&environment))
        .execute(
            tool_context("call-absolute-write"),
            json!({
                "file_path": external_file_text,
                "content": "outside-before\n"
            }),
        )
        .await
        .expect("Write 应能写入项目外的绝对路径");
    assert_eq!(
        fs::read_to_string(&external_file).expect("应读取项目外文件"),
        "outside-before\n"
    );

    EditTool::new(Arc::clone(&environment))
        .execute(
            tool_context("call-absolute-edit"),
            json!({
                "file_path": external_file.to_string_lossy(),
                "old_string": "outside-before",
                "new_string": "outside-after"
            }),
        )
        .await
        .expect("Edit 应能修改项目外的绝对路径");

    let read = ReadTool::new(Arc::clone(&environment))
        .execute(
            tool_context("call-absolute-read"),
            json!({ "file_path": external_file.to_string_lossy() }),
        )
        .await
        .expect("Read 应能读取项目外的绝对路径");
    assert!(output_text(&read).contains("outside-after"));

    #[cfg(windows)]
    let shell_output = PowerShellTool::new(Arc::clone(&environment))
        .execute(
            tool_context("call-absolute-shell-cwd"),
            json!({
                "command": "[Environment]::CurrentDirectory",
                "cwd": external_path.to_string_lossy()
            }),
        )
        .await
        .expect("PowerShell 应能使用项目外绝对 cwd");
    #[cfg(not(windows))]
    let shell_output = BashTool::new(Arc::clone(&environment))
        .execute(
            tool_context("call-absolute-shell-cwd"),
            json!({
                "command": "pwd -P",
                "cwd": external_path.to_string_lossy()
            }),
        )
        .await
        .expect("Bash 应能使用项目外绝对 cwd");
    let shell_text = output_text(&shell_output);
    let expected_cwd = external_path
        .to_string_lossy()
        .replace('\\', "/")
        .replace("//?/", "");
    assert!(
        shell_text.replace('\\', "/").contains(&expected_cwd),
        "Shell 报告必须包含实际的项目外绝对 cwd，实际报告：{shell_text}"
    );

    // 同一条真实 Agent Runner 路径确认项目外绝对路径不被当前工具环境拦截。
    let runner_file = external_path.join("runner-full-access.txt");
    let runner_file_text = runner_file.to_string_lossy().into_owned();
    #[cfg(windows)]
    let runner_shell_name = "PowerShell";
    #[cfg(not(windows))]
    let runner_shell_name = "Bash";
    #[cfg(windows)]
    let runner_shell_command = "Write-Output full-access-cwd";
    #[cfg(not(windows))]
    let runner_shell_command = "printf full-access-cwd";

    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[(
                "full-write",
                "Write",
                json!({
                    "file_path": runner_file_text,
                    "content": "runner-before\n"
                }),
            )]),
            tool_reply(&[
                (
                    "full-edit",
                    "Edit",
                    json!({
                        "file_path": runner_file.to_string_lossy(),
                        "old_string": "runner-before",
                        "new_string": "runner-after"
                    }),
                ),
                (
                    "full-shell",
                    runner_shell_name,
                    json!({
                        "command": runner_shell_command,
                        "cwd": external_path.to_string_lossy()
                    }),
                ),
            ]),
            text_reply("full-access-complete"),
        ],
    ));
    let mut runner_registry = ToolRegistry::new();
    runner_registry
        .register(Arc::new(ReadTool::new(Arc::clone(&environment))))
        .expect("Runner Read 工具应注册");
    runner_registry
        .register(Arc::new(EditTool::new(Arc::clone(&environment))))
        .expect("Runner Edit 工具应注册");
    runner_registry
        .register(Arc::new(WriteTool::new(Arc::clone(&environment))))
        .expect("Runner Write 工具应注册");
    #[cfg(windows)]
    runner_registry
        .register(Arc::new(PowerShellTool::new(Arc::clone(&environment))))
        .expect("Runner PowerShell 工具应注册");
    #[cfg(not(windows))]
    runner_registry
        .register(Arc::new(BashTool::new(Arc::clone(&environment))))
        .expect("Runner Bash 工具应注册");

    let request = TurnRequest::new(
        SessionId::new("session-full-access").expect("FullAccess Session ID 应有效"),
        TurnId::new("turn-full-access").expect("FullAccess Turn ID 应有效"),
        AgentId::new("agent-full-access").expect("FullAccess Agent ID 应有效"),
        "test-model",
        vec![Message::text(MessageRole::User, "验证项目内外完全访问")],
        PlanGuard::inactive(),
    );
    let result = AgentRunner::new(provider, runner_registry, RunLimits::default())
        .run_turn(request)
        .await;
    assert!(
        result.is_success(),
        "绝对路径 Runner 不应失败：{:?}",
        result.error
    );
    assert_eq!(
        fs::read_to_string(&runner_file).expect("绝对路径 Runner 应写入外部文件"),
        "runner-after\n"
    );
}
