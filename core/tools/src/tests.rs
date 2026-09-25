//! 文件与搜索工具的跨平台临时目录集成测试。

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use keencode_agent::{
    AgentId, AgentTool, SessionId, ToolCallId, ToolContext, ToolEffect, ToolError,
    ToolOutputArtifactSink, ToolRegistry, TurnCancellation, TurnId,
};
use keencode_model::ToolResultContent;
use serde_json::json;
use tempfile::tempdir;

use crate::{
    BoundedCommandRequest, EditTool, FileMutationRecorder, GlobTool, GrepTool,
    PreparedFileMutation, ReadTool, ToolEnvironment, ToolLimits, WriteTool, long_form_temp_dir,
    register_local_tools, run_bounded_command,
};

#[cfg(not(windows))]
use crate::BashTool;
#[cfg(windows)]
use crate::PowerShellTool;

/// 创建每次测试独立使用的工具上下文。
fn tool_context() -> ToolContext {
    ToolContext {
        session_id: SessionId::new("session-tools").expect("测试 Session ID 应有效"),
        turn_id: TurnId::new("turn-tools").expect("测试 Turn ID 应有效"),
        source_agent_id: AgentId::new("agent-tools").expect("测试 Agent ID 应有效"),
        tool_call_id: ToolCallId::new("call-tools").expect("测试 ToolCall ID 应有效"),
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

/// 按出现顺序收集 Read 输出中的一基行号（形如 `   12→正文`）。
fn collect_line_numbers(text: &str) -> Vec<usize> {
    text.lines()
        .filter_map(|line| line.split_once('→'))
        .filter_map(|(prefix, _)| prefix.trim().parse::<usize>().ok())
        .collect()
}

/// 以 Read 工具登记目标文件的读取指纹。
///
/// Write 覆写已有文本文件要求模型先建立内容认知，因此涉及覆写的用例都要经过
/// 真实 Read 路径，而不是直接构造文件后写入。
async fn read_before_write(environment: &Arc<ToolEnvironment>, file_path: &str) {
    ReadTool::new(Arc::clone(environment))
        .execute(tool_context(), json!({ "file_path": file_path }))
        .await
        .expect("测试前置读取应成功");
}

/// 记录文件变更准备阶段看到的完整上下文和原始字节。
#[derive(Debug, Default)]
struct MutationProbe {
    /// 每次准备阶段收到的变更快照。
    snapshots: Mutex<Vec<MutationSnapshot>>,
    /// 已提交的变更数量。
    mark_count: Arc<AtomicUsize>,
    /// 可选的准备阶段失败。
    prepare_error: Option<ToolError>,
    /// 可选的提交阶段失败。
    mark_error: Option<ToolError>,
    /// 是否在准备阶段取消当前 Turn。
    cancel_on_prepare: bool,
    /// 可选的准备阶段外部文件变更。
    mutate_on_prepare: Option<(PathBuf, Vec<u8>)>,
}

/// 单次文件变更准备阶段的可断言快照。
#[derive(Debug, PartialEq, Eq)]
struct MutationSnapshot {
    /// 工具调用所属的 Session 标识。
    session_id: String,
    /// 工具调用所属的 Turn 标识。
    turn_id: String,
    /// 发起工具调用的 Agent 标识。
    source_agent_id: String,
    /// 可信工具调用标识。
    tool_call_id: String,
    /// 规范化后的文件路径。
    path: PathBuf,
    /// 替换前的完整字节；缺失文件为 `None`。
    before: Option<Vec<u8>>,
    /// 替换后的完整字节。
    after: Vec<u8>,
}

/// 测试用的已准备文件变更。
#[derive(Debug)]
struct PreparedMutationProbe {
    /// 共享提交计数器。
    mark_count: Arc<AtomicUsize>,
    /// 可选的提交阶段失败。
    mark_error: Option<ToolError>,
}

impl PreparedFileMutation for PreparedMutationProbe {
    /// 记录提交调用，并按测试配置返回结果。
    fn mark_applied(&self) -> Result<(), ToolError> {
        self.mark_count.fetch_add(1, Ordering::SeqCst);
        self.mark_error.clone().map_or(Ok(()), Err)
    }
}

impl FileMutationRecorder for MutationProbe {
    /// 捕获完整变更输入，并按测试配置模拟失败、取消或外部修改。
    fn prepare(
        &self,
        context: &ToolContext,
        path: &Path,
        before: Option<&[u8]>,
        after: &[u8],
    ) -> Result<Box<dyn PreparedFileMutation>, ToolError> {
        if let Some(error) = &self.prepare_error {
            return Err(error.clone());
        }
        self.snapshots
            .lock()
            .expect("测试记录器锁不应中毒")
            .push(MutationSnapshot {
                session_id: context.session_id.as_str().to_owned(),
                turn_id: context.turn_id.as_str().to_owned(),
                source_agent_id: context.source_agent_id.as_str().to_owned(),
                tool_call_id: context.tool_call_id.as_str().to_owned(),
                path: path.to_path_buf(),
                before: before.map(ToOwned::to_owned),
                after: after.to_owned(),
            });
        if self.cancel_on_prepare {
            context.cancellation.cancel();
        }
        if let Some((path, bytes)) = &self.mutate_on_prepare {
            fs::write(path, bytes).expect("测试记录器应能模拟外部文件变更");
        }
        Ok(Box::new(PreparedMutationProbe {
            mark_count: Arc::clone(&self.mark_count),
            mark_error: self.mark_error.clone(),
        }))
    }
}

/// 按生产格式构造 Read 文本结果的文件头。
fn read_header(path: &Path) -> String {
    let parent = path.parent().expect("Read 测试路径应有父目录");
    let canonical = fs::canonicalize(parent)
        .expect("Read 测试父目录应可规范化")
        .join(path.file_name().expect("Read 测试路径应有文件名"));
    format!("文件：{}\n", canonical.to_string_lossy().replace('\\', "/"))
}

/// 使用显式资源上限创建 Read 工具。
fn read_tool_with_limits(directory: &Path, limits: ToolLimits) -> ReadTool {
    let environment =
        Arc::new(ToolEnvironment::with_limits(directory, limits).expect("显式工具资源上限应有效"));
    ReadTool::new(environment)
}

/// 默认 Read 文本与图片上限必须保守，且文本字节上限不能配置为零。
#[test]
fn tool_limits_bound_read_text_and_images() {
    let defaults = ToolLimits::default();
    assert_eq!(defaults.max_read_output_bytes, 32 * 1024);
    assert_eq!(defaults.max_image_bytes, 8 * 1024 * 1024);

    let error = ToolLimits {
        max_read_output_bytes: 0,
        ..defaults
    }
    .validate()
    .expect_err("Read 文本字节上限为零必须失败");
    assert_eq!(error.code, "invalid_tool_limits");
    assert!(!error.retryable);
}

/// 本地工具注册必须提供稳定且不重复的名称；PowerShell 仅在 Windows 注册。
#[test]
fn local_tool_registration_is_stable() {
    let directory = tempdir().expect("应创建临时目录");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let mut registry = ToolRegistry::new();
    register_local_tools(&mut registry, environment).expect("本地工具应全部注册");

    let names = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    let expected: Vec<&str> = if cfg!(windows) {
        vec![
            "Bash",
            "Edit",
            "Glob",
            "Grep",
            "PowerShell",
            "Read",
            "Write",
        ]
    } else {
        vec!["Bash", "Edit", "Glob", "Grep", "Read", "Write"]
    };
    assert_eq!(names, expected);
}

/// Write 必须创建父目录、原子写入内容并识别完全相同的重复写入。
#[tokio::test]
async fn write_creates_parent_and_skips_identical_content() {
    let directory = tempdir().expect("应创建临时目录");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = WriteTool::new(environment);
    let input = json!({
        "file_path": "nested/example.txt",
        "content": "第一行\r\nsecond\r\n"
    });

    assert_eq!(tool.effect(&input), Ok(ToolEffect::ChangesState));
    let first = tool
        .execute(tool_context(), input.clone())
        .await
        .expect("首次写入应成功");
    assert!(output_text(&first).contains("原子创建"));
    assert_eq!(
        fs::read(directory.path().join("nested/example.txt")).expect("应读取写入文件"),
        "第一行\r\nsecond\r\n".as_bytes()
    );

    let second = tool
        .execute(tool_context(), input)
        .await
        .expect("重复写入应成功返回无变化");
    assert!(output_text(&second).contains("未变化"));
}

/// Write 的记录器必须收到完整上下文和原始字节，并只在原子写入后提交。
#[tokio::test]
async fn mutation_recorder_receives_full_write_snapshot() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("binary.bin");
    let before = vec![0x00, 0xff, 0x80, b'\r', b'\n'];
    fs::write(&path, &before).expect("应写入二进制测试文件");
    let recorder = Arc::new(MutationProbe::default());
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    let context = tool_context();
    let output = WriteTool::new(environment)
        .execute(
            context,
            json!({
                "file_path": "binary.bin",
                "content": "after"
            }),
        )
        .await
        .expect("带记录器的写入应成功");

    assert!(output_text(&output).contains("原子覆盖"));
    let snapshots = recorder.snapshots.lock().expect("测试记录器锁不应中毒");
    assert_eq!(
        snapshots.as_slice(),
        &[MutationSnapshot {
            session_id: "session-tools".to_owned(),
            turn_id: "turn-tools".to_owned(),
            source_agent_id: "agent-tools".to_owned(),
            tool_call_id: "call-tools".to_owned(),
            path: fs::canonicalize(&path).expect("目标路径应可规范化"),
            before: Some(before),
            after: b"after".to_vec(),
        }]
    );
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 1);
}

