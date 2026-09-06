//! 真实 Agent Runner、工具注册表与 Plan 只读守卫的边界集成测试。

use std::collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher};
#[cfg(windows)]
use std::ffi::c_void;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use keencode_agent::{
    AgentId, AgentRunner, AgentTool, PlanGuard, RunLimits, SessionId, ToolConcurrency, ToolContext,
    ToolEffect, ToolError, ToolFuture, ToolRegistry, TurnId, TurnRequest,
};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities, ResponseMetadata,
    ScriptedProvider, ScriptedReply, StopReason,
};
#[cfg(not(windows))]
use keencode_tools::BashTool;
#[cfg(windows)]
use keencode_tools::PowerShellTool;
use keencode_tools::{EditTool, GitTool, GlobTool, GrepTool, ReadTool, ToolEnvironment, WriteTool};
use serde_json::{Value, json};
use tempfile::tempdir;

/// 记录真实工具是否进入 execute；definition、effect 与并发属性仍全部委托给真实实现。
struct CountingTool {
    /// 被观测的真实内置工具。
    inner: Arc<dyn AgentTool>,
    /// 真实 execute 进入次数。
    executions: Arc<AtomicUsize>,
}

impl CountingTool {
    /// 创建一个只增加观测计数、不改变工具行为的包装器。
    fn new(inner: Arc<dyn AgentTool>, executions: Arc<AtomicUsize>) -> Self {
        Self { inner, executions }
    }
}

impl AgentTool for CountingTool {
    /// 返回真实工具注册时冻结的定义。
    fn definition(&self) -> keencode_model::ToolDefinition {
        self.inner.definition()
    }

    /// 使用真实工具判断本次调用的副作用类别。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        self.inner.effect(input)
    }

    /// 使用真实工具声明的并发边界。
    fn concurrency(&self) -> ToolConcurrency {
        self.inner.concurrency()
    }

    /// 记录进入真实执行阶段后再委托调用。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        self.inner.execute(context, input)
    }
}

/// 创建一段包含指定工具调用的脚本化模型响应。
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

/// 递归采集安全临时目录的文件树、文件长度与内容哈希。
fn tree_snapshot(root: &Path) -> Vec<String> {
    fn visit(root: &Path, current: &Path, snapshot: &mut Vec<String>) {
        let mut children = fs::read_dir(current)
            .expect("临时目录应可遍历")
            .map(|entry| entry.expect("临时目录项应可读取").path())
            .collect::<Vec<_>>();
        children.sort();
        for path in children {
            let relative = path
                .strip_prefix(root)
                .expect("临时目录项必须位于根目录下")
                .to_string_lossy()
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(&path).expect("临时目录项元数据应可读取");
            if metadata.is_dir() {
                snapshot.push(format!("D:{relative}"));
                visit(root, &path, snapshot);
            } else if metadata.is_file() {
                let bytes = fs::read(&path).expect("临时文件内容应可读取");
                let mut hasher = DefaultHasher::new();
                bytes.hash(&mut hasher);
                snapshot.push(format!(
                    "F:{relative}:{}:{:016x}",
                    bytes.len(),
                    hasher.finish()
                ));
            } else {
                snapshot.push(format!("O:{relative}"));
            }
        }
    }

    let mut snapshot = Vec::new();
    visit(root, root, &mut snapshot);
    snapshot
}

