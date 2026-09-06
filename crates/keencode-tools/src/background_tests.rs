//! 真实后台 Shell 任务、持久输出、停止和 shutdown 集成测试。

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolContext, ToolEffect, ToolRegistry,
    TurnCancellation, TurnId,
};
use keencode_model::ToolResultContent;
use serde_json::json;
use tempfile::TempDir;

use crate::{
    BackgroundOutputCursor, BackgroundTaskManager, BackgroundTaskStatus, TaskOutputTool,
    TaskStopTool, ToolEnvironment, register_local_tools_with_background,
};

/// Windows 并发测试负载下等待真实进程输出或终态的宽松上限。
const TEST_PROCESS_WAIT: Duration = Duration::from_secs(15);
/// 传入 `TaskOutput` Schema 的测试等待毫秒数。
const TEST_PROCESS_WAIT_MILLISECONDS: u64 = 15_000;

#[cfg(not(windows))]
use crate::BashTool;
#[cfg(windows)]
use crate::PowerShellTool;

/// 创建一个测试独占目录、工具环境与跨 Turn Manager。
fn background_fixture() -> (TempDir, Arc<ToolEnvironment>, Arc<BackgroundTaskManager>) {
    background_fixture_with_limit(None)
}

/// 创建一个可覆盖单次输出字节预算的测试独占后台任务环境。
fn background_fixture_with_limit(
    max_output_chunk_bytes: Option<usize>,
) -> (TempDir, Arc<ToolEnvironment>, Arc<BackgroundTaskManager>) {
    let directory = tempfile::tempdir().expect("应创建后台任务测试目录");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_artifact_directory(directory.path().join("artifacts"))
            .expect("输出目录应有效"),
    );
    let manager = Arc::new(
        BackgroundTaskManager::new(
            directory.path().join("background"),
            max_output_chunk_bytes.unwrap_or(environment.limits().max_command_preview_bytes),
        )
        .expect("后台任务 Manager 应有效"),
    );
    (directory, environment, manager)
}

/// 创建每次测试独立的可信工具调用上下文。
fn background_tool_context() -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-background").expect("测试 Session ID 应有效"),
        turn_id: TurnId::new("turn-background").expect("测试 Turn ID 应有效"),
        source_agent_id: AgentId::new("agent-background").expect("测试 Agent ID 应有效"),
        tool_call_id: ToolCallId::new("call-background").expect("测试 ToolCall ID 应有效"),
        cancellation: TurnCancellation::new(),
    }
}

/// 提取工具的唯一文本结果。
fn background_output_text(output: &keencode_agent::ToolOutput) -> &str {
    let [ToolResultContent::Text { text }] = output.content.as_slice() else {
        panic!("后台工具结果必须只有一个文本块");
    };
    text
}

/// 使用当前平台 Shell 启动一个后台命令。
#[cfg(windows)]
async fn launch_background(
    environment: Arc<ToolEnvironment>,
    manager: Arc<BackgroundTaskManager>,
    command: &str,
) -> Result<keencode_agent::ToolOutput, keencode_agent::ToolError> {
    PowerShellTool::with_background_tasks(environment, manager)
        .execute(
            background_tool_context(),
            json!({
                "command": command,
                "description": "后台任务集成测试",
                "run_in_background": true
            }),
        )
        .await
}

/// 使用当前平台 Shell 启动一个带明确超时的后台命令。
#[cfg(windows)]
async fn launch_background_with_timeout(
    environment: Arc<ToolEnvironment>,
    manager: Arc<BackgroundTaskManager>,
    command: &str,
    timeout_ms: u64,
) -> Result<keencode_agent::ToolOutput, keencode_agent::ToolError> {
    PowerShellTool::with_background_tasks(environment, manager)
        .execute(
            background_tool_context(),
            json!({
                "command": command,
                "description": "后台任务超时测试",
                "timeout_ms": timeout_ms,
                "run_in_background": true
            }),
        )
        .await
}

