//! 非交互 CLI 参数模型与严格解析。

use std::fmt;
use std::path::PathBuf;

/// CLI 解析错误；调用方应把它映射为退出码 2，并把详情写入 stderr。
#[derive(Debug, Eq, PartialEq)]
pub struct CliParseError {
    message: String,
}

impl CliParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 返回安全的参数错误说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CliParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliParseError {}

/// CLI 顶层命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliCommand {
    /// 打开或创建 Session 并运行一次 Prompt。
    Run(RunCommand),
    /// 管理已保存 Session。
    Session(SessionCommand),
    /// 控制 Host 提供的 Web 服务。
    Web(WebCommand),
    /// 只显示使用说明。
    Help,
    /// 请求启动独立 headless Host；该 Host 复用平台无关 Session Runtime。
    Headless(HeadlessOptions),
}

/// `run` 命令参数。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunCommand {
    /// 用户 Prompt 原文，不做 trim 或拼接重写。
    pub prompt: String,
    /// 可选已有 Session；省略时由 Host 按 cwd 新建。
    pub session_id: Option<String>,
    /// 新 Session 的工作目录。
    pub cwd: Option<PathBuf>,
    /// 以 NDJSON 输出全部结果/事件。
    pub json: bool,
    /// CLI 断开后让 Host 继续执行。
    pub detach: bool,
}

/// Session 子命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionCommand {
    /// 列出当前数据根内的 Session。
    List {
        /// 限定 Session 所属的工作目录。
        cwd: Option<PathBuf>,
        /// 是否使用 NDJSON 输出。
        json: bool,
    },
    /// 显示一个 Session 的元数据。
    Show {
        /// 要读取的 Session 标识。
        session_id: String,
        /// 是否使用 NDJSON 输出。
        json: bool,
    },
    /// 向已有 Session 发送一条 Prompt。
    Send {
        /// 目标 Session 标识。
        session_id: String,
        /// Prompt 文本。
        text: String,
        /// 是否使用 NDJSON 输出。
        json: bool,
        /// 是否让 Host 在 CLI 断开后继续执行。
        detach: bool,
    },
    /// 观察 Session 实时事件直到回合结束或 Ctrl+C。
    Attach {
        /// 要观察的 Session 标识。
        session_id: String,
        /// 是否使用 NDJSON 输出。
        json: bool,
    },
    /// 取消 Session 当前根回合。
    Stop {
        /// 目标 Session 标识。
        session_id: String,
        /// 可选的目标 Turn 标识。
        turn_id: Option<String>,
        /// 是否使用 NDJSON 输出。
        json: bool,
    },
}

/// Web 子命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebCommand {
    /// 启动 Web 服务；可选固定端口。
    Start {
        /// 可选的固定监听端口。
        port: Option<u16>,
        /// 是否使用 NDJSON 输出。
        json: bool,
    },
    /// 停止 Web 服务。
    Stop {
        /// 是否使用 NDJSON 输出。
        json: bool,
    },
    /// 查询 Web 服务状态。
    Status {
        /// 是否使用 NDJSON 输出。
        json: bool,
    },
}

/// `headless` 入口参数；不携带 Runtime 或 Tauri 类型。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadlessOptions {
    /// 是否输出 NDJSON 服务状态。
    pub json: bool,
}

/// 全局 CLI 选项和子命令。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliOptions {
    /// 覆盖默认数据根；没有时与 Desktop 使用相同默认规则。
    pub data_root: Option<PathBuf>,
    /// 是否声明表单问答能力；为 false 时 Host 不注册 AskUser 工具。
    pub declare_form_capability: bool,
    /// 解析后的子命令。
    pub command: CliCommand,
}

