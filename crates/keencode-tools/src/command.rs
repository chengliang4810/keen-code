//! 带超时、取消、输出落盘和跨平台进程树清理的 Shell 与 Git 工具。

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use command_group::{AsyncCommandGroup, AsyncGroupChild};
use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
    TurnCancellation,
};
use keencode_model::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until};

use crate::background::BackgroundTaskManager;
use crate::environment::{ToolEnvironment, display_path, invalid_input};

/// 进程退出状态轮询间隔。
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// 通用有界命令端口允许的最大单流输出上限。
const MAX_BOUNDED_COMMAND_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// 通用有界命令端口允许写入的最大标准输入。
const MAX_BOUNDED_COMMAND_STDIN_BYTES: usize = 8 * 1024 * 1024;

/// 通过系统 Bash 执行一个非交互命令字符串的工具。
pub struct BashTool {
    /// 当前 Session 的工作目录、超时和输出目录。
    environment: Arc<ToolEnvironment>,
    /// 可选的跨 Turn 后台任务 Manager；未注入时拒绝后台执行。
    background_tasks: Option<Arc<BackgroundTaskManager>>,
}

impl BashTool {
    /// 创建一个绑定到指定 Session 环境的 Bash 工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self {
            environment,
            background_tasks: None,
        }
    }

    /// 创建一个同时支持真实后台执行的 Bash 工具。
    pub fn with_background_tasks(
        environment: Arc<ToolEnvironment>,
        background_tasks: Arc<BackgroundTaskManager>,
    ) -> Self {
        Self {
            environment,
            background_tasks: Some(background_tasks),
        }
    }
}

impl AgentTool for BashTool {
    /// 返回命令、工作目录和超时的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Bash",
            "使用系统 Bash 以 -lc 非交互执行命令。命令可修改项目内外状态，因此始终按副作用工具处理；取消或超时会终止完整进程组。",
            shell_schema(self.background_tasks.is_some()),
        )
    }

    /// 任意 Shell 命令都可能改变文件、进程或远端状态。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let input = parse_shell_input(input, &self.environment)?;
        resolve_command_cwd(&self.environment, input.cwd.as_deref())?;
        Ok(ToolEffect::ChangesState)
    }

    /// Shell 命令必须形成顺序副作用屏障。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在独立监督任务中运行 Bash，外层 Future 被取消时监督任务仍会清理进程树。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_shell_input(&input, &environment)?;
            let cwd = resolve_command_cwd(&environment, input.cwd.as_deref())?;
            let timeout = command_timeout(&environment, input.timeout_ms)?;
            let summary = command_summary("Bash", input.description.as_deref())?;
            let spec = ProcessSpec {
                label: "Bash",
                programs: bash_candidates(),
                args: vec![OsString::from("-lc"), OsString::from(input.command)],
                cwd,
                timeout,
                environment: Vec::new(),
            };
            if input.run_in_background {
                if context.cancellation.is_cancelled() {
                    return Err(ToolError::permanent(
                        "cancelled",
                        "后台命令尚未启动，当前 Turn 已取消",
                    ));
                }
                let manager = self.background_tasks.as_ref().ok_or_else(|| {
                    ToolError::permanent(
                        "background_manager_unavailable",
                        "当前 Session 未注入后台任务 Manager",
                    )
                })?;
                let task = manager
                    .start_process(context.session_id.as_str(), summary, spec)
                    .await?;
                return Ok(ToolOutput::text(render_background_start(&task)));
            }
            run_supervised(environment, context.cancellation, spec).await
        })
    }
}

/// 通过系统 PowerShell 执行一个非交互命令字符串的工具。
pub struct PowerShellTool {
    /// 当前 Session 的工作目录、超时和输出目录。
    environment: Arc<ToolEnvironment>,
    /// 可选的跨 Turn 后台任务 Manager；未注入时拒绝后台执行。
    background_tasks: Option<Arc<BackgroundTaskManager>>,
}

impl PowerShellTool {
    /// 创建一个绑定到指定 Session 环境的 PowerShell 工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self {
            environment,
            background_tasks: None,
        }
    }

    /// 创建一个同时支持真实后台执行的 PowerShell 工具。
    pub fn with_background_tasks(
        environment: Arc<ToolEnvironment>,
        background_tasks: Arc<BackgroundTaskManager>,
    ) -> Self {
        Self {
            environment,
            background_tasks: Some(background_tasks),
        }
    }
}

impl AgentTool for PowerShellTool {
    /// 返回命令、工作目录和超时的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "PowerShell",
            "使用系统 PowerShell 以无配置、非交互方式执行命令，并强制 UTF-8 管道输出。命令始终按副作用工具处理；取消或超时会终止完整进程树。",
            shell_schema(self.background_tasks.is_some()),
        )
    }

    /// 任意 PowerShell 命令都可能改变文件、进程或远端状态。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let input = parse_shell_input(input, &self.environment)?;
        resolve_command_cwd(&self.environment, input.cwd.as_deref())?;
        Ok(ToolEffect::ChangesState)
    }

    /// PowerShell 命令必须形成顺序副作用屏障。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 在独立监督任务中运行 PowerShell，外层 Future 被取消时仍会清理进程树。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_shell_input(&input, &environment)?;
            let cwd = resolve_command_cwd(&environment, input.cwd.as_deref())?;
            let timeout = command_timeout(&environment, input.timeout_ms)?;
            let summary = command_summary("PowerShell", input.description.as_deref())?;
            let script = powershell_script(&input.command);
            let spec = ProcessSpec {
                label: "PowerShell",
                programs: powershell_candidates(),
                args: vec![
                    OsString::from("-NoLogo"),
                    OsString::from("-NoProfile"),
                    OsString::from("-NonInteractive"),
                    OsString::from("-Command"),
                    OsString::from(script),
                ],
                cwd,
                timeout,
                environment: Vec::new(),
            };
            if input.run_in_background {
                if context.cancellation.is_cancelled() {
                    return Err(ToolError::permanent(
                        "cancelled",
                        "后台命令尚未启动，当前 Turn 已取消",
                    ));
                }
                let manager = self.background_tasks.as_ref().ok_or_else(|| {
                    ToolError::permanent(
                        "background_manager_unavailable",
                        "当前 Session 未注入后台任务 Manager",
                    )
                })?;
                let task = manager
                    .start_process(context.session_id.as_str(), summary, spec)
                    .await?;
                return Ok(ToolOutput::text(render_background_start(&task)));
            }
            run_supervised(environment, context.cancellation, spec).await
        })
    }
}

