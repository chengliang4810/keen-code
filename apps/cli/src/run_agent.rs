use rcode_agent::{
    AgentId, PlanGuard, SessionId, TerminalReason, ToolRegistry, TurnId, TurnRequest,
};
use rcode_model::{Message, MessageRole, ProviderProtocol};
use rcode_provider::ApiKey;
use rcode_runtime::{
    register_workspace_tools, AgentRuntime, EventBridge, ModelConfig, PermissionMode,
};
use serde_json::Value;
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, PartialEq)]
pub(super) struct RunOptions {
    base_url: String,
    model: String,
    provider: String,
    protocol: ProviderProtocol,
    cwd: PathBuf,
    permission: PermissionMode,
    plan: bool,
    prompt: String,
}

pub(super) fn parse(args: Vec<OsString>) -> Result<RunOptions, String> {
    let mut args = args.into_iter();
    let mut base_url = None;
    let mut model = None;
    let mut provider = "openai-compatible".to_owned();
    let mut protocol = ProviderProtocol::ChatCompletions;
    let mut cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut permission = PermissionMode::Ask;
    let mut plan = false;
    let mut prompt = Vec::new();
    let mut after_separator = false;
    while let Some(arg) = args.next() {
        let text = arg.to_str().ok_or("run arguments must be valid UTF-8")?;
        if after_separator {
            prompt.push(text.to_owned());
            continue;
        }
        match text {
            "--" => after_separator = true,
            "--plan" => plan = true,
            "--base-url" | "--model" | "--provider" | "--protocol" | "--cwd" | "--permission" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{text} requires a value"))?;
                if text == "--cwd" {
                    cwd = PathBuf::from(value);
                    continue;
                }
                let value = value
                    .into_string()
                    .map_err(|_| format!("{text} requires valid UTF-8"))?;
                match text {
                    "--base-url" => base_url = Some(value),
                    "--model" => model = Some(value),
                    "--provider" => provider = value,
                    "--protocol" => {
                        protocol =
                            match value.as_str() {
                                "chat-completions" => ProviderProtocol::ChatCompletions,
                                "responses" => ProviderProtocol::Responses,
                                "messages" => ProviderProtocol::Messages,
                                _ => return Err(
                                    "--protocol must be chat-completions, responses or messages"
                                        .into(),
                                ),
                            }
                    }
                    "--permission" => {
                        permission = serde_json::from_value(Value::String(value))
                            .map_err(|_| "--permission must be ask, edit or full-access")?
                    }
                    _ => unreachable!(),
                }
            }
            value if value.starts_with('-') => return Err(format!("unknown run option '{value}'")),
            value => prompt.push(value.to_owned()),
        }
    }
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() || prompt.len() > 256 * 1024 {
        return Err("run requires a nonempty prompt of at most 256 KiB".into());
    }
    Ok(RunOptions {
        base_url: base_url.ok_or("run requires --base-url")?,
        model: model.ok_or("run requires --model")?,
        provider,
        protocol,
        cwd,
        permission,
        plan,
        prompt,
    })
}

pub(super) fn run(options: RunOptions, json: bool) -> Result<(), String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(execute(options, json))
}

fn system_instructions(root: &Path) -> Result<String, String> {
    let mut system = String::from("You are RCode, a coding agent. Use the available tools to complete the user's task in the current workspace. Respect tool approvals and the workspace boundary.");
    for path in [
        rcode_runtime::storage::path("AGENTS.md")?,
        rcode_runtime::security::check_file_path(root, "AGENTS.md")?,
    ] {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.is_file() {
            return Err("AGENTS.md must be a regular file".into());
        }
        let mut instructions = String::new();
        std::fs::File::open(path)
            .map_err(|error| error.to_string())?
            .take(256 * 1024 + 1)
            .read_to_string(&mut instructions)
            .map_err(|error| error.to_string())?;
        if system.len() + instructions.len() + 2 > 256 * 1024 {
            return Err("Agent instructions exceed 256 KiB".into());
        }
        system.push_str("\n\n");
        system.push_str(&instructions);
    }
    Ok(system)
}

