//! 后台 Shell 任务的进程生命周期、持久输出与模型工具。

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant as StdInstant, SystemTime, UNIX_EPOCH};

use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    TurnCancellation,
};
use keencode_model::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::command::{
    ProcessGroupGuard, ProcessSpec, ProcessTermination, monitor_process, spawn_error, spawn_group,
    terminate_and_wait,
};
use crate::environment::invalid_input;

/// 单次 `TaskOutput` 阻塞等待允许的最长时间。
const MAX_TASK_OUTPUT_WAIT: Duration = Duration::from_secs(600);

/// 未显式指定时 `TaskOutput` 等待新增输出或终态的时间。
const DEFAULT_TASK_OUTPUT_WAIT: Duration = Duration::from_secs(30);

/// 完成事件广播缓冲区容量；慢消费者会收到 Tokio 的明确 lag 错误。
const COMPLETION_EVENT_CAPACITY: usize = 256;

/// 模型可提交的后台任务标识最大 UTF-8 字节数。
const MAX_BACKGROUND_TASK_ID_BYTES: usize = 256;

/// 防止任意二进制损失解码膨胀后超过 Agent 单文本块硬上限的原始字节预算。
const MAX_SAFE_BACKGROUND_OUTPUT_CHUNK_BYTES: usize = 128 * 1024;

/// 一个 UTF-8 标量允许占用的最大字节数。
const MAX_UTF8_SCALAR_BYTES: usize = 4;

/// stdout 与 stderr 同时活跃时，各自容纳一个完整 UTF-8 标量所需的总预算下限。
const MIN_BACKGROUND_OUTPUT_CHUNK_BYTES: usize = 2 * MAX_UTF8_SCALAR_BYTES;

/// 一个 UTF-8 标量在完整编码前最多暂存的尾部字节数。
const MAX_INCOMPLETE_UTF8_SUFFIX_BYTES: usize = MAX_UTF8_SCALAR_BYTES - 1;

/// 为同一进程中的后台 Shell 任务分配不会回退的后缀。
static NEXT_BACKGROUND_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// 后台 Shell 任务对外可见的生命周期状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundTaskStatus {
    /// 进程仍在运行，或者已经收到取消信号但尚未完成回收。
    Running,
    /// 进程以成功退出状态结束，且两个输出流均完整落盘。
    Succeeded,
    /// 进程非零退出、超时、等待失败或输出持久化失败。
    Failed,
    /// 用户或 Runtime 已取消任务并完成进程树回收。
    Cancelled,
}

impl BackgroundTaskStatus {
    /// 返回当前状态是否已经不可逆地结束。
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// 返回供模型和桌面层使用的稳定 snake_case 文本。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 一个真实后台 Shell 任务的当前只读快照。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundTaskInfo {
    /// 任务所属的根 Session。
    pub session_id: String,
    /// Runtime 分配且可用于读取或停止任务的稳定标识。
    pub task_id: String,
    /// 不包含完整命令文本的单行任务说明。
    pub summary: String,
    /// 任务启动的 Unix 毫秒时间戳。
    pub started_at_unix_ms: u64,
    /// 从任务启动到快照时刻或终态的持续毫秒数。
    pub duration_ms: u64,
    /// 当前真实生命周期状态。
    pub status: BackgroundTaskStatus,
    /// 系统报告的根进程标识。
    pub pid: Option<u32>,
    /// 进程正常退出时的真实退出码；信号终止或未结束时为空。
    pub exit_code: Option<i32>,
    /// 已经持久写入且可从 stdout 安全增量读取的字节数。
    pub stdout_bytes: u64,
    /// 已经持久写入且可从 stderr 安全增量读取的字节数。
    pub stderr_bytes: u64,
    /// 是否已经向仍在运行的进程树发出停止信号。
    pub stop_requested: bool,
}

/// 后台任务输出的两个独立字节游标。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BackgroundOutputCursor {
    /// 下一次读取 stdout 时使用的字节偏移。
    pub stdout_offset: u64,
    /// 下一次读取 stderr 时使用的字节偏移。
    pub stderr_offset: u64,
}

/// 从持久输出文件读取的一次有界增量。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundTaskOutput {
    /// 读取完成时任务的真实状态快照。
    pub task: BackgroundTaskInfo,
    /// 下一次增量读取应传回的字节游标。
    pub next_cursor: BackgroundOutputCursor,
    /// 本次从 stdout 新读取的损失解码 UTF-8 文本。
    pub stdout: String,
    /// 本次从 stderr 新读取的损失解码 UTF-8 文本。
    pub stderr: String,
    /// stdout 在新游标之后是否仍有未返回字节。
    pub stdout_has_more: bool,
    /// stderr 在新游标之后是否仍有未返回字节。
    pub stderr_has_more: bool,
}

/// 后台 Shell 任务提交唯一终态后广播的完成事件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundTaskCompletion {
    /// 任务所属的根 Session。
    pub session_id: String,
    /// 已经结束的后台任务标识。
    pub task_id: String,
    /// 已经提交的唯一终态。
    pub status: BackgroundTaskStatus,
    /// 从进程启动到完成回收及输出落盘的持续毫秒数。
    pub duration_ms: u64,
    /// 不含命令正文或输出正文的有界完成摘要。
    pub summary: String,
}

/// 批量取消真实发出信号后的精确结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackgroundCancelReport {
    /// 本次首次收到取消信号的任务标识。
    pub requested_task_ids: Vec<String>,
    /// 查询时已经处于终态、因此没有伪造取消成功的任务数量。
    pub already_terminal: usize,
    /// 查询时已经收到过停止信号、因此没有重复报告成功的任务数量。
    pub already_requested: usize,
}

/// 后台任务 Manager 停止接受新任务并完成回收后的结果。
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackgroundShutdownReport {
    /// shutdown 首次发出取消信号的任务标识。
    pub cancelled_task_ids: Vec<String>,
    /// shutdown 开始前已经结束的任务数量。
    pub already_terminal: usize,
    /// shutdown 前已经收到停止信号、但仍等待回收的任务数量。
    pub already_requested: usize,
}

/// 后台任务查询、取消、清理或持久输出读取失败。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundTaskError {
    /// 适合 Runtime 映射和自动化断言的稳定错误码。
    pub code: String,
    /// 不回显命令或输出正文的安全错误说明。
    pub message: String,
}

