//! 非交互 CLI 的 Host 调用编排。
//!
//! 本模块只组合 ACP 请求、事件等待和稳定 stdout 记录，不把 JSON-RPC request id
//! 当作 operationId、turnId 或 taskId。`headless` 由独立 CLI Host 装配，Desktop
//! 仍通过自己的 adapter 持有 Tauri 事件和 Provider 配置。

use crate::client::{ClientError, HostClient, HostClientConfig, HostEvent};
use crate::command::{
    CliCommand, CliOptions, HeadlessOptions, RunCommand, SessionCommand, WebCommand,
};
use crate::{ExitCode, HeadlessHost, default_data_root};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 当前 Host adapter 预留的 Prompt admission 方法；它返回稳定执行三元组。
pub const DETACHED_PROMPT_METHOD: &str = "keencode/operation/admit";
/// 当前 Host adapter 预留的操作状态查询方法。
pub const OPERATION_STATUS_METHOD: &str = "keencode/operation/status";
/// Web 控制面方法前缀；Web server 仍由 Host 所有权和配置决定。
pub const WEB_START_METHOD: &str = "keencode/web/start";
/// Web 停止方法。
pub const WEB_STOP_METHOD: &str = "keencode/web/stop";
/// Web 状态方法。
pub const WEB_STATUS_METHOD: &str = "keencode/web/status";

static NEXT_OPERATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// 一次 CLI 命令产生的有序 stdout 记录。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CliExecutionResult {
    /// 按接收顺序排列的结构化记录；`--json` 时每条记录独占一行。
    pub records: Vec<Value>,
    /// 是否使用 JSON 输出模式。
    pub json: bool,
}

impl CliExecutionResult {
    fn new(json: bool) -> Self {
        Self {
            records: Vec::new(),
            json,
        }
    }

    fn push(&mut self, value: Value) {
        self.records.push(value);
    }
}

/// CLI 执行失败；记录仍会在 main 中先写入 stdout，再把错误写到 stderr。
#[derive(Debug)]
pub struct CliExecutionError {
    /// 对用户安全的错误摘要，不回显 Prompt、路径或 Provider 原文。
    message: String,
    /// 对外稳定退出码。
    code: ExitCode,
    /// 错误发生前已经收集的结构化记录。
    records: Vec<Value>,
}

impl CliExecutionError {
    fn new(message: impl Into<String>, code: ExitCode) -> Self {
        Self {
            message: message.into(),
            code,
            records: Vec::new(),
        }
    }

    fn with_records(mut self, records: Vec<Value>) -> Self {
        self.records = records;
        self
    }

    /// 返回安全错误摘要。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 返回稳定退出码。
    pub const fn exit_code(&self) -> ExitCode {
        self.code
    }

    /// 返回错误发生前的结构化 stdout 记录。
    pub fn records(&self) -> &[Value] {
        &self.records
    }
}

impl fmt::Display for CliExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliExecutionError {}

impl From<ClientError> for CliExecutionError {
    fn from(error: ClientError) -> Self {
        let code = error.exit_code();
        Self::new(error.to_string(), code)
    }
}

/// 执行已经解析的 CLI 命令。
pub async fn execute_command(options: CliOptions) -> Result<CliExecutionResult, CliExecutionError> {
    let json_output = command_json(&options.command);
    if matches!(&options.command, CliCommand::Help) {
        let mut result = CliExecutionResult::new(json_output);
        result.push(json!({"type":"help","usage":crate::usage()}));
        return Ok(result);
    }

    if let CliCommand::Headless(HeadlessOptions { json: json_output }) = &options.command {
        let data_root = options
            .data_root
            .or_else(|| default_data_root().ok())
            .ok_or_else(|| {
                CliExecutionError::new("无法确定 KeenCode 数据根目录", ExitCode::InvalidArguments)
            })?;
        let host = HeadlessHost::start(data_root)
            .await
            .map_err(|error| CliExecutionError::new(error.to_string(), error.exit_code()))?;
        let mut result = CliExecutionResult::new(*json_output);
        result.push(json!({
            "type": "headless_started",
            "ownerKind": "headless",
            "dataRootFingerprint": host.data_root_fingerprint(),
            "hostId": host.host_id(),
        }));
        host.wait().await.map_err(|error| {
            CliExecutionError::new(error.to_string(), error.exit_code())
                .with_records(result.records.clone())
        })?;
        result.push(json!({"type":"headless_stopped"}));
        return Ok(result);
    }

    let data_root = options
        .data_root
        .or_else(|| default_data_root().ok())
        .ok_or_else(|| {
            CliExecutionError::new("无法确定 KeenCode 数据根目录", ExitCode::InvalidArguments)
        })?;
    let client = connect_or_start_headless(data_root, options.declare_form_capability).await?;
    execute_with_client(&client, options.command).await
}