async fn execute(options: RunOptions, json: bool) -> Result<(), String> {
    let root = options
        .cwd
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !root.is_dir() {
        return Err("--cwd must be a directory".into());
    }
    let model = ModelConfig {
        provider_id: options.provider,
        protocol: options.protocol,
        base_url: options.base_url,
        model: options.model,
        context_limit: 65536,
        max_output_tokens: None,
        image_input: false,
        reasoning_body: None,
        // The endpoint is explicitly supplied by the caller.
        allow_private_network: true,
    };
    let key = std::env::var("RCODE_API_KEY")
        .ok()
        .filter(|value| !value.is_empty())
        .map(ApiKey::new)
        .transpose()
        .map_err(|error| error.to_string())?;
    let provider = Arc::new(model.provider(key).await?);
    let runtime = AgentRuntime::default();
    let run_id = format!("cli-{}", super::request_id());
    let session_id = format!("session-{run_id}");
    let lease = runtime.register(&run_id, &session_id)?;
    let cancellation = lease.cancellation.clone();
    let interrupt = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancellation.cancel();
        }
    });
    let approval_runtime = runtime.clone();
    let events = EventBridge::new(move |event| {
        if json {
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "{event}").map_err(|error| error.to_string())?;
        } else if event["type"] == "model" && event["event"]["type"] == "text_delta" {
            let mut stdout = io::stdout().lock();
            write!(
                stdout,
                "{}",
                event["event"]["delta"].as_str().unwrap_or_default()
            )
            .and_then(|_| stdout.flush())
            .map_err(|error| error.to_string())?;
        }
        if event["type"] == "approval" {
            let id = event["id"]
                .as_str()
                .ok_or("approval is missing an ID")?
                .to_owned();
            if !io::stdin().is_terminal() {
                approval_runtime.approve(&id, false)?;
            } else {
                let runtime = approval_runtime.clone();
                // A cancelled turn can exit while terminal input is still pending.
                std::thread::spawn(move || {
                    eprint!("\nApprove {} {}? [y/N] ", event["name"], event["input"]);
                    let _ = io::stderr().flush();
                    let mut answer = String::new();
                    let approved = io::stdin().read_line(&mut answer).is_ok()
                        && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
                    let _ = runtime.approve(&id, approved);
                });
            }
        }
        Ok(())
    });
    let environment = runtime.environment(&session_id, &root, None)?;
    let mut tools = ToolRegistry::new();
    register_workspace_tools(&mut tools, environment, &root, options.plan)?;
    let system = system_instructions(&root)?;
    let request = TurnRequest::new(
        SessionId::new(&session_id).map_err(|error| error.to_string())?,
        TurnId::new(&run_id).map_err(|error| error.to_string())?,
        AgentId::new("main").expect("static agent ID"),
        model.model,
        vec![
            Message::text(MessageRole::System, system),
            Message::text(MessageRole::User, options.prompt),
        ],
        if options.plan {
            PlanGuard::read_only()
        } else {
            PlanGuard::inactive()
        },
    );
    let result = runtime
        .run_turn(
            &lease,
            provider,
            tools,
            request,
            events.clone(),
            options.permission,
        )
        .await;
    interrupt.abort();
    let result = result?;
    let cancelled = lease.cancellation.is_cancelled();
    let hit_step_cap = result.state.terminal_reason() == Some(TerminalReason::LimitReached);
    let error = if cancelled {
        Some("Agent run cancelled".to_owned())
    } else if hit_step_cap {
        Some("Agent run reached its step limit".to_owned())
    } else {
        result
            .error
            .map(|error| rcode_model::redact_error_secrets_bounded(&error.to_string(), 2048))
    };
    if let Some(message) = &error {
        events.emit(serde_json::json!({"type":"error", "message":message}))?;
    }
    events.emit(
        serde_json::json!({"type":"finish", "cancelled":cancelled, "hitStepCap":hit_step_cap}),
    )?;
    drop(lease);
    events.emit(serde_json::json!({"type":"end"}))?;
    if !json {
        println!();
    }
    error.map_or(Ok(()), Err)
}