/// 不经过 Shell、按参数数组调用系统 Git 的工具。
pub struct GitTool {
    /// 当前 Session 的工作目录、超时和输出目录。
    environment: Arc<ToolEnvironment>,
}

impl GitTool {
    /// 创建一个绑定到指定 Session 环境的 Git 工具。
    pub fn new(environment: Arc<ToolEnvironment>) -> Self {
        Self { environment }
    }
}

impl AgentTool for GitTool {
    /// 返回参数数组、工作目录和超时的严格 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Git",
            "不经过 Shell，按 args 数组调用系统 Git。明确的只读子命令可并发执行；其他子命令按副作用工具受 Plan 只读边界约束。禁用终端凭据提示，取消或超时会终止完整进程树。",
            json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1
                    },
                    "cwd": { "type": "string", "minLength": 1 },
                    "timeout_ms": { "type": "integer", "minimum": 1 }
                },
                "required": ["args"],
                "additionalProperties": false
            }),
        )
    }

    /// 明确只读 Git 子命令直接执行，其余命令按可能变更状态处理。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        let input = parse_git_input(input, &self.environment)?;
        resolve_command_cwd(&self.environment, input.cwd.as_deref())?;
        Ok(classify_git_effect(&input.args))
    }

    /// 只读 Git 子命令可并发；Agent Runner 会对变更命令强制顺序屏障。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 在独立监督任务中直接运行 Git 参数数组。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let environment = self.environment.clone();
        Box::pin(async move {
            let input = parse_git_input(&input, &environment)?;
            let cwd = resolve_command_cwd(&environment, input.cwd.as_deref())?;
            let timeout = command_timeout(&environment, input.timeout_ms)?;
            let effect = classify_git_effect(&input.args);
            let mut process_environment = vec![
                (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
                (OsString::from("GIT_PAGER"), OsString::from("cat")),
                (OsString::from("GIT_EDITOR"), OsString::from("true")),
                (
                    OsString::from("GIT_SEQUENCE_EDITOR"),
                    OsString::from("true"),
                ),
            ];
            let args = if effect == ToolEffect::ReadOnly {
                // 只读 Git 也可能根据仓库配置运行 diff/textconv/fsmonitor 外部程序；
                // 关闭这些扩展，避免 Plan 只读边界被配置文件或宿主环境绕过。
                process_environment.extend([
                    (OsString::from("GIT_EXTERNAL_DIFF"), OsString::new()),
                    (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
                    (
                        OsString::from("GIT_CONFIG_KEY_0"),
                        OsString::from("core.fsmonitor"),
                    ),
                    (
                        OsString::from("GIT_CONFIG_VALUE_0"),
                        OsString::from("false"),
                    ),
                    (OsString::from("GIT_CONFIG_PARAMETERS"), OsString::new()),
                ]);
                harden_read_only_git_args(input.args)
            } else {
                input.args
            };
            if effect == ToolEffect::ReadOnly {
                process_environment
                    .push((OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0")));
            }
            let spec = ProcessSpec {
                label: "Git",
                programs: vec![OsString::from("git")],
                args: args.into_iter().map(OsString::from).collect(),
                cwd,
                timeout,
                environment: process_environment,
            };
            run_supervised(environment, context.cancellation, spec).await
        })
    }
}

/// Shell 工具共享的严格反序列化输入。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellInput {
    /// 交给目标 Shell 解释的非空命令字符串。
    command: String,
    /// 不包含命令正文的可选单行任务说明。
    description: Option<String>,
    /// 绝对路径或相对 Session 工作目录的可选执行目录。
    cwd: Option<String>,
    /// 可选执行超时毫秒数。
    timeout_ms: Option<u64>,
    /// 为 true 时立即返回任务标识，并由 Session Manager 独立监督进程。
    #[serde(default)]
    run_in_background: bool,
}

/// Git 工具的严格反序列化输入。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GitInput {
    /// 不经过 Shell 展开的 Git 参数数组。
    args: Vec<String>,
    /// 绝对路径或相对 Session 工作目录的可选执行目录。
    cwd: Option<String>,
    /// 可选执行超时毫秒数。
    timeout_ms: Option<u64>,
}

/// 不经过工具 Schema 的通用有界子进程请求。
///
/// 该端口仅暴露程序、参数和资源边界；内部的 Unix 进程组与 Windows
/// Job Object 始终由本模块管理。
#[derive(Clone, Debug)]
pub struct BoundedCommandRequest {
    /// 不经过 Shell 拆词的可执行程序。
    program: OsString,
    /// 不经过二次拼接的参数列表。
    args: Vec<OsString>,
    /// 仅显式 Windows Shell 请求使用的原始 CMD 脚本；普通程序始终走独立参数。
    #[cfg(windows)]
    windows_shell_script: Option<OsString>,
    /// 已由调用方选定的工作目录。
    cwd: PathBuf,
    /// 从成功启动开始计算的硬超时。
    timeout: Duration,
    /// 标准输出与标准错误各自允许的最大字节数。
    max_output_bytes: usize,
    /// 启动后一次性写入并关闭的标准输入。
    stdin: Vec<u8>,
    /// 仅对当前子进程树生效的环境变量覆盖。
    environment: Vec<(OsString, OsString)>,
}

impl BoundedCommandRequest {
    /// 创建一个默认无参数、无输入和无环境覆盖的有界命令。
    pub fn new(
        program: impl Into<OsString>,
        cwd: impl Into<PathBuf>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            #[cfg(windows)]
            windows_shell_script: None,
            cwd: cwd.into(),
            timeout,
            max_output_bytes,
            stdin: Vec::new(),
            environment: Vec::new(),
        }
    }

    /// 创建显式系统 Shell 请求；保留脚本本身的引号与控制符，不按 CRT 参数二次转义。
    ///
    /// 仅用于调用方已明确选择 Shell 语义的完整脚本；普通程序应继续使用 new/with_args。
    pub fn shell(
        script: impl Into<OsString>,
        cwd: impl Into<PathBuf>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Self {
        let script = script.into();
        #[cfg(windows)]
        {
            let mut request = Self::new("cmd.exe", cwd, timeout, max_output_bytes).with_args(vec![
                "/D".into(),
                "/S".into(),
                "/C".into(),
            ]);
            request.windows_shell_script = Some(script);
            request
        }
        #[cfg(not(windows))]
        {
            Self::new("sh", cwd, timeout, max_output_bytes).with_args(vec!["-c".into(), script])
        }
    }

    /// 替换不经 Shell 处理的完整参数列表，同时取消先前的显式 Shell 脚本尾部。
    pub fn with_args(mut self, args: Vec<OsString>) -> Self {
        self.args = args;
        #[cfg(windows)]
        {
            self.windows_shell_script = None;
        }
        self
    }

    /// 替换启动后写入子进程的标准输入。
    pub fn with_stdin(mut self, stdin: Vec<u8>) -> Self {
        self.stdin = stdin;
        self
    }

    /// 替换仅对当前子进程树生效的环境变量。
    pub fn with_environment(mut self, environment: Vec<(OsString, OsString)>) -> Self {
        self.environment = environment;
        self
    }
}

