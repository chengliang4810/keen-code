//! Claude Code 插件 Hooks 兼容运行时。
//!
//! 本模块只保存热更新后的内存注册表，不读取 KeenCode 配置文件。桌面层负责按
//! Claude 插件格式解析、插值并一次性替换注册表；Agent 中间件负责在生命周期
//! 边界执行 command、HTTP、prompt 与 agent 四类 Hook。

use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use parking_lot::{Mutex, RwLock};
use peri_agent::agent::react::{AgentOutput, ReactLLM, ToolCall, ToolResult};
use peri_agent::error::{AgentError, AgentResult};
use peri_agent::messages::BaseMessage;
use peri_agent::middleware::{r#trait::Middleware, state::MiddlewareState};
use peri_agent::session::{MessageSource, QueuedMessage};
use peri_agent::tools::{BaseTool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// 单个 Hook 标准输出允许保留的最大字节数。
const MAX_HOOK_OUTPUT_BYTES: usize = 64 * 1024;
/// HTTP Hook 响应体允许读取的最大字节数。
const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;
/// Command Hook 没有显式 timeout 时的默认秒数。
const DEFAULT_COMMAND_TIMEOUT_SECONDS: u64 = 60;
/// Prompt Hook 没有显式 timeout 时的默认秒数。
const DEFAULT_PROMPT_TIMEOUT_SECONDS: u64 = 30;
/// Agent Hook 没有显式 timeout 时的默认秒数。
const DEFAULT_AGENT_TIMEOUT_SECONDS: u64 = 60;
/// Agent Hook 允许执行的最大模型轮数。
const MAX_AGENT_HOOK_TURNS: usize = 50;

/// Hook LLM 工厂返回的统一模型实例。
pub type HookLlmFactory =
    Arc<dyn Fn(Option<&str>) -> Box<dyn ReactLLM + Send + Sync> + Send + Sync>;

/// Claude Code 当前公开的 Hook 生命周期名称。
///
/// 注册表仍允许未来事件名称通过，数组用于桌面层和测试校验当前实现边界。
pub const CLAUDE_HOOK_EVENTS: &[&str] = &[
    "Setup",
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PermissionDenied",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "TeammateIdle",
    "TaskCreated",
    "TaskCompleted",
    "PreCompact",
    "PostCompact",
    "Notification",
    "Elicitation",
    "ElicitationResult",
    "ConfigChange",
    "WorktreeCreate",
    "WorktreeRemove",
    "InstructionsLoaded",
    "CwdChanged",
    "FileChanged",
    "SessionEnd",
];

/// Claude 插件清单解析后的一条 Hook 注册记录。
#[derive(Clone, Debug)]
pub struct ClaudeHookRegistration {
    /// 热刷新之间保持稳定的 Hook 标识。
    pub id: String,
    /// 提供此 Hook 的完整插件标识。
    pub plugin_id: String,
    /// Claude 生命周期事件名称。
    pub event: String,
    /// Claude matcher 正则表达式；空值匹配全部。
    pub matcher: Option<String>,
    /// 已完成插件变量插值的 Hook 配置。
    pub command: ClaudeHookCommand,
    /// Hook 子进程继承的插件专用环境变量。
    pub environment: BTreeMap<String, String>,
}

/// Claude Code 支持的四类持久化 Hook。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClaudeHookCommand {
    /// 通过本机 Shell 执行命令。
    Command {
        /// 要交给 Shell 执行的命令文本。
        command: String,
        /// 可选的工具权限规则过滤表达式。
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        /// bash 或 powershell。
        #[serde(default)]
        shell: Option<String>,
        /// 单次执行超时秒数。
        #[serde(default)]
        timeout: Option<f64>,
        /// 当前 Session 中是否只执行一次。
        #[serde(default)]
        once: bool,
        /// 是否在后台执行且不阻塞当前流程。
        #[serde(default, rename = "async")]
        asynchronous: bool,
        /// 后台命令以状态码 2 退出时是否唤醒模型。
        #[serde(default, rename = "asyncRewake")]
        async_rewake: bool,
        /// 界面状态提示文本；当前只用于诊断日志。
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
    },
    /// 通过当前模型执行结构化判定提示。
    Prompt {
        /// `$ARGUMENTS` 会被替换为 Hook 输入 JSON。
        prompt: String,
        /// 可选的工具权限规则过滤表达式。
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        /// 单次执行超时秒数。
        #[serde(default)]
        timeout: Option<f64>,
        /// 可选模型标识；未声明时继承当前模型。
        #[serde(default)]
        model: Option<String>,
        /// 当前 Session 中是否只执行一次。
        #[serde(default)]
        once: bool,
        /// 界面状态提示文本；当前只用于诊断日志。
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
    },
    /// 向指定 HTTP 地址 POST Hook 输入 JSON。
    Http {
        /// HTTP Hook 目标 URL。
        url: String,
        /// 可选的工具权限规则过滤表达式。
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        /// 单次执行超时秒数。
        #[serde(default)]
        timeout: Option<f64>,
        /// 附加请求头。
        #[serde(default)]
        headers: BTreeMap<String, String>,
        /// 允许在请求头中展开的环境变量名。
        #[serde(default, rename = "allowedEnvVars")]
        allowed_env_vars: Vec<String>,
        /// 当前 Session 中是否只执行一次。
        #[serde(default)]
        once: bool,
        /// 界面状态提示文本；当前只用于诊断日志。
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
    },
    /// 运行一个可调用文件、终端和 MCP 工具的校验 Agent。
    Agent {
        /// `$ARGUMENTS` 会被替换为 Hook 输入 JSON。
        prompt: String,
        /// 可选的工具权限规则过滤表达式。
        #[serde(default, rename = "if")]
        if_condition: Option<String>,
        /// 单次执行超时秒数。
        #[serde(default)]
        timeout: Option<f64>,
        /// 可选模型标识；未声明时继承当前模型。
        #[serde(default)]
        model: Option<String>,
        /// 当前 Session 中是否只执行一次。
        #[serde(default)]
        once: bool,
        /// 界面状态提示文本；当前只用于诊断日志。
        #[serde(default, rename = "statusMessage")]
        status_message: Option<String>,
    },
}

impl ClaudeHookCommand {
    /// 返回此 Hook 是否声明为 Session 内只运行一次。
    fn once(&self) -> bool {
        match self {
            Self::Command { once, .. }
            | Self::Prompt { once, .. }
            | Self::Http { once, .. }
            | Self::Agent { once, .. } => *once,
        }
    }

    /// 返回工具调用级 `if` 过滤条件。
    fn if_condition(&self) -> Option<&str> {
        match self {
            Self::Command { if_condition, .. }
            | Self::Prompt { if_condition, .. }
            | Self::Http { if_condition, .. }
            | Self::Agent { if_condition, .. } => if_condition.as_deref(),
        }
    }
}

/// 全局热更新 Hook 注册表及 Session 一次性状态。
#[derive(Default)]
struct ClaudeHookRuntime {
    /// 当前启用插件提供的 Hook 快照。
    registrations: RwLock<Vec<ClaudeHookRegistration>>,
    /// 已执行 `once` Hook 的 `(session_id, hook_id)` 集合。
    once_executed: Mutex<HashSet<(String, String)>>,
    /// 已发送 Session 级生命周期事件的 `(session_id, event)` 集合。
    lifecycle_emitted: Mutex<HashSet<(String, String)>>,
}

/// 返回进程内唯一的 Claude Hook 运行时。
fn hook_runtime() -> &'static ClaudeHookRuntime {
    static RUNTIME: OnceLock<ClaudeHookRuntime> = OnceLock::new();
    RUNTIME.get_or_init(ClaudeHookRuntime::default)
}