/// 优先连接 Desktop/既有 headless；仅在本地发现或传输不可用时拉起同一版本的
/// headless 子进程。子进程独立持有 root lease，CLI 退出后 detached 操作仍可继续。
///
/// `declare_form_capability` 决定本次连接是否声明表单问答能力：未声明时 Host
/// 不注册 AskUser 工具，非交互请求不会以「需要用户输入」中断。
async fn connect_or_start_headless(
    data_root: PathBuf,
    declare_form_capability: bool,
) -> Result<HostClient, CliExecutionError> {
    let mut config = HostClientConfig::new(data_root.clone());
    config.declare_form_capability = declare_form_capability;
    match HostClient::connect(config.clone()).await {
        Ok(client) => return Ok(client),
        Err(error)
            if !matches!(
                error,
                ClientError::Discovery(_) | ClientError::Transport(_) | ClientError::Closed
            ) =>
        {
            return Err(error.into());
        }
        Err(_) => {}
    }

    let executable = std::env::current_exe().map_err(|_| {
        CliExecutionError::new(
            "Host 不可用，且无法定位 keencode 可执行文件",
            ExitCode::HostUnavailable,
        )
    })?;
    let mut command = Command::new(executable);
    command
        .arg("--data-root")
        .arg(&data_root)
        .arg("headless")
        .arg("--json")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|_| {
        CliExecutionError::new(
            "Host 不可用，且 headless Host 启动失败",
            ExitCode::HostUnavailable,
        )
    })?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_error = None;
    while Instant::now() < deadline {
        match HostClient::connect(config.clone()).await {
            Ok(client) => return Ok(client),
            Err(error) => last_error = Some(error),
        }
        // 竞争启动时本子进程可能因 owner lease 已被另一进程取得而退出；仍继续
        // 等待胜出的 Host 发布 discovery，不能把正常竞争误报为启动失败。
        let _ = child.try_wait();
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(last_error.map_or_else(
        || CliExecutionError::new("headless Host 启动超时", ExitCode::HostUnavailable),
        CliExecutionError::from,
    ))
}

/// 使用已经连接的 Client 执行命令；测试和内嵌 adapter 可以复用该入口。
pub async fn execute_with_client(
    client: &HostClient,
    command: CliCommand,
) -> Result<CliExecutionResult, CliExecutionError> {
    let json_output = command_json(&command);
    let mut output = CliExecutionResult::new(json_output);
    let execution = match command {
        CliCommand::Run(command) => run_command(client, command, &mut output).await,
        CliCommand::Session(command) => session_command(client, command, &mut output).await,
        CliCommand::Web(command) => web_command(client, command, &mut output).await,
        CliCommand::Help | CliCommand::Headless(_) => unreachable!("已在 execute_command 处理"),
    };
    match execution {
        Ok(()) => Ok(output),
        Err(error) => Err(error.with_records(output.records.clone())),
    }
}

async fn run_command(
    client: &HostClient,
    command: RunCommand,
    output: &mut CliExecutionResult,
) -> Result<(), CliExecutionError> {
    let operation_id = next_operation_id("run");
    let session_id = match command.session_id {
        Some(session_id) => validate_identifier(session_id, "sessionId")?,
        None => {
            let cwd = canonical_cwd(command.cwd.as_deref())?;
            create_session(client, &cwd, &operation_id, output).await?
        }
    };
    if command.detach {
        admit_detached(client, &session_id, &command.prompt, &operation_id, output).await
    } else {
        prompt(client, &session_id, &command.prompt, &operation_id, output).await
    }
}