/// 通用有界命令的完整退出状态与两个输出流。
#[derive(Debug)]
pub struct BoundedCommandOutput {
    /// 主进程的真实退出状态。
    pub status: ExitStatus,
    /// 容量上限内的完整标准输出字节。
    pub stdout: Vec<u8>,
    /// 容量上限内的完整标准错误字节。
    pub stderr: Vec<u8>,
}

/// 通用有界命令无法安全启动、监督或回收时的错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedCommandError {
    /// 便于上层稳定归一的 ASCII 错误码。
    code: &'static str,
    /// 不包含标准输入和环境变量的安全说明。
    message: String,
}

impl BoundedCommandError {
    /// 创建一个不暴露子进程输入的错误。
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 返回供上层稳定分类的错误码。
    pub const fn code(&self) -> &'static str {
        self.code
    }

    /// 返回不包含命令输入的安全说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BoundedCommandError {
    /// 输出安全错误说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for BoundedCommandError {}

/// 一个已经完成输入校验的底层进程请求。
pub(crate) struct ProcessSpec {
    /// 用于模型输出和错误分类的固定工具名称。
    pub(crate) label: &'static str,
    /// 按优先级尝试的可执行文件名称或绝对路径。
    pub(crate) programs: Vec<OsString>,
    /// 不经过额外拼接或拆词的进程参数。
    pub(crate) args: Vec<OsString>,
    /// 已验证存在的绝对执行目录。
    pub(crate) cwd: PathBuf,
    /// 从进程启动成功开始计算的硬超时。
    pub(crate) timeout: Duration,
    /// 仅对当前子进程树生效的环境变量覆盖。
    pub(crate) environment: Vec<(OsString, OsString)>,
}

/// 进程监督结束的唯一原因。
pub(crate) enum ProcessTermination {
    /// 进程组正常自行结束并返回状态。
    Exited(ExitStatus),
    /// 超过调用输入允许的执行时长。
    TimedOut,
    /// 当前 Turn 已被上层取消。
    Cancelled,
}

/// 一个输出流的有界预览、完整字节数与可选落盘位置。
struct CapturedStream {
    /// 保留开头与结尾的 UTF-8 损失解码预览。
    preview: String,
    /// 从管道实际读取并排空的完整字节数。
    total_bytes: u64,
    /// 超过预览上限时保留的完整输出文件。
    artifact_path: Option<PathBuf>,
    /// 输出文件创建或写入失败但管道仍被排空时的说明。
    artifact_error: Option<String>,
}

/// 在 Drop 时仍会向完整进程组发送强制终止，防止监督任务异常泄漏进程。
pub(crate) struct ProcessGroupGuard {
    /// command-group 提供的 Unix 进程组或 Windows Job Object。
    pub(crate) child: AsyncGroupChild,
    /// `true` 表示异常 Drop 时仍需发送最后一次强制终止。
    pub(crate) armed: bool,
}

impl Drop for ProcessGroupGuard {
    /// 最后防线只发起终止；正常路径会在 Drop 前完成等待或退出确认。
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.start_kill();
        }
    }
}

/// 返回 Bash 与 PowerShell 共享的工具输入 Schema。
fn shell_schema(background_enabled: bool) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "command": { "type": "string", "minLength": 1 },
            "cwd": { "type": "string", "minLength": 1 },
            "timeout_ms": { "type": "integer", "minimum": 1 }
        },
        "required": ["command"],
        "additionalProperties": false
    });
    if !background_enabled {
        return schema;
    }
    let Some(properties) = schema["properties"].as_object_mut() else {
        return schema;
    };
    properties.insert(
        "description".to_owned(),
        json!({ "type": "string", "minLength": 1, "maxLength": 160 }),
    );
    properties.insert("run_in_background".to_owned(), json!({ "type": "boolean" }));
    schema
}

/// 解析 Shell 输入并校验命令与超时。
fn parse_shell_input(
    input: &Value,
    environment: &ToolEnvironment,
) -> Result<ShellInput, ToolError> {
    let input: ShellInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.command.trim().is_empty() {
        return Err(ToolError::permanent(
            "empty_command",
            "Shell command 不能为空",
        ));
    }
    validate_optional_cwd(input.cwd.as_deref())?;
    command_summary("Shell", input.description.as_deref())?;
    command_timeout(environment, input.timeout_ms)?;
    Ok(input)
}

/// 解析 Git 输入并校验参数、目录与超时。
fn parse_git_input(input: &Value, environment: &ToolEnvironment) -> Result<GitInput, ToolError> {
    let input: GitInput = serde_json::from_value(input.clone()).map_err(invalid_input)?;
    if input.args.is_empty() {
        return Err(ToolError::permanent(
            "empty_git_args",
            "Git args 至少需要一个参数",
        ));
    }
    if input.args.iter().any(|argument| argument.contains('\0')) {
        return Err(ToolError::permanent(
            "invalid_git_arg",
            "Git 参数不能包含 NUL 字节",
        ));
    }
    validate_optional_cwd(input.cwd.as_deref())?;
    command_timeout(environment, input.timeout_ms)?;
    Ok(input)
}

/// 校验可选工作目录文本非空。
fn validate_optional_cwd(cwd: Option<&str>) -> Result<(), ToolError> {
    if cwd.is_some_and(|path| path.trim().is_empty()) {
        return Err(ToolError::permanent(
            "invalid_command_cwd",
            "命令工作目录不能为空",
        ));
    }
    Ok(())
}

/// 解析并确认命令工作目录存在且是目录。
fn resolve_command_cwd(
    environment: &ToolEnvironment,
    cwd: Option<&str>,
) -> Result<PathBuf, ToolError> {
    let path = match cwd {
        Some(path) => environment.resolve_path(path)?,
        None => environment.working_directory().to_path_buf(),
    };
    if !path.is_dir() {
        return Err(ToolError::permanent(
            "invalid_command_cwd",
            format!("命令工作目录不存在或不是目录：{}", display_path(&path)),
        ));
    }
    Ok(path)
}