/// 原子替换全部已启用插件 Hook；已有 Session 的 `once` 状态保留。
pub fn configure_claude_hooks(registrations: Vec<ClaudeHookRegistration>) {
    *hook_runtime().registrations.write() = registrations;
}

/// Hook 命令的有限输出。
#[derive(Debug, Default)]
struct CommandOutput {
    /// 进程退出码；被信号终止时为空。
    status: Option<i32>,
    /// 截断后的标准输出。
    stdout: String,
    /// 截断后的标准错误。
    stderr: String,
}

/// Claude Hook JSON 输出的兼容子集。
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookResponse {
    /// false 表示阻止当前生命周期继续。
    #[serde(default)]
    r#continue: Option<bool>,
    /// 是否隐藏普通输出；运行时仍保留错误日志。
    #[serde(default)]
    suppress_output: bool,
    /// `continue=false` 时返回给模型或用户的原因。
    #[serde(default)]
    stop_reason: Option<String>,
    /// approve/block 字符串，或 PermissionRequest 的结构化 decision。
    #[serde(default)]
    decision: Option<Value>,
    /// Hook 判定说明。
    #[serde(default)]
    reason: Option<String>,
    /// 注入当前上下文的系统消息。
    #[serde(default)]
    system_message: Option<String>,
    /// 生命周期专用输出，如 updatedInput 或 updatedMCPToolOutput。
    #[serde(default)]
    hook_specific_output: Option<Value>,
    /// Prompt/Agent Hook 的简化成功标志。
    #[serde(default)]
    ok: Option<bool>,
}

impl HookResponse {
    /// 返回顶层或 `hookSpecificOutput` 内的结构化判定字段。
    fn decision_value(&self) -> Option<&Value> {
        self.decision.as_ref().or_else(|| {
            self.hook_specific_output
                .as_ref()
                .and_then(Value::as_object)
                .and_then(|output| output.get("decision"))
        })
    }