async fn session_command(
    client: &HostClient,
    command: SessionCommand,
    output: &mut CliExecutionResult,
) -> Result<(), CliExecutionError> {
    match command {
        SessionCommand::List { cwd, .. } => {
            let cwd = cwd
                .as_deref()
                .map(|path| canonical_cwd(Some(path)))
                .transpose()?;
            let sessions = list_sessions(client, cwd.as_deref()).await?;
            output.push(json!({"type":"session_list","sessions":sessions}));
            Ok(())
        }
        SessionCommand::Show { session_id, .. } => {
            let session_id = validate_identifier(session_id, "sessionId")?;
            let sessions = list_sessions(client, None).await?;
            let session = sessions
                .into_iter()
                .find(|value| {
                    value.get("sessionId").and_then(Value::as_str) == Some(session_id.as_str())
                })
                .ok_or_else(|| {
                    CliExecutionError::new(
                        "Session 不存在或不在当前授权范围内",
                        ExitCode::TaskFailed,
                    )
                })?;
            output.push(json!({"type":"session","session":session}));
            Ok(())
        }
        SessionCommand::Send {
            session_id,
            text,
            detach,
            ..
        } => {
            let session_id = validate_identifier(session_id, "sessionId")?;
            let operation_id = next_operation_id("send");
            if detach {
                admit_detached(client, &session_id, &text, &operation_id, output).await
            } else {
                prompt(client, &session_id, &text, &operation_id, output).await
            }
        }
        SessionCommand::Attach { session_id, .. } => {
            attach_session(client, &session_id, output).await
        }
        SessionCommand::Stop {
            session_id,
            turn_id,
            ..
        } => {
            let session_id = validate_identifier(session_id, "sessionId")?;
            let mut params = json!({"sessionId":session_id});
            let turn_id = turn_id
                .map(|turn_id| validate_identifier(turn_id, "turnId"))
                .transpose()?;
            if let Some(turn_id) = &turn_id {
                params["_meta"] = json!({"keencode/turnId": turn_id});
            }
            client.notify("session/cancel", params).await?;
            output.push(json!({"type":"cancel_requested","sessionId":session_id,"turnId":turn_id}));
            Ok(())
        }
    }
}

async fn web_command(
    client: &HostClient,
    command: WebCommand,
    output: &mut CliExecutionResult,
) -> Result<(), CliExecutionError> {
    let (method, params) = match command {
        WebCommand::Start { port, .. } => (WEB_START_METHOD, json!({"port":port})),
        WebCommand::Stop { .. } => (WEB_STOP_METHOD, json!({})),
        WebCommand::Status { .. } => (WEB_STATUS_METHOD, json!({})),
    };
    let result = client.request(method, params).await?.result;
    output.push(json!({"type":"web","method":method,"result":result}));
    Ok(())
}

async fn create_session(
    client: &HostClient,
    cwd: &Path,
    operation_id: &str,
    output: &mut CliExecutionResult,
) -> Result<String, CliExecutionError> {
    let result = client
        .request(
            "session/new",
            json!({
                "cwd": cwd.to_string_lossy(),
                "mcpServers": [],
                "_meta": {"keencode/operationId": operation_id}
            }),
        )
        .await?
        .result;
    let session_id = result
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_response("session/new 缺少 sessionId"))?
        .to_owned();
    output.push(json!({"type":"session_created","sessionId":session_id}));
    Ok(session_id)
}

async fn admit_detached(
    client: &HostClient,
    session_id: &str,
    prompt_text: &str,
    operation_id: &str,
    output: &mut CliExecutionResult,
) -> Result<(), CliExecutionError> {
    let result = client
        .request(
            DETACHED_PROMPT_METHOD,
            json!({
                "sessionId": session_id,
                "prompt": prompt_text,
                "detached": true,
                "_meta": {"keencode/operationId": operation_id}
            }),
        )
        .await?
        .result;
    let identity = parse_execution_identity(&result)?;
    output.push(json!({
        "type":"detached",
        "operationId":operation_id,
        "sessionId":identity.0,
        "turnId":identity.1,
        "taskId":identity.2
    }));
    Ok(())
}