impl BackgroundTaskError {
    /// 创建一个稳定的后台任务错误。
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// 转成 Agent 工具边界使用的不可重试错误。
    fn into_tool_error(self) -> ToolError {
        ToolError::permanent(self.code, self.message)
    }
}

impl fmt::Display for BackgroundTaskError {
    /// 输出稳定错误码和安全说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}：{}", self.code, self.message)
    }
}

impl Error for BackgroundTaskError {}

/// 管理一个 Runtime 作用域内全部后台 Shell 任务的共享句柄。
#[derive(Clone)]
pub struct BackgroundTaskManager {
    /// 由工具、桌面命令和退出流程共同持有的真实状态。
    inner: Arc<BackgroundTaskManagerInner>,
}

/// 后台任务 Manager 的共享可变状态。
struct BackgroundTaskManagerInner {
    /// 每个任务独占子目录所在的应用数据目录。
    output_directory: PathBuf,
    /// 单次增量读取允许返回的 stdout 与 stderr 总字节上限。
    max_output_chunk_bytes: usize,
    /// 开始任务与 shutdown 之间的异步互斥门。
    lifecycle_gate: AsyncMutex<()>,
    /// shutdown 后永久变为 false，禁止重新接受任务。
    accepting_tasks: AtomicBool,
    /// 按稳定任务标识排序保存运行中和已完成记录。
    tasks: RwLock<BTreeMap<String, Arc<BackgroundTaskRecord>>>,
    /// 向 Runtime 桥接层广播唯一完成事件。
    completion_events: broadcast::Sender<BackgroundTaskCompletion>,
}

impl Drop for BackgroundTaskManagerInner {
    /// 最后一个 Manager 句柄消失时向所有残留进程发出取消信号。
    fn drop(&mut self) {
        let tasks = match self.tasks.get_mut() {
            Ok(tasks) => tasks,
            Err(poisoned) => poisoned.into_inner(),
        };
        for task in tasks.values() {
            task.cancellation.cancel();
        }
    }
}

/// 单个后台任务的不可变身份、输出路径和受锁状态。
struct BackgroundTaskRecord {
    /// 任务所属根 Session。
    session_id: String,
    /// Runtime 生成的稳定任务标识。
    task_id: String,
    /// 模型与 UI 可见的有界单行说明。
    summary: String,
    /// Unix 毫秒启动时间。
    started_at_unix_ms: u64,
    /// 用于单调计算持续时间的进程内时钟。
    started: StdInstant,
    /// stdout 完整持久输出文件。
    stdout_path: PathBuf,
    /// stderr 完整持久输出文件。
    stderr_path: PathBuf,
    /// 用户停止、批量取消和 shutdown 共用的幂等信号。
    cancellation: TurnCancellation,
    /// 输出增长或任务进入终态时唤醒阻塞读取者。
    changed: Notify,
    /// 串行化模型工具的隐式增量游标消费。
    tool_output_gate: AsyncMutex<()>,
    /// 受同步锁保护的生命周期、计数与模型读取游标。
    state: Mutex<BackgroundTaskState>,
}

/// 单个后台任务所有需要原子快照的可变字段。
struct BackgroundTaskState {
    /// 当前生命周期状态。
    status: BackgroundTaskStatus,
    /// 系统根进程标识。
    pid: Option<u32>,
    /// 正常退出时的进程退出码。
    exit_code: Option<i32>,
    /// 进入终态后冻结的持续时间。
    terminal_duration_ms: Option<u64>,
    /// stdout 已完成落盘且不会截断潜在有效 UTF-8 标量的公开字节数。
    stdout_bytes: u64,
    /// stderr 已完成落盘且不会截断潜在有效 UTF-8 标量的公开字节数。
    stderr_bytes: u64,
    /// 是否已经发出停止信号。
    stop_requested: bool,
    /// `TaskOutput` 工具跨调用共享的隐式游标。
    tool_cursor: BackgroundOutputCursor,
}

impl BackgroundTaskManager {
    /// 创建输出目录固定的任务 Manager；过大的增量预算会收紧到安全硬上限。
    pub fn new(
        output_directory: impl AsRef<Path>,
        max_output_chunk_bytes: usize,
    ) -> Result<Self, BackgroundTaskError> {
        if max_output_chunk_bytes < MIN_BACKGROUND_OUTPUT_CHUNK_BYTES {
            return Err(BackgroundTaskError::new(
                "invalid_background_output_limit",
                "后台任务单次输出上限不能小于 8 字节",
            ));
        }
        let output_directory = std::path::absolute(output_directory.as_ref()).map_err(|error| {
            BackgroundTaskError::new(
                "invalid_background_output_directory",
                format!("无法解析后台任务输出目录：{error}"),
            )
        })?;
        let (completion_events, _) = broadcast::channel(COMPLETION_EVENT_CAPACITY);
        Ok(Self {
            inner: Arc::new(BackgroundTaskManagerInner {
                output_directory,
                max_output_chunk_bytes: max_output_chunk_bytes
                    .min(MAX_SAFE_BACKGROUND_OUTPUT_CHUNK_BYTES),
                lifecycle_gate: AsyncMutex::new(()),
                accepting_tasks: AtomicBool::new(true),
                tasks: RwLock::new(BTreeMap::new()),
                completion_events,
            }),
        })
    }

    /// 返回 Manager 是否仍允许启动新后台任务。
    pub fn is_accepting_tasks(&self) -> bool {
        self.inner.accepting_tasks.load(Ordering::Acquire)
    }

    /// 在等待全部任务终态前先封锁新的后台进程创建。
    pub fn stop_accepting_tasks(&self) {
        self.inner.accepting_tasks.store(false, Ordering::Release);
    }

    /// 返回保存每个任务独占输出子目录的绝对根目录。
    pub fn output_directory(&self) -> &Path {
        &self.inner.output_directory
    }

    /// 订阅此后提交的后台任务唯一完成事件。
    pub fn subscribe_completions(&self) -> broadcast::Receiver<BackgroundTaskCompletion> {
        self.inner.completion_events.subscribe()
    }