/// 捕获 Git 对指定目录的可观察状态，即使该目录尚未初始化仓库也保留退出码与错误。
fn git_status_snapshot(root: &Path) -> (Option<i32>, String, String) {
    let output = Command::new("git")
        .args(["-C"])
        .arg(root)
        .args(["status", "--short", "--untracked-files=all"])
        .output()
        .expect("测试环境应提供 Git");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// 捕获当前测试进程树快照；被 Plan 守卫拦截的工具不应引入新的外部进程。
fn process_snapshot() -> Vec<String> {
    #[cfg(windows)]
    {
        process_tree_snapshot()
    }
    #[cfg(not(windows))]
    {
        let output = Command::new("ps")
            .args(["-e", "-o", "pid=,ppid=,comm="])
            .output()
            .expect("Unix 应提供 ps");
        assert!(output.status.success(), "进程快照命令应成功");
        let processes = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter_map(process_record)
            .collect::<Vec<_>>();
        let current_pid = std::process::id();
        let mut children = BTreeMap::<u32, Vec<(u32, String)>>::new();
        for (pid, parent_pid, image) in processes {
            children.entry(parent_pid).or_default().push((pid, image));
        }
        let mut pending = vec![current_pid];
        let mut snapshot = Vec::new();
        while let Some(parent_pid) = pending.pop() {
            for (pid, image) in children.remove(&parent_pid).unwrap_or_default() {
                snapshot.push(format!("{image}:{pid}"));
                pending.push(pid);
            }
        }
        snapshot.sort();
        snapshot
    }
}

/// 只判断测试期间是否出现新进程；与测试无关的系统进程退出不应造成误报。
fn assert_no_new_processes(before: &[String], after: &[String]) {
    let before = before.iter().collect::<BTreeSet<_>>();
    let new_processes = after
        .iter()
        .filter(|identity| !before.contains(identity))
        .collect::<Vec<_>>();
    assert!(
        new_processes.is_empty(),
        "Plan 模式引入了外部进程：{new_processes:?}"
    );
}

/// 只保留进程名和 PID，忽略每次查询都会变化的内存占用及快照命令自身。
#[cfg(windows)]
fn process_tree_snapshot() -> Vec<String> {
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Process32FirstW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    assert_ne!(
        snapshot, INVALID_HANDLE_VALUE,
        "Windows 进程树快照句柄应有效"
    );
    let mut entry = ProcessEntry32W {
        dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
        cnt_usage: 0,
        th32_process_id: 0,
        th32_default_heap_id: 0,
        th32_module_id: 0,
        cnt_threads: 0,
        th32_parent_process_id: 0,
        pc_pri_class_base: 0,
        dw_flags: 0,
        exe_file: [0; 260],
    };
    let mut processes = BTreeMap::<u32, Vec<(u32, String)>>::new();
    let first = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    if first {
        loop {
            let length = entry
                .exe_file
                .iter()
                .position(|character| *character == 0)
                .unwrap_or(entry.exe_file.len());
            let image = String::from_utf16_lossy(&entry.exe_file[..length]);
            processes
                .entry(entry.th32_parent_process_id)
                .or_default()
                .push((entry.th32_process_id, image));
            if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
                break;
            }
        }
    }
    assert_ne!(
        unsafe { CloseHandle(snapshot) },
        0,
        "Windows 进程树快照句柄应可关闭"
    );

    let mut pending = vec![std::process::id()];
    let mut result = Vec::new();
    while let Some(parent_pid) = pending.pop() {
        for (pid, image) in processes.remove(&parent_pid).unwrap_or_default() {
            result.push(format!("{image}:{pid}"));
            pending.push(pid);
        }
    }
    result.sort();
    result
}

/// 解析 Unix 进程表中的 PID、父 PID 和程序名。
#[cfg(not(windows))]
fn process_record(line: &str) -> Option<(u32, u32, String)> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let parent_pid = fields.next()?.parse().ok()?;
    let image = fields.next()?.to_owned();
    if image == "ps" {
        return None;
    }
    Some((pid, parent_pid, image))
}