/// 记录器准备失败时不能创建缺失的父目录或目标文件。
#[tokio::test]
async fn mutation_recorder_prepare_failure_has_no_write_side_effect() {
    let directory = tempdir().expect("应创建临时目录");
    let recorder = Arc::new(MutationProbe {
        prepare_error: Some(ToolError::permanent("prepare_failed", "测试准备失败")),
        ..MutationProbe::default()
    });
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    let error = WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": "missing/child/file.txt",
                "content": "new"
            }),
        )
        .await
        .expect_err("准备失败必须向调用方返回错误");

    assert_eq!(error.code, "prepare_failed");
    assert!(!directory.path().join("missing").exists());
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 0);
}

/// mark_applied 失败不能把已经完成的原子写入伪装成成功结果。
#[tokio::test]
async fn mutation_recorder_mark_failure_is_not_success() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("mark.txt");
    fs::write(&path, b"before").expect("应写入初始文件");
    let recorder = Arc::new(MutationProbe {
        mark_error: Some(ToolError::permanent("mark_failed", "测试提交失败")),
        ..MutationProbe::default()
    });
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    read_before_write(&environment, "mark.txt").await;
    let error = WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": "mark.txt",
                "content": "after"
            }),
        )
        .await
        .expect_err("提交失败不能返回成功");

    assert_eq!(error.code, "mark_failed");
    assert_eq!(fs::read(&path).expect("原子写入结果应存在"), b"after");
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 1);
}

/// 目标内容没有变化时不得创建变更记录。
#[tokio::test]
async fn mutation_recorder_skips_identical_write() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("same.txt");
    fs::write(&path, b"same").expect("应写入初始文件");
    let recorder = Arc::new(MutationProbe::default());
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    read_before_write(&environment, "same.txt").await;

    WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": "same.txt",
                "content": "same"
            }),
        )
        .await
        .expect("无变化写入应成功");

    assert!(
        recorder
            .snapshots
            .lock()
            .expect("测试记录器锁不应中毒")
            .is_empty()
    );
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 0);
}

/// 缺失文件的空写入必须区别于已有空文件的无变化写入。
#[tokio::test]
async fn mutation_recorder_distinguishes_missing_and_empty_file() {
    let directory = tempdir().expect("应创建临时目录");
    let recorder = Arc::new(MutationProbe::default());
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    let input = json!({
        "file_path": "empty.txt",
        "content": ""
    });
    WriteTool::new(Arc::clone(&environment))
        .execute(tool_context(), input.clone())
        .await
        .expect("缺失文件的空写入应成功");
    WriteTool::new(environment)
        .execute(tool_context(), input)
        .await
        .expect("已有空文件的重复写入应成功");

    let snapshots = recorder.snapshots.lock().expect("测试记录器锁不应中毒");
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].before, None);
    assert!(snapshots[0].after.is_empty());
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 1);
}

/// 记录器准备阶段取消 Turn 时不得创建或覆盖文件。
#[tokio::test]
async fn mutation_recorder_cancellation_prevents_write() {
    let directory = tempdir().expect("应创建临时目录");
    let recorder = Arc::new(MutationProbe {
        cancel_on_prepare: true,
        ..MutationProbe::default()
    });
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    let error = WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": "cancelled/new.txt",
                "content": "new"
            }),
        )
        .await
        .expect_err("准备阶段取消必须阻止写入");

    assert_eq!(error.code, "cancelled");
    assert!(!directory.path().join("cancelled").exists());
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 0);
}

/// 记录器准备阶段发现外部修改时，原子写入必须被拒绝。
#[tokio::test]
async fn mutation_recorder_external_change_prevents_write() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("changed.txt");
    fs::write(&path, b"before").expect("应写入初始文件");
    let recorder = Arc::new(MutationProbe {
        mutate_on_prepare: Some((path.clone(), b"external".to_vec())),
        ..MutationProbe::default()
    });
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    read_before_write(&environment, "changed.txt").await;
    let error = WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": "changed.txt",
                "content": "after"
            }),
        )
        .await
        .expect_err("外部修改必须阻止覆盖");

    assert_eq!(error.code, "file_changed_during_tool");
    assert_eq!(fs::read(path).expect("外部文件内容应保留"), b"external");
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 0);
}

/// 父路径不是目录时，无论在哪个文件系统检查阶段失败都不得提交变更记录。
#[tokio::test]
async fn mutation_recorder_does_not_mark_blocked_parent_failure() {
    let directory = tempdir().expect("应创建临时目录");
    let blocked_parent = directory.path().join("blocked");
    fs::write(&blocked_parent, b"not a directory").expect("应创建阻塞父路径");
    let recorder = Arc::new(MutationProbe::default());
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_file_mutation_recorder(recorder.clone()),
    );
    let error = WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": "blocked/child.txt",
                "content": "new"
            }),
        )
        .await
        .expect_err("父目录创建失败必须返回错误");

    #[cfg(unix)]
    assert_eq!(error.code, "symlink_metadata_failed");
    #[cfg(windows)]
    assert_eq!(error.code, "create_parent_failed");
    assert_eq!(recorder.mark_count.load(Ordering::SeqCst), 0);
    assert_eq!(
        fs::read(blocked_parent).expect("父路径内容应保留"),
        b"not a directory"
    );
    assert!(!directory.path().join("blocked/child.txt").exists());
}

/// Edit 必须在创建过大的替换字符串前计入 BOM 并拒绝写入。
#[tokio::test]
async fn edit_rejects_result_over_limit_before_write() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("limited.txt");
    let original = b"\xEF\xBB\xBFa";
    fs::write(&path, original).expect("应写入 BOM 文件");
    let limits = ToolLimits {
        max_mutation_file_bytes: 4,
        ..ToolLimits::default()
    };
    let environment =
        Arc::new(ToolEnvironment::with_limits(directory.path(), limits).expect("工具环境应有效"));
    let error = EditTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": "limited.txt",
                "old_string": "a",
                "new_string": "bb"
            }),
        )
        .await
        .expect_err("计入 BOM 后超过上限必须失败");

    assert_eq!(error.code, "edit_file_too_large");
    assert_eq!(fs::read(path).expect("失败后原文件应保留"), original);
}

/// 文件工具必须接受项目目录外的显式绝对路径，访问范围由加入项目的授权边界决定。
#[tokio::test]
async fn write_accepts_absolute_path_outside_working_directory() {
    let directory = tempdir().expect("应创建临时目录");
    let project = directory.path().join("project");
    fs::create_dir(&project).expect("应创建项目目录");
    let outside = directory.path().join("outside.txt");
    let environment = Arc::new(ToolEnvironment::new(&project).expect("工具环境应有效"));
    let output = WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": outside.to_string_lossy(),
                "content": "outside"
            }),
        )
        .await
        .expect("绝对外部路径写入应成功");

    assert!(output_text(&output).contains("原子创建"));
    assert_eq!(
        fs::read_to_string(outside).expect("应读取外部文件"),
        "outside"
    );
}

/// 覆写已有文本文件必须先经过 Read：未读取、读取后外部改动、读取后被删除
/// 三种情形都要被拒绝，避免整文件覆写静默丢弃外部改动。
#[tokio::test]
async fn write_requires_fresh_read_of_existing_text_file() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("guarded.txt");
    fs::write(&path, b"original").expect("应写入初始文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = WriteTool::new(Arc::clone(&environment));
    let overwrite = json!({ "file_path": "guarded.txt", "content": "replacement" });

    // 未读取过：拒绝覆写，磁盘内容保持原样。
    let unread = tool
        .execute(tool_context(), overwrite.clone())
        .await
        .expect_err("未读取就覆写必须被拒绝");
    assert_eq!(unread.code, "write_requires_read");
    assert_eq!(fs::read(&path).expect("文件应保持原样"), b"original");

    // 读取后外部改动：拒绝覆写，外部改动不得被静默丢弃。
    read_before_write(&environment, "guarded.txt").await;
    fs::write(&path, b"changed by user").expect("应模拟外部修改");
    let stale = tool
        .execute(tool_context(), overwrite.clone())
        .await
        .expect_err("读取后外部改动必须阻止覆写");
    assert_eq!(stale.code, "file_changed_since_read");
    assert_eq!(fs::read(&path).expect("外部改动应保留"), b"changed by user");

    // 重新读取后可以正常覆写。
    read_before_write(&environment, "guarded.txt").await;
    let written = tool
        .execute(tool_context(), overwrite)
        .await
        .expect("重新读取后覆写应成功");
    assert!(output_text(&written).contains("原子覆盖"));
    assert_eq!(fs::read(&path).expect("应读取写入结果"), b"replacement");
}