    /// 返回全部运行中和已完成任务的真实排序快照。
    pub fn list(&self) -> Result<Vec<BackgroundTaskInfo>, BackgroundTaskError> {
        self.snapshot_tasks(false)
    }

    /// 只返回查询时仍未提交终态的任务。
    pub fn list_running(&self) -> Result<Vec<BackgroundTaskInfo>, BackgroundTaskError> {
        self.snapshot_tasks(true)
    }

    /// 返回属于指定 Session 的一个精确任务状态快照。
    pub fn task_info(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<BackgroundTaskInfo, BackgroundTaskError> {
        let task = self.task(task_id)?;
        ensure_task_session(&task, session_id)?;
        task.info()
    }

    /// 从显式字节游标读取持久文件；可阻塞到有增量、终态或等待到期。
    pub async fn read_output(
        &self,
        session_id: &str,
        task_id: &str,
        cursor: BackgroundOutputCursor,
        block_for: Option<Duration>,
    ) -> Result<BackgroundTaskOutput, BackgroundTaskError> {
        let task = self.task(task_id)?;
        ensure_task_session(&task, session_id)?;
        let _gate = task.tool_output_gate.lock().await;
        wait_until_readable(&task, cursor, block_for, None).await?;
        read_persisted_output(&task, cursor, self.inner.max_output_chunk_bytes).await
    }

    /// 向一个仍在运行且尚未停止的任务首次发出进程树取消信号。
    pub fn cancel(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<BackgroundTaskInfo, BackgroundTaskError> {
        let task = self.task(task_id)?;
        ensure_task_session(&task, session_id)?;
        {
            let mut state = lock_task_state(&task)?;
            if state.status.is_terminal() {
                return Err(BackgroundTaskError::new(
                    "background_task_not_running",
                    format!("后台任务 {task_id} 已处于 {} 终态", state.status.as_str()),
                ));
            }
            if state.stop_requested {
                return Err(BackgroundTaskError::new(
                    "background_task_stop_already_requested",
                    format!("后台任务 {task_id} 已经收到停止请求"),
                ));
            }
            state.stop_requested = true;
        }
        task.cancellation.cancel();
        task.changed.notify_waiters();
        task.info()
    }

    /// 对查询时全部运行中任务真实发出取消信号，并报告未被伪装成成功的终态数量。
    pub fn cancel_all(&self) -> Result<BackgroundCancelReport, BackgroundTaskError> {
        let tasks = self.tasks_snapshot()?;
        let mut report = BackgroundCancelReport::default();
        for task in tasks {
            let mut state = lock_task_state(&task)?;
            if state.status.is_terminal() {
                report.already_terminal = report.already_terminal.saturating_add(1);
                continue;
            }
            if state.stop_requested {
                report.already_requested = report.already_requested.saturating_add(1);
                continue;
            }
            state.stop_requested = true;
            report.requested_task_ids.push(task.task_id.clone());
            drop(state);
            task.cancellation.cancel();
            task.changed.notify_waiters();
        }
        Ok(report)
    }

    /// 永久停止接收新任务，取消全部运行中任务并等待每个进程树完成回收。
    pub async fn shutdown(&self) -> Result<BackgroundShutdownReport, BackgroundTaskError> {
        let lifecycle_gate = self.inner.lifecycle_gate.lock().await;
        self.inner.accepting_tasks.store(false, Ordering::Release);
        let tasks = self.tasks_snapshot()?;
        let mut report = BackgroundShutdownReport::default();
        for task in &tasks {
            let mut state = lock_task_state(task)?;
            if state.status.is_terminal() {
                report.already_terminal = report.already_terminal.saturating_add(1);
                continue;
            }
            if !state.stop_requested {
                state.stop_requested = true;
                report.cancelled_task_ids.push(task.task_id.clone());
                drop(state);
                task.cancellation.cancel();
                task.changed.notify_waiters();
            } else {
                report.already_requested = report.already_requested.saturating_add(1);
            }
        }
        drop(lifecycle_gate);
        for task in tasks {
            wait_for_terminal(&task).await?;
        }
        Ok(report)
    }

    /// 删除一个已经结束任务的内存记录与两个持久输出文件。
    pub async fn remove_finished(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<(), BackgroundTaskError> {
        let task = self.task(task_id)?;
        ensure_task_session(&task, session_id)?;
        let _output_gate = task.tool_output_gate.lock().await;
        if !task.info()?.status.is_terminal() {
            return Err(BackgroundTaskError::new(
                "background_task_still_running",
                format!("后台任务 {task_id} 仍在运行，不能删除输出"),
            ));
        }
        {
            let mut tasks = self.inner.tasks.write().map_err(|_| {
                BackgroundTaskError::new("background_task_state_unavailable", "后台任务表锁已损坏")
            })?;
            tasks.remove(task_id).ok_or_else(|| {
                BackgroundTaskError::new(
                    "background_task_not_found",
                    format!("找不到后台任务 {task_id}"),
                )
            })?;
        }
        let task_directory = task.stdout_path.parent().ok_or_else(|| {
            BackgroundTaskError::new(
                "invalid_background_output_path",
                "后台任务输出路径缺少父目录",
            )
        })?;
        match tokio::fs::remove_dir_all(task_directory).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(BackgroundTaskError::new(
                "background_output_cleanup_failed",
                format!("删除后台任务输出失败：{error}"),
            )),
        }
    }

    /// 启动一个已经过 Shell 工具输入校验的真实后台进程组。
    pub(crate) async fn start_process(
        &self,
        session_id: &str,
        summary: String,
        spec: ProcessSpec,
    ) -> Result<BackgroundTaskInfo, ToolError> {
        let _lifecycle_gate = self.inner.lifecycle_gate.lock().await;
        if !self.is_accepting_tasks() {
            return Err(ToolError::permanent(
                "background_manager_shut_down",
                "后台任务 Manager 已关闭，不能启动新任务",
            ));
        }
        let (task_id, task_directory) = self.create_task_directory().await?;
        let stdout_path = task_directory.join("stdout.log");
        let stderr_path = task_directory.join("stderr.log");
        let stdout_file = match create_output_file(&stdout_path).await {
            Ok(file) => file,
            Err(error) => {
                let _ = tokio::fs::remove_dir_all(&task_directory).await;
                return Err(error);
            }
        };
        let stderr_file = match create_output_file(&stderr_path).await {
            Ok(file) => file,
            Err(error) => {
                drop(stdout_file);
                let _ = tokio::fs::remove_dir_all(&task_directory).await;
                return Err(error);
            }
        };
        let mut last_not_found = None;
        let mut spawned = None;
        for program in &spec.programs {
            match spawn_group(program, &spec) {
                Ok(guard) => {
                    spawned = Some(guard);
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    last_not_found = Some(error);
                }
                Err(error) => {
                    drop(stdout_file);
                    drop(stderr_file);
                    let _ = tokio::fs::remove_dir_all(&task_directory).await;
                    return Err(spawn_error(spec.label, program, error));
                }
            }
        }
        let Some(mut guard) = spawned else {
            drop(stdout_file);
            drop(stderr_file);
            let _ = tokio::fs::remove_dir_all(&task_directory).await;
            return Err(ToolError::permanent(
                "command_not_found",
                format!(
                    "{} 可执行文件不可用：{}",
                    spec.label,
                    last_not_found
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "没有候选可执行文件".to_owned())
                ),
            ));
        };
        let pid = guard.child.inner().id();
        let stdout = match guard.child.inner().stdout.take() {
            Some(stdout) => stdout,
            None => {
                drop(stdout_file);
                drop(stderr_file);
                if terminate_and_wait(&mut guard.child).await.is_ok() {
                    guard.armed = false;
                }
                let _ = tokio::fs::remove_dir_all(&task_directory).await;
                return Err(ToolError::permanent(
                    "stdout_unavailable",
                    "后台命令标准输出管道不可用",
                ));
            }
        };
        let stderr = match guard.child.inner().stderr.take() {
            Some(stderr) => stderr,
            None => {
                drop(stdout);
                drop(stdout_file);
                drop(stderr_file);
                if terminate_and_wait(&mut guard.child).await.is_ok() {
                    guard.armed = false;
                }
                let _ = tokio::fs::remove_dir_all(&task_directory).await;
                return Err(ToolError::permanent(
                    "stderr_unavailable",
                    "后台命令标准错误管道不可用",
                ));
            }
        };
        let cancellation = TurnCancellation::new();
        let task = Arc::new(BackgroundTaskRecord {
            session_id: session_id.to_owned(),
            task_id: task_id.clone(),
            summary,
            started_at_unix_ms: unix_milliseconds(),
            started: StdInstant::now(),
            stdout_path,
            stderr_path,
            cancellation: cancellation.clone(),
            changed: Notify::new(),
            tool_output_gate: AsyncMutex::new(()),
            state: Mutex::new(BackgroundTaskState {
                status: BackgroundTaskStatus::Running,
                pid,
                exit_code: None,
                terminal_duration_ms: None,
                stdout_bytes: 0,
                stderr_bytes: 0,
                stop_requested: false,
                tool_cursor: BackgroundOutputCursor::default(),
            }),
        });
        let task_id_collision = {
            let mut tasks = self.inner.tasks.write().map_err(|_| {
                ToolError::permanent("background_task_state_unavailable", "后台任务表锁已损坏")
            })?;
            match tasks.entry(task_id.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(task.clone());
                    false
                }
                Entry::Occupied(_) => true,
            }
        };
        if task_id_collision {
            drop(stdout);
            drop(stderr);
            drop(stdout_file);
            drop(stderr_file);
            if terminate_and_wait(&mut guard.child).await.is_ok() {
                guard.armed = false;
            }
            let _ = tokio::fs::remove_dir_all(&task_directory).await;
            return Err(ToolError::permanent(
                "background_task_id_collision",
                "后台任务标识发生不可恢复冲突",
            ));
        }
        let stdout_task = tokio::spawn(capture_background_stream(
            stdout,
            stdout_file,
            task.clone(),
            OutputStream::Stdout,
        ));
        let stderr_task = tokio::spawn(capture_background_stream(
            stderr,
            stderr_file,
            task.clone(),
            OutputStream::Stderr,
        ));
        let completion_events = self.inner.completion_events.clone();
        let timeout = spec.timeout;
        let label = spec.label;
        tokio::spawn(async move {
            supervise_background_task(
                task,
                guard,
                cancellation,
                timeout,
                label,
                stdout_task,
                stderr_task,
                completion_events,
            )
            .await;
        });
        self.task(&task_id)
            .and_then(|task| task.info())
            .map_err(BackgroundTaskError::into_tool_error)
    }

