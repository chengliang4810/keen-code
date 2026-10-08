use crate::{subagent::SubagentTool, AgentRuntime, EventBridge, ResolvedSubagent};
use rcode_agent::{
    AgentTool, InMemoryRuntimeState, RuntimeStateError, SessionId, TodoChange, TodoController,
    TodoItem, TodoSnapshot, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture,
    ToolOutput, ToolRegistry,
};
use rcode_model::ToolDefinition;
use rcode_tools::{
    BackgroundOutputCursor, BackgroundTaskManager, BoundedCommandRequest, TodoWriteTool,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub const BUSINESS_TOOL_NAMES: &[&str] = &[
    "todo_write",
    "run_subagent",
    "bash_background",
    "bash_logs",
    "bash_list",
    "bash_kill",
];

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ShellTarget {
    #[default]
    Local,
    Wsl {
        distro: String,
        cwd: String,
    },
}

pub struct BusinessToolOptions {
    pub session_id: String,
    pub root: PathBuf,
    pub output_directory: PathBuf,
    pub shell_target: ShellTarget,
    pub initial_todos: Vec<TodoItem>,
    pub subagents: Vec<ResolvedSubagent>,
    pub events: Arc<EventBridge>,
    pub plan_mode: bool,
}

impl AgentRuntime {
    pub fn background(&self, directory: &Path) -> Result<Arc<BackgroundTaskManager>, String> {
        let mut state = self.0.lock().map_err(|_| "Agent 状态锁不可用")?;
        if let Some(background) = &state.background {
            return Ok(background.clone());
        }
        let background =
            Arc::new(BackgroundTaskManager::new(directory, 64 * 1024).map_err(|e| e.to_string())?);
        state.background = Some(background.clone());
        Ok(background)
    }

    pub async fn shutdown_background(&self) -> Result<(), String> {
        let manager = self
            .0
            .lock()
            .map_err(|_| "Agent 状态锁不可用")?
            .background
            .clone();
        if let Some(manager) = manager {
            manager.shutdown().await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn release_session(&self, session: &str) {
        if let Ok(mut state) = self.0.lock() {
            for (id, cancellation) in state.runs.values() {
                if id == session {
                    cancellation.cancel();
                }
            }
            state.environments.retain(|(id, _), _| id != session);
            state.todos.remove(session);
            if let Some(background) = &state.background {
                if let Ok(tasks) = background.list_running() {
                    for task in tasks.into_iter().filter(|task| task.session_id == session) {
                        let _ = background.cancel(session, &task.task_id);
                    }
                }
            }
        }
    }

    pub fn register_business_tools(
        &self,
        registry: &mut ToolRegistry,
        options: BusinessToolOptions,
    ) -> Result<(), String> {
        if options.subagents.len() > 132 {
            return Err("子 Agent 总数超过上限".into());
        }
        let mut seen = std::collections::HashSet::new();
        for agent in &options.subagents {
            agent.template.validate()?;
            if !seen.insert(&agent.template.id) {
                return Err("子 Agent 标识不能重复".into());
            }
        }
        let todos = {
            let mut state = self.0.lock().map_err(|_| "Agent 状态锁不可用")?;
            if !state.todos.contains_key(&options.session_id) {
                let todos = Arc::new(InMemoryRuntimeState::new(
                    SessionId::new(&options.session_id).map_err(|e| e.to_string())?,
                ));
                todos
                    .replace_todos("initial", options.initial_todos)
                    .map_err(|e| e.to_string())?;
                if state.todos.len() >= 16 {
                    let active: std::collections::HashSet<_> =
                        state.runs.values().map(|(id, _)| id.clone()).collect();
                    state.todos.retain(|id, _| active.contains(id));
                }
                state.todos.insert(options.session_id.clone(), todos);
            }
            state.todos[&options.session_id].clone()
        };
        registry
            .register(Arc::new(NamedTodoTool(TodoWriteTool::new(Arc::new(
                TodoEvents {
                    session: options.session_id.clone(),
                    state: todos,
                    events: options.events,
                },
            )))))
            .map_err(|e| e.to_string())?;
        if !options.subagents.is_empty() {
            registry
                .register(Arc::new(SubagentTool {
                    runtime: self.clone(),
                    root: options.root.clone(),
                    agents: options.subagents,
                }))
                .map_err(|e| e.to_string())?;
        }
        let manager = self.background(&options.output_directory)?;
        for action in [
            BackgroundAction::Start,
            BackgroundAction::Logs,
            BackgroundAction::List,
            BackgroundAction::Kill,
        ] {
            if options.plan_mode
                && matches!(action, BackgroundAction::Start | BackgroundAction::Kill)
            {
                continue;
            }
            registry
                .register(Arc::new(BackgroundTool {
                    manager: manager.clone(),
                    root: options.root.clone(),
                    target: options.shell_target.clone(),
                    action,
                }))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

struct TodoEvents {
    session: String,
    state: Arc<InMemoryRuntimeState>,
    events: Arc<EventBridge>,
}
impl TodoController for TodoEvents {
    fn todo_snapshot(&self) -> Result<TodoSnapshot, RuntimeStateError> {
        self.state.todo_snapshot()
    }
    fn replace_todos(
        &self,
        id: &str,
        items: Vec<TodoItem>,
    ) -> Result<TodoChange, RuntimeStateError> {
        let change = self.state.replace_todos(id, items)?;
        self.events.emit(json!({"type":"todos","sessionId":self.session,"todos":change.submitted,"revision":change.current.revision})).map_err(RuntimeStateError::storage)?;
        Ok(change)
    }
}
struct NamedTodoTool(TodoWriteTool);
impl AgentTool for NamedTodoTool {
    fn definition(&self) -> ToolDefinition {
        let mut definition = self.0.definition();
        definition.name = "todo_write".into();
        definition
    }
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        self.0.effect(input)
    }
    fn concurrency(&self) -> ToolConcurrency {
        self.0.concurrency()
    }
    fn execute(&self, ctx: ToolContext, input: Value) -> ToolFuture<'_> {
        self.0.execute(ctx, input)
    }
}

#[derive(Clone, Copy)]
enum BackgroundAction {
    Start,
    Logs,
    List,
    Kill,
}
struct BackgroundTool {
    manager: Arc<BackgroundTaskManager>,
    root: PathBuf,
    target: ShellTarget,
    action: BackgroundAction,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {
    command: String,
    cwd: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleInput {
    handle: String,
    since_offset: Option<u64>,
    stderr_offset: Option<u64>,
}
impl AgentTool for BackgroundTool {
    fn definition(&self) -> ToolDefinition {
        let(name,description,properties,required)=match self.action {
            BackgroundAction::Start=>("bash_background","Start a supervised background command and return its handle. Use bash_list before starting a dev server, bash_logs to read output, and bash_kill to stop the process tree.",json!({"command":{"type":"string","minLength":1,"maxLength":65536},"cwd":{"type":"string"}}),vec!["command"]),
            BackgroundAction::Logs=>("bash_logs","Read up to 64 KiB of background stdout/stderr. Each stream retains its first 4 MiB; dropped reports discarded bytes. Pass the returned next_offset and stderr_offset to read the next page.",json!({"handle":{"type":"string","minLength":1,"maxLength":256},"since_offset":{"type":"integer","minimum":0},"stderr_offset":{"type":"integer","minimum":0}}),vec!["handle"]),
            BackgroundAction::List=>("bash_list","List this session's running and recently completed background commands before starting another dev server.",json!({}),vec![]),
            BackgroundAction::Kill=>("bash_kill","Stop a background command and its supervised process tree.",json!({"handle":{"type":"string","minLength":1,"maxLength":256}}),vec!["handle"]),
        };
        ToolDefinition::new(
            name,
            description,
            json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        )
    }
    fn effect(&self, _: &Value) -> Result<ToolEffect, ToolError> {
        Ok(
            if matches!(
                self.action,
                BackgroundAction::Start | BackgroundAction::Kill
            ) {
                ToolEffect::ChangesState
            } else {
                ToolEffect::ReadOnly
            },
        )
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::permanent("cancelled", "后台工具已取消"));
            }
            let session = context.session_id.as_str();
            let value = match self.action {
                BackgroundAction::Start => {
                    let input: StartInput = serde_json::from_value(input).map_err(|_| invalid())?;
                    if input.command.trim().is_empty()
                        || input.command.len() > 65536
                        || input.command.contains('\0')
                    {
                        return Err(invalid());
                    }
                    let (cwd, logical) = self.command_cwd(input.cwd.as_deref())?;
                    let env = rcode_tools::ToolEnvironment::new(&self.root)?
                        .with_workspace_guard()
                        .with_file_access_policy(Arc::new(crate::security::WorkspacePathPolicy(
                            self.root.clone(),
                        )));
                    env.check_workspace_path(&cwd)?;
                    env.check_command_boundary(&input.command)?;
                    let display_cwd = if logical.is_empty() {
                        cwd.to_string_lossy().replace('\\', "/")
                    } else {
                        logical.clone()
                    };
                    let request = self.command_request(&input.command, cwd, logical)?;
                    let summary = rcode_model::redact_error_secrets_bounded(
                        &input.command.replace(['\n', '\r'], " "),
                        2048,
                    );
                    let info = self
                        .manager
                        .start_command(session, summary, request, None)
                        .await?;
                    json!({"ok":true,"handle":info.task_id,"command":input.command,"cwd":display_cwd,"started_at_ms":info.started_at_unix_ms})
                }
                BackgroundAction::List => {
                    if !input.as_object().is_some_and(|o| o.is_empty()) {
                        return Err(invalid());
                    }
                    let tasks = self.manager.list().map_err(background_error)?;
                    json!({"processes":tasks.into_iter().filter(|task|task.session_id==session).map(|task|json!({"handle":task.task_id,"command":task.summary,"exited":task.status.is_terminal(),"exit_code":task.exit_code,"started_at_ms":task.started_at_unix_ms})).collect::<Vec<_>>()})
                }
                BackgroundAction::Logs | BackgroundAction::Kill => {
                    let input: HandleInput =
                        serde_json::from_value(input).map_err(|_| invalid())?;
                    if matches!(self.action, BackgroundAction::Kill) {
                        if input.since_offset.is_some() || input.stderr_offset.is_some() {
                            return Err(invalid());
                        }
                        self.manager
                            .cancel_and_wait(session, &input.handle)
                            .await
                            .map_err(background_error)?;
                        json!({"ok":true,"handle":input.handle})
                    } else {
                        let output = self
                            .manager
                            .read_output(
                                session,
                                &input.handle,
                                BackgroundOutputCursor {
                                    stdout_offset: input.since_offset.unwrap_or(0),
                                    stderr_offset: input.stderr_offset.unwrap_or(0),
                                },
                                None,
                            )
                            .await
                            .map_err(background_error)?;
                        json!({"bytes":format!("{}{}",output.stdout,output.stderr),"stdout":output.stdout,"stderr":output.stderr,"next_offset":output.next_cursor.stdout_offset,"stderr_offset":output.next_cursor.stderr_offset,"has_more":output.stdout_has_more||output.stderr_has_more,"dropped":output.task.discarded_bytes,"exited":output.task.status.is_terminal(),"exit_code":output.task.exit_code})
                    }
                }
            };
            Ok(ToolOutput::text(value.to_string()))
        })
    }
}

impl BackgroundTool {
    fn command_cwd(&self, cwd: Option<&str>) -> Result<(PathBuf, String), ToolError> {
        match &self.target {
            ShellTarget::Local => {
                let path = crate::security::check_file_path(&self.root, cwd.unwrap_or("."))
                    .map_err(|e| ToolError::permanent("path_denied", e))?;
                let path = path
                    .canonicalize()
                    .map_err(|_| ToolError::permanent("path_denied", "工作目录不可访问"))?;
                if !path.is_dir() {
                    return Err(invalid());
                }
                Ok((path, String::new()))
            }
            ShellTarget::Wsl { cwd: base, .. } => {
                let raw = cwd.unwrap_or(base);
                let relative = if raw.starts_with('/') {
                    raw.strip_prefix(base)
                        .and_then(|s| {
                            if s.is_empty() {
                                Some(".")
                            } else {
                                s.strip_prefix('/')
                            }
                        })
                        .ok_or_else(|| {
                            ToolError::permanent("path_denied", "WSL 目录超出任务工作区")
                        })?
                } else {
                    raw
                };
                let host = crate::security::check_file_path(&self.root, relative)
                    .map_err(|e| ToolError::permanent("path_denied", e))?;
                let host = host.canonicalize().map_err(|_| invalid())?;
                let suffix = host
                    .strip_prefix(&self.root)
                    .map_err(|_| invalid())?
                    .to_string_lossy()
                    .replace('\\', "/");
                Ok((
                    host,
                    if suffix.is_empty() {
                        base.clone()
                    } else {
                        format!("{}/{suffix}", base.trim_end_matches('/'))
                    },
                ))
            }
        }
    }
    fn command_request(
        &self,
        command: &str,
        cwd: PathBuf,
        logical: String,
    ) -> Result<BoundedCommandRequest, ToolError> {
        match &self.target {
            ShellTarget::Local => {
                #[cfg(windows)]
                let request = BoundedCommandRequest::new(
                    "powershell.exe",
                    cwd,
                    Duration::from_secs(3600),
                    65536,
                )
                .with_args(vec![
                    "-NoProfile".into(),
                    "-Command".into(),
                    command.into(),
                ]);
                #[cfg(not(windows))]
                let request = BoundedCommandRequest::new(
                    std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into()),
                    cwd,
                    Duration::from_secs(3600),
                    65536,
                )
                .with_args(vec!["-lc".into(), command.into()]);
                Ok(request)
            }
            ShellTarget::Wsl { distro, .. } => {
                if !cfg!(windows) {
                    return Err(ToolError::permanent(
                        "unsupported_workspace",
                        "WSL 只能在 Windows 使用",
                    ));
                }
                if distro.is_empty()
                    || distro.len() > 128
                    || distro.starts_with('-')
                    || !distro
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
                {
                    return Err(invalid());
                }
                use rcode_agent::CollaborationIdGenerator;
                let token = rcode_agent::UuidCollaborationIdGenerator
                    .next_agent_id()
                    .as_str()
                    .strip_prefix("agent-")
                    .expect("generated UUID")
                    .replace('-', "");
                Ok(
                    BoundedCommandRequest::new("wsl.exe", cwd, Duration::from_secs(3600), 65536)
                        .with_args(vec![
                            "-d".into(),
                            distro.into(),
                            "--cd".into(),
                            logical.into(),
                            "--exec".into(),
                            "setsid".into(),
                            "--wait".into(),
                            "env".into(),
                            format!("RCODE_SHELL_TOKEN={token}").into(),
                            "sh".into(),
                            "-lc".into(),
                            command.into(),
                        ])
                        .with_environment(vec![
                            ("RCODE_WSL_TREE_DISTRO".into(), distro.into()),
                            ("RCODE_WSL_TREE_TOKEN".into(), token.into()),
                        ]),
                )
            }
        }
    }
}
fn invalid() -> ToolError {
    ToolError::permanent("invalid_input", "业务工具输入无效")
}
fn background_error(error: rcode_tools::BackgroundTaskError) -> ToolError {
    ToolError::permanent(error.code, error.message)
}
