//! 短生命周期 Tokio 外部进程的统一生命周期管理。

use std::{
    fmt,
    future::Future,
    io,
    process::{ExitStatus, Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use peri_agent::agent::async_tasks::ProcessTreeGuard;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
    time::Instant,
};

/// 根进程退出后允许后代关闭继承管道的最长时间。
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_secs(1);
/// 终止进程树后等待根进程句柄完成回收的最长时间。
const PROCESS_REAP_GRACE: Duration = Duration::from_millis(500);
/// 每个输出管道最多保留的字节数；超出部分仍会排空但不会进入内存缓冲。
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// 短生命周期外部进程执行失败的分类。
#[derive(Debug)]
pub enum ProcessLifecycleError {
    /// 命令未在指定的总体超时时间内完成。
    Timeout,
    /// 启动、输入、等待或输出读取过程中发生 I/O 错误。
    Io(io::Error),
}

impl fmt::Display for ProcessLifecycleError {
    /// 以稳定的简短文本展示生命周期失败原因，供跨 crate 调用方记录日志。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => formatter.write_str("外部命令执行超时"),
            Self::Io(error) => write!(formatter, "外部命令 I/O 失败: {error}"),
        }
    }
}

impl std::error::Error for ProcessLifecycleError {
    /// 暴露底层 I/O 错误，便于调用方保留原始诊断信息。
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Timeout => None,
            Self::Io(error) => Some(error),
        }
    }
}

/// 一个输出管道的有界共享缓冲区。
#[derive(Default)]
struct CapturedOutput {
    /// 保留给调用方读取的输出前缀。
    bytes: Vec<u8>,
    /// 是否有内容因达到上限而被丢弃。
    truncated: bool,
}

/// 一个输出管道的共享缓冲区。
type OutputBuffer = Arc<Mutex<CapturedOutput>>;

/// 短生命周期命令的有界 stdout/stderr 捕获句柄。
///
/// 调用方可以在命令仍运行时读取当前快照，也可以在总体超时返回后取得已经排空的
/// 部分输出。两个流分别受 [`MAX_CAPTURE_BYTES`] 限制，读取任务会继续消费超出上限
/// 的字节，避免子进程因管道写满而阻塞。
#[derive(Default)]
pub struct OutputCapture {
    /// stdout 的有界共享缓冲区。
    stdout: OutputBuffer,
    /// stderr 的有界共享缓冲区。
    stderr: OutputBuffer,
    /// 已启动命令的根进程 PID，供超时诊断读取状态快照。
    pid: Mutex<Option<u32>>,
}

impl OutputCapture {
    /// 创建一个空的 stdout/stderr 捕获句柄。
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 返回当前已经捕获的 stdout 与 stderr 字节前缀。
    pub fn snapshot(&self) -> (Vec<u8>, Vec<u8>) {
        (clone_output(&self.stdout), clone_output(&self.stderr))
    }

    /// 返回当前已经捕获的 stdout 与 stderr 文本快照。
    pub fn snapshot_lossy(&self) -> (String, String) {
        let (stdout, stderr) = self.snapshot();
        (
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
        )
    }

    /// 返回当前 stdout/stderr 文本快照及是否有字节因上限而被丢弃。
    pub fn snapshot_lossy_with_truncation(&self) -> (String, String, bool) {
        let (stdout, stderr) = self.snapshot();
        let truncated = output_was_truncated(&self.stdout) || output_was_truncated(&self.stderr);
        (
            String::from_utf8_lossy(&stdout).into_owned(),
            String::from_utf8_lossy(&stderr).into_owned(),
            truncated,
        )
    }

