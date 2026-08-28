use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};
use tauri::{AppHandle, Emitter, State};

#[cfg(windows)]
use crate::app_settings;
use crate::app_settings::TerminalShell;

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 30;
const OUTPUT_FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const OUTPUT_FLUSH_BYTES: usize = 4096;
const OUTPUT_QUEUE_CAPACITY: usize = 64;

fn should_flush_output(pending_bytes: usize, force: bool) -> bool {
    pending_bytes > 0 && (force || pending_bytes >= OUTPUT_FLUSH_BYTES)
}

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

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalShellOption {
    id: TerminalShell,
    name: &'static str,
    path: String,
}

#[cfg(windows)]
fn executable_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(';')
        .find_map(|dir| {
            let path = Path::new(dir).join(name);
            path.is_file().then_some(path)
        })
}

#[cfg(windows)]
fn detected_windows_shells() -> Vec<TerminalShellOption> {
    let mut shells = Vec::new();
    let candidates = [
        (
            TerminalShell::PowerShell7,
            "PowerShell 7",
            executable_on_path("pwsh.exe"),
        ),
        (
            TerminalShell::PowerShell,
            "Windows PowerShell",
            executable_on_path("powershell.exe"),
        ),
        (
            TerminalShell::GitBash,
            "Git Bash",
            ["ProgramFiles", "ProgramFiles(x86)"]
                .into_iter()
                .filter_map(std::env::var_os)
                .map(PathBuf::from)
                .map(|path| path.join("Git\\bin\\bash.exe"))
                .chain(
                    std::env::var_os("LOCALAPPDATA")
                        .map(PathBuf::from)
                        .map(|path| path.join("Programs\\Git\\bin\\bash.exe")),
                )
                .find(|path| path.is_file()),
        ),
        (
            TerminalShell::Cmd,
            "Command Prompt",
            std::env::var_os("COMSPEC")
                .map(PathBuf::from)
                .filter(|path| path.is_file())
                .or_else(|| executable_on_path("cmd.exe")),
        ),
    ];
    for (id, name, path) in candidates {
        if let Some(path) = path {
            shells.push(TerminalShellOption {
                id,
                name,
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
    shells
}

#[tauri::command]
pub fn terminal_shells_list() -> Vec<TerminalShellOption> {
    #[cfg(windows)]
    return detected_windows_shells();
    #[cfg(not(windows))]
    Vec::new()
}

fn shell_command(_app: &AppHandle) -> Result<CommandBuilder, String> {
    #[cfg(windows)]
    {
        let selected = app_settings::get(_app)
            .map_err(|error| error.to_string())?
            .terminal_shell;
        let shells = detected_windows_shells();
        let shell = shells
            .iter()
            .find(|shell| shell.id == selected)
            .or_else(|| {
                (selected == TerminalShell::Auto)
                    .then(|| shells.first())
                    .flatten()
            })
            .ok_or_else(|| "选择的集成终端 Shell 当前不可用".to_owned())?;
        let mut command = CommandBuilder::new(&shell.path);
        if shell.id == TerminalShell::GitBash {
            command.args(["--login", "-i"]);
        }
        Ok(command)
    }
    #[cfg(not(windows))]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
        let mut command = CommandBuilder::new(shell);
        // PTY 已提供交互环境；-l 让 shell 加载用户登录配置（PATH、代理等）。
        command.arg("-l");
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        Ok(command)
    }
}

/// 将 Windows 扩展长度路径转换为 CMD 可接受的普通本地路径。
fn shell_working_directory(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(local) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{local}"));
        }
        if let Some(local) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(local);
        }
    }
    path.to_path_buf()
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
    let mut command = shell_command(&app)?;
    let shell_cwd = shell_working_directory(cwd_path);
    command.cwd(&shell_cwd);
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

    let output_id = id.clone();
    let output_app = app.clone();
    let (output_tx, output_rx) = mpsc::sync_channel::<Vec<u8>>(OUTPUT_QUEUE_CAPACITY);
    std::thread::spawn(move || {
        let mut pending = Vec::new();
        loop {
            let force = match output_rx.recv_timeout(OUTPUT_FLUSH_INTERVAL) {
                Ok(chunk) => {
                    pending.extend_from_slice(&chunk);
                    false
                }
                Err(mpsc::RecvTimeoutError::Timeout) => true,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if should_flush_output(pending.len(), true) {
                        let _ = output_app.emit(
                            "terminal://output",
                            TerminalOutput {
                                id: output_id.clone(),
                                data: std::mem::take(&mut pending),
                            },
                        );
                    }
                    let _ = output_app.emit("terminal://exited", TerminalExited { id: output_id });
                    break;
                }
            };
            if should_flush_output(pending.len(), force) {
                let _ = output_app.emit(
                    "terminal://output",
                    TerminalOutput {
                        id: output_id.clone(),
                        data: std::mem::take(&mut pending),
                    },
                );
            }
        }
    });

    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if output_tx.send(buffer[..read].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::{OUTPUT_FLUSH_BYTES, shell_working_directory, should_flush_output};
    use std::path::Path;

    /// 验证普通路径不会被终端工作目录转换改写。
    #[test]
    fn preserves_regular_working_directory() {
        let path = Path::new(r"D:\projects\keen-code");
        assert_eq!(shell_working_directory(path), path);
    }

    #[test]
    fn terminal_output_flushes_on_size_timeout_or_exit() {
        assert!(!should_flush_output(0, false));
        assert!(!should_flush_output(0, true));
        assert!(!should_flush_output(16, false));
        assert!(should_flush_output(16, true));
        assert!(should_flush_output(OUTPUT_FLUSH_BYTES, false));
    }

    /// 验证 Windows 扩展长度盘符路径会转换为 CMD 支持的本地路径。
    #[cfg(windows)]
    #[test]
    fn removes_windows_extended_length_prefix() {
        assert_eq!(
            shell_working_directory(Path::new(r"\\?\D:\projects\keen-code")),
            Path::new(r"D:\projects\keen-code")
        );
    }

    /// 验证扩展长度 UNC 路径仍保留标准 UNC 语义。
    #[cfg(windows)]
    #[test]
    fn converts_windows_extended_unc_prefix() {
        assert_eq!(
            shell_working_directory(Path::new(r"\\?\UNC\server\share\project")),
            Path::new(r"\\server\share\project")
        );
    }
}