    /// 返回此响应是否阻止当前生命周期。
    fn blocking_reason(&self) -> Option<String> {
        let decision_blocks = match self.decision_value() {
            Some(Value::String(value)) => {
                value.eq_ignore_ascii_case("block") || value.eq_ignore_ascii_case("deny")
            }
            Some(Value::Object(value)) => value
                .get("behavior")
                .and_then(Value::as_str)
                .is_some_and(|behavior| behavior.eq_ignore_ascii_case("deny")),
            _ => false,
        };
        let blocked = self.r#continue == Some(false) || decision_blocks || self.ok == Some(false);
        blocked.then(|| {
            self.stop_reason
                .clone()
                .or_else(|| self.reason.clone())
                .or_else(|| {
                    self.decision_value().and_then(|decision| {
                        decision
                            .as_object()
                            .and_then(|object| object.get("message"))
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                })
                .unwrap_or_else(|| "Claude 插件 Hook 阻止了当前操作".to_owned())
        })
    }

    /// 返回生命周期专用的附加上下文。
    fn additional_context(&self) -> Option<String> {
        self.hook_specific_output
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|output| output.get("additionalContext"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// 返回 PreToolUse 修改后的输入。
    fn updated_input(&self) -> Option<Value> {
        self.hook_specific_output
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|output| output.get("updatedInput"))
            .cloned()
            .or_else(|| {
                self.decision_value()
                    .and_then(Value::as_object)
                    .and_then(|decision| decision.get("updatedInput"))
                    .cloned()
            })
    }

    /// 返回 PostToolUse 修改后的 MCP 工具输出。
    fn updated_mcp_output(&self) -> Option<Value> {
        self.hook_specific_output
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|output| output.get("updatedMCPToolOutput"))
            .cloned()
    }
}

/// 每个 Agent 执行实例使用的 Claude Hook 中间件。
pub struct ClaudeHookMiddleware {
    /// 当前根 Session 标识。
    session_id: String,
    /// 当前会话转录文件路径；无独立 JSONL 时允许为空。
    transcript_path: String,
    /// Hook Prompt/Agent 使用的模型工厂。
    llm_factory: HookLlmFactory,
    /// 当前 Agent 可执行工具的共享注册表。
    tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
}

impl ClaudeHookMiddleware {
    /// 创建绑定当前 Session、模型和工具注册表的 Hook 中间件。
    pub fn new(
        session_id: impl Into<String>,
        transcript_path: impl Into<String>,
        llm_factory: HookLlmFactory,
        tools: Arc<RwLock<BTreeMap<String, Arc<dyn BaseTool>>>>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            transcript_path: transcript_path.into(),
            llm_factory,
            tools,
        }
    }

    /// 构造所有 Hook 共享的基础输入字段。
    fn base_input(&self, event: &str, cwd: &str) -> Map<String, Value> {
        Map::from_iter([
            (
                "session_id".to_owned(),
                Value::String(self.session_id.clone()),
            ),
            (
                "transcript_path".to_owned(),
                Value::String(self.transcript_path.clone()),
            ),
            ("cwd".to_owned(), Value::String(cwd.to_owned())),
            (
                "hook_event_name".to_owned(),
                Value::String(event.to_owned()),
            ),
        ])
    }

    /// Session 级事件在同一 Session 中只发送一次。
    fn begin_lifecycle_once(&self, event: &str) -> bool {
        hook_runtime()
            .lifecycle_emitted
            .lock()
            .insert((self.session_id.clone(), event.to_owned()))
    }

    /// 执行一个事件下所有匹配 Hook，并返回响应列表。
    async fn execute_event(
        &self,
        state: &mut dyn MiddlewareState,
        event: &str,
        match_value: &str,
        input: Value,
    ) -> AgentResult<Vec<HookResponse>> {
        let registrations = hook_runtime().registrations.read().clone();
        let mut responses = Vec::new();
        for registration in registrations {
            if registration.event != event
                || !matcher_matches(registration.matcher.as_deref(), match_value)
                || !if_condition_matches(registration.command.if_condition(), &input)
            {
                continue;
            }
            if registration.command.once()
                && !hook_runtime()
                    .once_executed
                    .lock()
                    .insert((self.session_id.clone(), registration.id.clone()))
            {
                continue;
            }
            if let Some(response) = self
                .execute_registration(state, &registration, input.clone())
                .await?
            {
                if !response.suppress_output {
                    if let Some(message) = response
                        .system_message
                        .clone()
                        .or_else(|| response.additional_context())
                    {
                        state.add_message(BaseMessage::system(message));
                    }
                }
                responses.push(response);
            }
        }
        Ok(responses)
    }

    /// 执行一条具体 Hook 注册记录。
    async fn execute_registration(
        &self,
        state: &mut dyn MiddlewareState,
        registration: &ClaudeHookRegistration,
        input: Value,
    ) -> AgentResult<Option<HookResponse>> {
        let arguments = serde_json::to_string(&input).map_err(AgentError::SerializationError)?;
        match &registration.command {
            ClaudeHookCommand::Command {
                command,
                shell,
                timeout,
                asynchronous,
                async_rewake,
                ..
            } => {
                let command = command.replace("$ARGUMENTS", &arguments);
                let duration = seconds_or_default(*timeout, DEFAULT_COMMAND_TIMEOUT_SECONDS);
                if *asynchronous || *async_rewake {
                    let cwd = PathBuf::from(state.cwd());
                    let shell = shell.clone();
                    let environment = registration.environment.clone();
                    let queue = state.v2_queue().clone();
                    let hook_id = registration.id.clone();
                    let rewake = *async_rewake;
                    tokio::spawn(async move {
                        match execute_command_hook(
                            &command,
                            shell.as_deref(),
                            &cwd,
                            &environment,
                            &arguments,
                            duration,
                        )
                        .await
                        {
                            Ok(output) if rewake && output.status == Some(2) => {
                                let reason = preferred_command_message(&output);
                                queue.push(QueuedMessage::defer(
                                    MessageSource::SystemInjected,
                                    BaseMessage::human(format!(
                                        "异步 Claude Hook {hook_id} 阻止了停止：{reason}"
                                    )),
                                ));
                            }
                            Ok(_) => {}
                            Err(error) => tracing::warn!(
                                hook_id = %hook_id,
                                error = %error,
                                "异步 Claude Hook 执行失败"
                            ),
                        }
                    });
                    return Ok(None);
                }
                let output = execute_command_hook(
                    &command,
                    shell.as_deref(),
                    &PathBuf::from(state.cwd()),
                    &registration.environment,
                    &arguments,
                    duration,
                )
                .await
                .map_err(|reason| hook_error(&registration.id, reason))?;
                command_output_to_response(&registration.id, output)
            }
            ClaudeHookCommand::Http {
                url,
                timeout,
                headers,
                allowed_env_vars,
                ..
            } => {
                let duration = seconds_or_default(*timeout, DEFAULT_COMMAND_TIMEOUT_SECONDS);
                let headers = resolve_allowed_headers(headers, allowed_env_vars);
                let body = execute_http_hook(url, &headers, input, duration)
                    .await
                    .map_err(|reason| hook_error(&registration.id, reason))?;
                parse_hook_response(&body).map(Some).map_err(|reason| {
                    hook_error(&registration.id, format!("HTTP 响应无效：{reason}"))
                })
            }
            ClaudeHookCommand::Prompt {
                prompt,
                timeout,
                model,
                ..
            } => {
                let prompt = prompt.replace("$ARGUMENTS", &arguments);
                let duration = seconds_or_default(*timeout, DEFAULT_PROMPT_TIMEOUT_SECONDS);
                let llm = (self.llm_factory)(model.as_deref());
                let text = tokio::time::timeout(duration, run_prompt_hook(llm, prompt))
                    .await
                    .map_err(|_| hook_error(&registration.id, "Prompt Hook 执行超时"))?
                    .map_err(|reason| hook_error(&registration.id, reason))?;
                parse_hook_response(&text).map(Some).map_err(|reason| {
                    hook_error(&registration.id, format!("Prompt Hook 输出无效：{reason}"))
                })
            }
            ClaudeHookCommand::Agent {
                prompt,
                timeout,
                model,
                ..
            } => {
                let prompt = prompt.replace("$ARGUMENTS", &arguments);
                let duration = seconds_or_default(*timeout, DEFAULT_AGENT_TIMEOUT_SECONDS);
                let llm = (self.llm_factory)(model.as_deref());
                let tools = self.tools.read().clone();
                let cwd = state.cwd().to_owned();
                let text = tokio::time::timeout(duration, run_agent_hook(llm, prompt, cwd, tools))
                    .await
                    .map_err(|_| hook_error(&registration.id, "Agent Hook 执行超时"))?
                    .map_err(|reason| hook_error(&registration.id, reason))?;
                parse_hook_response(&text).map(Some).map_err(|reason| {
                    hook_error(&registration.id, format!("Agent Hook 输出无效：{reason}"))
                })
            }
        }
    }

    /// 将阻塞响应转换为中间件错误。
    fn reject_if_blocked(event: &str, responses: &[HookResponse]) -> AgentResult<()> {
        if let Some(reason) = responses.iter().find_map(HookResponse::blocking_reason) {
            return Err(AgentError::MiddlewareError {
                middleware: "ClaudeHookMiddleware".to_owned(),
                reason: format!("{event} Hook 阻止执行：{reason}"),
            });
        }
        Ok(())
    }
}

#[async_trait]
impl Middleware for ClaudeHookMiddleware {
    /// 返回用于日志和调试的中间件名称。
    fn name(&self) -> &str {
        "ClaudeHookMiddleware"
    }