/// 解析不含程序名的命令行参数。
pub fn parse_args<I>(args: I) -> Result<CliOptions, CliParseError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let mut data_root = None;
    let mut global_json = false;
    // `--no-input` 影响 ACP 握手，必须在建立连接前确定，因此既可作全局参数，
    // 也可出现在 run / session send 的位置参数中；两处归并到同一开关。
    let mut declare_form_capability = true;
    let command_name = loop {
        let Some(value) = args.next() else {
            return Ok(CliOptions {
                data_root,
                declare_form_capability,
                command: CliCommand::Help,
            });
        };
        match value.as_str() {
            "--data-root" => {
                let path = required_value(&mut args, "--data-root")?;
                data_root = Some(PathBuf::from(path));
            }
            "--json" => global_json = true,
            "--no-input" => declare_form_capability = false,
            "-h" | "--help" => {
                return Ok(CliOptions {
                    data_root,
                    declare_form_capability,
                    command: CliCommand::Help,
                });
            }
            "--" => return Err(CliParseError::new("顶层命令前不能使用 --")),
            other if other.starts_with('-') => {
                return Err(CliParseError::new(format!("未知全局参数: {other}")));
            }
            _ => break value,
        }
    };
    let positional = args.collect::<Vec<_>>();
    let command = match command_name.as_str() {
        "run" => parse_run(positional, global_json, &mut declare_form_capability)?,
        "session" => parse_session(positional, global_json, &mut declare_form_capability)?,
        "web" => parse_web(positional, global_json)?,
        "headless" => parse_headless(positional, global_json)?,
        "help" => CliCommand::Help,
        other => return Err(CliParseError::new(format!("未知命令: {other}"))),
    };
    Ok(CliOptions {
        data_root,
        declare_form_capability,
        command,
    })
}

fn parse_run(
    args: Vec<String>,
    global_json: bool,
    declare_form_capability: &mut bool,
) -> Result<CliCommand, CliParseError> {
    let mut json = global_json;
    let mut detach = false;
    let mut session_id = None;
    let mut cwd = None;
    let mut prompt = Vec::new();
    let mut args = args.into_iter();
    let mut literal_prompt = false;
    while let Some(value) = args.next() {
        if literal_prompt {
            prompt.push(value);
            continue;
        }
        match value.as_str() {
            "--" => literal_prompt = true,
            "--json" => json = true,
            "--detach" => detach = true,
            "--no-input" => *declare_form_capability = false,
            "--session" => session_id = Some(required_value(&mut args, "--session")?),
            "--cwd" => cwd = Some(PathBuf::from(required_value(&mut args, "--cwd")?)),
            "-h" | "--help" => return Ok(CliCommand::Help),
            other if other.starts_with('-') => {
                return Err(CliParseError::new(format!("run 未知参数: {other}")));
            }
            _ => prompt.push(value),
        }
    }
    if prompt.is_empty() {
        return Err(CliParseError::new("run 需要 Prompt 文本"));
    }
    Ok(CliCommand::Run(RunCommand {
        prompt: prompt.join(" "),
        session_id,
        cwd,
        json,
        detach,
    }))
}

fn parse_session(
    args: Vec<String>,
    global_json: bool,
    declare_form_capability: &mut bool,
) -> Result<CliCommand, CliParseError> {
    let mut args = args.into_iter();
    let action = args
        .next()
        .ok_or_else(|| CliParseError::new("session 缺少子命令"))?;
    let mut json = global_json;
    let command = match action.as_str() {
        "list" => {
            let mut cwd = None;
            while let Some(value) = args.next() {
                match value.as_str() {
                    "--json" => json = true,
                    "--cwd" => cwd = Some(PathBuf::from(required_value(&mut args, "--cwd")?)),
                    other => {
                        return Err(CliParseError::new(format!(
                            "session list 未知参数: {other}"
                        )));
                    }
                }
            }
            SessionCommand::List { cwd, json }
        }
        "show" => {
            let session_id = required_positional(&mut args, "session show 需要 sessionId")?;
            parse_json_flag_and_no_extra(&mut args, &mut json, "session show")?;
            SessionCommand::Show { session_id, json }
        }
        "send" => {
            let session_id = required_positional(&mut args, "session send 需要 sessionId")?;
            let mut detach = false;
            let mut text = Vec::new();
            let mut literal_prompt = false;
            for value in args {
                if literal_prompt {
                    text.push(value);
                    continue;
                }
                match value.as_str() {
                    "--" => literal_prompt = true,
                    "--json" => json = true,
                    "--detach" => detach = true,
                    "--no-input" => *declare_form_capability = false,
                    other if other.starts_with('-') => {
                        return Err(CliParseError::new(format!(
                            "session send 未知参数: {other}"
                        )));
                    }
                    _ => text.push(value),
                }
            }
            if text.is_empty() {
                return Err(CliParseError::new("session send 需要 Prompt 文本"));
            }
            SessionCommand::Send {
                session_id,
                text: text.join(" "),
                json,
                detach,
            }
        }
        "attach" => {
            let session_id = required_positional(&mut args, "session attach 需要 sessionId")?;
            parse_json_flag_and_no_extra(&mut args, &mut json, "session attach")?;
            SessionCommand::Attach { session_id, json }
        }
        "stop" => {
            let session_id = required_positional(&mut args, "session stop 需要 sessionId")?;
            let mut turn_id = None;
            while let Some(value) = args.next() {
                match value.as_str() {
                    "--json" => json = true,
                    "--turn" => turn_id = Some(required_value(&mut args, "--turn")?),
                    other => {
                        return Err(CliParseError::new(format!(
                            "session stop 未知参数: {other}"
                        )));
                    }
                }
            }
            SessionCommand::Stop {
                session_id,
                turn_id,
                json,
            }
        }
        other => return Err(CliParseError::new(format!("未知 session 子命令: {other}"))),
    };
    Ok(CliCommand::Session(command))
}