/// 新建文件不受"写前读取"约束，且写入后自身登记指纹使连续覆写无需重读。
#[tokio::test]
async fn write_new_file_then_overwrite_without_reread() {
    let directory = tempdir().expect("应创建临时目录");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = WriteTool::new(Arc::clone(&environment));

    let created = tool
        .execute(
            tool_context(),
            json!({ "file_path": "fresh.txt", "content": "first" }),
        )
        .await
        .expect("新建文件应成功");
    assert!(output_text(&created).contains("原子创建"));

    let overwritten = tool
        .execute(
            tool_context(),
            json!({ "file_path": "fresh.txt", "content": "second" }),
        )
        .await
        .expect("写入者自身登记指纹后应可直接覆写");
    assert!(output_text(&overwritten).contains("原子覆盖"));
    assert_eq!(
        fs::read(directory.path().join("fresh.txt")).expect("应读取文件"),
        b"second"
    );
}

/// 二进制文件无法经 Read 观察，因此不适用"写前读取"约束。
#[tokio::test]
async fn write_binary_file_is_not_blocked_by_read_requirement() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("blob.bin");
    fs::write(&path, [0x00, 0xff, 0x01]).expect("应写入二进制文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));

    let output = WriteTool::new(environment)
        .execute(
            tool_context(),
            json!({ "file_path": "blob.bin", "content": "text now" }),
        )
        .await
        .expect("二进制文件覆写不应被读取要求拦截");

    assert!(output_text(&output).contains("原子覆盖"));
    assert_eq!(fs::read(&path).expect("应读取写入结果"), b"text now");
}

/// Edit 必须拒绝歧义匹配，并在全量替换时保留 BOM 与 CRLF。
#[tokio::test]
async fn edit_is_exact_atomic_and_preserves_encoding_shape() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("bom.txt");
    let original = b"\xEF\xBB\xBFalpha\r\nsame\r\nsame\r\n";
    fs::write(&path, original).expect("应写入测试文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = EditTool::new(environment);

    let ambiguous = tool
        .execute(
            tool_context(),
            json!({
                "file_path": "bom.txt",
                "old_string": "same",
                "new_string": "changed"
            }),
        )
        .await
        .expect_err("非唯一匹配必须失败");
    assert_eq!(ambiguous.code, "old_string_not_unique");
    assert_eq!(fs::read(&path).expect("失败后文件仍应可读"), original);

    let changed = tool
        .execute(
            tool_context(),
            json!({
                "file_path": "bom.txt",
                "old_string": "same",
                "new_string": "changed",
                "replace_all": true
            }),
        )
        .await
        .expect("全量精确替换应成功");
    assert!(output_text(&changed).contains("替换 2 处"));
    let bytes = fs::read(path).expect("应读取编辑结果");
    assert!(bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert_eq!(
        std::str::from_utf8(&bytes[3..]).expect("结果应为 UTF-8"),
        "alpha\r\nchanged\r\nchanged\r\n"
    );
}

/// Read 会把行尾 \r 归一为 \n；模型据此复制的多行 old_string 必须能在
/// 一致 CRLF 的文件上命中，且替换后的新增行保持 CRLF。
#[tokio::test]
async fn edit_multiline_old_string_matches_crlf_file_via_newline_normalization() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("crlf.rs");
    fs::write(&path, "fn a() {\r\n    old();\r\n}\r\n").expect("应写入 CRLF 文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = EditTool::new(environment);

    let edited = tool
        .execute(
            tool_context(),
            json!({
                "file_path": "crlf.rs",
                "old_string": "fn a() {\n    old();\n}",
                "new_string": "fn a() {\n    new();\n}"
            }),
        )
        .await
        .expect("行尾归一后多行编辑应成功");

    assert!(output_text(&edited).contains("替换 1 处"));
    assert_eq!(
        fs::read_to_string(&path).expect("应读取编辑结果"),
        "fn a() {\r\n    new();\r\n}\r\n",
        "替换行应保持 CRLF，未编辑行尾不得被改写"
    );
}

/// CRLF 文件上仍未命中的多行 old_string 必须在错误里提示行尾差异。
#[tokio::test]
async fn edit_not_found_on_crlf_file_hints_line_endings() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(directory.path().join("crlf.txt"), "alpha\r\nbeta\r\n").expect("应写入 CRLF 文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = EditTool::new(environment);

    let error = tool
        .execute(
            tool_context(),
            json!({
                "file_path": "crlf.txt",
                "old_string": "alpha\nmissing\n",
                "new_string": "x"
            }),
        )
        .await
        .expect_err("内容不匹配必须失败");

    assert_eq!(error.code, "old_string_not_found");
    assert!(
        error.message.contains("CRLF"),
        "错误应提示 CRLF 行尾：{}",
        error.message
    );
}

/// 覆写一致 CRLF 的文件时，模型按 Read 的 LF 视图重建的内容保持 CRLF 行尾。
#[tokio::test]
async fn write_over_existing_file_preserves_crlf_style() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("style.txt");
    fs::write(&path, "one\r\ntwo\r\n").expect("应写入 CRLF 文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = WriteTool::new(Arc::clone(&environment));

    read_before_write(&environment, "style.txt").await;
    let written = tool
        .execute(
            tool_context(),
            json!({ "file_path": "style.txt", "content": "one\ntwo\nthree\n" }),
        )
        .await
        .expect("重读后覆写应成功");

    assert!(output_text(&written).contains("原子覆盖"));
    assert_eq!(
        fs::read_to_string(&path).expect("应读取写入结果"),
        "one\r\ntwo\r\nthree\r\n",
        "整文件覆写必须保持 CRLF 行尾"
    );
}

/// LF 文件与新建文件不受行尾提升影响，写入字节与内容逐字一致。
#[tokio::test]
async fn write_keeps_verbatim_bytes_for_lf_file() {
    let directory = tempdir().expect("应创建临时目录");
    let existing = directory.path().join("lf.txt");
    fs::write(&existing, "one\ntwo\n").expect("应写入 LF 文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = WriteTool::new(Arc::clone(&environment));

    read_before_write(&environment, "lf.txt").await;
    tool.execute(
        tool_context(),
        json!({ "file_path": "lf.txt", "content": "one\ntwo\nthree\n" }),
    )
    .await
    .expect("覆写 LF 文件应成功");
    assert_eq!(
        fs::read_to_string(&existing).expect("应读取写入结果"),
        "one\ntwo\nthree\n"
    );
}

/// Read 必须按一基行号分页，并把受支持图片作为图片内容返回。
#[tokio::test]
async fn read_supports_line_windows_and_inline_images() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(directory.path().join("lines.txt"), "one\ntwo\nthree\n").expect("应写入文本文件");
    fs::write(
        directory.path().join("pixel.png"),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
    )
    .expect("应写入图片测试数据");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let tool = ReadTool::new(environment);

    let text = tool
        .execute(
            tool_context(),
            json!({ "file_path": "lines.txt", "offset": 2, "limit": 1 }),
        )
        .await
        .expect("分段读取应成功");
    let text = output_text(&text);
    assert!(text.contains("     2→two"));
    assert!(text.contains("offset=3"));
    assert!(!text.contains("one"));

    let image = tool
        .execute(tool_context(), json!({ "file_path": "pixel.png" }))
        .await
        .expect("图片读取应成功");
    assert!(matches!(
        image.content.as_slice(),
        [
            ToolResultContent::Text { .. },
            ToolResultContent::Image { .. }
        ]
    ));
}

/// 默认页限制和字节限制都须提供可继续读取的位置，不丢行或重复读同一页。
///
/// 默认读取同时受行数上限与输出字节上限约束，哪个先生效取决于每行长度；
/// 本测试因此不写死页大小，而是断言两页合起来恰好覆盖全部行且无重叠。
#[tokio::test]
async fn read_default_budget_supports_continuation() {
    let directory = tempdir().unwrap();
    let environment = Arc::new(ToolEnvironment::new(directory.path()).unwrap());
    let tool = ReadTool::new(environment);
    let total_lines = 2_050_usize;
    fs::write(
        directory.path().join("pages.txt"),
        (1..=total_lines)
            .map(|line| format!("line-{line}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let first = tool
        .execute(tool_context(), json!({"file_path":"pages.txt"}))
        .await
        .unwrap();
    let first = output_text(&first);
    let first_lines = collect_line_numbers(first);
    assert!(!first_lines.is_empty(), "默认读取必须返回内容：{first}");
    assert!(
        first_lines.last().copied().unwrap() < total_lines,
        "默认读取不应一次返回整个大文件"
    );
    let next_offset = first_lines.last().copied().unwrap() + 1;
    assert!(
        first.contains(&format!("offset={next_offset}")),
        "截断后必须给出下一页的起始行：{first}"
    );

    let next = tool
        .execute(
            tool_context(),
            json!({"file_path":"pages.txt","offset":next_offset}),
        )
        .await
        .unwrap();
    let next = output_text(&next);
    let next_lines = collect_line_numbers(next);
    assert_eq!(
        next_lines.last().copied(),
        Some(total_lines),
        "第二页必须读到文件末尾"
    );
    // 两页拼接后应恰好覆盖 1..=total_lines，既不丢行也不重复。
    let mut covered = first_lines.clone();
    covered.extend(next_lines);
    assert_eq!(
        covered,
        (1..=total_lines).collect::<Vec<_>>(),
        "两页合起来必须无重叠、无缺口地覆盖全部行"
    );
    assert!(!next.contains("offset="), "读到最后不应再给续读位置");

    fs::write(
        directory.path().join("wide.txt"),
        "x".repeat(1_024) + "\n" + &"y".repeat(1_024).repeat(40),
    )
    .unwrap();
    let wide = tool
        .execute(
            tool_context(),
            json!({"file_path":"wide.txt","limit":20_000}),
        )
        .await
        .unwrap();
    let wide = output_text(&wide);
    assert!(wide.len() <= ToolLimits::default().max_read_output_bytes);
    assert!(wide.contains("offset=2"));
}

/// Read 文本输出在精确 N 字节时成功，增加一个字节后必须固定失败而不能越界。
#[tokio::test]
async fn read_enforces_exact_output_byte_boundary() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("boundary.txt");
    fs::write(&path, "a").expect("应写入边界文本");
    let expected = format!("{}     1→a", read_header(&path));
    let limits = ToolLimits {
        max_read_output_bytes: expected.len(),
        ..ToolLimits::default()
    };
    let tool = read_tool_with_limits(directory.path(), limits);

    let output = tool
        .execute(tool_context(), json!({ "file_path": "boundary.txt" }))
        .await
        .expect("精确 N 字节输出应成功");
    assert_eq!(output_text(&output), expected);
    assert_eq!(output_text(&output).len(), limits.max_read_output_bytes);

    fs::write(&path, "ab").expect("应写入 N 加一字节文本");
    let error = tool
        .execute(tool_context(), json!({ "file_path": "boundary.txt" }))
        .await
        .expect_err("N 加一字节输出必须失败");
    assert_eq!(error.code, "read_line_too_large");
    assert!(!error.retryable);
}

/// Read 必须把多字节 UTF-8 当作完整字符和完整行处理，不能按字节截断正文。
#[tokio::test]
async fn read_preserves_multibyte_utf8_at_byte_boundary() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("utf8.txt");
    fs::write(&path, "你").expect("应写入多字节文本");
    let expected = format!("{}     1→你", read_header(&path));
    let exact_tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: expected.len(),
            ..ToolLimits::default()
        },
    );
    let output = exact_tool
        .execute(tool_context(), json!({ "file_path": "utf8.txt" }))
        .await
        .expect("完整 UTF-8 字符应在精确边界成功");
    assert_eq!(output_text(&output), expected);

    let short_tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: expected.len() - 1,
            ..ToolLimits::default()
        },
    );
    let error = short_tool
        .execute(tool_context(), json!({ "file_path": "utf8.txt" }))
        .await
        .expect_err("不足一个字节时不得返回半个 UTF-8 字符");
    assert_eq!(error.code, "read_line_too_large");
}