/// 使用当前平台 Shell 启动一个后台命令。
#[cfg(not(windows))]
async fn launch_background(
    environment: Arc<ToolEnvironment>,
    manager: Arc<BackgroundTaskManager>,
    command: &str,
) -> Result<keencode_agent::ToolOutput, keencode_agent::ToolError> {
    BashTool::with_background_tasks(environment, manager)
        .execute(
            background_tool_context(),
            json!({
                "command": command,
                "description": "后台任务集成测试",
                "run_in_background": true
            }),
        )
        .await
}

/// 使用当前平台 Shell 启动一个带明确超时的后台命令。
#[cfg(not(windows))]
async fn launch_background_with_timeout(
    environment: Arc<ToolEnvironment>,
    manager: Arc<BackgroundTaskManager>,
    command: &str,
    timeout_ms: u64,
) -> Result<keencode_agent::ToolOutput, keencode_agent::ToolError> {
    BashTool::with_background_tasks(environment, manager)
        .execute(
            background_tool_context(),
            json!({
                "command": command,
                "description": "后台任务超时测试",
                "timeout_ms": timeout_ms,
                "run_in_background": true
            }),
        )
        .await
}

/// 当前平台用于验证 stdout、stderr 和增量等待的命令。
#[cfg(windows)]
fn incremental_command() -> &'static str {
    "[Console]::Out.Write('first'); [Console]::Error.Write('problem'); Start-Sleep -Milliseconds 500; [Console]::Out.Write('second')"
}

/// 当前平台用于验证 stdout、stderr 和增量等待的命令。
#[cfg(not(windows))]
fn incremental_command() -> &'static str {
    "printf first; printf problem >&2; sleep 0.5; printf second"
}

/// 当前平台用于验证完整进程树停止的长任务命令。
#[cfg(windows)]
fn cancellable_command() -> &'static str {
    "Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile','-NonInteractive','-Command',\"Start-Sleep -Milliseconds 800; Set-Content -LiteralPath 'leaked.txt' -Value 'leaked'\") -WindowStyle Hidden; Start-Sleep -Seconds 10"
}

/// 当前平台用于验证后台硬超时的长任务命令。
#[cfg(windows)]
fn timeout_command() -> &'static str {
    "Start-Sleep -Seconds 10"
}

/// 当前平台用于验证完整进程树停止的长任务命令。
#[cfg(not(windows))]
fn cancellable_command() -> &'static str {
    "(sleep 0.8; printf leaked > leaked.txt) & wait"
}

/// 当前平台用于验证后台硬超时的长任务命令。
#[cfg(not(windows))]
fn timeout_command() -> &'static str {
    "sleep 10"
}

/// 用于验证 UTF-8 边界的完整预期文本。
const UTF8_BOUNDARY_TEXT: &str = "abcdef🙂中文🚀尾";

/// 用于验证损失解码与游标活性的任意二进制字节。
const BINARY_OUTPUT_BYTES: &[u8] = &[
    0xff, 0x41, 0x42, 0x43, 0x44, 0x45, 0xf0, 0x9f, 0xff, 0x80, 0xc2,
];

/// 当前平台一次写出完整 UTF-8 测试文本的命令。
#[cfg(windows)]
fn utf8_bulk_output_command() -> &'static str {
    "$stream = [Console]::OpenStandardOutput(); $bytes = [byte[]](0x61,0x62,0x63,0x64,0x65,0x66,0xf0,0x9f,0x99,0x82,0xe4,0xb8,0xad,0xe6,0x96,0x87,0xf0,0x9f,0x9a,0x80,0xe5,0xb0,0xbe); $stream.Write($bytes, 0, $bytes.Length); $stream.Flush()"
}

/// 当前平台一次写出完整 UTF-8 测试文本的命令。
#[cfg(not(windows))]
fn utf8_bulk_output_command() -> &'static str {
    "printf '\\141\\142\\143\\144\\145\\146\\360\\237\\231\\202\\344\\270\\255\\346\\226\\207\\360\\237\\232\\200\\345\\260\\276'"
}