/// 把输入超时限制到环境声明的正区间。
fn command_timeout(
    environment: &ToolEnvironment,
    requested_ms: Option<u64>,
) -> Result<Duration, ToolError> {
    let limits = environment.limits();
    let milliseconds = requested_ms.unwrap_or(limits.default_command_timeout_ms);
    if milliseconds == 0 || milliseconds > limits.max_command_timeout_ms {
        return Err(ToolError::permanent(
            "invalid_command_timeout",
            format!(
                "timeout_ms 必须在 1 到 {} 之间",
                limits.max_command_timeout_ms
            ),
        ));
    }
    Ok(Duration::from_millis(milliseconds))
}

/// 返回当前平台可尝试的 Bash 可执行文件。
fn bash_candidates() -> Vec<OsString> {
    #[cfg(windows)]
    {
        vec![
            OsString::from("bash.exe"),
            OsString::from(r"C:\Program Files\Git\bin\bash.exe"),
            OsString::from(r"C:\Program Files\Git\usr\bin\bash.exe"),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![OsString::from("bash")]
    }
}

/// 返回当前平台可尝试的 PowerShell 可执行文件。
fn powershell_candidates() -> Vec<OsString> {
    #[cfg(windows)]
    {
        vec![OsString::from("pwsh.exe"), OsString::from("powershell.exe")]
    }
    #[cfg(not(windows))]
    {
        vec![OsString::from("pwsh")]
    }
}

/// 为 PowerShell 设置无 BOM UTF-8 管道并保留脚本块退出状态。
fn powershell_script(command: &str) -> String {
    format!(
        "$__keencodeUtf8 = [System.Text.UTF8Encoding]::new($false)\n\
         [Console]::OutputEncoding = $__keencodeUtf8\n\
         $OutputEncoding = $__keencodeUtf8\n\
         & {{\n{command}\n}}\n\
         $__keencodeSuccess = $?\n\
         if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}\n\
         if (-not $__keencodeSuccess) {{ exit 1 }}"
    )
}

/// 将少量确定只读的 Git 子命令与所有潜在变更命令分开。
fn classify_git_effect(args: &[String]) -> ToolEffect {
    let Some((command, remainder)) = git_subcommand(args) else {
        return ToolEffect::ChangesState;
    };
    match command {
        // status 的 fsmonitor、diff-like 命令的 textconv/ext-diff 以及 Git
        // 环境变量均在执行阶段关闭；显式请求这些能力时则保守视为有副作用。
        "status" => ToolEffect::ReadOnly,
        "diff" | "log" | "show" if diff_like_is_read_only(remainder) => ToolEffect::ReadOnly,
        "rev-parse" | "ls-files" | "ls-tree" | "for-each-ref" | "describe" | "name-rev"
        | "shortlog" | "check-ignore" | "merge-base" | "version" => ToolEffect::ReadOnly,
        "cat-file" if cat_file_is_read_only(remainder) => ToolEffect::ReadOnly,
        "blame" if blame_is_read_only(remainder) => ToolEffect::ReadOnly,
        "grep" if grep_is_read_only(remainder) => ToolEffect::ReadOnly,
        "branch" if branch_is_read_only(remainder) => ToolEffect::ReadOnly,
        "tag" if tag_is_read_only(remainder) => ToolEffect::ReadOnly,
        "remote" if remote_is_read_only(remainder) => ToolEffect::ReadOnly,
        "config" if config_is_read_only(remainder) => ToolEffect::ReadOnly,
        "stash" if matches!(remainder.first().map(String::as_str), Some("list" | "show")) => {
            ToolEffect::ReadOnly
        }
        "worktree" if matches!(remainder.first().map(String::as_str), Some("list")) => {
            ToolEffect::ReadOnly
        }
        "submodule" if matches!(remainder.first().map(String::as_str), Some("status")) => {
            ToolEffect::ReadOnly
        }
        "reflog"
            if matches!(
                remainder.first().map(String::as_str),
                Some("show" | "exists")
            ) =>
        {
            ToolEffect::ReadOnly
        }
        _ => ToolEffect::ChangesState,
    }
}

/// 判断 diff/log/show 是否明确关闭了会运行外部程序的选项且未请求文件输出。
fn diff_like_is_read_only(args: &[String]) -> bool {
    !args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--ext-diff" | "--textconv" | "--output" | "-o" | "--show-signature"
        ) || argument.starts_with("--output=")
            || argument.starts_with("-o")
            || argument == "--submodule=diff"
    }) && !args
        .windows(2)
        .any(|pair| pair[0] == "--submodule" && pair[1] == "diff")
}

/// 判断 cat-file 是否没有请求 clean/smudge 或 textconv 外部过滤器。
fn cat_file_is_read_only(args: &[String]) -> bool {
    !args
        .iter()
        .any(|argument| matches!(argument.as_str(), "--filters" | "--textconv"))
}

/// 判断 blame 是否没有请求 textconv 外部过滤器。
fn blame_is_read_only(args: &[String]) -> bool {
    !args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--textconv" | "--show-signature" | "--output" | "-o"
        ) || argument.starts_with("--output=")
            || argument.starts_with("-o")
    })
}

/// 判断 grep 是否没有请求 textconv、外部 grep 或自定义 pager。
fn grep_is_read_only(args: &[String]) -> bool {
    !args.iter().any(|argument| {
        matches!(argument.as_str(), "--textconv" | "--ext-grep")
            || argument.starts_with("--open-files-in-pager")
    })
}

/// 为仍允许在 Plan 中执行的 diff-like/grep 命令插入安全的禁用选项。
fn harden_read_only_git_args(args: Vec<String>) -> Vec<String> {
    let Some((command, remainder)) = git_subcommand(&args) else {
        return args;
    };
    let options: &[&str] = match command {
        "diff" | "log" | "show" | "blame" => &["--no-ext-diff", "--no-textconv"],
        "grep" => &["--no-textconv"],
        _ => &[],
    };
    if options.is_empty() {
        return args;
    }
    // git_subcommand 返回的是原数组中的尾部切片，借此定位实际子命令后的
    // 参数区；安全选项必须插在 `--` 路径分隔符之前，否则会被当成路径。
    let remainder_start = args.len().saturating_sub(remainder.len());
    let insert_at = args[remainder_start..]
        .iter()
        .position(|argument| argument == "--")
        .map(|offset| remainder_start + offset)
        .unwrap_or(remainder_start);
    let mut hardened = Vec::with_capacity(args.len() + options.len());
    hardened.extend(args[..insert_at].iter().cloned());
    hardened.extend(options.iter().map(|option| (*option).to_owned()));
    hardened.extend(args[insert_at..].iter().cloned());
    hardened
}