async fn prompt(
    client: &HostClient,
    session_id: &str,
    prompt_text: &str,
    operation_id: &str,
    output: &mut CliExecutionResult,
) -> Result<(), CliExecutionError> {
    if prompt_text.is_empty() {
        return Err(CliExecutionError::new(
            "Prompt 不能为空",
            ExitCode::InvalidArguments,
        ));
    }
    let mut events = client.subscribe();
    let request = client.request_without_timeout(
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type":"text","text":prompt_text}],
            "_meta": {"keencode/operationId": operation_id}
        }),
    );
    tokio::pin!(request);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            response = &mut request => {
                let result = response?.result;
                let stop_reason = result
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid_response("session/prompt 缺少 stopReason"))?;
                let turn_id = result
                    .pointer("/_meta/keencode~1turnId")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid_response("session/prompt 缺少 Host 返回的 turnId"))?;
                output.push(json!({"type":"completed","sessionId":session_id,"turnId":turn_id,"stopReason":stop_reason,"result":result}));
                return match stop_reason {
                    "cancelled" => Err(CliExecutionError::new("Prompt 已取消", ExitCode::Cancelled)),
                    "refusal" => Err(CliExecutionError::new("Agent 拒绝继续执行", ExitCode::TaskFailed)),
                    _ => Ok(()),
                };
            }
            event = events.recv() => {
                let event = event.map_err(|error| CliExecutionError::new(format!("Host 事件订阅失败: {error}"), ExitCode::HostUnavailable))?;
                if event.method == "elicitation/create" {
                    let record = event_record(&event);
                    output.push(json!({"type":"needs_input","request":record}));
                    return Err(CliExecutionError::new("当前 Prompt 需要交互式用户输入", ExitCode::UserInputRequired).with_records(output.records.clone()));
                }
                output.push(event_record(&event));
            }
            signal = &mut ctrl_c => {
                signal.map_err(|error| CliExecutionError::new(format!("无法监听 Ctrl+C: {error}"), ExitCode::HostUnavailable))?;
                client.notify("session/cancel", json!({
                    "sessionId":session_id,
                    "_meta":{"keencode/operationId":operation_id}
                })).await?;
                output.push(json!({"type":"cancel_requested","sessionId":session_id,"operationId":operation_id}));
                wait_for_cancelled_operation(client, operation_id).await?;
                return Err(CliExecutionError::new("当前 Prompt 已取消", ExitCode::Cancelled).with_records(output.records.clone()));
            }
        }
    }
}

/// 取消通知本身没有 JSON-RPC 响应；通过同一 operationId 的权威状态确认终态，
/// 避免 Ctrl+C 在通知尚未落账时直接退出并遗留继续执行的副作用。
async fn wait_for_cancelled_operation(
    client: &HostClient,
    operation_id: &str,
) -> Result<(), CliExecutionError> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = client
            .request(OPERATION_STATUS_METHOD, json!({"operationId":operation_id}))
            .await?
            .result;
        match status.get("state").and_then(Value::as_str) {
            Some("cancelled") => return Ok(()),
            Some("completed" | "failed") => {
                return Err(CliExecutionError::new(
                    "取消请求到达前 Prompt 已结束",
                    ExitCode::TaskFailed,
                ));
            }
            _ if Instant::now() >= deadline => {
                return Err(CliExecutionError::new(
                    "等待 Prompt 取消终态超时",
                    ExitCode::HostUnavailable,
                ));
            }
            _ => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
}