    /// 为模型工具读取并推进单任务的共享隐式游标。
    async fn read_next_for_tool(
        &self,
        session_id: &str,
        task_id: &str,
        block_for: Option<Duration>,
        cancellation: &TurnCancellation,
    ) -> Result<BackgroundTaskOutput, BackgroundTaskError> {
        let task = self.task(task_id)?;
        ensure_task_session(&task, session_id)?;
        let _gate = task.tool_output_gate.lock().await;
        let cursor = lock_task_state(&task)?.tool_cursor;
        wait_until_readable(&task, cursor, block_for, Some(cancellation)).await?;
        let output =
            read_persisted_output(&task, cursor, self.inner.max_output_chunk_bytes).await?;
        lock_task_state(&task)?.tool_cursor = output.next_cursor;
        Ok(output)
    }

    /// 返回按任务标识排序的记录 Arc 快照，不在后续阻塞期间持有表锁。
    fn tasks_snapshot(&self) -> Result<Vec<Arc<BackgroundTaskRecord>>, BackgroundTaskError> {
        self.inner
            .tasks
            .read()
            .map(|tasks| tasks.values().cloned().collect())
            .map_err(|_| {
                BackgroundTaskError::new("background_task_state_unavailable", "后台任务表锁已损坏")
            })
    }

    /// 查找一个精确任务，不把不存在包装成取消或读取成功。
    fn task(&self, task_id: &str) -> Result<Arc<BackgroundTaskRecord>, BackgroundTaskError> {
        if task_id.trim().is_empty() || task_id.len() > MAX_BACKGROUND_TASK_ID_BYTES {
            return Err(BackgroundTaskError::new(
                "invalid_background_task_id",
                "后台任务标识必须非空且不超过 256 字节",
            ));
        }
        self.inner
            .tasks
            .read()
            .map_err(|_| {
                BackgroundTaskError::new("background_task_state_unavailable", "后台任务表锁已损坏")
            })?
            .get(task_id)
            .cloned()
            .ok_or_else(|| {
                BackgroundTaskError::new(
                    "background_task_not_found",
                    format!("找不到后台任务 {task_id}"),
                )
            })
    }