    /// 在本次任务第一次模型调用前执行 Setup、SessionStart 与 UserPromptSubmit。
    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        for event in ["Setup", "SessionStart"] {
            if self.begin_lifecycle_once(event) {
                let input = Value::Object(self.base_input(event, state.cwd()));
                let responses = self.execute_event(state, event, "", input).await?;
                Self::reject_if_blocked(event, &responses)?;
            }
        }
        let prompt = state
            .messages()
            .iter()
            .rev()
            .find(|message| matches!(message, BaseMessage::Human { .. }))
            .map(BaseMessage::content)
            .unwrap_or_default();
        let mut input = self.base_input("UserPromptSubmit", state.cwd());
        input.insert("prompt".to_owned(), Value::String(prompt));
        let responses = self
            .execute_event(state, "UserPromptSubmit", "", Value::Object(input))
            .await?;
        Self::reject_if_blocked("UserPromptSubmit", &responses)
    }

    /// 在 ACP Session 生命周期开始时触发 Setup 与 SessionStart。
    async fn on_session_start(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        for event in ["Setup", "SessionStart"] {
            if self.begin_lifecycle_once(event) {
                let input = Value::Object(self.base_input(event, state.cwd()));
                let responses = self.execute_event(state, event, "", input).await?;
                Self::reject_if_blocked(event, &responses)?;
            }
        }
        Ok(())
    }

    /// 在用户输入钩子可用时优先使用框架传入的原始 prompt。
    async fn on_user_prompt(
        &self,
        state: &mut dyn MiddlewareState,
        prompt: &str,
    ) -> AgentResult<()> {
        let mut input = self.base_input("UserPromptSubmit", state.cwd());
        input.insert("prompt".to_owned(), Value::String(prompt.to_owned()));
        let responses = self
            .execute_event(state, "UserPromptSubmit", "", Value::Object(input))
            .await?;
        Self::reject_if_blocked("UserPromptSubmit", &responses)
    }

    /// Session 销毁时执行 Claude SessionEnd Hooks。
    async fn on_session_end(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        let input = Value::Object(self.base_input("SessionEnd", state.cwd()));
        let responses = self.execute_event(state, "SessionEnd", "", input).await?;
        Self::reject_if_blocked("SessionEnd", &responses)
    }

    /// 在工具调用前执行 PreToolUse，并允许 Hook 修改输入或阻止调用。
    async fn before_tool(
        &self,
        state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
    ) -> AgentResult<ToolCall> {
        // KeenCode 没有交互式权限审批分支，但 Claude PermissionRequest Hook
        // 仍可自动 allow/deny 工具。deny 会补发 PermissionDenied，保持插件
        // 的审计与清理逻辑可用。
        let mut permission_input = self.base_input("PermissionRequest", state.cwd());
        permission_input.insert(
            "tool_name".to_owned(),
            Value::String(tool_call.name.clone()),
        );
        permission_input.insert("tool_input".to_owned(), tool_call.input.clone());
        permission_input.insert(
            "tool_use_id".to_owned(),
            Value::String(tool_call.id.clone()),
        );
        let permission_responses = self
            .execute_event(
                state,
                "PermissionRequest",
                &tool_call.name,
                Value::Object(permission_input.clone()),
            )
            .await?;
        if let Some(reason) = permission_responses
            .iter()
            .find_map(HookResponse::blocking_reason)
        {
            let mut denied_input = self.base_input("PermissionDenied", state.cwd());
            denied_input.insert(
                "tool_name".to_owned(),
                Value::String(tool_call.name.clone()),
            );
            denied_input.insert("tool_input".to_owned(), tool_call.input.clone());
            denied_input.insert("reason".to_owned(), Value::String(reason.clone()));
            let denied = self
                .execute_event(
                    state,
                    "PermissionDenied",
                    &tool_call.name,
                    Value::Object(denied_input),
                )
                .await?;
            Self::reject_if_blocked("PermissionDenied", &denied)?;
            return Err(AgentError::MiddlewareError {
                middleware: "ClaudeHookMiddleware".to_owned(),
                reason: format!(
                    "PermissionRequest Hook 阻止工具 {}：{reason}",
                    tool_call.name
                ),
            });
        }
        let mut updated = tool_call.clone();
        for response in &permission_responses {
            if let Some(input) = response.updated_input() {
                updated.input = input;
            }
        }
        let mut input = self.base_input("PreToolUse", state.cwd());
        input.insert("tool_name".to_owned(), Value::String(updated.name.clone()));
        input.insert("tool_input".to_owned(), updated.input.clone());
        input.insert("tool_use_id".to_owned(), Value::String(updated.id.clone()));
        let responses = self
            .execute_event(state, "PreToolUse", &updated.name, Value::Object(input))
            .await?;
        Self::reject_if_blocked("PreToolUse", &responses)?;
        for response in responses {
            if let Some(input) = response.updated_input() {
                updated.input = input;
            }
        }
        Ok(updated)
    }