async fn attach_session(
    client: &HostClient,
    session_id: &str,
    output: &mut CliExecutionResult,
) -> Result<(), CliExecutionError> {
    let session_id = validate_identifier(session_id.to_owned(), "sessionId")?;
    let sessions = list_sessions(client, None).await?;
    let cwd = sessions
        .iter()
        .find(|value| value.get("sessionId").and_then(Value::as_str) == Some(session_id.as_str()))
        .and_then(|value| value.get("cwd").and_then(Value::as_str))
        .ok_or_else(|| {
            CliExecutionError::new("Session 不存在或不在当前授权范围内", ExitCode::TaskFailed)
        })?;
    let mut events = client.subscribe();
    let loaded = client
        .request(
            "session/load",
            json!({"sessionId":session_id,"cwd":cwd,"mcpServers":[]}),
        )
        .await?
        .result;
    output.push(json!({"type":"session_loaded","sessionId":session_id,"result":loaded}));
    let active_turn = loaded
        .pointer("/_meta/keencode~1snapshot/activeTurnId")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let Some(active_turn) = active_turn else {
        return Ok(());
    };
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut cancel_deadline = None;
    loop {
        tokio::select! {
            event = events.recv() => {
                let event = event.map_err(|error| CliExecutionError::new(format!("Host 事件订阅失败: {error}"), ExitCode::HostUnavailable))?;
                let terminal = event_is_terminal(&event, &session_id, &active_turn);
                let cancelled = event_is_cancelled(&event, &session_id, &active_turn);
                output.push(event_record(&event));
                if terminal {
                    return if cancel_deadline.is_some() {
                        if cancelled {
                            Err(CliExecutionError::new("当前 Prompt 已取消", ExitCode::Cancelled))
                        } else {
                            Err(CliExecutionError::new("取消请求到达前 Prompt 已结束", ExitCode::TaskFailed))
                        }
                    } else {
                        Ok(())
                    };
                }
            }
            signal = &mut ctrl_c, if cancel_deadline.is_none() => {
                signal.map_err(|error| CliExecutionError::new(format!("无法监听 Ctrl+C: {error}"), ExitCode::HostUnavailable))?;
                client.notify("session/cancel", json!({"sessionId":session_id,"_meta":{"keencode/turnId":active_turn}})).await?;
                output.push(json!({"type":"cancel_requested","sessionId":session_id,"turnId":active_turn}));
                cancel_deadline = Some(Instant::now() + Duration::from_secs(10));
            }
            _ = tokio::time::sleep(Duration::from_millis(25)), if cancel_deadline.is_some() => {
                if cancel_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return Err(CliExecutionError::new("等待 attach 取消终态超时", ExitCode::HostUnavailable));
                }
            }
        }
    }
}

async fn list_sessions(
    client: &HostClient,
    cwd: Option<&Path>,
) -> Result<Vec<Value>, CliExecutionError> {
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut sessions = Vec::new();
    loop {
        let mut params = json!({});
        if let Some(cwd) = cwd {
            params["cwd"] = Value::String(cwd.to_string_lossy().into_owned());
        }
        if let Some(cursor) = &cursor {
            params["cursor"] = Value::String(cursor.clone());
        }
        let result = client.request("session/list", params).await?.result;
        let page = result
            .get("sessions")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_response("session/list 缺少 sessions 数组"))?;
        for item in page {
            if item.get("sessionId").and_then(Value::as_str).is_none()
                || item.get("cwd").and_then(Value::as_str).is_none()
            {
                return Err(invalid_response("session/list 项缺少 sessionId 或 cwd"));
            }
            sessions.push(item.clone());
        }
        let next = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(next) = next else {
            break;
        };
        if next.is_empty() || !seen.insert(next.clone()) {
            return Err(invalid_response("session/list 游标未推进"));
        }
        cursor = Some(next);
    }
    Ok(sessions)
}

fn parse_execution_identity(result: &Value) -> Result<(String, String, String), CliExecutionError> {
    let source = result.get("execution").unwrap_or(result);
    let session_id = source
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|value| valid_identifier(value));
    let turn_id = source
        .get("turnId")
        .and_then(Value::as_str)
        .filter(|value| valid_identifier(value));
    let task_id = source
        .get("taskId")
        .and_then(Value::as_str)
        .filter(|value| valid_identifier(value));
    match (session_id, turn_id, task_id) {
        (Some(session_id), Some(turn_id), Some(task_id)) => Ok((
            session_id.to_owned(),
            turn_id.to_owned(),
            task_id.to_owned(),
        )),
        _ => Err(invalid_response(
            "Host admission 未返回完整 sessionId/turnId/taskId",
        )),
    }
}

fn event_record(event: &HostEvent) -> Value {
    json!({"type":"event","method":event.method,"id":event.request_id,"params":event.params})
}