    /// 构造全部或仅运行中任务的状态快照。
    fn snapshot_tasks(
        &self,
        running_only: bool,
    ) -> Result<Vec<BackgroundTaskInfo>, BackgroundTaskError> {
        self.tasks_snapshot()?
            .into_iter()
            .filter_map(|task| match task.info() {
                Ok(info) if !running_only || info.status == BackgroundTaskStatus::Running => {
                    Some(Ok(info))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    /// 在 Manager 根目录下原子创建一个不会覆盖既有日志的任务子目录。
    async fn create_task_directory(&self) -> Result<(String, PathBuf), ToolError> {
        tokio::fs::create_dir_all(&self.inner.output_directory)
            .await
            .map_err(|error| {
                ToolError::permanent(
                    "background_output_directory_failed",
                    format!("创建后台任务输出目录失败：{error}"),
                )
            })?;
        for _ in 0..128 {
            let task_id = allocate_task_id();
            let path = self.inner.output_directory.join(&task_id);
            match tokio::fs::create_dir(&path).await {
                Ok(()) => return Ok((task_id, path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(ToolError::permanent(
                        "background_output_directory_failed",
                        format!("创建后台任务独占输出目录失败：{error}"),
                    ));
                }
            }
        }
        Err(ToolError::permanent(
            "background_task_id_exhausted",
            "连续生成的后台任务标识均发生冲突",
        ))
    }
}

/// 确保 Session 级工具不能读取、停止或清理其他 Session 的任务。
fn ensure_task_session(
    task: &BackgroundTaskRecord,
    session_id: &str,
) -> Result<(), BackgroundTaskError> {
    if task.session_id != session_id {
        return Err(BackgroundTaskError::new(
            "background_task_session_mismatch",
            "后台任务不属于当前 Session",
        ));
    }
    Ok(())
}

impl BackgroundTaskRecord {
    /// 基于受锁状态生成持续时间不会倒退的只读快照。
    fn info(&self) -> Result<BackgroundTaskInfo, BackgroundTaskError> {
        let state = lock_task_state(self)?;
        let duration_ms = state
            .terminal_duration_ms
            .unwrap_or_else(|| duration_milliseconds(self.started.elapsed()));
        Ok(BackgroundTaskInfo {
            session_id: self.session_id.clone(),
            task_id: self.task_id.clone(),
            summary: self.summary.clone(),
            started_at_unix_ms: self.started_at_unix_ms,
            duration_ms,
            status: state.status,
            pid: state.pid,
            exit_code: state.exit_code,
            stdout_bytes: state.stdout_bytes,
            stderr_bytes: state.stderr_bytes,
            stop_requested: state.stop_requested,
        })
    }
}

/// 供模型显式读取后台 Shell 任务增量输出的工具。
pub struct TaskOutputTool {
    /// 跨 Turn 注入的 Session 级后台任务 Manager。
    manager: Arc<BackgroundTaskManager>,
}

impl TaskOutputTool {
    /// 创建绑定到指定后台任务 Manager 的输出工具。
    pub fn new(manager: Arc<BackgroundTaskManager>) -> Self {
        Self { manager }
    }
}

impl AgentTool for TaskOutputTool {
    /// 返回任务标识、阻塞选择和有界等待时间的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskOutput",
            "读取一个真实后台 Shell 任务自上次调用后的持久 stdout/stderr 增量，并同时返回当前状态。默认阻塞到出现新输出、任务结束或等待到期。",
            json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_BACKGROUND_TASK_ID_BYTES
                    },
                    "block": { "type": "boolean" },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TASK_OUTPUT_WAIT.as_millis()
                    }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
        )
    }

    /// 读取已经持久化的任务输出不会改变外部系统状态。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_task_output_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 不同任务的输出读取可与其他只读工具并行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 读取并原子推进此任务的模型消费游标。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let input = parse_task_output_input(&input)?;
            let block_for = if input.block {
                Some(Duration::from_millis(
                    input
                        .timeout_ms
                        .unwrap_or(duration_milliseconds(DEFAULT_TASK_OUTPUT_WAIT)),
                ))
            } else {
                None
            };
            let output = self
                .manager
                .read_next_for_tool(
                    context.session_id.as_str(),
                    &input.task_id,
                    block_for,
                    &context.cancellation,
                )
                .await
                .map_err(BackgroundTaskError::into_tool_error)?;
            Ok(ToolOutput::text(render_task_output(&output)))
        })
    }
}

/// 供模型显式停止一个后台 Shell 进程树的工具。
pub struct TaskStopTool {
    /// 跨 Turn 注入的 Session 级后台任务 Manager。
    manager: Arc<BackgroundTaskManager>,
}

impl TaskStopTool {
    /// 创建绑定到指定后台任务 Manager 的停止工具。
    pub fn new(manager: Arc<BackgroundTaskManager>) -> Self {
        Self { manager }
    }
}

impl AgentTool for TaskStopTool {
    /// 返回只接受精确任务标识的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "TaskStop",
            "停止一个仍在运行的后台 Shell 任务及其完整进程树。不存在、已结束或已经收到停止请求的任务都会明确失败。",
            json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_BACKGROUND_TASK_ID_BYTES
                    }
                },
                "required": ["task_id"],
                "additionalProperties": false
            }),
        )
    }

    /// 停止进程树会改变外部系统状态。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_task_stop_input(input)?;
        Ok(ToolEffect::ChangesState)
    }

    /// 进程停止必须形成顺序副作用屏障。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 仅在真实首次发出停止信号后返回成功。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            let input = parse_task_stop_input(&input)?;
            let task = self
                .manager
                .cancel(context.session_id.as_str(), &input.task_id)
                .map_err(BackgroundTaskError::into_tool_error)?;
            Ok(ToolOutput::text(format!(
                "已向后台任务 {} 的完整进程树发出停止信号；当前状态：{}",
                task.task_id,
                task.status.as_str()
            )))
        })
    }
}