/// Plan 模式拒绝全部变更工具，但仍让真实 Read、Glob、Grep 工具完成执行。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_guard_blocks_mutations_without_side_effects_or_processes() {
    let project = tempdir().expect("应创建项目临时目录");
    let external = tempdir().expect("应创建平级外部临时目录");
    let temp_root = fs::canonicalize(std::env::temp_dir()).expect("系统临时根目录应可解析");
    let project_path = fs::canonicalize(project.path()).expect("项目临时目录应可解析");
    let external_path = fs::canonicalize(external.path()).expect("外部临时目录应可解析");
    assert_eq!(
        project_path.parent(),
        external_path.parent(),
        "项目与外部夹具必须平级"
    );
    assert!(
        project_path.starts_with(&temp_root) && external_path.starts_with(&temp_root),
        "所有测试写入必须留在安全系统临时目录"
    );

    let project_source = project_path.join("src").join("main.rs");
    fs::create_dir_all(project_source.parent().expect("源文件父目录应存在"))
        .expect("应创建项目源目录");
    fs::write(&project_source, "fn main() { println!(\"needle\"); }\n")
        .expect("应创建项目测试源文件");
    let external_file = external_path.join("outside.txt");
    fs::write(&external_file, "outside-before\n").expect("应创建外部测试文件");
    let blocked_write = external_path.join("blocked-write.txt");

    let project_before = tree_snapshot(&project_path);
    let external_before = tree_snapshot(&external_path);
    let git_before = git_status_snapshot(&external_path);
    let processes_before = process_snapshot();

    let environment = Arc::new(ToolEnvironment::new(&project_path).expect("工具环境应有效"));
    let mut registry = ToolRegistry::new();

    let write_executions = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingTool::new(
            Arc::new(WriteTool::new(Arc::clone(&environment))),
            Arc::clone(&write_executions),
        )))
        .expect("Write 应注册");
    let edit_executions = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingTool::new(
            Arc::new(EditTool::new(Arc::clone(&environment))),
            Arc::clone(&edit_executions),
        )))
        .expect("Edit 应注册");
    let read_executions = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingTool::new(
            Arc::new(ReadTool::new(Arc::clone(&environment))),
            Arc::clone(&read_executions),
        )))
        .expect("Read 应注册");
    let glob_executions = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingTool::new(
            Arc::new(GlobTool::new(Arc::clone(&environment))),
            Arc::clone(&glob_executions),
        )))
        .expect("Glob 应注册");
    let grep_executions = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingTool::new(
            Arc::new(GrepTool::new(Arc::clone(&environment))),
            Arc::clone(&grep_executions),
        )))
        .expect("Grep 应注册");

    #[cfg(windows)]
    let shell_name = "PowerShell";
    #[cfg(not(windows))]
    let shell_name = "Bash";
    #[cfg(windows)]
    let shell_command = "Set-Content -LiteralPath 'blocked-shell.txt' -Value blocked";
    #[cfg(not(windows))]
    let shell_command = "printf blocked > blocked-shell.txt";
    let shell_executions = Arc::new(AtomicUsize::new(0));
    #[cfg(windows)]
    let shell = Arc::new(PowerShellTool::new(Arc::clone(&environment)));
    #[cfg(not(windows))]
    let shell = Arc::new(BashTool::new(Arc::clone(&environment)));
    registry
        .register(Arc::new(CountingTool::new(
            shell,
            Arc::clone(&shell_executions),
        )))
        .expect("平台 Shell 应注册");

    let git_executions = Arc::new(AtomicUsize::new(0));
    registry
        .register(Arc::new(CountingTool::new(
            Arc::new(GitTool::new(Arc::clone(&environment))),
            Arc::clone(&git_executions),
        )))
        .expect("Git 应注册");

    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply(&[
                (
                    "plan-write",
                    "Write",
                    json!({
                        "file_path": blocked_write.to_string_lossy(),
                        "content": "must-not-write\n"
                    }),
                ),
                (
                    "plan-edit",
                    "Edit",
                    json!({
                        "file_path": external_file.to_string_lossy(),
                        "old_string": "outside-before",
                        "new_string": "must-not-edit"
                    }),
                ),
                (
                    "plan-shell",
                    shell_name,
                    json!({
                        "command": shell_command,
                        "cwd": external_path.to_string_lossy()
                    }),
                ),
                (
                    "plan-git",
                    "Git",
                    json!({
                        "args": ["init", "--quiet"],
                        "cwd": external_path.to_string_lossy()
                    }),
                ),
                (
                    "plan-read",
                    "Read",
                    json!({ "file_path": project_source.to_string_lossy() }),
                ),
                (
                    "plan-glob",
                    "Glob",
                    json!({
                        "pattern": "**/*.rs",
                        "path": project_path.to_string_lossy()
                    }),
                ),
                (
                    "plan-grep",
                    "Grep",
                    json!({
                        "pattern": "needle",
                        "path": project_path.to_string_lossy(),
                        "glob": "**/*.rs"
                    }),
                ),
            ]),
            text_reply("plan-read-only-complete"),
        ],
    ));
    let request = TurnRequest::new(
        SessionId::new("session-plan-integration").expect("Plan Session ID 应有效"),
        TurnId::new("turn-plan-integration").expect("Plan Turn ID 应有效"),
        AgentId::new("agent-plan-integration").expect("Plan Agent ID 应有效"),
        "test-model",
        vec![Message::text(MessageRole::User, "只读检查项目")],
        PlanGuard::read_only(),
    );
    let result = AgentRunner::new(provider.clone(), registry, RunLimits::default())
        .run_turn(request)
        .await;

    assert!(
        result.is_success(),
        "Plan Runner 应成功完成只读响应：{:?}",
        result.error
    );
    assert_eq!(
        write_executions.load(Ordering::SeqCst),
        0,
        "Write execute 不应进入"
    );
    assert_eq!(
        edit_executions.load(Ordering::SeqCst),
        0,
        "Edit execute 不应进入"
    );
    assert_eq!(
        shell_executions.load(Ordering::SeqCst),
        0,
        "Shell execute 不应进入"
    );
    assert_eq!(
        git_executions.load(Ordering::SeqCst),
        0,
        "Git execute 不应进入"
    );
    assert!(
        read_executions.load(Ordering::SeqCst) > 0,
        "Read 应实际执行"
    );
    assert!(
        glob_executions.load(Ordering::SeqCst) > 0,
        "Glob 应实际执行"
    );
    assert!(
        grep_executions.load(Ordering::SeqCst) > 0,
        "Grep 应实际执行"
    );

    let requests = provider.requests().expect("Provider 请求快照应可读取");
    assert_eq!(requests.len(), 2, "应有一轮工具请求和一轮最终文本请求");
    let tool_results = requests
        .iter()
        .flat_map(|request| request.messages.iter())
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_result } => Some(tool_result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_results.len(), 7, "每个模型工具调用都应有配对结果");
    for call_id in ["plan-read", "plan-glob", "plan-grep"] {
        let result = tool_results
            .iter()
            .find(|result| result.tool_call_id == call_id)
            .unwrap_or_else(|| panic!("缺少只读工具 {call_id} 的结果"));
        assert!(!result.is_error, "只读工具 {call_id} 不应执行失败");
    }
    for call_id in ["plan-write", "plan-edit", "plan-shell", "plan-git"] {
        let result = tool_results
            .iter()
            .find(|result| result.tool_call_id == call_id)
            .unwrap_or_else(|| panic!("缺少变更工具 {call_id} 的结果"));
        assert!(result.is_error, "Plan 守卫应拒绝变更工具 {call_id}");
    }

    assert_eq!(
        tree_snapshot(&project_path),
        project_before,
        "项目文件树或哈希发生变化"
    );
    assert_eq!(
        tree_snapshot(&external_path),
        external_before,
        "项目外文件树或哈希发生变化"
    );
    assert_eq!(
        git_status_snapshot(&external_path),
        git_before,
        "Git 状态发生变化"
    );
    assert_no_new_processes(&processes_before, &process_snapshot());
}
