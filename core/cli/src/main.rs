//! KeenCode 非交互命令行入口。

use keencode_cli::{
    CliCommand, CliExecutionError, CliExecutionResult, ExitCode, SessionCommand, WebCommand,
    execute_command, parse_args, usage,
};
use serde_json::{Value, json};
use std::env;
use std::process::ExitCode as ProcessExitCode;

/// 解析参数、执行命令并映射为稳定的进程退出码。
#[tokio::main]
async fn main() -> ProcessExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let json_requested = args.iter().any(|value| value == "--json");
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            if json_requested {
                emit_json_record(&json!({
                    "type": "error",
                    "code": ExitCode::InvalidArguments.as_i32(),
                    "message": error.message(),
                }));
            }
            eprintln!("参数错误: {error}");
            return process_exit(ExitCode::InvalidArguments);
        }
    };

    let is_help = matches!(&options.command, CliCommand::Help);
    let command_json = command_json(&options.command);
    match execute_command(options).await {
        Ok(result) => {
            if is_help && !json_requested {
                print!("{}", usage());
            } else {
                emit_result(
                    &result,
                    if is_help {
                        json_requested
                    } else {
                        command_json
                    },
                );
            }
            process_exit(ExitCode::Success)
        }
        Err(error) => {
            emit_error(&error, command_json);
            process_exit(error.exit_code())
        }
    }
}

fn process_exit(code: ExitCode) -> ProcessExitCode {
    ProcessExitCode::from(code.as_i32() as u8)
}

fn emit_result(result: &CliExecutionResult, json_mode: bool) {
    for record in &result.records {
        if json_mode {
            emit_json_record(record);
        } else {
            emit_human_record(record);
        }
    }
}

fn emit_error(error: &CliExecutionError, json_mode: bool) {
    if json_mode {
        for record in error.records() {
            emit_json_record(record);
        }
        emit_json_record(&json!({
            "type": "error",
            "code": error.exit_code().as_i32(),
            "message": error.message(),
        }));
    } else {
        for record in error.records() {
            emit_human_record(record);
        }
    }
    eprintln!("错误: {error}");
}

fn emit_json_record(record: &Value) {
    // Value 由内部结构化记录构造，序列化失败只可能表示程序内部错误；即使发生，
    // 也不应在错误路径上 panic，避免把稳定退出码变成不可预测的 abort。
    match serde_json::to_string(record) {
        Ok(encoded) => println!("{encoded}"),
        Err(error) => eprintln!("输出 JSON 失败: {error}"),
    }
}

fn emit_human_record(record: &Value) {
    let kind = record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("record");
    match kind {
        "session_created" => println!("已创建 Session: {}", text_field(record, "sessionId")),
        "session_loaded" => println!("已加载 Session: {}", text_field(record, "sessionId")),
        "completed" => println!(
            "Prompt 完成: session={} turn={} stopReason={}",
            text_field(record, "sessionId"),
            text_field(record, "turnId"),
            text_field(record, "stopReason")
        ),
        "detached" => println!(
            "已转为后台执行: session={} turn={} task={}",
            text_field(record, "sessionId"),
            text_field(record, "turnId"),
            text_field(record, "taskId")
        ),
        "stopped" => println!("已请求停止 Session: {}", text_field(record, "sessionId")),
        "cancel_requested" => println!(
            "已请求取消 Prompt: session={} turn={}",
            text_field(record, "sessionId"),
            text_field(record, "turnId")
        ),
        "needs_input" => println!(
            "需要用户输入: {}",
            compact_json(record.get("request").unwrap_or(&Value::Null))
        ),
        "event" => println!(
            "事件 {}: {}",
            text_field(record, "method"),
            compact_json(record.get("params").unwrap_or(&Value::Null))
        ),
        "session_list" => emit_session_list(record),
        "session" => println!(
            "Session: {}",
            compact_json(record.get("session").unwrap_or(&Value::Null))
        ),
        "web" => println!(
            "Web {}: {}",
            text_field(record, "method"),
            compact_json(record.get("result").unwrap_or(&Value::Null))
        ),
        _ => println!("{}", compact_json(record)),
    }
}

fn emit_session_list(record: &Value) {
    let Some(sessions) = record.get("sessions").and_then(Value::as_array) else {
        println!("Session 列表: {}", compact_json(record));
        return;
    };
    if sessions.is_empty() {
        println!("没有可用 Session");
        return;
    }
    for session in sessions {
        println!(
            "Session {}: {}",
            text_field(session, "sessionId"),
            text_field(session, "cwd")
        );
    }
}

fn text_field(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| "-".to_owned())
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
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