    /// 在工具调用后执行 PostToolUse/PostToolUseFailure，并链式更新 MCP 输出。
    async fn after_tool(
        &self,
        state: &mut dyn MiddlewareState,
        tool_call: &ToolCall,
        result: &ToolResult,
    ) -> AgentResult<ToolResult> {
        let event = if result.is_error {
            "PostToolUseFailure"
        } else {
            "PostToolUse"
        };
        let mut input = self.base_input(event, state.cwd());
        input.insert(
            "tool_name".to_owned(),
            Value::String(tool_call.name.clone()),
        );
        input.insert("tool_input".to_owned(), tool_call.input.clone());
        input.insert(
            "tool_use_id".to_owned(),
            Value::String(tool_call.id.clone()),
        );
        input.insert(
            "tool_response".to_owned(),
            Value::String(result.output.clone()),
        );
        let responses = self
            .execute_event(state, event, &tool_call.name, Value::Object(input))
            .await?;
        Self::reject_if_blocked(event, &responses)?;
        let mut updated = result.clone();
        for response in responses {
            if let Some(output) = response.updated_mcp_output() {
                updated.output = match output {
                    Value::String(text) => text,
                    value => {
                        serde_json::to_string(&value).unwrap_or_else(|_| updated.output.clone())
                    }
                };
            }
        }
        Ok(updated)
    }

    /// 在最终答案生成后执行 Stop；阻塞时通过 Defer 让模型继续处理原因。
    async fn after_agent(
        &self,
        state: &mut dyn MiddlewareState,
        output: &AgentOutput,
    ) -> AgentResult<AgentOutput> {
        let mut input = self.base_input("Stop", state.cwd());
        input.insert(
            "last_assistant_message".to_owned(),
            Value::String(output.text.clone()),
        );
        let responses = self
            .execute_event(state, "Stop", "", Value::Object(input))
            .await?;
        if let Some(reason) = responses.iter().find_map(HookResponse::blocking_reason) {
            state.v2_queue().push(QueuedMessage::defer(
                MessageSource::SystemInjected,
                BaseMessage::human(format!("Stop Hook 要求继续处理：{reason}")),
            ));
        }
        Ok(output.clone())
    }

    /// 在 Compact 前执行 PreCompact。
    async fn before_compact(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        let input = Value::Object(self.base_input("PreCompact", state.cwd()));
        let responses = self.execute_event(state, "PreCompact", "", input).await?;
        Self::reject_if_blocked("PreCompact", &responses)
    }

    /// 在 Compact 后执行 PostCompact。
    async fn after_compact(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        let input = Value::Object(self.base_input("PostCompact", state.cwd()));
        let responses = self.execute_event(state, "PostCompact", "", input).await?;
        Self::reject_if_blocked("PostCompact", &responses)
    }

    /// 在子智能体启动时执行 SubagentStart。
    async fn on_subagent_start(
        &self,
        state: &mut dyn MiddlewareState,
        agent_id: &str,
        name: &str,
    ) -> AgentResult<()> {
        let mut input = self.base_input("SubagentStart", state.cwd());
        input.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
        input.insert("agent_type".to_owned(), Value::String(name.to_owned()));
        let responses = self
            .execute_event(state, "SubagentStart", name, Value::Object(input))
            .await?;
        Self::reject_if_blocked("SubagentStart", &responses)
    }

    /// 在子智能体停止时执行 SubagentStop。
    async fn on_subagent_stop(
        &self,
        state: &mut dyn MiddlewareState,
        agent_id: &str,
        reason: &str,
    ) -> AgentResult<()> {
        let mut input = self.base_input("SubagentStop", state.cwd());
        input.insert("agent_id".to_owned(), Value::String(agent_id.to_owned()));
        input.insert("reason".to_owned(), Value::String(reason.to_owned()));
        let responses = self
            .execute_event(state, "SubagentStop", agent_id, Value::Object(input))
            .await?;
        Self::reject_if_blocked("SubagentStop", &responses)
    }

    /// 在通知事件到达时执行 Notification。
    async fn on_notification(
        &self,
        state: &mut dyn MiddlewareState,
        message: &str,
    ) -> AgentResult<()> {
        let mut input = self.base_input("Notification", state.cwd());
        input.insert("message".to_owned(), Value::String(message.to_owned()));
        let responses = self
            .execute_event(state, "Notification", message, Value::Object(input))
            .await?;
        Self::reject_if_blocked("Notification", &responses)
    }