fn parse_web(args: Vec<String>, global_json: bool) -> Result<CliCommand, CliParseError> {
    let mut args = args.into_iter();
    let action = args
        .next()
        .ok_or_else(|| CliParseError::new("web 缺少子命令"))?;
    let mut json = global_json;
    let command = match action.as_str() {
        "start" => {
            let mut port = None;
            while let Some(value) = args.next() {
                match value.as_str() {
                    "--json" => json = true,
                    "--port" => {
                        let value = required_value(&mut args, "--port")?;
                        let parsed = value
                            .parse::<u16>()
                            .map_err(|_| CliParseError::new("--port 必须是 1..65535"))?;
                        if parsed == 0 {
                            return Err(CliParseError::new("--port 必须是 1..65535"));
                        }
                        port = Some(parsed);
                    }
                    other => {
                        return Err(CliParseError::new(format!("web start 未知参数: {other}")));
                    }
                }
            }
            WebCommand::Start { port, json }
        }
        "stop" => {
            parse_json_flag_and_no_extra(&mut args, &mut json, "web stop")?;
            WebCommand::Stop { json }
        }
        "status" => {
            parse_json_flag_and_no_extra(&mut args, &mut json, "web status")?;
            WebCommand::Status { json }
        }
        other => return Err(CliParseError::new(format!("未知 web 子命令: {other}"))),
    };
    Ok(CliCommand::Web(command))
}

fn parse_headless(args: Vec<String>, global_json: bool) -> Result<CliCommand, CliParseError> {
    let mut json = global_json;
    for value in args {
        match value.as_str() {
            "--json" => json = true,
            other => return Err(CliParseError::new(format!("headless 未知参数: {other}"))),
        }
    }
    Ok(CliCommand::Headless(HeadlessOptions { json }))
}

fn required_value<I>(args: &mut I, option: &str) -> Result<String, CliParseError>
where
    I: Iterator<Item = String>,
{
    let value = args
        .next()
        .ok_or_else(|| CliParseError::new(format!("{option} 缺少值")))?;
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(CliParseError::new(format!("{option} 值无效")));
    }
    Ok(value)
}

fn required_positional<I>(args: &mut I, message: &str) -> Result<String, CliParseError>
where
    I: Iterator<Item = String>,
{
    let value = args.next().ok_or_else(|| CliParseError::new(message))?;
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(CliParseError::new(message));
    }
    Ok(value)
}

fn parse_json_flag_and_no_extra<I>(
    args: &mut I,
    json: &mut bool,
    command: &str,
) -> Result<(), CliParseError>
where
    I: Iterator<Item = String>,
{
    for value in args {
        if value == "--json" {
            *json = true;
        } else {
            return Err(CliParseError::new(format!("{command} 未知参数: {value}")));
        }
    }
    Ok(())
}