/// 当前平台逐字节刷新 UTF-8 测试文本的命令，用来强制管道产生半个标量。
#[cfg(windows)]
fn utf8_bytewise_output_command() -> &'static str {
    "Start-Sleep -Milliseconds 250; $stream = [Console]::OpenStandardOutput(); $bytes = [byte[]](0x61,0x62,0x63,0x64,0x65,0x66,0xf0,0x9f,0x99,0x82,0xe4,0xb8,0xad,0xe6,0x96,0x87,0xf0,0x9f,0x9a,0x80,0xe5,0xb0,0xbe); foreach ($byte in $bytes) { $stream.WriteByte($byte); $stream.Flush(); Start-Sleep -Milliseconds 40 }"
}

/// 当前平台逐字节刷新 UTF-8 测试文本的命令，用来强制管道产生半个标量。
#[cfg(not(windows))]
fn utf8_bytewise_output_command() -> &'static str {
    "sleep 0.25; for byte in 141 142 143 144 145 146 360 237 231 202 344 270 255 346 226 207 360 237 232 200 345 260 276; do printf \"\\\\$byte\"; sleep 0.04; done"
}

/// 当前平台写出包含无效和截断 UTF-8 序列的任意二进制命令。
#[cfg(windows)]
fn binary_output_command() -> &'static str {
    "$stream = [Console]::OpenStandardOutput(); $bytes = [byte[]](0xff,0x41,0x42,0x43,0x44,0x45,0xf0,0x9f,0xff,0x80,0xc2); $stream.Write($bytes, 0, $bytes.Length); $stream.Flush()"
}

/// 当前平台写出包含无效和截断 UTF-8 序列的任意二进制命令。
#[cfg(not(windows))]
fn binary_output_command() -> &'static str {
    "printf '\\377\\101\\102\\103\\104\\105\\360\\237\\377\\200\\302'"
}

/// 显式后台注册入口必须提供两个任务工具和支持后台参数的 Shell 工具。
#[test]
fn background_registration_exposes_real_task_tools() {
    let (_directory, environment, manager) = background_fixture();
    let mut registry = ToolRegistry::new();
    register_local_tools_with_background(&mut registry, environment, manager)
        .expect("完整后台工具应注册");

    let definitions = registry.definitions();
    let names = definitions
        .iter()
        .map(|definition| definition.name.as_str())
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
            "TaskOutput",
            "TaskStop",
            "Write",
        ]
    );
    let shell = definitions
        .iter()
        .find(|definition| definition.name == if cfg!(windows) { "PowerShell" } else { "Bash" })
        .expect("应注册当前平台 Shell");
    assert_eq!(
        shell.input_schema["properties"]["run_in_background"]["type"],
        "boolean"
    );
}

/// 总输出预算必须保证两个同时活跃的流都能容纳一个完整四字节 UTF-8 标量。
#[test]
fn background_output_limit_rejects_budget_too_small_for_utf8_progress() {
    let directory = tempfile::tempdir().expect("应创建后台任务测试目录");
    let error = BackgroundTaskManager::new(directory.path().join("background"), 7)
        .err()
        .expect("小于八字节的总预算必须拒绝");
    assert_eq!(error.code, "invalid_background_output_limit");
}

/// 已完成输出即使在读取预算边缘跨越中文或 emoji，也不能产生替换符或丢字节。
#[tokio::test]
async fn completed_utf8_output_keeps_scalars_whole_across_read_chunks() {
    let (_directory, environment, manager) = background_fixture_with_limit(Some(8));
    let mut completions = manager.subscribe_completions();
    launch_background(environment, manager.clone(), utf8_bulk_output_command())
        .await
        .expect("UTF-8 后台命令应启动");
    let task_id = manager.list().expect("应列出 UTF-8 任务")[0]
        .task_id
        .clone();
    let completion = tokio::time::timeout(TEST_PROCESS_WAIT, completions.recv())
        .await
        .expect("UTF-8 任务完成不应超时")
        .expect("UTF-8 任务完成事件应可读");
    assert_eq!(completion.status, BackgroundTaskStatus::Succeeded);

    let mut cursor = BackgroundOutputCursor::default();
    let mut combined = String::new();
    for _ in 0..16 {
        let output = manager
            .read_output("session-background", &task_id, cursor, None)
            .await
            .expect("UTF-8 增量应可读");
        assert!(!output.stdout.contains('\u{fffd}'));
        if output.task.stdout_bytes > cursor.stdout_offset {
            assert!(output.next_cursor.stdout_offset > cursor.stdout_offset);
        }
        combined.push_str(&output.stdout);
        cursor = output.next_cursor;
        if !output.stdout_has_more {
            break;
        }
    }

    assert_eq!(combined, UTF8_BOUNDARY_TEXT);
    assert_eq!(cursor.stdout_offset, UTF8_BOUNDARY_TEXT.len() as u64);
}