    /// 接收宿主转发的 Claude 扩展事件。
    ///
    /// 事件名不在这里硬编码拒绝，未知未来事件仍会按注册表执行；当前
    /// `CLAUDE_HOOK_EVENTS` 常量供调用方校验。所有输入都会补齐会话元数据。
    async fn on_claude_event(
        &self,
        state: &mut dyn MiddlewareState,
        event: &str,
        input: &Value,
    ) -> AgentResult<()> {
        let mut payload = self.base_input(event, state.cwd());
        if let Value::Object(fields) = input {
            payload.extend(fields.clone());
        } else {
            payload.insert("data".to_owned(), input.clone());
        }
        payload.insert(
            "hook_event_name".to_owned(),
            Value::String(event.to_owned()),
        );
        let match_value = payload
            .get("tool_name")
            .or_else(|| payload.get("name"))
            .or_else(|| payload.get("path"))
            .or_else(|| payload.get("server_name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let responses = self
            .execute_event(state, event, &match_value, Value::Object(payload))
            .await?;
        Self::reject_if_blocked(event, &responses)
    }

    /// 在运行时错误发生后执行 StopFailure，Hook 自身错误只写诊断日志。
    async fn on_error(
        &self,
        state: &mut dyn MiddlewareState,
        error: &AgentError,
    ) -> AgentResult<()> {
        let mut input = self.base_input("StopFailure", state.cwd());
        input.insert("error".to_owned(), Value::String(error.to_string()));
        let responses = self
            .execute_event(state, "StopFailure", "", Value::Object(input))
            .await?;
        Self::reject_if_blocked("StopFailure", &responses)
    }
}

/// 把可选秒数转换为正数 Duration，否则使用默认值。
fn seconds_or_default(seconds: Option<f64>, default_seconds: u64) -> Duration {
    let seconds = seconds.filter(|value| value.is_finite() && *value > 0.0);
    seconds
        .map(Duration::from_secs_f64)
        .unwrap_or_else(|| Duration::from_secs(default_seconds))
}

/// 将 Hook 执行失败包装成统一中间件错误。
fn hook_error(hook_id: &str, reason: impl Into<String>) -> AgentError {
    AgentError::MiddlewareError {
        middleware: "ClaudeHookMiddleware".to_owned(),
        reason: format!("Hook {hook_id}：{}", reason.into()),
    }
}

/// 判断 Claude matcher 是否匹配当前事件目标。
fn matcher_matches(matcher: Option<&str>, value: &str) -> bool {
    let Some(matcher) = matcher.map(str::trim).filter(|matcher| !matcher.is_empty()) else {
        return true;
    };
    if matcher == "*" {
        return true;
    }
    regex::Regex::new(matcher)
        .map(|pattern| pattern.is_match(value))
        .unwrap_or(false)
}

/// 判断工具权限规则形式的 `if` 条件是否匹配 Hook 输入。
fn if_condition_matches(condition: Option<&str>, input: &Value) -> bool {
    let Some(condition) = condition.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let tool_name = input
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some((expected_tool, pattern)) = condition
        .strip_suffix(')')
        .and_then(|value| value.split_once('('))
    else {
        return false;
    };
    if !tool_name.eq_ignore_ascii_case(expected_tool)
        && !(expected_tool.eq_ignore_ascii_case("Bash")
            && tool_name.eq_ignore_ascii_case("Terminal"))
    {
        return false;
    }
    let candidate = input
        .get("tool_input")
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            input
                .get("tool_input")
                .and_then(Value::as_str)
                .unwrap_or_default()
        });
    glob::Pattern::new(pattern)
        .map(|rule| rule.matches(candidate))
        .unwrap_or(false)
}

/// 执行 Command Hook 并限制运行时间与输出大小。
async fn execute_command_hook(
    command: &str,
    shell: Option<&str>,
    cwd: &PathBuf,
    environment: &BTreeMap<String, String>,
    stdin_json: &str,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut process = build_shell_command(command, shell)?;
    process
        .current_dir(cwd)
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = process
        .spawn()
        .map_err(|error| format!("无法启动 Hook 子进程：{error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_json.as_bytes())
            .await
            .map_err(|error| format!("无法写入 Hook stdin：{error}"))?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Hook 子进程缺少 stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Hook 子进程缺少 stderr".to_owned())?;
    let stdout_task = tokio::spawn(read_stream_limited(stdout, MAX_HOOK_OUTPUT_BYTES));
    let stderr_task = tokio::spawn(read_stream_limited(stderr, MAX_HOOK_OUTPUT_BYTES));
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result.map_err(|error| format!("等待 Hook 子进程失败：{error}"))?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(format!(
                "Hook 子进程在 {:.1} 秒后超时",
                timeout.as_secs_f64()
            ));
        }
    };
    let stdout = stdout_task
        .await
        .map_err(|error| format!("读取 Hook stdout 任务失败：{error}"))??;
    let stderr = stderr_task
        .await
        .map_err(|error| format!("读取 Hook stderr 任务失败：{error}"))??;
    Ok(CommandOutput {
        status: status.code(),
        stdout,
        stderr,
    })
}

/// 按当前平台构造 Claude Hook Shell 命令。
fn build_shell_command(
    command: &str,
    shell: Option<&str>,
) -> Result<tokio::process::Command, String> {
    let requested = shell.unwrap_or("bash");
    if requested != "bash" && requested != "powershell" {
        return Err(format!("不支持的 Hook shell：{requested}"));
    }
    #[cfg(windows)]
    {
        let mut process = if requested == "powershell" {
            tokio::process::Command::new("pwsh")
        } else {
            let executable = std::env::var_os("SHELL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("bash"));
            tokio::process::Command::new(executable)
        };
        if requested == "powershell" {
            process.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command,
            ]);
        } else {
            process.args(["-lc", command]);
        }
        Ok(process)
    }
    #[cfg(not(windows))]
    {
        let mut process = if requested == "powershell" {
            tokio::process::Command::new("pwsh")
        } else {
            let executable = std::env::var_os("SHELL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/bin/sh"));
            tokio::process::Command::new(executable)
        };
        if requested == "powershell" {
            process.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command,
            ]);
        } else {
            process.args(["-lc", command]);
        }
        Ok(process)
    }
}