/// `TaskOutput` 的严格输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskOutputInput {
    /// 要读取的稳定任务标识。
    task_id: String,
    /// 是否等待新增输出或终态；缺失时默认为 true。
    #[serde(default = "default_true")]
    block: bool,
    /// 阻塞等待的可选毫秒上限。
    timeout_ms: Option<u64>,
}

/// `TaskStop` 的严格输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskStopInput {
    /// 要停止的稳定任务标识。
    task_id: String,
}

/// 后台输出所属的持久文件。
#[derive(Clone, Copy)]
enum OutputStream {
    /// 标准输出文件。
    Stdout,
    /// 标准错误文件。
    Stderr,
}

/// 创建并截断一个只属于新任务的输出文件。
async fn create_output_file(path: &Path) -> Result<File, ToolError> {
    File::create(path).await.map_err(|error| {
        ToolError::permanent(
            "background_output_create_failed",
            format!("创建后台任务输出文件失败：{error}"),
        )
    })
}

/// 持续排空一个进程管道，在每次完整落盘后发布增量通知。
async fn capture_background_stream<R>(
    mut reader: R,
    mut file: File,
    task: Arc<BackgroundTaskRecord>,
    stream: OutputStream,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
{
    let mut total = 0_u64;
    let mut chunk = vec![0_u8; 16 * 1024];
    let mut utf8_tail = Vec::with_capacity(MAX_INCOMPLETE_UTF8_SUFFIX_BYTES);
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        file.write_all(&chunk[..read]).await?;
        total = total.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        retain_utf8_tail(&mut utf8_tail, &chunk[..read]);
        let pending = incomplete_utf8_suffix_len(&utf8_tail);
        let published = total.saturating_sub(u64::try_from(pending).unwrap_or(u64::MAX));
        if publish_stream_bytes(&task, stream, published)? {
            task.changed.notify_waiters();
        }
    }
    file.flush().await?;
    if publish_stream_bytes(&task, stream, total)? {
        task.changed.notify_waiters();
    }
    Ok(total)
}

/// 只保留完整字节流末尾可能构成未完成 UTF-8 标量所需的三个字节。
fn retain_utf8_tail(tail: &mut Vec<u8>, appended: &[u8]) {
    if appended.len() >= MAX_INCOMPLETE_UTF8_SUFFIX_BYTES {
        tail.clear();
        tail.extend_from_slice(&appended[appended.len() - MAX_INCOMPLETE_UTF8_SUFFIX_BYTES..]);
        return;
    }
    let retained = MAX_INCOMPLETE_UTF8_SUFFIX_BYTES.saturating_sub(appended.len());
    if tail.len() > retained {
        tail.drain(..tail.len() - retained);
    }
    tail.extend_from_slice(appended);
}

/// 返回字节尾部仍可能补全为有效 UTF-8 标量的长度，完整或确定无效的尾部返回零。
fn incomplete_utf8_suffix_len(bytes: &[u8]) -> usize {
    let first_candidate = bytes.len().saturating_sub(MAX_INCOMPLETE_UTF8_SUFFIX_BYTES);
    (first_candidate..bytes.len())
        .find_map(|start| {
            let suffix = &bytes[start..];
            std::str::from_utf8(suffix).err().and_then(|error| {
                (error.valid_up_to() == 0 && error.error_len().is_none()).then_some(suffix.len())
            })
        })
        .unwrap_or(0)
}

/// 单调推进指定输出流的公开字节边界，并报告是否需要唤醒读取者。
fn publish_stream_bytes(
    task: &BackgroundTaskRecord,
    stream: OutputStream,
    published: u64,
) -> io::Result<bool> {
    let mut state = lock_task_state_io(task)?;
    let current = match stream {
        OutputStream::Stdout => &mut state.stdout_bytes,
        OutputStream::Stderr => &mut state.stderr_bytes,
    };
    if published <= *current {
        return Ok(false);
    }
    *current = published;
    Ok(true)
}

/// 监督后台进程终态并在输出任务结束后广播一次完成事件。
#[allow(clippy::too_many_arguments)]
async fn supervise_background_task(
    task: Arc<BackgroundTaskRecord>,
    mut guard: ProcessGroupGuard,
    cancellation: TurnCancellation,
    timeout: Duration,
    label: &'static str,
    stdout_task: JoinHandle<io::Result<u64>>,
    stderr_task: JoinHandle<io::Result<u64>>,
    completion_events: broadcast::Sender<BackgroundTaskCompletion>,
) {
    let deadline = Instant::now() + timeout;
    let termination = monitor_process(&mut guard, &cancellation, deadline).await;
    if termination.is_err() && terminate_and_wait(&mut guard.child).await.is_ok() {
        guard.armed = false;
    }
    let stdout_result = join_capture(stdout_task, "stdout").await;
    let stderr_result = join_capture(stderr_task, "stderr").await;
    let (status, exit_code, summary) = completion_outcome(
        label,
        termination.as_ref(),
        stdout_result.as_ref(),
        stderr_result.as_ref(),
    );
    let duration_ms = duration_milliseconds(task.started.elapsed());
    {
        let mut state = match lock_task_state(&task) {
            Ok(state) => state,
            Err(_) => return,
        };
        state.status = status;
        state.exit_code = exit_code;
        state.terminal_duration_ms = Some(duration_ms);
    }
    task.changed.notify_waiters();
    let _ = completion_events.send(BackgroundTaskCompletion {
        session_id: task.session_id.clone(),
        task_id: task.task_id.clone(),
        status,
        duration_ms,
        summary,
    });
}

/// 把一个输出捕获任务的 Join 和 IO 错误合并为安全文本。
async fn join_capture(task: JoinHandle<io::Result<u64>>, label: &str) -> Result<u64, String> {
    match task.await {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(format!("{label} 输出持久化失败：{error}")),
        Err(error) => Err(format!("{label} 输出捕获任务异常结束：{error}")),
    }
}