/// 管道逐字节发布中文和 emoji 时，只能在完整标量落盘后唤醒增量读取者。
#[tokio::test]
async fn bytewise_utf8_pipe_never_publishes_partial_scalars() {
    let (_directory, environment, manager) = background_fixture_with_limit(Some(8));
    launch_background(environment, manager.clone(), utf8_bytewise_output_command())
        .await
        .expect("逐字节 UTF-8 后台命令应启动");
    let task_id = manager.list().expect("应列出逐字节任务")[0].task_id.clone();

    let mut cursor = BackgroundOutputCursor::default();
    let mut combined = String::new();
    let mut reached_terminal = false;
    for _ in 0..64 {
        let output = manager
            .read_output(
                "session-background",
                &task_id,
                cursor,
                Some(TEST_PROCESS_WAIT),
            )
            .await
            .expect("逐字节 UTF-8 增量应可读");
        assert!(!output.stdout.contains('\u{fffd}'));
        if output.task.stdout_bytes > cursor.stdout_offset {
            assert!(output.next_cursor.stdout_offset > cursor.stdout_offset);
        }
        combined.push_str(&output.stdout);
        cursor = output.next_cursor;
        if output.task.status.is_terminal() && !output.stdout_has_more {
            reached_terminal = true;
            break;
        }
    }

    assert!(reached_terminal, "逐字节任务必须在有限读取次数内结束");
    assert_eq!(combined, UTF8_BOUNDARY_TEXT);
    assert_eq!(cursor.stdout_offset, UTF8_BOUNDARY_TEXT.len() as u64);
}

/// 任意二进制必须保留原始持久字节、执行有界损失解码并在有限次数内耗尽游标。
#[tokio::test]
async fn binary_output_is_lossy_bounded_and_always_advances_cursor() {
    let (_directory, environment, manager) = background_fixture_with_limit(Some(8));
    let mut completions = manager.subscribe_completions();
    launch_background(environment, manager.clone(), binary_output_command())
        .await
        .expect("二进制后台命令应启动");
    let task_id = manager.list().expect("应列出二进制任务")[0].task_id.clone();
    tokio::time::timeout(TEST_PROCESS_WAIT, completions.recv())
        .await
        .expect("二进制任务完成不应超时")
        .expect("二进制任务完成事件应可读");

    let mut cursor = BackgroundOutputCursor::default();
    let mut combined = String::new();
    let mut reads = 0_usize;
    loop {
        reads += 1;
        assert!(reads <= BINARY_OUTPUT_BYTES.len(), "二进制读取不得死循环");
        let output = manager
            .read_output("session-background", &task_id, cursor, None)
            .await
            .expect("二进制增量应可损失读取");
        if output.task.stdout_bytes > cursor.stdout_offset {
            assert!(output.next_cursor.stdout_offset > cursor.stdout_offset);
        }
        assert!(output.next_cursor.stdout_offset - cursor.stdout_offset <= 8);
        combined.push_str(&output.stdout);
        cursor = output.next_cursor;
        if !output.stdout_has_more {
            break;
        }
    }

    assert_eq!(
        combined,
        String::from_utf8_lossy(BINARY_OUTPUT_BYTES).into_owned()
    );
    assert_eq!(cursor.stdout_offset, BINARY_OUTPUT_BYTES.len() as u64);
    assert_eq!(
        fs::read(manager.output_directory().join(&task_id).join("stdout.log"))
            .expect("二进制 stdout 必须原样持久化"),
        BINARY_OUTPUT_BYTES
    );
}