    /// 返回已启动命令的根进程 PID；命令尚未 spawn 时为 `None`。
    pub fn pid(&self) -> Option<u32> {
        match self.pid.lock() {
            Ok(value) => *value,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    /// 记录已启动命令的根进程 PID，供超时分支生成诊断信息。
    fn set_pid(&self, pid: u32) {
        match self.pid.lock() {
            Ok(mut value) => *value = Some(pid),
            Err(poisoned) => *poisoned.into_inner() = Some(pid),
        }
    }
}

/// stdout/stderr 的异步排空任务集合。
///
/// 句柄必须由当前执行 future 持有；这样 future 被取消时，`Drop` 可以主动
/// abort 任务，不把仍持有管道句柄的读取任务遗留在 Tokio runtime 中。
struct DrainTasks {
    /// stdout 排空任务；`None` 表示任务已经被主循环消费。
    stdout: Option<JoinHandle<io::Result<()>>>,
    /// stderr 排空任务；`None` 表示任务已经被主循环消费。
    stderr: Option<JoinHandle<io::Result<()>>>,
}

impl DrainTasks {
    /// 终止尚未完成的输出排空任务，并清空其句柄。
    fn abort(&mut self) {
        if let Some(task) = self.stdout.take() {
            task.abort();
        }
        if let Some(task) = self.stderr.take() {
            task.abort();
        }
    }

    /// 终止并异步回收尚未完成的输出排空任务。
    async fn abort_and_join(&mut self) {
        let stdout = self.stdout.take();
        let stderr = self.stderr.take();
        for task in [stdout, stderr].into_iter().flatten() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for DrainTasks {
    /// future 被取消时立即 abort 读取任务，避免它们脱离进程生命周期继续持有管道。
    fn drop(&mut self) {
        self.abort();
    }
}

/// 读取单个输出管道并追加到共享缓冲区。
async fn drain_pipe<R>(mut reader: R, output: OutputBuffer) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0_u8; 8192];
    loop {
        let bytes_read = reader.read(&mut chunk).await?;
        if bytes_read == 0 {
            return Ok(());
        }
        let mut output = match output.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(output.bytes.len());
        if bytes_read > remaining {
            output.truncated = true;
        }
        if remaining > 0 {
            output
                .bytes
                .extend_from_slice(&chunk[..bytes_read.min(remaining)]);
        }
    }
}

/// 将输出排空任务的 JoinError 转换为统一的 I/O 错误。
fn flatten_drain_result(result: Result<io::Result<()>, tokio::task::JoinError>) -> io::Result<()> {
    match result {
        Ok(result) => result,
        Err(error) => Err(io::Error::other(format!("输出排空任务失败: {error}"))),
    }
}

/// 等待两个输出排空任务完成，并传播读取或任务错误。
async fn await_drains(drains: &mut DrainTasks) -> io::Result<()> {
    loop {
        if drains.stdout.is_none() && drains.stderr.is_none() {
            return Ok(());
        }

        tokio::select! {
            stdout_result = async {
                drains
                    .stdout
                    .as_mut()
                    .expect("stdout 排空任务存在时才启用对应分支")
                    .await
            }, if drains.stdout.is_some() => {
                drains.stdout = None;
                flatten_drain_result(stdout_result)?;
            }
            stderr_result = async {
                drains
                    .stderr
                    .as_mut()
                    .expect("stderr 排空任务存在时才启用对应分支")
                    .await
            }, if drains.stderr.is_some() => {
                drains.stderr = None;
                flatten_drain_result(stderr_result)?;
            }
        }
    }
}

/// 根进程存活期间等待其退出，同时及时发现 stdout/stderr 排空错误。
async fn wait_for_process(
    child: &mut tokio::process::Child,
    drains: &mut DrainTasks,
) -> io::Result<ExitStatus> {
    loop {
        tokio::select! {
            status = child.wait() => return status,
            stdout_result = async {
                drains
                    .stdout
                    .as_mut()
                    .expect("stdout 排空任务存在时才启用对应分支")
                    .await
            }, if drains.stdout.is_some() => {
                drains.stdout = None;
                flatten_drain_result(stdout_result)?;
            }
            stderr_result = async {
                drains
                    .stderr
                    .as_mut()
                    .expect("stderr 排空任务存在时才启用对应分支")
                    .await
            }, if drains.stderr.is_some() => {
                drains.stderr = None;
                flatten_drain_result(stderr_result)?;
            }
        }
    }
}

/// 终止整个进程树、回收根进程，并终止输出排空任务。
async fn terminate_process_tree(
    process_tree_guard: &mut ProcessTreeGuard,
    child: &mut tokio::process::Child,
    drains: &mut DrainTasks,
) {
    process_tree_guard.terminate();
    // guard 负责整树终止；start_kill 是根进程的最后一道兜底，随后 wait 负责回收句柄。
    let _ = child.start_kill();
    let _ = tokio::time::timeout(PROCESS_REAP_GRACE, child.wait()).await;
    drains.abort_and_join().await;
}

/// 复制输出缓冲区中的当前内容。
fn clone_output(output: &OutputBuffer) -> Vec<u8> {
    match output.lock() {
        Ok(value) => value.bytes.clone(),
        Err(poisoned) => poisoned.into_inner().bytes.clone(),
    }
}

/// 返回输出是否触发了有界捕获上限。
fn output_was_truncated(output: &OutputBuffer) -> bool {
    match output.lock() {
        Ok(value) => value.truncated,
        Err(poisoned) => poisoned.into_inner().truncated,
    }
}

/// 在有限窗口内执行超时诊断钩子；钩子失败或超时都不改变主命令的超时结果。
async fn run_timeout_hook<F, Fut>(hook: &mut Option<F>, pid: u32)
where
    F: FnOnce(u32) -> Fut,
    Fut: Future<Output = ()>,
{
    if let Some(hook) = hook.take() {
        let _ = tokio::time::timeout(OUTPUT_DRAIN_GRACE, hook(pid)).await;
    }
}

/// 关闭标准输入管道；有输入时写入全部字节后再关闭，避免子进程等待 EOF。
async fn close_stdin(child: &mut tokio::process::Child, stdin: Option<&[u8]>) -> io::Result<()> {
    if let Some(input) = stdin {
        let mut pipe = child.stdin.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "外部命令 stdin 管道不可用")
        })?;
        pipe.write_all(input).await?;
    } else {
        // Stdio::null 通常不会产生可取出的句柄；显式 take 保证生命周期结束时关闭。
        child.stdin.take();
    }
    Ok(())
}