/// Read 的字节预算必须完整计入文件头、行号前缀、换行与续读提示。
#[tokio::test]
async fn read_budgets_header_prefix_newlines_and_marker() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("marker.txt");
    fs::write(&path, "a\nsecond-line-is-deliberately-long\nthird\n").expect("应写入续读边界文本");
    let expected = format!(
        "{}     1→a\n[仍有后续内容；下一次使用 offset=2]",
        read_header(&path)
    );
    let tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: expected.len(),
            ..ToolLimits::default()
        },
    );

    let output = tool
        .execute(
            tool_context(),
            json!({ "file_path": "marker.txt", "limit": 3 }),
        )
        .await
        .expect("首行与续读提示应刚好装入预算");
    assert_eq!(output_text(&output), expected);
    assert_eq!(output_text(&output).len(), expected.len());
}

/// Read 因字节预算分页时，offset 与 limit 组合必须指向首个未返回的真实行号。
#[tokio::test]
async fn read_returns_exact_offset_for_byte_and_line_windows() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("offset.txt");
    fs::write(&path, "one\ntwo\nthree-is-long\nfour\n").expect("应写入分页文本");
    let expected = format!(
        "{}     2→two\n[仍有后续内容；下一次使用 offset=3]",
        read_header(&path)
    );
    let tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: expected.len(),
            ..ToolLimits::default()
        },
    );

    let output = tool
        .execute(
            tool_context(),
            json!({ "file_path": "offset.txt", "offset": 2, "limit": 2 }),
        )
        .await
        .expect("offset 与 limit 组合应成功分页");
    assert_eq!(output_text(&output), expected);
}

/// Read 必须在有界缓冲内拒绝无法容纳的单行，并返回固定不可重试错误。
#[tokio::test]
async fn read_rejects_oversized_single_line_with_stable_error() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("long-line.txt");
    let header = read_header(&path);
    let maximum_output_bytes = header.len() + 64;
    fs::write(&path, "x".repeat(maximum_output_bytes * 4)).expect("应写入超长单行");
    let tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: maximum_output_bytes,
            ..ToolLimits::default()
        },
    );

    let error = tool
        .execute(tool_context(), json!({ "file_path": "long-line.txt" }))
        .await
        .expect_err("超长单行必须在硬上限处失败");
    assert_eq!(error.code, "read_line_too_large");
    assert_eq!(
        error.message,
        "单行内容无法在 Read 输出字节上限内与必要的文件头和续读提示一起完整返回"
    );
    assert!(!error.retryable);
}

/// 当前页已有完整行时，后续超长行必须转为续读提示而不是丢弃本页进展。
#[tokio::test]
async fn read_paginates_before_later_oversized_line() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("later-long-line.txt");
    let expected = format!(
        "{}     1→first\n[仍有后续内容；下一次使用 offset=2]",
        read_header(&path)
    );
    let maximum_output_bytes = expected.len();
    let mut content = String::from("first\n");
    content.push_str(&"x".repeat(maximum_output_bytes * 4));
    content.push_str("\nthird\n");
    fs::write(&path, content).expect("应写入后续超长行文本");
    let tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: maximum_output_bytes,
            ..ToolLimits::default()
        },
    );

    let output = tool
        .execute(
            tool_context(),
            json!({ "file_path": "later-long-line.txt", "limit": 3 }),
        )
        .await
        .expect("已有完整行时应在超长候选行前成功分页");
    assert_eq!(output_text(&output), expected);

    let error = tool
        .execute(
            tool_context(),
            json!({ "file_path": "later-long-line.txt", "offset": 2, "limit": 1 }),
        )
        .await
        .expect_err("从超长行开始的新页必须返回固定错误");
    assert_eq!(error.code, "read_line_too_large");
    assert!(!error.retryable);
}

/// Read 跳过超长 UTF-8 行时不得缓存整行，并仍应返回请求范围的准确行号。
#[tokio::test]
async fn read_skips_oversized_lines_with_bounded_memory() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("skip-long-line.txt");
    let mut content = "前".repeat(4_096);
    content.push_str("\r\nsecond\r\n");
    fs::write(&path, content).expect("应写入待跳过的超长行");
    let expected = format!("{}     2→second", read_header(&path));
    let tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: expected.len(),
            ..ToolLimits::default()
        },
    );

    let output = tool
        .execute(
            tool_context(),
            json!({ "file_path": "skip-long-line.txt", "offset": 2, "limit": 1 }),
        )
        .await
        .expect("超长已跳过行不应占用输出缓冲");
    assert_eq!(output_text(&output), expected);
}

/// Read 必须去除首行 BOM 与 CRLF，同时保留多字节正文和一基行号。
#[tokio::test]
async fn read_preserves_bom_and_crlf_semantics() {
    let directory = tempdir().expect("应创建临时目录");
    let path = directory.path().join("bom-crlf.txt");
    fs::write(&path, b"\xEF\xBB\xBFfirst\r\n\xE7\xAC\xAC\xE4\xBA\x8C\r\n")
        .expect("应写入 BOM 与 CRLF 文本");
    let expected = format!("{}     1→first\n     2→第二", read_header(&path));
    let tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_read_output_bytes: expected.len(),
            ..ToolLimits::default()
        },
    );

    let output = tool
        .execute(
            tool_context(),
            json!({ "file_path": "bom-crlf.txt", "limit": 2 }),
        )
        .await
        .expect("BOM 与 CRLF 文本应成功读取");
    assert_eq!(output_text(&output), expected);
    assert!(!output_text(&output).contains('\u{feff}'));
    assert!(!output_text(&output).contains('\r'));
}