/// 跳过不会注入 Git 配置的全局选项并返回实际子命令。
///
/// `-c` 故意不在白名单中；它可以把只读子命令重绑定为执行任意命令的
/// alias，或者注入其他会产生副作用的运行时配置，必须按未知变更处理。
fn git_subcommand(args: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0_usize;
    while index < args.len() {
        let argument = args[index].as_str();
        if matches!(argument, "--version" | "-v") {
            return Some(("version", &args[index.saturating_add(1)..]));
        }
        if matches!(argument, "--help" | "-h") {
            return Some(("help", &args[index.saturating_add(1)..]));
        }
        if matches!(argument, "-C" | "--git-dir" | "--work-tree" | "--namespace") {
            index = index.saturating_add(2);
            continue;
        }
        if argument.starts_with("--git-dir=")
            || argument.starts_with("--work-tree=")
            || argument.starts_with("--namespace=")
            || matches!(
                argument,
                "--bare" | "--no-pager" | "--paginate" | "--literal-pathspecs"
            )
        {
            index = index.saturating_add(1);
            continue;
        }
        if argument.starts_with('-') {
            return None;
        }
        return Some((argument, &args[index.saturating_add(1)..]));
    }
    None
}

/// 启动并监督一个通用有界子进程树。
///
/// 取消该 Future 会触发进程组守卫的 Drop 保护；超时、输出超限和主进程
/// 正常退出都会显式终止并回收同组后代进程。
pub async fn run_bounded_command(
    request: BoundedCommandRequest,
) -> Result<BoundedCommandOutput, BoundedCommandError> {
    validate_bounded_command(&request)?;
    let mut guard = spawn_bounded_group(&request).map_err(|error| {
        BoundedCommandError::new(
            "command_spawn_failed",
            format!("无法启动有界子进程：{error}"),
        )
    })?;
    let mut stdin = guard.child.inner().stdin.take().ok_or_else(|| {
        BoundedCommandError::new("command_stdin_unavailable", "子进程标准输入不可用")
    })?;
    let stdout = guard.child.inner().stdout.take().ok_or_else(|| {
        BoundedCommandError::new("command_stdout_unavailable", "子进程标准输出不可用")
    })?;
    let stderr = guard.child.inner().stderr.take().ok_or_else(|| {
        BoundedCommandError::new("command_stderr_unavailable", "子进程标准错误不可用")
    })?;

    let output_exceeded = Arc::new(AtomicBool::new(false));
    let stdout_task = tokio::spawn(capture_bounded_stream(
        stdout,
        request.max_output_bytes,
        Arc::clone(&output_exceeded),
    ));
    let stderr_task = tokio::spawn(capture_bounded_stream(
        stderr,
        request.max_output_bytes,
        Arc::clone(&output_exceeded),
    ));
    let input = request.stdin;
    let stdin_task = tokio::spawn(async move {
        stdin.write_all(&input).await?;
        stdin.shutdown().await
    });

    let process_result = monitor_bounded_process(
        &mut guard,
        Instant::now() + request.timeout,
        &output_exceeded,
    )
    .await;
    let stdin_result = stdin_task.await;
    let stdout_result = stdout_task.await;
    let stderr_result = stderr_task.await;
    let status = process_result?;
    flatten_bounded_io_task(
        stdin_result,
        "command_stdin_failed",
        "写入子进程标准输入失败",
    )?;
    let stdout = flatten_bounded_io_task(
        stdout_result,
        "command_stdout_failed",
        "读取子进程标准输出失败",
    )?;
    let stderr = flatten_bounded_io_task(
        stderr_result,
        "command_stderr_failed",
        "读取子进程标准错误失败",
    )?;
    if output_exceeded.load(Ordering::Acquire) {
        return Err(BoundedCommandError::new(
            "command_output_too_large",
            "子进程输出超过容量上限",
        ));
    }
    Ok(BoundedCommandOutput {
        status,
        stdout,
        stderr,
    })
}

/// 在启动前校验通用有界命令的资源上限与工作目录。
fn validate_bounded_command(request: &BoundedCommandRequest) -> Result<(), BoundedCommandError> {
    if request.program.is_empty() {
        return Err(BoundedCommandError::new(
            "invalid_command_program",
            "有界子进程程序不能为空",
        ));
    }
    if !request.cwd.is_dir() {
        return Err(BoundedCommandError::new(
            "invalid_command_cwd",
            "有界子进程工作目录不存在或不是目录",
        ));
    }
    if request.timeout.is_zero() {
        return Err(BoundedCommandError::new(
            "invalid_command_timeout",
            "有界子进程超时必须大于零",
        ));
    }
    if request.max_output_bytes == 0 || request.max_output_bytes > MAX_BOUNDED_COMMAND_OUTPUT_BYTES
    {
        return Err(BoundedCommandError::new(
            "invalid_command_output_limit",
            format!("单流输出上限必须在 1 到 {MAX_BOUNDED_COMMAND_OUTPUT_BYTES} 字节之间"),
        ));
    }
    if request.stdin.len() > MAX_BOUNDED_COMMAND_STDIN_BYTES {
        return Err(BoundedCommandError::new(
            "invalid_command_stdin_limit",
            format!("标准输入不能超过 {MAX_BOUNDED_COMMAND_STDIN_BYTES} 字节"),
        ));
    }
    Ok(())
}