fn event_is_terminal(event: &HostEvent, session_id: &str, turn_id: &str) -> bool {
    let envelope = event.params.get("envelope").unwrap_or(&event.params);
    if envelope.get("sessionId").and_then(Value::as_str) != Some(session_id)
        || envelope.get("turnId").and_then(Value::as_str) != Some(turn_id)
    {
        return false;
    }
    envelope
        .pointer("/event/type")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "turn_completed" | "turn_cancelled" | "turn_failed"))
}

fn event_is_cancelled(event: &HostEvent, session_id: &str, turn_id: &str) -> bool {
    let envelope = event.params.get("envelope").unwrap_or(&event.params);
    envelope.get("sessionId").and_then(Value::as_str) == Some(session_id)
        && envelope.get("turnId").and_then(Value::as_str) == Some(turn_id)
        && envelope
            .pointer("/event/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "turn_cancelled")
}

fn command_json(command: &CliCommand) -> bool {
    match command {
        CliCommand::Run(command) => command.json,
        CliCommand::Session(command) => match command {
            SessionCommand::List { json, .. }
            | SessionCommand::Show { json, .. }
            | SessionCommand::Send { json, .. }
            | SessionCommand::Attach { json, .. }
            | SessionCommand::Stop { json, .. } => *json,
        },
        CliCommand::Web(command) => match command {
            WebCommand::Start { json, .. }
            | WebCommand::Stop { json }
            | WebCommand::Status { json } => *json,
        },
        CliCommand::Help => true,
        CliCommand::Headless(options) => options.json,
    }
}

fn canonical_cwd(path: Option<&Path>) -> Result<PathBuf, CliExecutionError> {
    let path = path.map_or_else(
        || {
            std::env::current_dir().map_err(|_| {
                CliExecutionError::new("无法读取当前工作目录", ExitCode::InvalidArguments)
            })
        },
        |path| Ok(path.to_path_buf()),
    )?;
    let canonical = std::fs::canonicalize(path).map_err(|_| {
        CliExecutionError::new("cwd 必须是存在的绝对目录", ExitCode::InvalidArguments)
    })?;
    if !canonical.is_dir() {
        return Err(CliExecutionError::new(
            "cwd 必须是目录",
            ExitCode::InvalidArguments,
        ));
    }
    Ok(canonical)
}

fn validate_identifier(value: String, field: &str) -> Result<String, CliExecutionError> {
    if !valid_identifier(&value) {
        return Err(CliExecutionError::new(
            format!("{field} 无效"),
            ExitCode::InvalidArguments,
        ));
    }
    Ok(value)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn invalid_response(message: impl Into<String>) -> CliExecutionError {
    CliExecutionError::new(message, ExitCode::HostUnavailable)
}

fn next_operation_id(scope: &str) -> String {
    let sequence = NEXT_OPERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis());
    format!("cli-{scope}-{millis}-{sequence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach身份严格要求host返回三元组() {
        assert!(parse_execution_identity(&json!({"sessionId":"s","turnId":"t"})).is_err());
        assert_eq!(
            parse_execution_identity(
                &json!({"execution":{"sessionId":"s","turnId":"t","taskId":"x"}})
            )
            .unwrap(),
            ("s".to_owned(), "t".to_owned(), "x".to_owned())
        );
    }

    #[test]
    fn terminal事件只匹配同一session和turn() {
        let event = HostEvent {
            method: "acp://delivery".to_owned(),
            id: None,
            request_id: None,
            params: json!({"envelope":{"sessionId":"s","turnId":"t","event":{"type":"turn_completed"}}}),
        };
        assert!(event_is_terminal(&event, "s", "t"));
        assert!(!event_is_cancelled(&event, "s", "t"));
        assert!(!event_is_terminal(&event, "s2", "t"));

        let cancelled = HostEvent {
            params: json!({"envelope":{"sessionId":"s","turnId":"t","event":{"type":"turn_cancelled"}}}),
            ..event
        };
        assert!(event_is_terminal(&cancelled, "s", "t"));
        assert!(event_is_cancelled(&cancelled, "s", "t"));
    }
}