/// Read 必须继续用稳定错误区分 NUL 二进制内容与非法 UTF-8。
#[tokio::test]
async fn read_preserves_binary_and_invalid_utf8_errors() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(directory.path().join("nul.txt"), b"a\0b\n").expect("应写入 NUL 文本");
    fs::write(directory.path().join("invalid.txt"), [0xFF, b'\n']).expect("应写入非法 UTF-8 文本");
    let tool = ReadTool::new(Arc::new(
        ToolEnvironment::new(directory.path()).expect("工具环境应有效"),
    ));

    let nul = tool
        .execute(tool_context(), json!({ "file_path": "nul.txt" }))
        .await
        .expect_err("NUL 文本必须拒绝");
    assert_eq!(nul.code, "binary_file");

    let invalid = tool
        .execute(tool_context(), json!({ "file_path": "invalid.txt" }))
        .await
        .expect_err("非法 UTF-8 必须拒绝");
    assert_eq!(invalid.code, "invalid_utf8_or_read_failed");
}

/// Read 在开始读取以及固定缓冲块之间必须观察 Turn 取消。
#[tokio::test]
async fn cancelled_read_returns_stable_error() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(directory.path().join("cancel.txt"), "text\n").expect("应写入取消测试文件");
    let tool = ReadTool::new(Arc::new(
        ToolEnvironment::new(directory.path()).expect("工具环境应有效"),
    ));
    let context = tool_context();
    context.cancellation.cancel();

    let error = tool
        .execute(context, json!({ "file_path": "cancel.txt" }))
        .await
        .expect_err("预取消 Read 必须失败");
    assert_eq!(error.code, "cancelled");
    assert!(!error.retryable);
}

/// 图片限制必须在精确边界成功，并在多一个字节时于读取前拒绝。
#[tokio::test]
async fn read_enforces_image_byte_boundary_with_small_limit() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(
        directory.path().join("exact.png"),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
    )
    .expect("应写入边界内 PNG");
    fs::write(
        directory.path().join("large.png"),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0],
    )
    .expect("应写入超限 PNG");
    let tool = read_tool_with_limits(
        directory.path(),
        ToolLimits {
            max_image_bytes: 8,
            ..ToolLimits::default()
        },
    );

    let exact = tool
        .execute(tool_context(), json!({ "file_path": "exact.png" }))
        .await
        .expect("精确图片边界应成功");
    assert!(matches!(
        exact.content.as_slice(),
        [
            ToolResultContent::Text { .. },
            ToolResultContent::Image { .. }
        ]
    ));

    let error = tool
        .execute(tool_context(), json!({ "file_path": "large.png" }))
        .await
        .expect_err("超过一个字节的图片必须失败");
    assert_eq!(error.code, "image_too_large");
    assert!(!error.retryable);
}

/// Glob 与 Grep 必须遵循根目录 Git 忽略规则并提供内容、文件和计数输出。
#[tokio::test]
async fn glob_and_grep_respect_gitignore_and_output_modes() {
    let directory = tempdir().expect("应创建临时目录");
    fs::create_dir_all(directory.path().join(".git")).expect("应创建 Git 标记目录");
    fs::create_dir_all(directory.path().join("src")).expect("应创建源码目录");
    fs::create_dir_all(directory.path().join("ignored")).expect("应创建忽略目录");
    fs::write(directory.path().join(".gitignore"), "ignored/\n").expect("应写入忽略规则");
    fs::write(
        directory.path().join("src/a.rs"),
        "fn alpha() {}\n// TODO first\nnext\n",
    )
    .expect("应写入第一个源码文件");
    fs::write(
        directory.path().join("src/b.rs"),
        "// TODO second\nfn beta() {}\n",
    )
    .expect("应写入第二个源码文件");
    fs::write(
        directory.path().join("ignored/hidden.rs"),
        "// TODO ignored\n",
    )
    .expect("应写入被忽略文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));

    let glob = GlobTool::new(environment.clone())
        .execute(tool_context(), json!({ "pattern": "**/*.rs" }))
        .await
        .expect("Glob 应成功");
    let glob = output_text(&glob);
    assert!(glob.contains("src/a.rs"));
    assert!(glob.contains("src/b.rs"));
    assert!(!glob.contains("ignored/hidden.rs"));

    let grep_tool = GrepTool::new(environment);
    let content = grep_tool
        .execute(
            tool_context(),
            json!({
                "pattern": "TODO",
                "glob": "**/*.rs",
                "context_after": 1
            }),
        )
        .await
        .expect("内容搜索应成功");
    let content = output_text(&content);
    assert!(content.contains("2:// TODO first"));
    assert!(content.contains("3-next"));
    assert!(!content.contains("ignored"));

    let files = grep_tool
        .execute(
            tool_context(),
            json!({ "pattern": "TODO", "output_mode": "files_with_matches" }),
        )
        .await
        .expect("文件列表搜索应成功");
    assert_eq!(output_text(&files).matches(".rs").count(), 2);

    let counts = grep_tool
        .execute(
            tool_context(),
            json!({ "pattern": "TODO", "output_mode": "count" }),
        )
        .await
        .expect("计数搜索应成功");
    assert!(output_text(&counts).contains("a.rs:1"));
    assert!(output_text(&counts).contains("b.rs:1"));
}

/// 并行完成顺序不得改变路径排序、截断结果或截断前的跳过计数。
#[tokio::test]
async fn parallel_grep_preserves_order_and_truncation() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(directory.path().join("00.bin"), [0]).expect("应写入二进制文件");
    for index in (1..=24).rev() {
        let padding = "padding\n".repeat((25 - index) * 100);
        fs::write(
            directory.path().join(format!("{index:02}.txt")),
            format!("{padding}needle needle\n"),
        )
        .expect("应写入不同大小的文件");
    }
    fs::write(directory.path().join("99.bin"), [0]).expect("应写入末尾二进制文件");
    let root = fs::canonicalize(directory.path()).expect("应解析绝对路径");
    let tool = GrepTool::new(Arc::new(ToolEnvironment::new(&root).expect("环境应有效")));
    for mode in ["files_with_matches", "count"] {
        for limit in [1, 7, 24, 25] {
            let mut expected = (1..=limit.min(24))
                .map(|index| {
                    let path = root
                        .join(format!("{index:02}.txt"))
                        .to_string_lossy()
                        .replace('\\', "/");
                    if mode == "count" {
                        format!("{path}:2")
                    } else {
                        path
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            if limit <= 24 {
                expected.push_str(&format!("\n[结果已截断到 {limit} 项]"));
            }
            let binary_count = if limit <= 24 { 1 } else { 2 };
            expected.push_str(&format!(
                "\n[跳过：超大文件 0，二进制或非 UTF-8 文件 {binary_count}，不可读取文件 0]"
            ));
            for _ in 0..3 {
                let output = tokio::time::timeout(
                    Duration::from_secs(5),
                    tool.execute(
                        tool_context(),
                        json!({
                            "pattern": "needle", "output_mode": mode, "max_results": limit
                        }),
                    ),
                )
                .await
                .expect("截断后线程必须退出")
                .expect("搜索应成功");
                assert_eq!(output_text(&output), expected);
            }
        }
    }
}

/// 有界读取保留 BOM、上下文、恰好等于上限的文件，并过滤超限及非文本文件。
#[tokio::test]
async fn parallel_grep_preserves_content_and_file_limits() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(
        directory.path().join("a.txt"),
        "\u{feff}before\nneedle\nafter\nneedle\n",
    )
    .expect("应写入带 BOM 的文件");
    fs::write(
        directory.path().join("b.txt"),
        format!("needle{}", " ".repeat(26)),
    )
    .expect("应写入恰好 32 字节文件");
    fs::write(directory.path().join("c.txt"), "n".repeat(33)).expect("应写入超限文件");
    fs::write(directory.path().join("d.txt"), [0xff]).expect("应写入非 UTF-8 文件");
    fs::write(directory.path().join("e.bin"), [0]).expect("应写入被 Glob 排除文件");
    let root = fs::canonicalize(directory.path()).expect("应解析绝对路径");
    let environment = Arc::new(
        ToolEnvironment::with_limits(
            &root,
            ToolLimits {
                max_search_file_bytes: 32,
                ..ToolLimits::default()
            },
        )
        .expect("环境应有效"),
    );
    let tool = GrepTool::new(environment);
    let output = tool
        .execute(
            tool_context(),
            json!({
                "pattern": "needle", "glob": "*.txt", "max_results": 1,
                "context_before": 1, "context_after": 1
            }),
        )
        .await
        .expect("内容搜索应成功");
    let path = root.join("a.txt").to_string_lossy().replace('\\', "/");
    assert_eq!(
        output_text(&output),
        format!("{path}\n1-before\n2:needle\n3-after\n[结果已截断到 1 项]")
    );
    let output = tool
        .execute(
            tool_context(),
            json!({
                "pattern": "needle", "glob": "*.txt", "output_mode": "count"
            }),
        )
        .await
        .expect("计数搜索应成功");
    let second = root.join("b.txt").to_string_lossy().replace('\\', "/");
    assert_eq!(
        output_text(&output),
        format!(
            "{path}:2\n{second}:1\n[跳过：超大文件 1，二进制或非 UTF-8 文件 1，不可读取文件 0]"
        )
    );
    let context = tool_context();
    context.cancellation.cancel();
    let error = tool
        .execute(context, json!({"pattern": "needle"}))
        .await
        .expect_err("预取消的 Grep 必须失败");
    assert_eq!(error.code, "cancelled");
}

/// multiline=true 必须把跨行匹配映射到涉及的全部一基行号。
#[tokio::test]
async fn grep_multiline_maps_every_spanned_line() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(directory.path().join("multi.txt"), "alpha\nbeta\ngamma\n")
        .expect("应写入跨行测试文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let output = GrepTool::new(environment)
        .execute(
            tool_context(),
            json!({ "pattern": "alpha\\nbeta", "multiline": true }),
        )
        .await
        .expect("跨行搜索应成功");
    let output = output_text(&output);
    assert!(output.contains("1:alpha"));
    assert!(output.contains("2:beta"));
}

/// 预取消的搜索不能开始目录遍历。
#[tokio::test]
async fn cancelled_search_returns_stable_error() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(directory.path().join("a.txt"), "text").expect("应写入测试文件");
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));
    let context = tool_context();
    context.cancellation.cancel();
    let error = GlobTool::new(environment)
        .execute(context, json!({ "pattern": "**/*" }))
        .await
        .expect_err("预取消搜索必须失败");
    assert_eq!(error.code, "cancelled");
}