/// 以可写标准输入和可读双输出管道启动一个完整进程组。
fn spawn_bounded_group(request: &BoundedCommandRequest) -> io::Result<ProcessGroupGuard> {
    let mut command = Command::new(&request.program);
    command.args(&request.args);
    #[cfg(windows)]
    if let Some(script) = &request.windows_shell_script {
        use std::os::windows::process::CommandExt;

        // /S /C 只去掉最外层的一对引号；内部引号原样交给 CMD，不能使用 CRT 的反斜杠转义。
        let mut tail = OsString::from("\"");
        tail.push(script);
        tail.push("\"");
        command.as_std_mut().raw_arg(tail);
    }
    command
        .current_dir(&request.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &request.environment {
        command.env(name, value);
    }
    spawn_group_command(command)
}

/// 在超时或输出超限时终止完整进程组，正常退出时也清理留存后代。
async fn monitor_bounded_process(
    guard: &mut ProcessGroupGuard,
    deadline: Instant,
    output_exceeded: &AtomicBool,
) -> Result<ExitStatus, BoundedCommandError> {
    loop {
        if output_exceeded.load(Ordering::Acquire) {
            terminate_and_wait(&mut guard.child)
                .await
                .map_err(|error| {
                    BoundedCommandError::new(
                        "command_termination_failed",
                        format!("输出超限后回收子进程树失败：{error}"),
                    )
                })?;
            guard.armed = false;
            return Err(BoundedCommandError::new(
                "command_output_too_large",
                "子进程输出超过容量上限",
            ));
        }
        if Instant::now() >= deadline {
            terminate_and_wait(&mut guard.child)
                .await
                .map_err(|error| {
                    BoundedCommandError::new(
                        "command_termination_failed",
                        format!("超时后回收子进程树失败：{error}"),
                    )
                })?;
            guard.armed = false;
            return Err(BoundedCommandError::new(
                "command_timed_out",
                "子进程执行超时",
            ));
        }
        match guard.child.try_wait() {
            Ok(Some(status)) => {
                terminate_and_wait(&mut guard.child)
                    .await
                    .map_err(|error| {
                        BoundedCommandError::new(
                            "command_termination_failed",
                            format!("主进程退出后回收子进程树失败：{error}"),
                        )
                    })?;
                guard.armed = false;
                return Ok(status);
            }
            Ok(None) => {}
            Err(error) => {
                return Err(BoundedCommandError::new(
                    "command_wait_failed",
                    format!("等待子进程退出失败：{error}"),
                ));
            }
        }
        tokio::select! {
            _ = sleep_until(deadline) => {}
            _ = sleep(PROCESS_POLL_INTERVAL) => {}
        }
    }
}

/// 持续排空一个输出管道，超限后不再保留字节并通知监督器。
async fn capture_bounded_stream<R>(
    mut reader: R,
    maximum: usize,
    exceeded: Arc<AtomicBool>,
) -> io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(maximum.min(8192));
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = maximum.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
        if read > remaining {
            exceeded.store(true, Ordering::Release);
        }
    }
    Ok(retained)
}

/// 将管道任务的 Join 失败与 IO 失败归一为稳定错误。
fn flatten_bounded_io_task<T>(
    result: Result<Result<T, io::Error>, tokio::task::JoinError>,
    code: &'static str,
    message: &'static str,
) -> Result<T, BoundedCommandError> {
    result
        .map_err(|error| BoundedCommandError::new(code, format!("{message}：{error}")))?
        .map_err(|error| BoundedCommandError::new(code, format!("{message}：{error}")))
}

/// 仅允许不会创建、删除或移动分支的 branch 参数组合。
fn branch_is_read_only(args: &[String]) -> bool {
    args.is_empty()
        || args.iter().all(|argument| {
            matches!(
                argument.as_str(),
                "--list"
                    | "-l"
                    | "--show-current"
                    | "--contains"
                    | "--no-contains"
                    | "--merged"
                    | "--no-merged"
                    | "-a"
                    | "--all"
                    | "-r"
                    | "--remotes"
                    | "-v"
                    | "-vv"
                    | "--verbose"
                    | "--color"
                    | "--no-color"
            ) || argument.starts_with("--contains=")
                || argument.starts_with("--no-contains=")
                || argument.starts_with("--merged=")
                || argument.starts_with("--no-merged=")
                || argument.starts_with("--format=")
                || (!argument.starts_with('-')
                    && args.first().is_some_and(|first| first == "--list"))
        })
}

/// 仅允许列出或按条件筛选标签，不允许创建、删除或签名标签。
fn tag_is_read_only(args: &[String]) -> bool {
    args.is_empty()
        || args.iter().all(|argument| {
            matches!(
                argument.as_str(),
                "--list"
                    | "-l"
                    | "--contains"
                    | "--no-contains"
                    | "--merged"
                    | "--no-merged"
                    | "--points-at"
                    | "--sort"
                    | "--color"
                    | "--no-color"
                    | "-n"
            ) || argument.starts_with("--contains=")
                || argument.starts_with("--no-contains=")
                || argument.starts_with("--merged=")
                || argument.starts_with("--no-merged=")
                || argument.starts_with("--points-at=")
                || argument.starts_with("--sort=")
                || argument.starts_with("--format=")
                || argument.starts_with("-n")
                || (!argument.starts_with('-')
                    && args
                        .first()
                        .is_some_and(|first| matches!(first.as_str(), "--list" | "-l")))
        })
}

/// 仅允许列出远端或读取 URL 的 remote 参数组合。
fn remote_is_read_only(args: &[String]) -> bool {
    args.is_empty()
        || (args.len() == 1 && matches!(args[0].as_str(), "-v" | "--verbose"))
        || matches!(args.first().map(String::as_str), Some("get-url"))
        || (args.first().map(String::as_str) == Some("show")
            && args
                .iter()
                .any(|argument| matches!(argument.as_str(), "-n" | "--no-query"))
            && args.iter().enumerate().all(|(index, argument)| {
                (index == 0 && argument == "show")
                    || argument == "-n"
                    || argument == "--no-query"
                    || (!argument.starts_with('-') && index > 0)
            }))
}

/// 仅允许显式查询或列举配置的 config 参数组合。
fn config_is_read_only(args: &[String]) -> bool {
    args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--get"
                | "--get-all"
                | "--get-regexp"
                | "--get-urlmatch"
                | "--list"
                | "-l"
                | "--show-origin"
                | "--show-scope"
                | "--name-only"
        )
    }) && !args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--add"
                | "--replace-all"
                | "--unset"
                | "--unset-all"
                | "--remove-section"
                | "--rename-section"
        )
    })
}

/// 启动独立进程监督任务并把 Join 异常归一为工具错误。
async fn run_supervised(
    environment: Arc<ToolEnvironment>,
    cancellation: TurnCancellation,
    spec: ProcessSpec,
) -> Result<ToolOutput, ToolError> {
    tokio::spawn(async move { supervise_process(&environment, &cancellation, spec).await })
        .await
        .map_err(|error| {
            ToolError::permanent(
                "command_supervisor_failed",
                format!("命令监督任务异常结束：{error}"),
            )
        })?
}

/// 尝试可执行文件候选并完整监督首个成功启动的进程组。
async fn supervise_process(
    environment: &ToolEnvironment,
    cancellation: &TurnCancellation,
    spec: ProcessSpec,
) -> Result<ToolOutput, ToolError> {
    if cancellation.is_cancelled() {
        return Err(ToolError::permanent("cancelled", "命令调用已取消"));
    }
    let mut last_not_found = None;
    for program in &spec.programs {
        match spawn_group(program, &spec) {
            Ok(guard) => {
                return supervise_spawned(environment, cancellation, &spec, guard).await;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                last_not_found = Some(error);
            }
            Err(error) => return Err(spawn_error(spec.label, program, error)),
        }
    }
    Err(ToolError::permanent(
        "command_not_found",
        format!(
            "{} 可执行文件不可用：{}",
            spec.label,
            last_not_found
                .map(|error| error.to_string())
                .unwrap_or_else(|| "没有候选可执行文件".to_owned())
        ),
    ))
}

