use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    sync::Arc,
};
use tauri::{AppHandle, Emitter, State};

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 30;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutput {
    id: String,
    data: Vec<u8>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalExited {
    id: String,
}

struct TerminalSession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, TerminalSession>>,
}

fn shell_command() -> CommandBuilder {
    #[cfg(windows)]
    {
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_owned());
        CommandBuilder::new(shell)
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
        let mut command = CommandBuilder::new(shell);
        // PTY 已提供交互环境；-l 让 shell 加载用户登录配置（PATH、代理等）。
        command.arg("-l");
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command
    }
}

#[tauri::command]
pub fn terminal_create(
    id: String,
    cwd: String,
    cols: Option<u16>,
    rows: Option<u16>,
    app: AppHandle,
    manager: State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("终端标识不能为空".to_owned());
    }
    let cwd_path = Path::new(&cwd);
    if !cwd_path.is_dir() {
        return Err("终端工作目录不存在或不是目录".to_owned());
    }
    if manager.sessions.lock().contains_key(&id) {
        return Err("终端已经存在".to_owned());
    }

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: rows.unwrap_or(DEFAULT_ROWS).max(1),
            cols: cols.unwrap_or(DEFAULT_COLS).max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("创建 PTY 失败：{error}"))?;
    let mut command = shell_command();
    command.cwd(cwd_path);
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("启动系统 Shell 失败：{error}"))?;
    drop(pair.slave);
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| format!("打开终端输入失败：{error}"))?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("打开终端输出失败：{error}"))?;

    manager.sessions.lock().insert(
        id.clone(),
        TerminalSession {
            writer,
            master: pair.master,
            child,
        },
    );

    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let _ = app.emit(
                        "terminal://output",
                        TerminalOutput {
                            id: id.clone(),
                            data: buffer[..read].to_vec(),
                        },
                    );
                }
            }
        }
        let _ = app.emit("terminal://exited", TerminalExited { id });
    });
    Ok(())
}

#[tauri::command]
pub fn terminal_write(
    id: String,
    data: Vec<u8>,
    manager: State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    let mut sessions = manager.sessions.lock();
    let session = sessions
        .get_mut(&id)
        .ok_or_else(|| "终端不存在或已经退出".to_owned())?;
    session
        .writer
        .write_all(&data)
        .and_then(|_| session.writer.flush())
        .map_err(|error| format!("写入终端失败：{error}"))
}

#[tauri::command]
pub fn terminal_resize(
    id: String,
    cols: u16,
    rows: u16,
    manager: State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    let sessions = manager.sessions.lock();
    let session = sessions
        .get(&id)
        .ok_or_else(|| "终端不存在或已经退出".to_owned())?;
    session
        .master
        .resize(PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("调整终端尺寸失败：{error}"))
}

#[tauri::command]
pub fn terminal_close(id: String, manager: State<'_, Arc<TerminalManager>>) -> Result<(), String> {
    if let Some(mut session) = manager.sessions.lock().remove(&id) {
        session
            .child
            .kill()
            .map_err(|error| format!("关闭终端失败：{error}"))?;
    }
    Ok(())
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        for (_, mut session) in self.sessions.get_mut().drain() {
            let _ = session.child.kill();
        }
    }
}
