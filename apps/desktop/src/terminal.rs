use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Duration,
};
use tauri::{AppHandle, Emitter, State};

#[cfg(windows)]
use crate::app_settings;
use crate::app_settings::TerminalShell;
#[cfg(windows)]
use crate::path_utils::path_to_frontend;

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 30;
const OUTPUT_FLUSH_INTERVAL: Duration = Duration::from_millis(16);
const OUTPUT_FLUSH_BYTES: usize = 4096;
const OUTPUT_QUEUE_CAPACITY: usize = 64;
const HISTORY_MAX_BYTES: usize = 1024 * 1024;

/// PTY 库和后台任务错误进入 Tauri IPC 前统一移除可能携带的认证信息。
fn terminal_error(context: &str, error: impl std::fmt::Display) -> String {
    keencode_model::redact_error_secrets(&format!("{context}：{error}"))
}

fn should_flush_output(pending_bytes: usize, force: bool) -> bool {
    pending_bytes > 0 && (force || pending_bytes >= OUTPUT_FLUSH_BYTES)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutput {
    id: String,
    data: Vec<u8>,
    /// 累计字节位置用于快照与实时事件去重，不以字符串长度计数。
    byte_offset: u64,
}

#[derive(Default)]
struct TerminalHistory {
    bytes: VecDeque<u8>,
    byte_offset: u64,
    exited: bool,
    /// 仅记录操作系统已确认的退出状态，EOF 不能推断为成功退出。
    exit_code: Option<u32>,
    generation: u64,
}

impl TerminalHistory {
    /// 仅保留有界回放；截断时跳过 UTF-8 续字节，避免刷新后首字符乱码。
    fn append(&mut self, bytes: &[u8]) -> u64 {
        self.byte_offset += bytes.len() as u64;
        self.bytes.extend(bytes);
        let excess = self.bytes.len().saturating_sub(HISTORY_MAX_BYTES);
        self.bytes.drain(..excess);
        while self.bytes.front().is_some_and(|byte| byte & 0xc0 == 0x80) {
            self.bytes.pop_front();
        }
        self.byte_offset
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    cwd: String,
    pid: Option<u32>,
    data: Vec<u8>,
    byte_offset: u64,
    exited: bool,
    exit_code: Option<u32>,
    generation: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalCleared {
    id: String,
    byte_offset: u64,
    generation: u64,
}

/// 历史写入与事件发送使用同一把锁，清屏事件不能跨过更新后的输出。
fn publish_output(
    app: &AppHandle,
    id: &str,
    history: &Mutex<TerminalHistory>,
    pending: &mut Vec<u8>,
) {
    let mut history = history.lock();
    let byte_offset = history.append(pending);
    let _ = app.emit(
        "terminal://output",
        TerminalOutput {
            id: id.to_owned(),
            data: std::mem::take(pending),
            byte_offset,
        },
    );
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalExited {
    id: String,
}

/// 读取端 EOF 与子进程退出不是同一个时刻，尤其是 Windows ConPTY 可能先
/// 返回子进程状态、稍后才关闭输出句柄。独立观察子进程状态，确保底部终端
/// 能收到真实退出事件；退出码始终来自 portable-pty，不用 EOF 猜测结果。
fn spawn_exit_watcher(manager: Arc<TerminalManager>, app: AppHandle, id: String) {
    std::thread::spawn(move || {
        loop {
            let status = {
                let mut sessions = manager.sessions.lock();
                let Some(session) = sessions.get_mut(&id) else {
                    return;
                };
                match session.child.try_wait() {
                    Ok(Some(status)) => {
                        let code = status.exit_code();
                        let mut history = session.history.lock();
                        let should_emit = !history.exited;
                        history.exited = true;
                        history.exit_code = Some(code);
                        Some(should_emit)
                    }
                    Ok(None) => None,
                    Err(error) => {
                        tracing::warn!(%error, terminal_id = %id, "读取终端子进程退出状态失败");
                        return;
                    }
                }
            };

            if status == Some(true) {
                let _ = app.emit("terminal://exited", TerminalExited { id: id.clone() });
                return;
            }
            if status == Some(false) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

struct TerminalSession {
    /// 写句柄按会话独立加锁：写入期间不持有 sessions 全局锁，单会话
    /// 输入缓冲满（子进程停读）时不会拖死其他终端与主线程。
    writer: std::sync::Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    cwd: String,
    history: Arc<Mutex<TerminalHistory>>,
}

/// kill 后收割子进程，避免 Unix 留僵尸进程 / Windows 漏句柄。
/// wait 可能短暂阻塞，调用方不得在持有 sessions 锁时执行。
fn reap_child(child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    let _ = child.kill();
    let _ = child.wait();
}

#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, TerminalSession>>,
}

impl TerminalManager {
    /// 工作树删除前检查真实 PTY cwd；其他线程仍在此目录运行时保留 checkout。
    pub(crate) fn has_live_checkout(&self, checkout: &Path) -> Result<bool, String> {
        for session in self.sessions.lock().values_mut() {
            if std::fs::canonicalize(&session.cwd).is_ok_and(|cwd| cwd.starts_with(checkout))
                && session
                    .child
                    .try_wait()
                    .map_err(|error| terminal_error("检查终端退出状态失败", error))?
                    .is_none()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalShellOption {
    pub(crate) id: TerminalShell,
    name: &'static str,
    pub(crate) path: String,
    /// 前端统一使用斜杠路径；启动进程必须保留 Windows 原生路径。
    #[cfg(windows)]
    #[serde(skip)]
    native_path: PathBuf,
}

/// Settings 页使用的 Source shell 契约；不把 Rust 内部 PowerShell 变体暴露给
/// 只支持 cmd/git-bash 的 integratedTerminalShell 选择器。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IntegratedTerminalShellOption {
    pub(crate) dialect: &'static str,
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) path: String,
    pub(crate) source: &'static str,
}

fn terminal_shell_id(shell: TerminalShell) -> &'static str {
    match shell {
        TerminalShell::Auto => "auto",
        TerminalShell::PowerShell => "powerShell",
        TerminalShell::PowerShell7 => "powerShell7",
        TerminalShell::GitBash => "gitBash",
        TerminalShell::Cmd => "cmd",
    }
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
                path: path_to_frontend(&path),
                native_path: path,
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

/// 返回设置页可选择的 Windows Bash 方言，并复用同一份真实探测结果。
pub(crate) fn integrated_terminal_shells_list() -> Vec<IntegratedTerminalShellOption> {
    terminal_shells_list()
        .into_iter()
        .filter_map(|shell| {
            let (dialect, label) = match shell.id {
                TerminalShell::GitBash => ("git-bash", "Git Bash"),
                TerminalShell::Cmd => ("cmd", "Command Prompt"),
                TerminalShell::Auto | TerminalShell::PowerShell | TerminalShell::PowerShell7 => {
                    return None;
                }
            };
            Some(IntegratedTerminalShellOption {
                dialect,
                id: terminal_shell_id(shell.id),
                label,
                path: shell.path,
                // 当前探测路径来自系统安装/PATH，Source 只需展示来源类别。
                source: "system",
            })
        })
        .collect()
}

fn shell_command(_app: &AppHandle) -> Result<CommandBuilder, String> {
    #[cfg(windows)]
    {
        let settings = app_settings::get(_app).map_err(|error| error.to_string())?;
        let selected = settings.terminal_shell;
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
        let mut command = CommandBuilder::new(&shell.native_path);
        command.args(shell_arguments(
            shell.id,
            settings.terminal_inherit_system_profile,
        ));
        Ok(command)
    }
    #[cfg(not(windows))]
    {
        let inherit_system_profile = app_settings::get(_app)
            .map_err(|error| error.to_string())?
            .terminal_inherit_system_profile;
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_owned());
        let mut command = CommandBuilder::new(shell);
        // PTY 已提供交互环境；仅在用户开启时加载登录 profile（PATH、代理等）。
        if inherit_system_profile {
            command.arg("-l");
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        Ok(command)
    }
}

#[cfg(windows)]
fn shell_arguments(shell: TerminalShell, inherit_system_profile: bool) -> &'static [&'static str] {
    match shell {
        TerminalShell::GitBash if inherit_system_profile => &["--login", "-i"],
        TerminalShell::GitBash => &["--noprofile", "-i"],
        // CMD 的 /d 禁止 AutoRun；否则关闭 profile 仍会执行用户注册表脚本。
        TerminalShell::Cmd if !inherit_system_profile => &["/d"],
        TerminalShell::PowerShell | TerminalShell::PowerShell7 if !inherit_system_profile => {
            &["-NoProfile"]
        }
        TerminalShell::Auto
        | TerminalShell::PowerShell
        | TerminalShell::PowerShell7
        | TerminalShell::Cmd => &[],
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

/// 环境键不可包含等号/NUL，且输入有界；不记录变量值以免泄露凭据。
pub(crate) fn validate_environment(env: Option<&HashMap<String, String>>) -> Result<(), String> {
    let Some(env) = env else {
        return Ok(());
    };
    if env.len() > 256 || env.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > 1024 * 1024 {
        return Err("终端环境变量超过大小限制".to_owned());
    }
    if env.iter().any(|(name, value)| {
        name.is_empty()
            || name.len() > 256
            || name.contains(['=', '\0'])
            || value.len() > 65536
            || value.contains('\0')
    }) {
        return Err("终端环境变量格式无效".to_owned());
    }
    Ok(())
}

#[tauri::command]
pub fn terminal_create(
    id: String,
    cwd: String,
    cols: Option<u16>,
    rows: Option<u16>,
    env: Option<HashMap<String, String>>,
    app: AppHandle,
    manager: State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    // 同步 IPC 不阻塞等待异步 Git 门；清理中的目录不能插入新 PTY 引用。
    let _git = crate::ui_git_stash::STASH_GATE
        .try_lock()
        .map_err(|_| "工作树正在变更，请稍后重新打开终端".to_owned())?;
    if id.trim().is_empty() {
        return Err("终端标识不能为空".to_owned());
    }
    validate_environment(env.as_ref())?;
    let cwd_path = Path::new(&cwd);
    if !cwd_path.is_dir() {
        return Err("终端工作目录不存在或不是目录".to_owned());
    }
    // 先做一次轻量预检，真正的存在性判定在插入时原子完成，
    // 避免并发创建时后者静默覆盖前者并泄漏 PTY 子进程。
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
        .map_err(|error| terminal_error("创建 PTY 失败", error))?;
    let mut command =
        shell_command(&app).map_err(|error| keencode_model::redact_error_secrets(&error))?;
    let shell_cwd = shell_working_directory(cwd_path);
    command.cwd(&shell_cwd);
    // 原终端页面提供 TERM_PROGRAM 等展示环境；仅影响新建的 Shell 子进程。
    for (name, value) in env.unwrap_or_default() {
        command.env(name, value);
    }
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| terminal_error("启动系统 Shell 失败", error))?;
    drop(pair.slave);
    // take_writer / try_clone_reader 失败时，已启动的 shell 子进程必须就地
    // 收割，否则每次失败都会泄漏一个进程与句柄。
    let writer = match pair.master.take_writer() {
        Ok(writer) => std::sync::Arc::new(std::sync::Mutex::new(writer)),
        Err(error) => {
            let mut child = child;
            reap_child(&mut child);
            return Err(terminal_error("打开终端输入失败", error));
        }
    };
    let mut reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(error) => {
            let mut child = child;
            reap_child(&mut child);
            return Err(terminal_error("打开终端输出失败", error));
        }
    };

    // entry API 原子判定存在性：并发创建同一 id 时后来者报错，
    // 其已启动的 PTY 子进程必须就地收割，不能随局部变量泄漏。
    let history = Arc::new(Mutex::new(TerminalHistory::default()));
    if let std::collections::hash_map::Entry::Vacant(entry) =
        manager.sessions.lock().entry(id.clone())
    {
        entry.insert(TerminalSession {
            writer,
            master: pair.master,
            child,
            cwd,
            history: history.clone(),
        });
    } else {
        let mut child = child;
        reap_child(&mut child);
        return Err("终端已经存在".to_owned());
    }

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
                        publish_output(&output_app, &output_id, &history, &mut pending);
                    }
                    // 输出句柄 EOF 只表示 reader 已关闭，ConPTY 可能在子进程仍存活时先断开。
                    // 真实退出状态由 spawn_exit_watcher 从 child.try_wait 取得。
                    break;
                }
            };
            if should_flush_output(pending.len(), force) {
                publish_output(&output_app, &output_id, &history, &mut pending);
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
    spawn_exit_watcher(manager.inner().clone(), app, id);
    Ok(())
}

/// 页面刷新只重新附加原 PTY，绝不关闭或重启用户正在运行的进程。
#[tauri::command]
pub fn terminal_snapshot(
    id: String,
    manager: State<'_, Arc<TerminalManager>>,
) -> Result<Option<TerminalSnapshot>, String> {
    let mut sessions = manager.sessions.lock();
    let Some(session) = sessions.get_mut(&id) else {
        return Ok(None);
    };
    let status = session
        .child
        .try_wait()
        .map_err(|error| terminal_error("读取终端状态失败", error))?;
    let mut history = session.history.lock();
    if let Some(status) = status {
        history.exit_code = Some(status.exit_code());
    }
    Ok(Some(TerminalSnapshot {
        cwd: session.cwd.clone(),
        pid: session.child.process_id(),
        data: history.bytes.iter().copied().collect(),
        byte_offset: history.byte_offset,
        exited: history.exit_code.is_some() || history.exited,
        exit_code: history.exit_code,
        generation: history.generation,
    }))
}

/// 清除宿主回放缓存并广播位置，旧输出事件不能在刷新后重新出现。
#[tauri::command]
pub fn terminal_clear_history(
    id: String,
    app: AppHandle,
    manager: State<'_, Arc<TerminalManager>>,
) -> Result<u64, String> {
    let sessions = manager.sessions.lock();
    let session = sessions.get(&id).ok_or_else(|| "终端不存在".to_owned())?;
    let mut history = session.history.lock();
    history.bytes.clear();
    history.generation += 1;
    let byte_offset = history.byte_offset;
    let _ = app.emit(
        "terminal://cleared",
        TerminalCleared {
            id,
            byte_offset,
            generation: history.generation,
        },
    );
    Ok(byte_offset)
}

#[tauri::command]
pub async fn terminal_write(
    id: String,
    data: Vec<u8>,
    manager: State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    // PTY 输入是独占写入器，无法克隆：把阻塞写搬到 blocking 线程池，
    // 输入缓冲满（子进程停读）时只拖慢本次写入，不冻结 UI 与其他终端。
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // 只在拿取句柄时持有 sessions 锁；阻塞写使用会话独立写锁，
        // 其他终端与 close/resize 不再被单会话卡死的写拖住。
        let writer = {
            let sessions = manager.sessions.lock();
            sessions
                .get(&id)
                .map(|session| std::sync::Arc::clone(&session.writer))
                .ok_or_else(|| "终端不存在或已经退出".to_owned())?
        };
        let mut writer = writer.lock().map_err(|_| "终端写入器被占用".to_owned())?;
        writer
            .write_all(&data)
            .and_then(|_| writer.flush())
            .map_err(|error| terminal_error("写入终端失败", error))
    })
    .await
    .map_err(|error| terminal_error("终端写入后台任务失败", error))?
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
        .map_err(|error| terminal_error("调整终端尺寸失败", error))
}

#[tauri::command]
pub fn terminal_close(id: String, manager: State<'_, Arc<TerminalManager>>) -> Result<(), String> {
    // 先移出注册表再收割：wait 可能短暂阻塞，不能在持锁时执行；
    // 收割失败只记录不报错，关闭语义以"会话已移除"为准。
    if let Some(mut session) = manager.sessions.lock().remove(&id) {
        reap_child(&mut session.child);
    }
    Ok(())
}

impl Drop for TerminalManager {
    fn drop(&mut self) {
        for (_, mut session) in self.sessions.get_mut().drain() {
            reap_child(&mut session.child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OUTPUT_FLUSH_BYTES, shell_working_directory, should_flush_output, terminal_error};
    use std::path::Path;
    #[test]
    fn terminal_history_is_bounded_and_preserves_cumulative_offsets() {
        let mut history = super::TerminalHistory::default();
        let bytes = "中".repeat(super::HISTORY_MAX_BYTES / 3 + 10).into_bytes();
        assert_eq!(history.append(&bytes), bytes.len() as u64);
        assert!(history.bytes.len() <= super::HISTORY_MAX_BYTES);
        assert!(std::str::from_utf8(&history.bytes.iter().copied().collect::<Vec<_>>()).is_ok());
        history.bytes.clear();
        assert_eq!(history.append(b"new"), bytes.len() as u64 + 3);
        assert_eq!(history.bytes.iter().copied().collect::<Vec<_>>(), b"new");
    }
    #[test]
    fn presentation_environment_is_validated_without_exposing_values() {
        let mut values =
            std::collections::HashMap::from([("TERM_PROGRAM".to_owned(), "Synara".to_owned())]);
        assert!(super::validate_environment(Some(&values)).is_ok());
        values.insert("BAD=NAME".to_owned(), "private-value".to_owned());
        let error = super::validate_environment(Some(&values)).unwrap_err();
        assert!(!error.contains("private-value"));
        assert!(
            super::validate_environment(Some(&std::collections::HashMap::from([(
                "OK".to_owned(),
                "value\0".to_owned()
            )])))
            .is_err()
        );
    }

    /// 验证普通路径不会被终端工作目录转换改写。
    #[test]
    fn preserves_regular_working_directory() {
        let path = Path::new(r"D:\projects\keen-code");
        assert_eq!(shell_working_directory(path), path);
    }

    /// PTY 与阻塞任务的动态错误进入 IPC 前脱敏，但保留定位上下文。
    #[test]
    fn terminal_errors_are_redacted_before_ipc() {
        let safe = terminal_error(
            "写入终端失败",
            concat!(
                "request_id=req-terminal Authorization: Bearer terminal-secret ",
                "details={\"apiKey\":\"nested-terminal-secret\"}"
            ),
        );
        assert!(safe.starts_with("写入终端失败："));
        assert!(safe.contains("request_id=req-terminal"));
        assert!(safe.contains("Authorization: Bearer [REDACTED]"));
        assert!(!safe.contains("terminal-secret"));
        assert!(!safe.contains("nested-terminal-secret"));
    }

    #[test]
    fn terminal_output_flushes_on_size_timeout_or_exit() {
        assert!(!should_flush_output(0, false));
        assert!(!should_flush_output(0, true));
        assert!(!should_flush_output(16, false));
        assert!(should_flush_output(16, true));
        assert!(should_flush_output(OUTPUT_FLUSH_BYTES, false));
    }

    #[cfg(windows)]
    #[test]
    fn cmd_without_system_profile_disables_autorun() {
        assert_eq!(
            super::shell_arguments(super::TerminalShell::Cmd, false),
            &["/d"]
        );
        assert_eq!(
            super::shell_arguments(super::TerminalShell::Cmd, true),
            &[] as &[&str]
        );
        assert_eq!(
            super::shell_arguments(super::TerminalShell::GitBash, false),
            &["--noprofile", "-i"]
        );
    }

    /// 用真实 Windows ConPTY 验证 CMD 的交互生命周期；不能只依赖参数快照，
    /// 因为 UI 只会在收到真实退出事件后移除最后一个终端标签。
    #[cfg(windows)]
    #[test]
    fn cmd_pty_stays_interactive_until_explicit_exit() {
        use portable_pty::{CommandBuilder, PtySize, native_pty_system};
        use std::{
            io::{Read, Write},
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };

        let shell = super::detected_windows_shells()
            .into_iter()
            .find(|shell| shell.id == super::TerminalShell::Cmd)
            .expect("Windows 测试必须探测到 cmd.exe");
        let mut failures = Vec::new();

        for (label, args) in [
            (
                "inherit-profile",
                super::shell_arguments(super::TerminalShell::Cmd, true),
            ),
            (
                "without-profile",
                super::shell_arguments(super::TerminalShell::Cmd, false),
            ),
            ("keep-switch", &["/k"] as &[&str]),
        ] {
            let project = tempfile::tempdir().expect("创建 CMD PTY 测试目录");
            let marker = project.path().join(format!("cmd-{label}.marker"));
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows: 30,
                    cols: 100,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("创建 CMD ConPTY");
            let mut command = CommandBuilder::new(&shell.native_path);
            command.args(args);
            command.cwd(project.path());
            command.env("TERM_PROGRAM", "KeenCode");
            let mut child = pair
                .slave
                .spawn_command(command)
                .expect("启动 CMD ConPTY 子进程");
            drop(pair.slave);

            // 应用会把 master 存在 TerminalSession 中直到会话关闭；提前释放它会
            // 关闭 ConPTY 控制句柄，产生与 CMD 自身退出相同的假象。
            let master = pair.master;
            let mut writer = master.take_writer().expect("打开 CMD 输入");
            let mut reader = master.try_clone_reader().expect("打开 CMD 输出");
            let (output_tx, output_rx) = mpsc::channel();
            let mut reader_thread = Some(thread::spawn(move || {
                let mut output = Vec::new();
                let mut buffer = [0_u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(length) => output.extend_from_slice(&buffer[..length]),
                    }
                }
                let _ = output_tx.send(output);
            }));

            thread::sleep(Duration::from_millis(300));
            if let Some(status) = child.try_wait().expect("读取 CMD 初始状态") {
                let code = status.exit_code();
                let _ = child.wait();
                drop(writer);
                drop(master);
                let _ = output_rx.recv_timeout(Duration::from_secs(1));
                let _ = reader_thread.take().expect("reader thread 未初始化").join();
                failures.push(format!(
                    "CMD 在交互输入前退出: mode={label}, exit_code={code}"
                ));
                continue;
            }

            let command_line = format!(
                "echo NATIVE_CMD_PTY_OK > \"{}\"\r\n",
                marker.to_string_lossy()
            );
            writer
                .write_all(command_line.as_bytes())
                .expect("写入 CMD marker 命令");
            writer.flush().expect("刷新 CMD marker 命令");
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut marker_exit_code = None;
            while !marker.is_file() && Instant::now() < deadline {
                if let Some(status) = child.try_wait().expect("读取 CMD marker 状态") {
                    marker_exit_code = Some(status.exit_code());
                    break;
                }
                thread::sleep(Duration::from_millis(25));
            }
            if let Some(code) = marker_exit_code {
                failures.push(format!(
                    "CMD 在 marker 命令后退出: mode={label}, exit_code={code}"
                ));
                let _ = child.wait();
                drop(writer);
                drop(master);
                let _ = output_rx.recv_timeout(Duration::from_secs(1));
                let _ = reader_thread.take().expect("reader thread 未初始化").join();
                continue;
            }
            if !marker.is_file() {
                failures.push(format!("CMD 未在交互目录写入 marker: mode={label}"));
                let _ = child.kill();
                let _ = child.wait();
                drop(writer);
                drop(master);
                let _ = output_rx.recv_timeout(Duration::from_secs(1));
                let _ = reader_thread.take().expect("reader thread 未初始化").join();
                continue;
            }
            assert_eq!(
                std::fs::read_to_string(&marker)
                    .expect("读取 CMD marker")
                    .trim(),
                "NATIVE_CMD_PTY_OK"
            );

            writer.write_all(b"exit\r\n").expect("写入 CMD exit 命令");
            writer.flush().expect("刷新 CMD exit 命令");
            let deadline = Instant::now() + Duration::from_secs(2);
            while child.try_wait().expect("读取 CMD 退出状态").is_none()
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(25));
            }
            if child.try_wait().expect("读取 CMD 最终状态").is_none() {
                let _ = child.kill();
                let _ = child.wait();
                failures.push(format!("CMD 未响应显式 exit: mode={label}"));
                drop(writer);
                drop(master);
                let _ = output_rx.recv_timeout(Duration::from_secs(1));
                let _ = reader_thread.take().expect("reader thread 未初始化").join();
                continue;
            }
            drop(writer);
            drop(master);
            let _ = output_rx.recv_timeout(Duration::from_secs(1));
            let _ = reader_thread.take().expect("reader thread 未初始化").join();
        }
        assert!(failures.is_empty(), "{}", failures.join("; "));
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