/// 使用已由调用方配置好的 Tokio 命令构造器运行短生命周期外部进程。
///
/// 函数统一负责：可选 stdin 字节写入、总体 timeout、stdout/stderr 并发排空、
/// Unix 独立进程组、Windows `ProcessTreeGuard`、future drop/超时/I/O 错误时的
/// 整树清理、根进程回收，以及根进程退出后的有限输出排空窗口。
///
/// `stdin` 为 `Some` 时写入全部字节后关闭管道；为 `None` 时关闭标准输入，
/// 避免 Git/npm 等非交互命令等待输入。命令构造器应使用 peri-agent 提供的
/// `new_tokio_command` 或 `shell_command` 创建，再在调用方补充参数和环境变量。
pub async fn run_short_lived_command(
    command: tokio::process::Command,
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, ProcessLifecycleError> {
    run_short_lived_command_inner(
        command,
        stdin,
        Some(timeout),
        OutputCapture::new(),
        true,
        |_| async {},
    )
    .await
}

/// 运行短生命周期命令并向调用方暴露有界 stdout/stderr 快照。
///
/// `timeout` 为 `Some` 时覆盖 spawn、stdin 写入、根进程等待和排空窗口；`None`
/// 表示命令本身不设总体超时，但根进程退出后的后代排空仍有固定窗口。调用方可以
/// 在 future 返回超时后读取 `capture.snapshot_lossy()`，生成保留既有格式的部分输出提示。
/// 此入口保留有界输出但不因超限直接报错，由调用方自行决定如何展示截断结果。
pub async fn run_short_lived_command_with_capture(
    command: tokio::process::Command,
    stdin: Option<&[u8]>,
    timeout: Option<Duration>,
    capture: Arc<OutputCapture>,
) -> Result<Output, ProcessLifecycleError> {
    run_short_lived_command_inner(command, stdin, timeout, capture, false, |_| async {}).await
}

/// 运行短生命周期命令，并在总体超时终止进程树前执行一次有界诊断回调。
///
/// 回调接收根进程 PID，最长只占用 [`OUTPUT_DRAIN_GRACE`]；回调完成或超时后，
/// runner 才会终止进程树、回收根进程并返回 [`ProcessLifecycleError::Timeout`]。
/// 这让调用方可以在进程仍存在时取得 `ps` 等诊断快照，同时不会把诊断逻辑变成
/// 无界的清理阻塞。
pub async fn run_short_lived_command_with_capture_and_timeout_hook<F, Fut>(
    command: tokio::process::Command,
    stdin: Option<&[u8]>,
    timeout: Option<Duration>,
    capture: Arc<OutputCapture>,
    on_timeout: F,
) -> Result<Output, ProcessLifecycleError>
where
    F: FnOnce(u32) -> Fut,
    Fut: Future<Output = ()>,
{
    run_short_lived_command_inner(command, stdin, timeout, capture, false, on_timeout).await
}

/// 执行共享的短命令生命周期状态机。
///
/// `reject_output_limit` 供结构化解析调用方拒绝不完整输出；Bash 等展示型调用方
/// 使用 `false`，以保留其既有截断与部分输出展示语义。
async fn run_short_lived_command_inner<F, Fut>(
    mut command: tokio::process::Command,
    stdin: Option<&[u8]>,
    timeout: Option<Duration>,
    capture: Arc<OutputCapture>,
    reject_output_limit: bool,
    on_timeout: F,
) -> Result<Output, ProcessLifecycleError>
where
    F: FnOnce(u32) -> Fut,
    Fut: Future<Output = ()>,
{
    // 总体 deadline 在 spawn 前建立；None 表示显式不设总体超时。
    let deadline = timeout.map(|duration| Instant::now() + duration);

    #[cfg(unix)]
    {
        // Unix 以根进程为组长创建独立进程组，才能按组终止全部后代。
        command.process_group(0);
    }

    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // future drop 时由 Tokio 兜底终止根进程；整树清理由 ProcessTreeGuard 完成。
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(ProcessLifecycleError::Io)?;
    let pid = match child.id() {
        Some(pid) => pid,
        None => {
            // Tokio 正常 spawn 后应始终有 PID；异常时仍尽力终止并回收根进程。
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(ProcessLifecycleError::Io(io::Error::other(
                "外部命令启动后未返回 PID",
            )));
        }
    };
    capture.set_pid(pid);
    let mut on_timeout = Some(on_timeout);

    // 守卫必须在 spawn 后、任何后续 await 前建立，覆盖命令的完整异步生命周期。
    let mut process_tree_guard = ProcessTreeGuard::new(pid, &child);
    let stdout = child.stdout.take().ok_or_else(|| {
        ProcessLifecycleError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "外部命令 stdout 管道不可用",
        ))
    });
    let stderr = child.stderr.take().ok_or_else(|| {
        ProcessLifecycleError::Io(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "外部命令 stderr 管道不可用",
        ))
    });
    let (stdout, stderr) = match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Err(error), _) | (_, Err(error)) => {
            let mut drains = DrainTasks {
                stdout: None,
                stderr: None,
            };
            terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
            return Err(error);
        }
    };

    let mut drains = DrainTasks {
        stdout: Some(tokio::spawn(drain_pipe(stdout, capture.stdout.clone()))),
        stderr: Some(tokio::spawn(drain_pipe(stderr, capture.stderr.clone()))),
    };

    // 写入 stdin 也受总体 deadline 约束，避免不消费输入的 hook 永久占满写管道。
    let write_result = if let Some(deadline) = deadline {
        match tokio::time::timeout_at(deadline, close_stdin(&mut child, stdin)).await {
            Ok(result) => result,
            Err(_) => {
                run_timeout_hook(&mut on_timeout, pid).await;
                terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
                return Err(ProcessLifecycleError::Timeout);
            }
        }
    } else {
        close_stdin(&mut child, stdin).await
    };
    match write_result {
        Ok(()) => {}
        Err(error) => {
            terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
            return Err(ProcessLifecycleError::Io(error));
        }
    }

    // 根进程等待期间保持两个输出管道持续排空，避免大输出填满 OS 缓冲形成死锁。
    let status = match deadline {
        Some(deadline) => {
            match tokio::time::timeout_at(deadline, wait_for_process(&mut child, &mut drains)).await
            {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => {
                    terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
                    return Err(ProcessLifecycleError::Io(error));
                }
                Err(_) => {
                    run_timeout_hook(&mut on_timeout, pid).await;
                    terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
                    return Err(ProcessLifecycleError::Timeout);
                }
            }
        }
        None => match wait_for_process(&mut child, &mut drains).await {
            Ok(status) => status,
            Err(error) => {
                terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
                return Err(ProcessLifecycleError::Io(error));
            }
        },
    };

    // 根进程已退出但后代可能仍继承输出管道；排空窗口同时受总体 deadline 限制。
    let remaining = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
    if remaining.is_some_and(|remaining| remaining.is_zero()) {
        run_timeout_hook(&mut on_timeout, pid).await;
        terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
        return Err(ProcessLifecycleError::Timeout);
    }
    let drain_window = remaining.map_or(OUTPUT_DRAIN_GRACE, |remaining| {
        std::cmp::min(remaining, OUTPUT_DRAIN_GRACE)
    });
    let drain_result = tokio::time::timeout(drain_window, await_drains(&mut drains)).await;
    match drain_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
            return Err(ProcessLifecycleError::Io(error));
        }
        Err(_) if remaining.is_some_and(|remaining| remaining <= OUTPUT_DRAIN_GRACE) => {
            // deadline 先于有限排空窗口到期，按总体 timeout 处理。
            run_timeout_hook(&mut on_timeout, pid).await;
            terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
            return Err(ProcessLifecycleError::Timeout);
        }
        Err(_) => {
            // 根命令已经成功退出；排空窗口耗尽后清理残留后代，并保留已读取的输出。
            tracing::warn!(pid, "外部命令输出排空超时，终止残留进程树");
            terminate_process_tree(&mut process_tree_guard, &mut child, &mut drains).await;
        }
    }

    // 根进程已回收且输出任务已完成或被终止后，才解除 future-drop 清理守卫。
    // 超限内容已在读取线程中持续排空；此处拒绝把不完整输出交给结构化解析器。
    let output_limit = if output_was_truncated(&capture.stdout) {
        Some("stdout")
    } else if output_was_truncated(&capture.stderr) {
        Some("stderr")
    } else {
        None
    };
    process_tree_guard.disarm();
    if reject_output_limit {
        if let Some(stream) = output_limit {
            return Err(ProcessLifecycleError::Io(io::Error::new(
                io::ErrorKind::FileTooLarge,
                format!(
                    "外部命令 {stream} 输出超过 {} MiB 上限",
                    MAX_CAPTURE_BYTES / (1024 * 1024)
                ),
            )));
        }
    }
    Ok(Output {
        status,
        stdout: clone_output(&capture.stdout),
        stderr: clone_output(&capture.stderr),
    })
}