/// 后台进程必须立即登记、增量读取两个输出流、持久落盘并广播唯一成功事件。
#[tokio::test]
async fn background_process_persists_incremental_output_and_completion() {
    let (_directory, environment, manager) = background_fixture();
    let mut completions = manager.subscribe_completions();
    let started = launch_background(environment, manager.clone(), incremental_command())
        .await
        .expect("后台命令应启动");
    assert!(background_output_text(&started).contains("TaskOutput"));

    let running = manager.list_running().expect("应列出真实运行中任务");
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].status, BackgroundTaskStatus::Running);
    assert!(running[0].pid.is_some());
    let task_id = running[0].task_id.clone();

    let first = manager
        .read_output(
            "session-background",
            &task_id,
            BackgroundOutputCursor::default(),
            Some(TEST_PROCESS_WAIT),
        )
        .await
        .expect("应读取首个增量");
    assert!(first.stdout.contains("first") || first.stderr.contains("problem"));
    let mut cursor = first.next_cursor;
    let mut combined = format!("{}{}", first.stdout, first.stderr);
    for _ in 0..4 {
        let next = manager
            .read_output(
                "session-background",
                &task_id,
                cursor,
                Some(TEST_PROCESS_WAIT),
            )
            .await
            .expect("应读取后续增量或终态");
        cursor = next.next_cursor;
        combined.push_str(&next.stdout);
        combined.push_str(&next.stderr);
        if next.task.status.is_terminal() && !next.stdout_has_more && !next.stderr_has_more {
            break;
        }
    }
    assert!(combined.contains("first"));
    assert!(combined.contains("problem"));
    assert!(combined.contains("second"));

    let completion = tokio::time::timeout(TEST_PROCESS_WAIT, completions.recv())
        .await
        .expect("完成事件不应超时")
        .expect("完成事件广播应可读");
    assert_eq!(completion.task_id, task_id);
    assert_eq!(completion.status, BackgroundTaskStatus::Succeeded);
    assert!(manager.list_running().expect("应查询运行任务").is_empty());
    let finished = manager.list().expect("完成任务必须保留");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0].status, BackgroundTaskStatus::Succeeded);
    assert_eq!(
        fs::read(manager.output_directory().join(&task_id).join("stdout.log"))
            .expect("stdout 必须持久存在"),
        b"firstsecond"
    );
    assert_eq!(
        fs::read(manager.output_directory().join(&task_id).join("stderr.log"))
            .expect("stderr 必须持久存在"),
        b"problem"
    );
}

/// `TaskOutput` 必须推进共享游标，后续调用不能重复伪造同一输出。
#[tokio::test]
async fn task_output_tool_consumes_only_new_persisted_bytes() {
    let (_directory, environment, manager) = background_fixture();
    launch_background(environment, manager.clone(), incremental_command())
        .await
        .expect("后台命令应启动");
    let task_id = manager.list().expect("应列出任务")[0].task_id.clone();
    let tool = TaskOutputTool::new(manager);

    assert_eq!(
        tool.effect(&json!({ "task_id": task_id, "block": false })),
        Ok(ToolEffect::ReadOnly)
    );
    let first = tool
        .execute(
            background_tool_context(),
            json!({ "task_id": task_id, "timeout_ms": TEST_PROCESS_WAIT_MILLISECONDS }),
        )
        .await
        .expect("首次 TaskOutput 应成功");
    let first_text = background_output_text(&first);
    assert!(first_text.contains("first") || first_text.contains("problem"));
    let mut later_text = String::new();
    for _ in 0..4 {
        let next = tool
            .execute(
                background_tool_context(),
                json!({ "task_id": task_id, "timeout_ms": TEST_PROCESS_WAIT_MILLISECONDS }),
            )
            .await
            .expect("后续 TaskOutput 应成功");
        later_text.push_str(background_output_text(&next));
        if background_output_text(&next).contains("状态：succeeded") {
            break;
        }
    }
    assert!(!later_text.contains("first"));
    assert!(later_text.contains("second"));
    assert!(later_text.contains("状态：succeeded"));
}