/// 根据进程、stdout 和 stderr 三个真实结果选择唯一终态与安全摘要。
fn completion_outcome(
    label: &str,
    termination: Result<&ProcessTermination, &ToolError>,
    stdout: Result<&u64, &String>,
    stderr: Result<&u64, &String>,
) -> (BackgroundTaskStatus, Option<i32>, String) {
    if stdout.is_err() || stderr.is_err() {
        return (
            BackgroundTaskStatus::Failed,
            None,
            format!("{label} 后台任务输出持久化失败"),
        );
    }
    match termination {
        Ok(ProcessTermination::Exited(exit)) if exit.success() => (
            BackgroundTaskStatus::Succeeded,
            exit.code(),
            format!("{label} 后台任务成功完成"),
        ),
        Ok(ProcessTermination::Exited(exit)) => (
            BackgroundTaskStatus::Failed,
            exit.code(),
            match exit.code() {
                Some(code) => format!("{label} 后台任务以退出码 {code} 结束"),
                None => format!("{label} 后台任务被系统信号终止"),
            },
        ),
        Ok(ProcessTermination::TimedOut) => (
            BackgroundTaskStatus::Failed,
            None,
            format!("{label} 后台任务执行超时"),
        ),
        Ok(ProcessTermination::Cancelled) => (
            BackgroundTaskStatus::Cancelled,
            None,
            format!("{label} 后台任务已取消并回收进程树"),
        ),
        Err(_) => (
            BackgroundTaskStatus::Failed,
            None,
            format!("{label} 后台任务进程监督失败"),
        ),
    }
}

/// 等待任务出现未读字节、进入终态、到达等待期限或当前 Turn 被取消。
async fn wait_until_readable(
    task: &BackgroundTaskRecord,
    cursor: BackgroundOutputCursor,
    block_for: Option<Duration>,
    cancellation: Option<&TurnCancellation>,
) -> Result<(), BackgroundTaskError> {
    let Some(wait) = block_for else {
        validate_cursor(task, cursor)?;
        return Ok(());
    };
    let deadline = Instant::now() + wait.min(MAX_TASK_OUTPUT_WAIT);
    loop {
        let changed = task.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let readable = {
            let state = lock_task_state(task)?;
            validate_cursor_against_state(cursor, &state)?;
            state.stdout_bytes > cursor.stdout_offset
                || state.stderr_bytes > cursor.stderr_offset
                || state.status.is_terminal()
        };
        if readable {
            return Ok(());
        }
        if let Some(cancellation) = cancellation {
            tokio::select! {
                _ = changed.as_mut() => {}
                _ = tokio::time::sleep_until(deadline) => return Ok(()),
                _ = cancellation.cancelled() => {
                    return Err(BackgroundTaskError::new(
                        "cancelled",
                        "后台任务输出等待已随当前 Turn 取消",
                    ));
                }
            }
        } else {
            tokio::select! {
                _ = changed.as_mut() => {}
                _ = tokio::time::sleep_until(deadline) => return Ok(()),
            }
        }
    }
}

/// 等待一个任务提交不可逆终态。
async fn wait_for_terminal(task: &BackgroundTaskRecord) -> Result<(), BackgroundTaskError> {
    loop {
        let changed = task.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if lock_task_state(task)?.status.is_terminal() {
            return Ok(());
        }
        changed.as_mut().await;
    }
}

/// 从两个持久文件按总字节预算读取增量并返回下一游标。
async fn read_persisted_output(
    task: &BackgroundTaskRecord,
    cursor: BackgroundOutputCursor,
    max_bytes: usize,
) -> Result<BackgroundTaskOutput, BackgroundTaskError> {
    let info = task.info()?;
    validate_cursor_against_info(cursor, &info)?;
    let stdout_available = info.stdout_bytes.saturating_sub(cursor.stdout_offset);
    let stderr_available = info.stderr_bytes.saturating_sub(cursor.stderr_offset);
    let (stdout_limit, stderr_limit) =
        split_output_budget(max_bytes, stdout_available > 0, stderr_available > 0);
    let stdout_target = bounded_read_len(stdout_available, stdout_limit);
    let stderr_target = bounded_read_len(stderr_available, stderr_limit);
    let mut stdout =
        read_file_range(&task.stdout_path, cursor.stdout_offset, stdout_target).await?;
    let mut stderr =
        read_file_range(&task.stderr_path, cursor.stderr_offset, stderr_target).await?;
    truncate_incomplete_utf8_suffix(
        &mut stdout,
        u64::try_from(stdout_target).unwrap_or(u64::MAX) < stdout_available,
    );
    truncate_incomplete_utf8_suffix(
        &mut stderr,
        u64::try_from(stderr_target).unwrap_or(u64::MAX) < stderr_available,
    );
    let stdout_read = u64::try_from(stdout.len()).unwrap_or(u64::MAX);
    let stderr_read = u64::try_from(stderr.len()).unwrap_or(u64::MAX);
    let next_cursor = BackgroundOutputCursor {
        stdout_offset: cursor.stdout_offset.saturating_add(stdout_read),
        stderr_offset: cursor.stderr_offset.saturating_add(stderr_read),
    };
    Ok(BackgroundTaskOutput {
        task: info.clone(),
        next_cursor,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_has_more: next_cursor.stdout_offset < info.stdout_bytes,
        stderr_has_more: next_cursor.stderr_offset < info.stderr_bytes,
    })
}

/// 把流的可用字节数收紧到本次分配的内存与响应预算。
fn bounded_read_len(available: u64, limit: usize) -> usize {
    let bounded = available.min(u64::try_from(limit).unwrap_or(u64::MAX));
    usize::try_from(bounded).unwrap_or(limit)
}

/// 若文件中还有后续字节，则暂不消费片段末尾可能补全为有效 UTF-8 的部分。
fn truncate_incomplete_utf8_suffix(bytes: &mut Vec<u8>, has_more: bool) {
    if !has_more || bytes.is_empty() {
        return;
    }
    let pending = incomplete_utf8_suffix_len(bytes);
    let safe_len = bytes.len().saturating_sub(pending);
    if safe_len == 0 {
        // 构造器保证每个活跃流至少获得四字节；此分支仅在文件状态损坏时保底推进。
        return;
    }
    bytes.truncate(safe_len);
}

/// 在两个活跃流之间平均分配预算；只有一个流有数据时使用全部预算。
fn split_output_budget(max_bytes: usize, stdout_ready: bool, stderr_ready: bool) -> (usize, usize) {
    match (stdout_ready, stderr_ready) {
        (true, true) => {
            let stdout = max_bytes / 2;
            (stdout, max_bytes.saturating_sub(stdout))
        }
        (true, false) => (max_bytes, 0),
        (false, true) => (0, max_bytes),
        (false, false) => (0, 0),
    }
}