/// 持续排空输出流，只保留前 `limit` 字节并标记截断。
async fn read_stream_limited<R>(mut reader: R, limit: usize) -> Result<String, String>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| format!("读取 Hook 输出失败：{error}"))?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        let keep = remaining.min(read);
        retained.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    let mut text = String::from_utf8_lossy(&retained).into_owned();
    if truncated {
        text.push_str("\n[Hook 输出已截断]");
    }
    Ok(text)
}

/// 选择 Command Hook 最有用的阻塞说明。
fn preferred_command_message(output: &CommandOutput) -> String {
    let stderr = output.stderr.trim();
    let stdout = output.stdout.trim();
    if !stderr.is_empty() {
        stderr.to_owned()
    } else if !stdout.is_empty() {
        stdout.to_owned()
    } else {
        "Hook 以状态码 2 退出".to_owned()
    }
}

/// 将 Command Hook 退出状态和 stdout 转换为 Claude Hook 响应。
fn command_output_to_response(
    hook_id: &str,
    output: CommandOutput,
) -> AgentResult<Option<HookResponse>> {
    match output.status {
        Some(0) => {
            if output.stdout.trim().is_empty() {
                return Ok(None);
            }
            parse_hook_response(&output.stdout)
                .map(Some)
                .map_err(|reason| hook_error(hook_id, format!("stdout JSON 无效：{reason}")))
        }
        Some(2) => Ok(Some(HookResponse {
            r#continue: Some(false),
            reason: Some(preferred_command_message(&output)),
            ..HookResponse::default()
        })),
        status => {
            tracing::warn!(
                hook_id,
                ?status,
                stderr = %output.stderr.trim(),
                "Claude Command Hook 非阻塞失败"
            );
            Ok(None)
        }
    }
}

/// 只对显式允许的环境变量执行 HTTP Header 插值。
fn resolve_allowed_headers(
    headers: &BTreeMap<String, String>,
    allowed: &[String],
) -> BTreeMap<String, String> {
    let allowed = allowed.iter().collect::<HashSet<_>>();
    headers
        .iter()
        .map(|(name, value)| {
            let mut resolved = value.clone();
            for variable in &allowed {
                let replacement = std::env::var(variable).unwrap_or_default();
                resolved = resolved.replace(&format!("${{{variable}}}"), &replacement);
                resolved = resolved.replace(&format!("${variable}"), &replacement);
            }
            (name.clone(), resolved)
        })
        .collect()
}