/// 创建带标准管道、隐藏窗口和完整进程组的子进程。
pub(crate) fn spawn_group(program: &OsString, spec: &ProcessSpec) -> io::Result<ProcessGroupGuard> {
    let mut command = Command::new(program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (name, value) in &spec.environment {
        command.env(name, value);
    }
    spawn_group_command(command)
}

/// 将已配置的 Tokio Command 放入跨平台进程组并启动。
fn spawn_group_command(mut command: Command) -> io::Result<ProcessGroupGuard> {
    let mut group = command.group();
    group.kill_on_drop(true);
    #[cfg(windows)]
    group.creation_flags(0x0800_0000);
    group
        .spawn()
        .map(|child| ProcessGroupGuard { child, armed: true })
}

/// 监督已启动进程的退出、取消、超时和两个输出管道。
async fn supervise_spawned(
    environment: &ToolEnvironment,
    cancellation: &TurnCancellation,
    spec: &ProcessSpec,
    mut guard: ProcessGroupGuard,
) -> Result<ToolOutput, ToolError> {
    let stdout = guard
        .child
        .inner()
        .stdout
        .take()
        .ok_or_else(|| ToolError::permanent("stdout_unavailable", "命令标准输出管道不可用"))?;
    let stderr = guard
        .child
        .inner()
        .stderr
        .take()
        .ok_or_else(|| ToolError::permanent("stderr_unavailable", "命令标准错误管道不可用"))?;
    let preview_limit = environment.limits().max_command_preview_bytes;
    let artifact_directory = environment.artifact_directory().to_path_buf();
    let stdout_task = tokio::spawn(capture_stream(
        stdout,
        artifact_directory.clone(),
        "stdout",
        preview_limit,
    ));
    let stderr_task = tokio::spawn(capture_stream(
        stderr,
        artifact_directory,
        "stderr",
        preview_limit,
    ));
    let deadline = Instant::now() + spec.timeout;
    let termination = monitor_process(&mut guard, cancellation, deadline).await?;
    let stdout = await_capture(stdout_task, "stdout").await?;
    let stderr = await_capture(stderr_task, "stderr").await?;
    let report = render_process_report(spec, &termination, &stdout, &stderr);

    match termination {
        ProcessTermination::Exited(status) if status.success() => Ok(ToolOutput::text(report)),
        ProcessTermination::Exited(_) => Err(ToolError::permanent("command_failed", report)),
        ProcessTermination::TimedOut => Err(ToolError::retryable("command_timed_out", report)),
        ProcessTermination::Cancelled => Err(ToolError::permanent("cancelled", report)),
    }
}

/// 不取消 `wait` Future，通过安全轮询等待退出并在取消或超时时强制清理进程组。
pub(crate) async fn monitor_process(
    guard: &mut ProcessGroupGuard,
    cancellation: &TurnCancellation,
    deadline: Instant,
) -> Result<ProcessTermination, ToolError> {
    loop {
        if cancellation.is_cancelled() {
            terminate_and_wait(&mut guard.child).await?;
            guard.armed = false;
            return Ok(ProcessTermination::Cancelled);
        }
        if Instant::now() >= deadline {
            terminate_and_wait(&mut guard.child).await?;
            guard.armed = false;
            return Ok(ProcessTermination::TimedOut);
        }
        match guard.child.try_wait() {
            Ok(Some(status)) => {
                terminate_and_wait(&mut guard.child).await?;
                guard.armed = false;
                return Ok(ProcessTermination::Exited(status));
            }
            Ok(None) => {}
            Err(error) => {
                let _ = guard.child.start_kill();
                return Err(ToolError::permanent(
                    "command_wait_failed",
                    format!("等待命令退出失败：{error}"),
                ));
            }
        }
        tokio::select! {
            _ = cancellation.cancelled() => {}
            _ = sleep_until(deadline) => {}
            _ = sleep(PROCESS_POLL_INTERVAL) => {}
        }
    }
}

/// 向完整进程组发送强制终止并在不取消 wait Future 的前提下回收资源。
pub(crate) async fn terminate_and_wait(child: &mut AsyncGroupChild) -> Result<(), ToolError> {
    match child.start_kill() {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
            ) => {}
        Err(error) => {
            return Err(ToolError::permanent(
                "command_kill_failed",
                format!("终止命令进程树失败：{error}"),
            ));
        }
    }
    child.wait().await.map_err(|error| {
        ToolError::permanent(
            "command_reap_failed",
            format!("回收命令进程树失败：{error}"),
        )
    })?;
    Ok(())
}

/// 持续排空一个输出管道，同时保存完整文件和有界首尾预览。
async fn capture_stream<R>(
    mut reader: R,
    artifact_directory: PathBuf,
    label: &'static str,
    preview_limit: usize,
) -> CapturedStream
where
    R: AsyncRead + Unpin,
{
    let artifact = create_artifact(&artifact_directory, label);
    let (mut artifact_file, artifact_path, mut artifact_error) = match artifact {
        Ok((file, path)) => (Some(file), Some(path), None),
        Err(error) => (None, None, Some(error.to_string())),
    };
    let head_limit = preview_limit / 2;
    let tail_limit = preview_limit.saturating_sub(head_limit);
    let mut head = Vec::with_capacity(head_limit);
    let mut tail = Vec::with_capacity(tail_limit);
    let mut total_bytes = 0_u64;
    let mut chunk = vec![0_u8; 16 * 1024];

    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                artifact_error.get_or_insert_with(|| format!("读取管道失败：{error}"));
                break;
            }
        };
        let bytes = &chunk[..read];
        total_bytes = total_bytes.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if let Some(file) = artifact_file.as_mut() {
            if let Err(error) = file.write_all(bytes).await {
                artifact_error.get_or_insert_with(|| format!("保存完整输出失败：{error}"));
                artifact_file = None;
            }
        }
        retain_preview(bytes, &mut head, &mut tail, head_limit, tail_limit);
    }

    if let Some(file) = artifact_file.as_mut() {
        if let Err(error) = file.flush().await {
            artifact_error.get_or_insert_with(|| format!("刷新完整输出失败：{error}"));
            artifact_file = None;
        }
    }
    drop(artifact_file);

    let truncated = total_bytes > u64::try_from(preview_limit).unwrap_or(u64::MAX);
    let mut retained_path = artifact_path;
    if !truncated {
        if let Some(path) = retained_path.as_ref() {
            match tokio::fs::remove_file(path).await {
                Ok(()) => retained_path = None,
                Err(error) if error.kind() == io::ErrorKind::NotFound => retained_path = None,
                Err(error) => {
                    artifact_error.get_or_insert_with(|| format!("清理临时输出失败：{error}"));
                }
            }
        }
    }
    let preview = render_preview(&head, &tail, truncated, total_bytes);
    CapturedStream {
        preview,
        total_bytes,
        artifact_path: retained_path,
        artifact_error,
    }
}