/// 从一个持久输出文件的精确字节偏移读取有界片段。
async fn read_file_range(
    path: &Path,
    offset: u64,
    limit: usize,
) -> Result<Vec<u8>, BackgroundTaskError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).await.map_err(|error| {
        BackgroundTaskError::new(
            "background_output_open_failed",
            format!("打开后台任务持久输出失败：{error}"),
        )
    })?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(|error| {
            BackgroundTaskError::new(
                "background_output_seek_failed",
                format!("定位后台任务持久输出失败：{error}"),
            )
        })?;
    let mut bytes = vec![0_u8; limit];
    file.read_exact(&mut bytes).await.map_err(|error| {
        BackgroundTaskError::new(
            "background_output_read_failed",
            format!("读取后台任务持久输出失败：{error}"),
        )
    })?;
    Ok(bytes)
}

/// 校验显式游标没有越过当前已经落盘的两个文件结尾。
fn validate_cursor(
    task: &BackgroundTaskRecord,
    cursor: BackgroundOutputCursor,
) -> Result<(), BackgroundTaskError> {
    let state = lock_task_state(task)?;
    validate_cursor_against_state(cursor, &state)
}

/// 针对受锁状态校验两个输出偏移。
fn validate_cursor_against_state(
    cursor: BackgroundOutputCursor,
    state: &BackgroundTaskState,
) -> Result<(), BackgroundTaskError> {
    if cursor.stdout_offset > state.stdout_bytes || cursor.stderr_offset > state.stderr_bytes {
        return Err(BackgroundTaskError::new(
            "background_output_cursor_out_of_range",
            "后台任务输出游标超过已经持久化的字节范围",
        ));
    }
    Ok(())
}

/// 针对公开任务快照校验两个输出偏移。
fn validate_cursor_against_info(
    cursor: BackgroundOutputCursor,
    info: &BackgroundTaskInfo,
) -> Result<(), BackgroundTaskError> {
    if cursor.stdout_offset > info.stdout_bytes || cursor.stderr_offset > info.stderr_bytes {
        return Err(BackgroundTaskError::new(
            "background_output_cursor_out_of_range",
            "后台任务输出游标超过已经持久化的字节范围",
        ));
    }
    Ok(())
}

/// 获取任务状态锁并把锁损坏转为稳定错误。
fn lock_task_state(
    task: &BackgroundTaskRecord,
) -> Result<std::sync::MutexGuard<'_, BackgroundTaskState>, BackgroundTaskError> {
    task.state.lock().map_err(|_| {
        BackgroundTaskError::new("background_task_state_unavailable", "后台任务状态锁已损坏")
    })
}

/// 在输出捕获任务中把锁损坏转为 IO 错误，使监督器提交失败终态。
fn lock_task_state_io(
    task: &BackgroundTaskRecord,
) -> io::Result<std::sync::MutexGuard<'_, BackgroundTaskState>> {
    task.state
        .lock()
        .map_err(|_| io::Error::other("后台任务状态锁已损坏"))
}

/// 解析并校验 `TaskOutput` 输入组合。
fn parse_task_output_input(input: &Value) -> Result<TaskOutputInput, ToolError> {
    let input: TaskOutputInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.task_id.trim().is_empty() || input.task_id.len() > MAX_BACKGROUND_TASK_ID_BYTES {
        return Err(ToolError::permanent(
            "invalid_background_task_id",
            "后台任务标识必须非空且不超过 256 字节",
        ));
    }
    if input.timeout_ms.is_some_and(|timeout| {
        timeout == 0 || u128::from(timeout) > MAX_TASK_OUTPUT_WAIT.as_millis() || !input.block
    }) {
        return Err(ToolError::permanent(
            "invalid_task_output_timeout",
            "timeout_ms 仅能在 block=true 时使用，且必须在 1 到 600000 之间",
        ));
    }
    Ok(input)
}

/// 解析并校验 `TaskStop` 输入。
fn parse_task_stop_input(input: &Value) -> Result<TaskStopInput, ToolError> {
    let input: TaskStopInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.task_id.trim().is_empty() || input.task_id.len() > MAX_BACKGROUND_TASK_ID_BYTES {
        return Err(ToolError::permanent(
            "invalid_background_task_id",
            "后台任务标识必须非空且不超过 256 字节",
        ));
    }
    Ok(input)
}

/// 返回 Serde 用于 `TaskOutput.block` 的默认真值。
const fn default_true() -> bool {
    true
}

/// 把一次增量输出渲染为始终非空且含状态、游标和两个流的模型文本。
fn render_task_output(output: &BackgroundTaskOutput) -> String {
    let mut text = format!(
        "后台任务：{}\n状态：{}\n持续时间：{} 毫秒\n已读游标：stdout={}，stderr={}",
        output.task.task_id,
        output.task.status.as_str(),
        output.task.duration_ms,
        output.next_cursor.stdout_offset,
        output.next_cursor.stderr_offset
    );
    append_output_stream(&mut text, "stdout", &output.stdout, output.stdout_has_more);
    append_output_stream(&mut text, "stderr", &output.stderr, output.stderr_has_more);
    text
}

/// 向工具结果追加一个明确为空或仍有后续数据的输出流片段。
fn append_output_stream(text: &mut String, label: &str, content: &str, has_more: bool) {
    text.push_str(&format!("\n{label}："));
    if content.is_empty() {
        text.push_str("<本次无新增输出>");
    } else {
        text.push('\n');
        text.push_str(content);
    }
    if has_more {
        text.push_str("\n<仍有未读取输出，请继续调用 TaskOutput>");
    }
}

/// 生成只含进程、时间和单调计数的安全任务标识。
fn allocate_task_id() -> String {
    let counter = NEXT_BACKGROUND_TASK_ID.fetch_add(1, Ordering::Relaxed);
    format!(
        "shell-{}-{}-{counter}",
        std::process::id(),
        unix_milliseconds()
    )
}

/// 返回当前系统时间的非负 Unix 毫秒数。
fn unix_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(duration_milliseconds)
        .unwrap_or(0)
}

/// 把持续时间饱和转换成 u64 毫秒。
fn duration_milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