/// 返回 CLI 使用说明；内容固定，便于脚本检查。
pub fn usage() -> &'static str {
    "用法:\n  keencode run [--json] [--detach] [--no-input] [--session ID] [--cwd PATH] [--] PROMPT\n  keencode session list|show|send|attach|stop ...\n  keencode web start|stop|status ...\n  keencode headless [--json]\n\n以连字符开头的 Prompt 必须放在 -- 之后。普通命令在 Host 缺失时会启动同一可执行文件的 headless Host。\n--no-input 不声明表单问答能力：模型不会停下来提问，请求也不会以退出码 6 中断。\n退出码: 0 成功, 1 任务失败, 2 参数错误, 3 Host 不可用, 4 认证失败, 5 取消, 6 需要用户输入\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析run和稳定选项() {
        let options = parse_args([
            "--json".to_owned(),
            "run".to_owned(),
            "--detach".to_owned(),
            "--session".to_owned(),
            "s1".to_owned(),
            "修复".to_owned(),
            "测试".to_owned(),
        ])
        .unwrap();
        assert_eq!(
            options.command,
            CliCommand::Run(RunCommand {
                prompt: "修复 测试".to_owned(),
                session_id: Some("s1".to_owned()),
                cwd: None,
                json: true,
                detach: true,
            })
        );
    }

    #[test]
    fn 拒绝缺少prompt和未知命令() {
        assert!(parse_args(["run".to_owned()]).is_err());
        assert!(parse_args(["wat".to_owned()]).is_err());
        assert!(parse_args(["run".to_owned(), "修复".to_owned(), "--未知".to_owned(),]).is_err());
        assert!(
            parse_args([
                "session".to_owned(),
                "send".to_owned(),
                "s1".to_owned(),
                "--未知".to_owned(),
            ])
            .is_err()
        );
    }

    #[test]
    fn 双横线后参数按prompt原文处理() {
        let run = parse_args([
            "run".to_owned(),
            "--json".to_owned(),
            "--".to_owned(),
            "--detach".to_owned(),
            "--literal".to_owned(),
        ])
        .expect("-- 后内容应作为 Prompt");
        assert_eq!(
            run.command,
            CliCommand::Run(RunCommand {
                prompt: "--detach --literal".to_owned(),
                session_id: None,
                cwd: None,
                json: true,
                detach: false,
            })
        );

        let send = parse_args([
            "session".to_owned(),
            "send".to_owned(),
            "s1".to_owned(),
            "--".to_owned(),
            "--json".to_owned(),
        ])
        .expect("session send 应支持字面 Prompt");
        assert_eq!(
            send.command,
            CliCommand::Session(SessionCommand::Send {
                session_id: "s1".to_owned(),
                text: "--json".to_owned(),
                json: false,
                detach: false,
            })
        );
    }

    #[test]
    fn sessionstop支持turn标识() {
        let options = parse_args([
            "session".to_owned(),
            "stop".to_owned(),
            "s1".to_owned(),
            "--turn".to_owned(),
            "t1".to_owned(),
        ])
        .unwrap();
        assert_eq!(
            options.command,
            CliCommand::Session(SessionCommand::Stop {
                session_id: "s1".to_owned(),
                turn_id: Some("t1".to_owned()),
                json: false,
            })
        );
    }

    #[test]
    fn 选项值不能把下一个选项吞成标识() {
        assert!(
            parse_args([
                "run".to_owned(),
                "--session".to_owned(),
                "--json".to_owned(),
                "prompt".to_owned(),
            ])
            .is_err()
        );
        assert!(
            parse_args([
                "--data-root".to_owned(),
                "--json".to_owned(),
                "session".to_owned(),
                "list".to_owned(),
            ])
            .is_err()
        );
    }

    /// `--no-input` 既可作全局参数也可作 run / session send 的位置参数，
    /// 三种写法都必须归并到同一开关；缺省时保持声明表单能力。
    #[test]
    fn 解析no_input开关() {
        let default = parse_args(["run".to_owned(), "任务".to_owned()]).unwrap();
        assert!(default.declare_form_capability, "缺省必须声明表单能力");

        for args in [
            vec!["--no-input", "run", "任务"],
            vec!["run", "--no-input", "任务"],
            vec!["run", "--json", "--no-input", "任务"],
        ] {
            let options = parse_args(args.iter().map(|value| (*value).to_owned())).unwrap();
            assert!(
                !options.declare_form_capability,
                "run 的 --no-input 必须关闭表单能力: {args:?}"
            );
        }

        let send = parse_args([
            "session".to_owned(),
            "send".to_owned(),
            "s1".to_owned(),
            "--no-input".to_owned(),
            "继续".to_owned(),
        ])
        .unwrap();
        assert!(
            !send.declare_form_capability,
            "session send 的 --no-input 必须关闭表单能力"
        );

        // 其他子命令不接受该参数，必须按未知参数拒绝而不是静默忽略。
        assert!(
            parse_args([
                "session".to_owned(),
                "list".to_owned(),
                "--no-input".to_owned(),
            ])
            .is_err()
        );
    }

    /// 使用说明必须公布 --no-input 与全部退出码，便于脚本自检。
    #[test]
    fn 使用说明包含no_input与退出码() {
        let usage = usage();
        assert!(usage.contains("--no-input"));
        assert!(usage.contains("退出码: 0 成功, 1 任务失败, 2 参数错误, 3 Host 不可用, 4 认证失败, 5 取消, 6 需要用户输入"));
    }
}