/// `TaskStop` 只在真实发出一次停止信号时成功，并必须杀死后代进程。
#[tokio::test]
async fn task_stop_cancels_complete_process_tree_without_fake_success() {
    let (directory, environment, manager) = background_fixture();
    let mut completions = manager.subscribe_completions();
    launch_background(environment, manager.clone(), cancellable_command())
        .await
        .expect("长后台命令应启动");
    let task_id = manager.list_running().expect("应有运行任务")[0]
        .task_id
        .clone();
    let tool = TaskStopTool::new(manager.clone());

    assert_eq!(
        tool.effect(&json!({ "task_id": task_id })),
        Ok(ToolEffect::ChangesState)
    );
    let stopped = tool
        .execute(background_tool_context(), json!({ "task_id": task_id }))
        .await
        .expect("首次停止必须真实发出信号");
    assert!(background_output_text(&stopped).contains("已向后台任务"));
    let duplicate = tool
        .execute(background_tool_context(), json!({ "task_id": task_id }))
        .await
        .expect_err("重复停止不能伪装成功");
    assert!(matches!(
        duplicate.code.as_str(),
        "background_task_stop_already_requested" | "background_task_not_running"
    ));

    let completion = tokio::time::timeout(TEST_PROCESS_WAIT, completions.recv())
        .await
        .expect("取消完成事件不应超时")
        .expect("取消完成事件应可读");
    assert_eq!(completion.status, BackgroundTaskStatus::Cancelled);
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!directory.path().join("leaked.txt").exists());
    let missing = tool
        .execute(
            background_tool_context(),
            json!({ "task_id": "missing-task" }),
        )
        .await
        .expect_err("不存在任务不能伪装停止成功");
    assert_eq!(missing.code, "background_task_not_found");
}

/// 后台硬超时必须回收进程并提交失败事件，而不能继续显示 running。
#[tokio::test]
async fn background_timeout_commits_failed_completion() {
    let (_directory, environment, manager) = background_fixture();
    let mut completions = manager.subscribe_completions();
    launch_background_with_timeout(environment, manager.clone(), timeout_command(), 100)
        .await
        .expect("带超时后台任务应先成功启动");
    let task_id = manager.list_running().expect("应登记超时任务")[0]
        .task_id
        .clone();

    let completion = tokio::time::timeout(TEST_PROCESS_WAIT, completions.recv())
        .await
        .expect("超时完成事件不应继续挂起")
        .expect("超时完成事件应可读");
    assert_eq!(completion.task_id, task_id);
    assert_eq!(completion.status, BackgroundTaskStatus::Failed);
    assert!(completion.summary.contains("超时"));
    let task = manager
        .task_info("session-background", &task_id)
        .expect("超时任务状态应保留");
    assert_eq!(task.status, BackgroundTaskStatus::Failed);
    assert!(manager.list_running().expect("应查询运行任务").is_empty());
}

/// 任务读取和停止必须同时绑定 Session，显式越界游标也必须失败关闭。
#[tokio::test]
async fn task_access_rejects_cross_session_and_invalid_cursor() {
    let (_directory, environment, manager) = background_fixture();
    launch_background(environment, manager.clone(), incremental_command())
        .await
        .expect("后台任务应启动");
    let task_id = manager.list().expect("应列出任务")[0].task_id.clone();

    let read_error = manager
        .read_output(
            "other-session",
            &task_id,
            BackgroundOutputCursor::default(),
            None,
        )
        .await
        .expect_err("其他 Session 不能读取任务");
    assert_eq!(read_error.code, "background_task_session_mismatch");
    let stop_error = manager
        .cancel("other-session", &task_id)
        .expect_err("其他 Session 不能停止任务");
    assert_eq!(stop_error.code, "background_task_session_mismatch");
    let cursor_error = manager
        .read_output(
            "session-background",
            &task_id,
            BackgroundOutputCursor {
                stdout_offset: u64::MAX,
                stderr_offset: 0,
            },
            None,
        )
        .await
        .expect_err("越界游标必须失败关闭");
    assert_eq!(cursor_error.code, "background_output_cursor_out_of_range");
    manager.shutdown().await.expect("测试结束应回收进程");
}