/// Windows PowerShell 必须保留 UTF-8 stdout、stderr 和真实非零退出码。
#[cfg(windows)]
#[tokio::test]
async fn powershell_reports_utf8_and_nonzero_exit() {
    let directory = tempdir().expect("应创建临时目录");
    let artifact_directory = directory.path().join("artifacts");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_artifact_directory(&artifact_directory)
            .expect("输出目录应有效"),
    );
    let error = PowerShellTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "command": "[Console]::Out.WriteLine('中文输出'); [Console]::Error.WriteLine('错误流'); exit 7"
            }),
        )
        .await
        .expect_err("非零退出必须作为工具错误返回");

    assert_eq!(error.code, "command_failed");
    assert!(error.message.contains("退出码 7"));
    assert!(error.message.contains("中文输出"));
    assert!(error.message.contains("错误流"));
    assert_eq!(
        fs::read_dir(artifact_directory)
            .expect("应读取输出目录")
            .count(),
        0
    );
}

/// 非 Windows Bash 必须保留 stdout、stderr 和真实非零退出码。
#[cfg(not(windows))]
#[tokio::test]
async fn bash_reports_output_and_nonzero_exit() {
    let directory = tempdir().expect("应创建临时目录");
    let artifact_directory = directory.path().join("artifacts");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_artifact_directory(&artifact_directory)
            .expect("输出目录应有效"),
    );
    let error = BashTool::new(environment)
        .execute(
            tool_context(),
            json!({ "command": "printf 'hello'; printf 'bad' >&2; exit 7" }),
        )
        .await
        .expect_err("非零退出必须作为工具错误返回");

    assert_eq!(error.code, "command_failed");
    assert!(error.message.contains("退出码 7"));
    assert!(error.message.contains("hello"));
    assert!(error.message.contains("bad"));
    assert_eq!(
        fs::read_dir(artifact_directory)
            .expect("应读取输出目录")
            .count(),
        0
    );
}

/// 超过预览上限的 PowerShell 输出必须保留完整落盘文件。
#[cfg(windows)]
#[tokio::test]
async fn powershell_large_output_is_spilled_without_loss() {
    let directory = tempdir().expect("应创建临时目录");
    let limits = ToolLimits {
        max_command_preview_bytes: 32,
        ..ToolLimits::default()
    };
    let artifact_directory = directory.path().join("artifacts");
    let environment = Arc::new(
        ToolEnvironment::with_limits(directory.path(), limits)
            .expect("工具环境应有效")
            .with_artifact_directory(&artifact_directory)
            .expect("输出目录应有效"),
    );
    let output = PowerShellTool::new(environment)
        .execute(
            tool_context(),
            json!({ "command": "[Console]::Out.Write(('x' * 200))" }),
        )
        .await
        .expect("大输出命令应成功");
    assert!(output_text(&output).contains("完整输出"));
    let artifacts = fs::read_dir(&artifact_directory)
        .expect("应读取输出目录")
        .map(|entry| entry.expect("输出目录项应有效").path())
        .collect::<Vec<_>>();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        fs::read(&artifacts[0]).expect("应读取完整输出"),
        vec![b'x'; 200]
    );
}

/// 超过预览上限的 Bash 输出必须保留完整落盘文件。
#[cfg(not(windows))]
#[tokio::test]
async fn bash_large_output_is_spilled_without_loss() {
    let directory = tempdir().expect("应创建临时目录");
    let limits = ToolLimits {
        max_command_preview_bytes: 32,
        ..ToolLimits::default()
    };
    let artifact_directory = directory.path().join("artifacts");
    let environment = Arc::new(
        ToolEnvironment::with_limits(directory.path(), limits)
            .expect("工具环境应有效")
            .with_artifact_directory(&artifact_directory)
            .expect("输出目录应有效"),
    );
    let output = BashTool::new(environment)
        .execute(
            tool_context(),
            json!({ "command": "printf '%0200d' 0 | tr '0' 'x'" }),
        )
        .await
        .expect("大输出命令应成功");
    assert!(output_text(&output).contains("完整输出"));
    let artifacts = fs::read_dir(&artifact_directory)
        .expect("应读取输出目录")
        .map(|entry| entry.expect("输出目录项应有效").path())
        .collect::<Vec<_>>();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(
        fs::read(&artifacts[0]).expect("应读取完整输出"),
        vec![b'x'; 200]
    );
}

/// 默认预览预算下，大日志仍保留首尾、退出码和完整产物，命令副作用只发生一次。
#[cfg(not(windows))]
#[tokio::test]
async fn bash_default_preview_retains_error_and_completed_effect() {
    let directory = tempdir().unwrap();
    let artifacts = directory.path().join("artifacts");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .unwrap()
            .with_artifact_directory(&artifacts)
            .unwrap(),
    );
    let error = BashTool::new(environment).execute(tool_context(), json!({
        "command": "printf 'once\\n' >> effect.txt; printf 'LOG_START\\n'; printf '%040000d' 0; printf '\\nLOG_END\\n'; printf 'ERROR_AT_END\\n' >&2; exit 7"
    })).await.expect_err("非零退出码必须保留为工具错误");
    assert!(!error.retryable);
    let preview = &error.message;
    assert!(preview.contains("LOG_START") && preview.contains("LOG_END"));
    assert!(preview.contains("ERROR_AT_END") && preview.contains("7"));
    assert!(preview.contains("完整输出"));
    assert!(preview.len() <= keencode_agent::TOOL_OUTPUT_LIMITS.max_tool_error_message_bytes);
    assert_eq!(
        fs::read_to_string(directory.path().join("effect.txt")).unwrap(),
        "once\n"
    );
    let files = fs::read_dir(artifacts)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1);
    let stdout = files
        .iter()
        .find(|file| {
            file.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("keencode-stdout-")
        })
        .unwrap();
    let complete = fs::read_to_string(stdout).unwrap();
    assert_eq!(
        complete.len(),
        "LOG_START\n".len() + 40_000 + "\nLOG_END\n".len()
    );
    assert!(complete.starts_with("LOG_START") && complete.ends_with("LOG_END\n"));
}

/// Windows 超时必须终止 Job Object 内的后代，后代不能延迟写入标记文件。
#[cfg(windows)]
#[tokio::test]
async fn powershell_timeout_kills_descendant_process_tree() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(
        directory.path().join("child.ps1"),
        "Start-Sleep -Milliseconds 800\nSet-Content -LiteralPath 'leaked.txt' -Value 'leaked'\n",
    )
    .expect("应写入后代脚本");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_artifact_directory(directory.path().join("artifacts"))
            .expect("输出目录应有效"),
    );
    let error = PowerShellTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "command": "Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile','-NonInteractive','-File','child.ps1') -WindowStyle Hidden; Start-Sleep -Seconds 10",
                "timeout_ms": 150
            }),
        )
        .await
        .expect_err("超时命令必须失败");
    assert_eq!(error.code, "command_timed_out");
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    assert!(!directory.path().join("leaked.txt").exists());
}

/// Unix 超时必须终止进程组内的后代，后代不能延迟写入标记文件。
#[cfg(not(windows))]
#[tokio::test]
async fn bash_timeout_kills_descendant_process_tree() {
    let directory = tempdir().expect("应创建临时目录");
    let environment = Arc::new(
        ToolEnvironment::new(directory.path())
            .expect("工具环境应有效")
            .with_artifact_directory(directory.path().join("artifacts"))
            .expect("输出目录应有效"),
    );
    let error = BashTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "command": "(sleep 0.8; printf leaked > leaked.txt) & wait",
                "timeout_ms": 150
            }),
        )
        .await
        .expect_err("超时命令必须失败");
    assert_eq!(error.code, "command_timed_out");
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    assert!(!directory.path().join("leaked.txt").exists());
}