/// 在同步阻塞上下文中运行短生命周期命令，并复用异步 runner 的完整清理语义。
///
/// 调用方必须位于同步或 Tokio 的 blocking 上下文；若当前线程已经进入 Tokio
/// runtime，则改用专用线程，避免在 runtime worker 上嵌套 `block_on`。
pub fn run_short_lived_command_blocking(
    command: std::process::Command,
    timeout: Duration,
) -> Result<Output, ProcessLifecycleError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::spawn(move || run_short_lived_command_in_runtime(command, timeout))
            .join()
            .unwrap_or_else(|_| {
                Err(ProcessLifecycleError::Io(io::Error::other(
                    "外部命令阻塞 runner 线程异常退出",
                )))
            })
    } else {
        run_short_lived_command_in_runtime(command, timeout)
    }
}

/// 为同步适配器创建一个独立的 Tokio runtime 并运行共享异步 runner。
fn run_short_lived_command_in_runtime(
    command: std::process::Command,
    timeout: Duration,
) -> Result<Output, ProcessLifecycleError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(ProcessLifecycleError::Io)?;
    runtime.block_on(run_short_lived_command(
        tokio::process::Command::from(command),
        None,
        timeout,
    ))
}

#[cfg(test)]
mod tests {
    use super::{drain_pipe, output_was_truncated, CapturedOutput, MAX_CAPTURE_BYTES};
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncWriteExt;

    /// 输出超过上限时必须继续排空管道，同时只保留固定大小的前缀。
    #[tokio::test]
    async fn drain_pipe_caps_output_without_blocking_writer() {
        let (mut writer, reader) = tokio::io::duplex(MAX_CAPTURE_BYTES);
        let output = Arc::new(Mutex::new(CapturedOutput::default()));
        let output_for_reader = Arc::clone(&output);
        let reader_task = tokio::spawn(drain_pipe(reader, output_for_reader));
        let input = vec![b'x'; MAX_CAPTURE_BYTES + 1];

        writer
            .write_all(&input)
            .await
            .expect("写入方不应因读取上限而阻塞");
        drop(writer);
        reader_task
            .await
            .expect("输出读取任务应正常结束")
            .expect("输出读取不应失败");

        let captured_len = output.lock().expect("读取输出缓冲").bytes.len();
        assert_eq!(captured_len, MAX_CAPTURE_BYTES);
        assert!(output_was_truncated(&output));
    }
}