/// shutdown 必须停止接收新任务、取消全部运行进程并等待真实终态。
#[tokio::test]
async fn shutdown_cancels_all_tasks_and_rejects_new_work() {
    let (_directory, environment, manager) = background_fixture();
    launch_background(environment.clone(), manager.clone(), cancellable_command())
        .await
        .expect("第一个后台任务应启动");
    launch_background(environment.clone(), manager.clone(), cancellable_command())
        .await
        .expect("第二个后台任务应启动");
    assert_eq!(manager.list_running().expect("应列出任务").len(), 2);

    let report = manager.shutdown().await.expect("shutdown 应完成回收");
    assert_eq!(report.cancelled_task_ids.len(), 2);
    assert!(!manager.is_accepting_tasks());
    assert!(manager.list_running().expect("不应残留运行任务").is_empty());
    assert!(
        manager
            .list()
            .expect("终态记录应保留")
            .iter()
            .all(|task| task.status == BackgroundTaskStatus::Cancelled)
    );
    let rejected = launch_background(environment, manager, "echo should-not-run")
        .await
        .expect_err("shutdown 后不得接受新任务");
    assert_eq!(rejected.code, "background_manager_shut_down");
}

/// 完成任务只有显式清理后才移除记录与持久输出。
#[tokio::test]
async fn remove_finished_deletes_record_and_persisted_output() {
    let (_directory, environment, manager) = background_fixture();
    let mut completions = manager.subscribe_completions();
    launch_background(environment, manager.clone(), incremental_command())
        .await
        .expect("后台任务应启动");
    let task_id = manager.list().expect("应列出任务")[0].task_id.clone();
    tokio::time::timeout(TEST_PROCESS_WAIT, completions.recv())
        .await
        .expect("完成事件不应超时")
        .expect("完成事件应可读");
    let output_directory = manager.output_directory().join(&task_id);
    assert!(output_directory.exists());

    manager
        .remove_finished("session-background", &task_id)
        .await
        .expect("显式清理完成任务应成功");
    assert!(manager.list().expect("任务表应可读").is_empty());
    assert!(!output_directory.exists());
}

/// Manager 直接接口必须区分首次取消、重复取消、跨 Session 和已终态任务。
#[tokio::test]
async fn manager_cancel_reports_exact_idempotent_outcomes() {
    let (_directory, environment, manager) = background_fixture();
    let mut completions = manager.subscribe_completions();
    launch_background(environment, manager.clone(), cancellable_command())
        .await
        .expect("可取消后台命令应启动");
    let task_id = manager
        .list_running()
        .expect("应列出运行任务")
        .first()
        .expect("应存在运行任务")
        .task_id
        .clone();

    let cross_session = manager
        .cancel("other-session", &task_id)
        .expect_err("跨 Session 取消不得伪装成功");
    assert_eq!(cross_session.code, "background_task_session_mismatch");

    let first = manager
        .cancel("session-background", &task_id)
        .expect("首次 Manager 取消应真实发出信号");
    assert!(first.stop_requested);
    let duplicate = manager
        .cancel("session-background", &task_id)
        .expect_err("重复 Manager 取消必须明确失败");
    assert_eq!(duplicate.code, "background_task_stop_already_requested");

    let completion = tokio::time::timeout(TEST_PROCESS_WAIT, completions.recv())
        .await
        .expect("取消完成事件不应超时")
        .expect("取消完成事件应可读");
    assert_eq!(completion.task_id, task_id);
    assert_eq!(completion.status, BackgroundTaskStatus::Cancelled);

    let terminal = manager
        .cancel("session-background", &task_id)
        .expect_err("终态任务取消必须明确失败");
    assert_eq!(terminal.code, "background_task_not_running");
}