/// 使用当前平台的非交互 Shell 构造通用有界命令测试请求。
#[cfg(windows)]
fn bounded_shell_request(
    directory: &Path,
    command: &str,
    timeout: Duration,
    maximum_output: usize,
) -> BoundedCommandRequest {
    BoundedCommandRequest::new("powershell.exe", directory, timeout, maximum_output).with_args(
        vec![
            OsString::from("-NoLogo"),
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(command),
        ],
    )
}

/// 使用当前平台的非交互 Shell 构造通用有界命令测试请求。
#[cfg(not(windows))]
fn bounded_shell_request(
    directory: &Path,
    command: &str,
    timeout: Duration,
    maximum_output: usize,
) -> BoundedCommandRequest {
    BoundedCommandRequest::new("sh", directory, timeout, maximum_output)
        .with_args(vec![OsString::from("-c"), OsString::from(command)])
}

/// 通用命令端口必须原样传递输入和环境，并保留两个输出流与真实退出码。
#[cfg(windows)]
#[tokio::test]
async fn bounded_command_captures_windows_process_contract() {
    // 并行 CI 中 PowerShell 冷启动可能超过五秒；这里只验证进程契约，不验证启动耗时。
    let directory = tempdir().expect("应创建临时目录");
    let request = bounded_shell_request(
        directory.path(),
        "$reader = [System.IO.StreamReader]::new([Console]::OpenStandardInput()); $text = $reader.ReadToEnd(); [Console]::Out.Write($text + $env:KEENCODE_BOUNDED_TEST); [Console]::Error.Write('err'); exit 7",
        Duration::from_secs(20),
        1024,
    )
    .with_stdin(b"payload".to_vec())
    .with_environment(vec![(
        OsString::from("KEENCODE_BOUNDED_TEST"),
        OsString::from("-environment"),
    )]);

    let output = run_bounded_command(request)
        .await
        .expect("非零退出仍应返回完整进程结果");

    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"payload-environment");
    assert_eq!(output.stderr, b"err");
}

/// 通用命令端口必须原样传递输入和环境，并保留两个输出流与真实退出码。
#[cfg(not(windows))]
#[tokio::test]
async fn bounded_command_captures_unix_process_contract() {
    let directory = tempdir().expect("应创建临时目录");
    let request = bounded_shell_request(
        directory.path(),
        "input=$(cat); printf '%s%s' \"$input\" \"$KEENCODE_BOUNDED_TEST\"; printf err >&2; exit 7",
        Duration::from_secs(5),
        1024,
    )
    .with_stdin(b"payload".to_vec())
    .with_environment(vec![(
        OsString::from("KEENCODE_BOUNDED_TEST"),
        OsString::from("-environment"),
    )]);

    let output = run_bounded_command(request)
        .await
        .expect("非零退出仍应返回完整进程结果");

    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"payload-environment");
    assert_eq!(output.stderr, b"err");
}

/// 任一输出流超过调用方上限时必须失败并停止整个进程组。
#[cfg(windows)]
#[tokio::test]
async fn bounded_command_rejects_oversized_windows_output() {
    let directory = tempdir().expect("应创建临时目录");
    let error = run_bounded_command(bounded_shell_request(
        directory.path(),
        "[Console]::Out.Write(('x' * 4096)); Start-Sleep -Seconds 10",
        Duration::from_secs(20),
        64,
    ))
    .await
    .expect_err("超限输出必须失败");

    assert_eq!(error.code(), "command_output_too_large");
}

/// 任一输出流超过调用方上限时必须失败并停止整个进程组。
#[cfg(not(windows))]
#[tokio::test]
async fn bounded_command_rejects_oversized_unix_output() {
    let directory = tempdir().expect("应创建临时目录");
    let error = run_bounded_command(bounded_shell_request(
        directory.path(),
        "while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done",
        Duration::from_secs(5),
        64,
    ))
    .await
    .expect_err("超限输出必须失败");

    assert_eq!(error.code(), "command_output_too_large");
}

/// 硬超时必须返回稳定错误并终止整个进程组。
#[tokio::test]
async fn bounded_command_enforces_hard_timeout() {
    let directory = tempdir().expect("应创建临时目录");
    #[cfg(windows)]
    let command = "Start-Sleep -Seconds 10";
    #[cfg(not(windows))]
    let command = "sleep 10";
    let error = run_bounded_command(bounded_shell_request(
        directory.path(),
        command,
        Duration::from_millis(100),
        1024,
    ))
    .await
    .expect_err("超时命令必须失败");

    assert_eq!(error.code(), "command_timed_out");
}

/// 丢弃运行 Future 时，Drop 守卫必须终止可延迟写文件的后代进程。
#[cfg(windows)]
#[tokio::test]
async fn bounded_command_abort_kills_windows_descendants() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(
        directory.path().join("child.ps1"),
        "Start-Sleep -Milliseconds 800\nSet-Content -LiteralPath 'leaked.txt' -Value 'leaked'\n",
    )
    .expect("应写入后代脚本");
    let request = bounded_shell_request(
        directory.path(),
        "Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoLogo','-NoProfile','-NonInteractive','-File','child.ps1') -WindowStyle Hidden; Set-Content -LiteralPath 'ready.txt' -Value 'ready'; Start-Sleep -Seconds 10",
        Duration::from_secs(20),
        1024,
    );
    let task = tokio::spawn(run_bounded_command(request));
    wait_for_marker(directory.path().join("ready.txt").as_path()).await;
    task.abort();
    let _ = task.await;

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!directory.path().join("leaked.txt").exists());
}

/// 丢弃运行 Future 时，Drop 守卫必须终止可延迟写文件的后代进程。
#[cfg(not(windows))]
#[tokio::test]
async fn bounded_command_abort_kills_unix_descendants() {
    let directory = tempdir().expect("应创建临时目录");
    let request = bounded_shell_request(
        directory.path(),
        "(sleep 0.8; printf leaked > leaked.txt) & printf ready > ready.txt; sleep 10",
        Duration::from_secs(20),
        1024,
    );
    let task = tokio::spawn(run_bounded_command(request));
    wait_for_marker(directory.path().join("ready.txt").as_path()).await;
    task.abort();
    let _ = task.await;

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!directory.path().join("leaked.txt").exists());
}

/// 主进程正常退出也必须回收尚存后代，不能等到后代自行完成。
#[cfg(windows)]
#[tokio::test]
async fn bounded_command_normal_windows_exit_kills_descendants() {
    let directory = tempdir().expect("应创建临时目录");
    fs::write(
        directory.path().join("child.ps1"),
        "Start-Sleep -Milliseconds 800\nSet-Content -LiteralPath 'leaked.txt' -Value 'leaked'\n",
    )
    .expect("应写入后代脚本");
    let output = run_bounded_command(bounded_shell_request(
        directory.path(),
        "Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoLogo','-NoProfile','-NonInteractive','-File','child.ps1') -WindowStyle Hidden; [Console]::Out.Write('parent')",
        Duration::from_secs(20),
        1024,
    ))
    .await
    .expect("父进程正常退出应成功");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"parent");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!directory.path().join("leaked.txt").exists());
}

/// 主进程正常退出也必须回收尚存后代，不能等到后代自行完成。
#[cfg(not(windows))]
#[tokio::test]
async fn bounded_command_normal_unix_exit_kills_descendants() {
    let directory = tempdir().expect("应创建临时目录");
    let output = run_bounded_command(bounded_shell_request(
        directory.path(),
        "(sleep 0.8; printf leaked > leaked.txt) & printf parent",
        Duration::from_secs(5),
        1024,
    ))
    .await
    .expect("父进程正常退出应成功");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"parent");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!directory.path().join("leaked.txt").exists());
}

/// 等待子进程明确写出启动标记，避免取消测试出现未启动即通过的假阳性。
async fn wait_for_marker(path: &Path) {
    // 并行 Rust/PowerShell 集成测试会竞争 Windows 进程启动资源，最多等待十五秒。
    for _ in 0..750 {
        if path.is_file() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("子进程未在限定时间内写出启动标记：{}", path.display());
}

/// 大输入确保写入尚未完成时子进程已退出，稳定覆盖关闭 stdin 的竞态。
#[tokio::test]
async fn bounded_command_preserves_early_exit_without_reading_stdin() {
    let directory = tempdir().unwrap();
    #[cfg(windows)]
    let command = "[Console]::Out.Write('out'); [Console]::Error.Write('err'); exit 2";
    #[cfg(not(windows))]
    let command = "printf out; printf err >&2; exit 2";
    let request = bounded_shell_request(directory.path(), command, Duration::from_secs(20), 1024)
        .with_stdin(vec![b'x'; 8 * 1024 * 1024]);
    let output = run_bounded_command(request)
        .await
        .expect("提前退出应保留进程结果");
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");
}

/// 内置工具必须按契约声明外层墙钟：搜索与读取 15 秒，自管超时工具为 None。
#[test]
fn builtin_tools_declare_contracted_outer_wall_clock_timeouts() {
    let directory = tempdir().unwrap();
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));

    assert_eq!(
        AgentTool::timeout(&GlobTool::new(environment.clone())),
        Some(Duration::from_secs(15))
    );
    assert_eq!(
        AgentTool::timeout(&GrepTool::new(environment.clone())),
        Some(Duration::from_secs(15))
    );
    assert_eq!(
        AgentTool::timeout(&ReadTool::new(environment.clone())),
        Some(Duration::from_secs(15))
    );
    // 大文件写入（64MiB 上限）可能合法耗时，写入与编辑保持默认 120 秒。
    assert_eq!(
        AgentTool::timeout(&EditTool::new(environment.clone())),
        Some(Duration::from_secs(120))
    );
    assert_eq!(
        AgentTool::timeout(&WriteTool::new(environment)),
        Some(Duration::from_secs(120))
    );
}