/// 执行带 SSRF 地址校验、禁止重定向和响应大小限制的 HTTP Hook。
async fn execute_http_hook(
    url: &str,
    headers: &BTreeMap<String, String>,
    input: Value,
    timeout: Duration,
) -> Result<String, String> {
    validate_http_hook_target(url).await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(timeout.min(Duration::from_secs(10)))
        .timeout(timeout)
        .build()
        .map_err(|error| format!("无法创建 HTTP Hook 客户端：{error}"))?;
    let mut request = client.post(url).json(&input);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("HTTP Hook 请求失败：{error}"))?;
    if response.status().is_redirection() {
        return Err("HTTP Hook 禁止跟随重定向".to_owned());
    }
    if !response.status().is_success() {
        return Err(format!("HTTP Hook 返回状态码 {}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HTTP_RESPONSE_BYTES as u64)
    {
        return Err("HTTP Hook 响应超过 1 MiB 限制".to_owned());
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取 HTTP Hook 响应失败：{error}"))?;
        if body.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES {
            return Err("HTTP Hook 响应超过 1 MiB 限制".to_owned());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| "HTTP Hook 响应不是 UTF-8".to_owned())
}

/// 校验 HTTP Hook 目标，允许公网和 loopback，拒绝其他本机/内网地址。
async fn validate_http_hook_target(raw: &str) -> Result<(), String> {
    let url = url::Url::parse(raw).map_err(|error| format!("HTTP Hook URL 无效：{error}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("HTTP Hook 只允许 http:// 或 https://".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("HTTP Hook URL 不能包含用户凭据".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "HTTP Hook URL 缺少主机名".to_owned())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "HTTP Hook URL 缺少有效端口".to_owned())?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("HTTP Hook DNS 解析失败：{error}"))?
        .map(|address| address.ip())
        .collect::<HashSet<_>>();
    if addresses.is_empty() {
        return Err("HTTP Hook DNS 未返回地址".to_owned());
    }
    if addresses.iter().any(|address| !allowed_hook_ip(*address)) {
        return Err("HTTP Hook 目标解析到被禁止的内网、链路本地或保留地址".to_owned());
    }
    Ok(())
}

/// 判断一个 HTTP Hook IP 是否为公网或显式允许的 loopback。
fn allowed_hook_ip(address: IpAddr) -> bool {
    if address.is_loopback() {
        return true;
    }
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            !address.is_private()
                && !address.is_link_local()
                && !address.is_multicast()
                && !address.is_broadcast()
                && !address.is_unspecified()
                && octets[0] != 0
                && !(octets[0] == 100 && (64..=127).contains(&octets[1]))
                && !(octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                && !(octets[0] >= 240)
        }
        IpAddr::V6(address) => {
            !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_unique_local()
                && !address.is_unicast_link_local()
        }
    }
}

/// 使用无工具模型调用执行 Prompt Hook。
async fn run_prompt_hook(
    llm: Box<dyn ReactLLM + Send + Sync>,
    prompt: String,
) -> Result<String, String> {
    let messages = vec![BaseMessage::human(prompt)];
    let reasoning = llm
        .generate_reasoning(&messages, &[], None)
        .await
        .map_err(|error| format!("Prompt Hook 模型调用失败：{error}"))?;
    Ok(reasoning
        .final_answer
        .unwrap_or_else(|| reasoning.thought)
        .trim()
        .to_owned())
}

/// 使用文件、终端和 MCP 工具执行最多 50 轮 Agent Hook。
async fn run_agent_hook(
    llm: Box<dyn ReactLLM + Send + Sync>,
    prompt: String,
    cwd: String,
    tools: BTreeMap<String, Arc<dyn BaseTool>>,
) -> Result<String, String> {
    let tools = tools
        .into_iter()
        .filter(|(name, _)| name != "Agent" && name != "AgentResult")
        .collect::<BTreeMap<_, _>>();
    let tool_refs = tools
        .values()
        .map(|tool| tool.as_ref() as &dyn BaseTool)
        .collect::<Vec<_>>();
    let mut messages = vec![BaseMessage::human(prompt)];
    for _ in 0..MAX_AGENT_HOOK_TURNS {
        let reasoning = llm
            .generate_reasoning(&messages, &tool_refs, None)
            .await
            .map_err(|error| format!("Agent Hook 模型调用失败：{error}"))?;
        let ai_message = reasoning.source_message.clone().unwrap_or_else(|| {
            BaseMessage::ai(
                reasoning
                    .final_answer
                    .clone()
                    .unwrap_or_else(|| reasoning.thought.clone()),
            )
        });
        messages.push(ai_message);
        if reasoning.tool_calls.is_empty() {
            return Ok(reasoning
                .final_answer
                .unwrap_or_else(|| reasoning.thought)
                .trim()
                .to_owned());
        }
        for call in reasoning.tool_calls {
            let result = match tools.get(&call.name) {
                Some(tool) => {
                    let invocation =
                        tool.invoke(call.input.clone(), ToolContext::new(&messages, &cwd));
                    let result = match tool.timeout() {
                        Some(timeout) => tokio::time::timeout(timeout, invocation)
                            .await
                            .map_err(|_| "工具执行超时".to_owned())?,
                        None => invocation.await,
                    };
                    match result {
                        Ok(output) => BaseMessage::tool_result(call.id.clone(), output),
                        Err(error) => BaseMessage::tool_error(call.id.clone(), error.to_string()),
                    }
                }
                None => BaseMessage::tool_error(
                    call.id.clone(),
                    format!("Agent Hook 找不到工具 {}", call.name),
                ),
            };
            messages.push(result);
        }
    }
    Err("Agent Hook 超过 50 轮限制".to_owned())
}

/// 解析 Hook JSON；同时接受 Markdown JSON 代码块和 `{ok, reason}` 简化响应。
fn parse_hook_response(raw: &str) -> Result<HookResponse, String> {
    let trimmed = raw.trim();
    let json_text = if let Some(body) = trimmed.strip_prefix("```json") {
        body.strip_suffix("```").unwrap_or(body).trim()
    } else if let Some(body) = trimmed.strip_prefix("```") {
        body.strip_suffix("```").unwrap_or(body).trim()
    } else {
        trimmed
    };
    if json_text.is_empty() {
        return Ok(HookResponse::default());
    }
    serde_json::from_str(json_text).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// loopback 必须允许，内网与公网判断必须保持明确。
    #[test]
    fn hook_ip_policy_allows_loopback_and_public_only() {
        assert!(allowed_hook_ip("127.0.0.1".parse().unwrap()));
        assert!(allowed_hook_ip("::1".parse().unwrap()));
        assert!(allowed_hook_ip("8.8.8.8".parse().unwrap()));
        assert!(!allowed_hook_ip("10.0.0.1".parse().unwrap()));
        assert!(!allowed_hook_ip("169.254.169.254".parse().unwrap()));
    }

    /// Claude 的简化 Prompt 响应必须映射为阻塞原因。
    #[test]
    fn prompt_response_supports_ok_reason_shape() {
        let response = parse_hook_response(r#"{"ok":false,"reason":"tests failed"}"#).unwrap();
        assert_eq!(response.blocking_reason().as_deref(), Some("tests failed"));
    }

    /// updatedMCPToolOutput 必须可以从专用输出中提取。
    #[test]
    fn response_extracts_updated_mcp_output() {
        let response = parse_hook_response(
            r#"{"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedMCPToolOutput":{"ok":true}}}"#,
        )
        .unwrap();
        assert_eq!(
            response.updated_mcp_output(),
            Some(serde_json::json!({"ok": true}))
        );
    }

    /// PermissionRequest 的结构化 deny/updatedInput 不能被旧的字符串字段解析吞掉。
    #[test]
    fn permission_response_supports_deny_and_updated_input() {
        let response = parse_hook_response(
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":"no"}}}"#,
        )
        .unwrap();
        assert_eq!(response.blocking_reason().as_deref(), Some("no"));

        let updated = parse_hook_response(
            r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow","updatedInput":{"path":"/tmp"}}}}"#,
        )
        .unwrap();
        assert_eq!(
            updated.updated_input(),
            Some(serde_json::json!({"path":"/tmp"}))
        );
    }

    /// 常量覆盖 Claude Code 当前公布的完整 Hook 生命周期集合。
    #[test]
    fn declares_complete_claude_hook_event_set() {
        for event in [
            "PermissionRequest",
            "PermissionDenied",
            "TeammateIdle",
            "TaskCreated",
            "TaskCompleted",
            "Elicitation",
            "ElicitationResult",
            "ConfigChange",
            "WorktreeCreate",
            "WorktreeRemove",
            "InstructionsLoaded",
            "CwdChanged",
            "FileChanged",
        ] {
            assert!(CLAUDE_HOOK_EVENTS.contains(&event), "missing {event}");
        }
    }
}