/// 在应用数据目录创建权限由操作系统控制的随机输出文件。
fn create_artifact(directory: &Path, label: &str) -> io::Result<(tokio::fs::File, PathBuf)> {
    std::fs::create_dir_all(directory)?;
    let named = tempfile::Builder::new()
        .prefix(&format!("keencode-{label}-"))
        .suffix(".log")
        .tempfile_in(directory)?;
    let (file, path) = named.keep().map_err(|error| error.error)?;
    Ok((tokio::fs::File::from_std(file), path))
}

/// 保留输出开头和结尾，并持续丢弃中间预览但不停止排空管道。
fn retain_preview(
    bytes: &[u8],
    head: &mut Vec<u8>,
    tail: &mut Vec<u8>,
    head_limit: usize,
    tail_limit: usize,
) {
    let head_remaining = head_limit.saturating_sub(head.len());
    let head_take = head_remaining.min(bytes.len());
    head.extend_from_slice(&bytes[..head_take]);
    if tail_limit == 0 || head_take == bytes.len() {
        return;
    }
    tail.extend_from_slice(&bytes[head_take..]);
    if tail.len() > tail_limit {
        let excess = tail.len() - tail_limit;
        tail.drain(..excess);
    }
}

/// 将首尾预览损失解码为 UTF-8，并在截断时插入明确标记。
fn render_preview(head: &[u8], tail: &[u8], truncated: bool, total_bytes: u64) -> String {
    let mut output = String::from_utf8_lossy(head).into_owned();
    if truncated {
        output.push_str(&format!(
            "\n...[中间输出已从预览省略；完整流共 {total_bytes} 字节]...\n"
        ));
    }
    output.push_str(&String::from_utf8_lossy(tail));
    output
}

/// 等待输出捕获任务并把任务异常归一为工具错误。
async fn await_capture(
    task: JoinHandle<CapturedStream>,
    label: &str,
) -> Result<CapturedStream, ToolError> {
    task.await.map_err(|error| {
        ToolError::permanent(
            "output_capture_failed",
            format!("{label} 捕获任务异常结束：{error}"),
        )
    })
}

/// 生成包含退出原因、工作目录、首尾预览和完整输出路径的模型报告。
fn render_process_report(
    spec: &ProcessSpec,
    termination: &ProcessTermination,
    stdout: &CapturedStream,
    stderr: &CapturedStream,
) -> String {
    let status = match termination {
        ProcessTermination::Exited(status) => match status.code() {
            Some(code) => format!("退出码 {code}"),
            None => "被操作系统信号终止".to_owned(),
        },
        ProcessTermination::TimedOut => format!("执行超时（{} 毫秒）", spec.timeout.as_millis()),
        ProcessTermination::Cancelled => "已取消并清理进程树".to_owned(),
    };
    let mut report = format!(
        "{}：{status}\n工作目录：{}",
        spec.label,
        display_path(&spec.cwd)
    );
    append_stream_report(&mut report, "stdout", stdout);
    append_stream_report(&mut report, "stderr", stderr);
    report
}

/// 向进程报告追加一个输出流的预览、字节数和落盘信息。
fn append_stream_report(report: &mut String, label: &str, stream: &CapturedStream) {
    report.push_str(&format!("\n{label}（{} 字节）：", stream.total_bytes));
    if stream.preview.is_empty() {
        report.push_str("<空>");
    } else {
        report.push('\n');
        report.push_str(&stream.preview);
    }
    if let Some(path) = &stream.artifact_path {
        report.push_str(&format!("\n{label} 完整输出：{}", display_path(path)));
    }
    if let Some(error) = &stream.artifact_error {
        report.push_str(&format!("\n{label} 输出落盘警告：{error}"));
    }
}

/// 生成不包含命令字符串或参数内容的进程启动错误。
pub(crate) fn spawn_error(label: &str, program: &OsString, error: io::Error) -> ToolError {
    ToolError::permanent(
        "command_spawn_failed",
        format!("启动 {label} 可执行文件 {:?} 失败：{error}", program),
    )
}

/// 校验并归一后台任务说明，避免默认回显完整命令字符串。
fn command_summary(label: &str, description: Option<&str>) -> Result<String, ToolError> {
    let Some(description) = description else {
        return Ok(format!("{label} 后台命令"));
    };
    let description = description.trim();
    if description.is_empty()
        || description.len() > 160
        || description
            .chars()
            .any(|character| matches!(character, '\r' | '\n' | '\0'))
    {
        return Err(ToolError::permanent(
            "invalid_command_description",
            "description 必须是 1 到 160 字节且不含换行或 NUL 的单行文本",
        ));
    }
    Ok(description.to_owned())
}

/// 返回后台进程已经真实启动后的非空模型结果。
fn render_background_start(task: &crate::background::BackgroundTaskInfo) -> String {
    let pid = task
        .pid
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "系统未提供".to_owned());
    format!(
        "后台任务已启动\n任务 ID：{}\n进程 ID：{pid}\n说明：{}\n使用 TaskOutput 读取增量输出，使用 TaskStop 停止完整进程树。",
        task.task_id, task.summary
    )
}

#[cfg(all(test, windows))]
mod bounded_shell_tests {
    use super::{BoundedCommandRequest, Duration, run_bounded_command};

    /// 显式替换参数后不得残留原始脚本尾部，普通参数调用不继承 Shell 拼接语义。
    #[tokio::test]
    async fn replacing_shell_arguments_discards_raw_script_tail() {
        let directory = tempfile::tempdir().expect("创建隔离命令目录");
        let request = BoundedCommandRequest::shell(
            "exit /b 9",
            directory.path(),
            Duration::from_secs(3),
            1024,
        )
        .with_args(vec!["/D".into(), "/C".into(), "echo KC_DIRECT_ARGS".into()]);
        let output = run_bounded_command(request)
            .await
            .expect("普通参数应能执行");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "KC_DIRECT_ARGS"
        );
        assert!(output.stderr.is_empty());
    }
}