/// 命令类工具自管超时（可配置到 1 小时），必须声明为 None 以免被外层墙钟截断。
#[test]
fn command_tools_declare_self_managed_timeout() {
    let directory = tempdir().unwrap();
    let environment = Arc::new(ToolEnvironment::new(directory.path()).expect("工具环境应有效"));

    #[cfg(not(windows))]
    {
        assert_eq!(
            AgentTool::timeout(&BashTool::new(environment.clone())),
            None
        );
    }
    #[cfg(windows)]
    {
        assert_eq!(
            AgentTool::timeout(&PowerShellTool::new(environment.clone())),
            None
        );
    }
}

/// 工件落盘通道保存完整正文并净化工具名前缀；目录不可写时回传错误。
#[test]
fn environment_artifact_sink_saves_full_output_with_sanitized_prefix() {
    let directory = tempdir().expect("应创建临时目录");
    let environment = ToolEnvironment::new(directory.path())
        .expect("工具环境应有效")
        .with_artifact_directory(directory.path().join("artifacts"))
        .expect("输出目录应有效");
    let sink = crate::environment::EnvironmentArtifactSink::new(&environment);

    let path = sink
        .save_output("Read/工具:01", "完整输出正文")
        .expect("工件应保存成功");
    let file_name = Path::new(&path)
        .file_name()
        .expect("路径应包含文件名")
        .to_string_lossy()
        .into_owned();
    // 非文件名安全字符整体剔除，避免拼接出异常路径。
    assert!(file_name.starts_with("keencode-Read01-"), "{file_name}");
    assert!(file_name.ends_with(".log"));
    let saved = fs::read_to_string(directory.path().join("artifacts").join(&file_name))
        .expect("应读回工件");
    assert_eq!(saved, "完整输出正文");

    // 工件目录被一个已存在的文件挡住时，保存失败必须以 Err 回传。
    fs::write(directory.path().join("blocker"), "占位").expect("应写入占位文件");
    let blocked = ToolEnvironment::new(directory.path())
        .expect("工具环境应有效")
        .with_artifact_directory(directory.path().join("blocker").join("artifacts"))
        .expect("输出目录应有效");
    let blocked_sink = crate::environment::EnvironmentArtifactSink::new(&blocked);
    assert!(
        blocked_sink.save_output("Read", "内容").is_err(),
        "不可写目录应回传错误"
    );
}

/// 工作区白名单守卫：工作目录内读写自由，工作区外 Read/Write/Edit 一律拒绝，
/// 长名临时目录豁免（离树草稿的合法落点）。
#[tokio::test]
async fn workspace_guard_confines_file_tools() {
    let workspace = tempdir().expect("应创建工作区");
    // "外部"目录必须真正落在守卫允许根之外：不能放在 %TEMP% 下（临时目录本身豁免）。
    let outside = std::env::current_dir()
        .expect("应取得进程工作目录")
        .join(format!("guard-outside-{}", std::process::id()));
    std::fs::create_dir_all(&outside).expect("应创建外部目录");
    std::fs::write(workspace.path().join("inside.txt"), "inside").expect("应写入");
    std::fs::write(outside.join("secret.txt"), "secret").expect("应写入外部文件");

    let environment = Arc::new(
        ToolEnvironment::new(workspace.path())
            .expect("工具环境应有效")
            .with_workspace_guard(),
    );
    let context = tool_context();

    let inside_read = ReadTool::new(Arc::clone(&environment))
        .execute(context.clone(), json!({ "file_path": "inside.txt" }))
        .await
        .expect("工作区内读取应放行");
    assert!(output_text(&inside_read).contains("inside"));

    let outside_read = ReadTool::new(Arc::clone(&environment))
        .execute(
            context.clone(),
            json!({
                "file_path": outside.join("secret.txt")
            }),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("工作区外读取应被拒绝"));
    assert_eq!(outside_read.code, "path_outside_workspace");

    let outside_write = WriteTool::new(Arc::clone(&environment))
        .execute(
            context.clone(),
            json!({
                "file_path": outside.join("secret.txt"),
                "content": "tamper"
            }),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("工作区外写入应被拒绝"));
    assert_eq!(outside_write.code, "path_outside_workspace");

    let outside_edit = crate::EditTool::new(Arc::clone(&environment))
        .execute(
            context.clone(),
            json!({
                "file_path": outside.join("secret.txt"),
                "old_string": "secret",
                "new_string": "tamper"
            }),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("工作区外编辑应被拒绝"));
    assert_eq!(outside_edit.code, "path_outside_workspace");

    // 长名临时目录豁免：离树草稿（变异验证等）是合法落点。
    let scratch = long_form_temp_dir().join("keencode-guard-scratch");
    std::fs::create_dir_all(&scratch).expect("应创建临时草稿目录");
    let scratch_write = WriteTool::new(Arc::clone(&environment))
        .execute(
            context,
            json!({
                "file_path": scratch.join("probe.txt"),
                "content": "scratch"
            }),
        )
        .await
        .expect("临时目录写入应放行");
    assert!(
        output_text(&scratch_write).contains("写入")
            || output_text(&scratch_write).contains("scratch")
    );
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_dir_all(&outside);
}

/// 守卫关闭（默认）时保持既有行为：工作区外路径不拦截。
#[tokio::test]
async fn workspace_guard_off_keeps_legacy_access() {
    let workspace = tempdir().expect("应创建工作区");
    let outside = tempdir().expect("应创建工作区外目录");
    std::fs::write(outside.path().join("secret.txt"), "secret").expect("应写入外部文件");
    let environment = Arc::new(ToolEnvironment::new(workspace.path()).expect("工具环境应有效"));
    let output = ReadTool::new(environment)
        .execute(
            tool_context(),
            json!({
                "file_path": outside.path().join("secret.txt")
            }),
        )
        .await
        .expect("守卫关闭时外部读取应放行");
    assert!(output_text(&output).contains("secret"));
}

/// 命令边界检查：POSIX 挂载、~、盘符绝对路径的工作区外引用被拒，
/// 相对路径、工作区内绝对路径与临时目录豁免放行。
#[test]
fn command_boundary_rejects_out_of_workspace_path_tokens() {
    let workspace = tempdir().expect("应创建工作区");
    let environment = ToolEnvironment::new(workspace.path())
        .expect("工具环境应有效")
        .with_workspace_guard();

    let drive = workspace.path().to_string_lossy()[..2].to_string();
    let ps_case = format!(r"Get-ChildItem '{}\elsewhere'", drive);
    let inside_case = format!("cat \"{}/inside.txt\"", workspace.path().to_string_lossy());
    let cases = [
        (
            "ls \"D:/projects/harness-bench/fixtures/imported-qihoo\"",
            true,
        ),
        ("cat ~/.hb-oracle/qi-006/ground_truth.json", true),
        (ps_case.as_str(), true),
        ("type /d/elsewhere/file.txt", true),
        ("python -m pytest tests", false),
        ("cat inside.txt", false),
        ("grep -rn pattern .", false),
        (inside_case.as_str(), false),
        (
            "python -c \"import tempfile;print(tempfile.gettempdir())\"",
            false,
        ),
    ];
    for (command, should_reject) in cases {
        let result = environment.check_command_boundary(command);
        assert_eq!(
            result.is_err(),
            should_reject,
            "command {command:?} rejection mismatch: {result:?}"
        );
    }
}

/// 守卫开启时 `cd ..` 逃逸与 POSIX /tmp 映射语义。
#[test]
fn command_boundary_maps_posix_forms() {
    let workspace = tempdir().expect("应创建工作区");
    let environment = ToolEnvironment::new(workspace.path())
        .expect("工具环境应有效")
        .with_workspace_guard();
    // /tmp 映射到长名临时目录，豁免。
    assert!(
        environment
            .check_command_boundary("cd /tmp && ./probe")
            .is_ok()
    );
    // `cd ..` 逃出工作目录后落在豁免的临时根内（放行），再上一层即越界，拒绝。
    assert!(environment.check_command_boundary("cd ..").is_ok());
    assert!(environment.check_command_boundary("cd ../..").is_err());
}
