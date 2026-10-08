mod calls;
#[cfg(test)]
mod lifecycle_tests;
mod process;
pub mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
use crate::modules::workspace::validate_wsl_distro_name;
use crate::modules::workspace::{authorize_spawn_cwd, WorkspaceEnv, WorkspaceRegistry};

use process::{read_pipe, DrainControl, ProcessTree, DRAIN_TIMEOUT, PROCESS_POLL};
use session::{SessionRunOutput, ShellSession};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 300;
const MAX_OUTPUT_BYTES: usize = 256 * 1024;

#[derive(Debug, Serialize)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

#[tauri::command]
pub async fn shell_run_command(
    command: String,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
    workspace: Option<WorkspaceEnv>,
    registry: tauri::State<'_, WorkspaceRegistry>,
    state: tauri::State<'_, ShellState>,
) -> Result<CommandOutput, String> {
    let trimmed = command.trim().to_string();
    if trimmed.is_empty() {
        return Err("empty command".into());
    }
    let workspace = WorkspaceEnv::from_option(workspace);
    authorize_spawn_cwd(&registry, cwd.as_deref(), &workspace)?;
    let cwd_path = cwd
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string);
    let duration = timeout_duration(timeout_secs);
    let cancellation = state.cancellation.child_token();
    tauri::async_runtime::spawn_blocking(move || {
        run_blocking_cancel(trimmed, cwd_path, workspace, duration, cancellation)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn timeout_duration(timeout_secs: Option<u64>) -> Duration {
    Duration::from_secs(
        timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS),
    )
}

#[cfg(test)]
pub(crate) fn run_blocking_inner(
    command: String,
    cwd: Option<String>,
    workspace: WorkspaceEnv,
    duration: Duration,
) -> Result<CommandOutput, String> {
    run_blocking_cancel(command, cwd, workspace, duration, CancellationToken::new())
}

fn run_blocking_cancel(
    command: String,
    cwd: Option<String>,
    workspace: WorkspaceEnv,
    duration: Duration,
    cancellation: CancellationToken,
) -> Result<CommandOutput, String> {
    if cancellation.is_cancelled() {
        return Err("shell command cancelled".into());
    }
    let mut command = build_oneshot_command(&command, &workspace, cwd.as_deref())?;
    if let (WorkspaceEnv::Local, Some(directory)) = (&workspace, cwd) {
        command.current_dir(directory);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let process = ProcessTree::spawn(&mut command, &cancellation)?;
    let child = process.child();
    let stdout = child
        .take_stdout()
        .ok_or_else(|| "no stdout pipe".to_string())?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| "no stderr pipe".to_string())?;
    let drain = Arc::new(DrainControl::new());
    let stdout = capture_pipe(stdout, Arc::clone(&drain));
    let stderr = capture_pipe(stderr, Arc::clone(&drain));
    let deadline = Instant::now() + duration;
    let (exit_code, timed_out, cancelled) = loop {
        if cancellation.is_cancelled() {
            break (None, false, true);
        }
        if Instant::now() >= deadline {
            break (None, true, false);
        }
        match process.try_wait() {
            Ok(Some(status)) => break (status.code(), false, false),
            Ok(None) => thread::sleep(PROCESS_POLL),
            Err(error) => {
                process.terminate();
                drain.stop();
                return Err(error.to_string());
            }
        }
    };
    process.terminate();
    drain.finish();
    let cleanup_deadline = Instant::now() + DRAIN_TIMEOUT + PROCESS_POLL;
    while process
        .try_wait()
        .map_err(|error| error.to_string())?
        .is_none()
    {
        if Instant::now() >= cleanup_deadline {
            drain.stop();
            return Err("shell process tree did not exit before cleanup deadline".into());
        }
        thread::sleep(PROCESS_POLL);
    }
    while !(stdout.worker.is_finished() && stderr.worker.is_finished()) {
        if Instant::now() >= cleanup_deadline {
            drain.stop();
            return Err("shell output did not drain before cleanup deadline".into());
        }
        thread::sleep(PROCESS_POLL);
    }
    let stdout = stdout
        .worker
        .join()
        .map_err(|_| "shell stdout worker failed".to_string())??;
    let stderr = stderr
        .worker
        .join()
        .map_err(|_| "shell stderr worker failed".to_string())??;
    if cancelled {
        return Err("shell command cancelled".into());
    }
    Ok(CommandOutput {
        stdout: String::from_utf8_lossy(&stdout.0).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.0).into_owned(),
        exit_code,
        timed_out,
        truncated: stdout.1 || stderr.1,
    })
}

struct CapturedPipe {
    worker: thread::JoinHandle<Result<(Vec<u8>, bool), String>>,
}

fn capture_pipe(pipe: impl process::OutputPipe, drain: Arc<DrainControl>) -> CapturedPipe {
    let worker = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut truncated = false;
        let completed = read_pipe(pipe, &drain, |chunk| {
            let take = MAX_OUTPUT_BYTES
                .saturating_sub(bytes.len())
                .min(chunk.len());
            bytes.extend_from_slice(&chunk[..take]);
            truncated |= take < chunk.len();
        })
        .map_err(|error| error.to_string())?;
        if !completed {
            return Err("shell output drain deadline exceeded".into());
        }
        Ok((bytes, truncated))
    });
    CapturedPipe { worker }
}

pub struct ShellState {
    sessions: RwLock<HashMap<u32, Arc<ShellSession>>>,
    next_session_id: AtomicU32,
    cancellation: CancellationToken,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            next_session_id: AtomicU32::new(1),
            cancellation: CancellationToken::new(),
        }
    }
}

impl ShellState {
    pub fn shutdown(&self) {
        self.cancellation.cancel();
        let sessions = std::mem::take(&mut *self.sessions.write().unwrap());
        for session in sessions.values() {
            session.close();
        }
    }
}

impl Drop for ShellState {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[tauri::command]
pub fn shell_session_open(
    state: tauri::State<ShellState>,
    registry: tauri::State<WorkspaceRegistry>,
    cwd: Option<String>,
    workspace: Option<WorkspaceEnv>,
) -> Result<u32, String> {
    if state.cancellation.is_cancelled() {
        return Err("shell state closed".into());
    }
    let workspace = WorkspaceEnv::from_option(workspace);
    authorize_spawn_cwd(&registry, cwd.as_deref(), &workspace)?;
    let initial = match cwd.as_deref().filter(|path| !path.is_empty()) {
        Some(path) => path.to_string(),
        None => {
            if let WorkspaceEnv::Wsl { distro } = &workspace {
                crate::modules::workspace::wsl_home(distro.clone())?
            } else {
                crate::modules::fs::to_canon(dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")))
            }
        }
    };
    let session = Arc::new(ShellSession::new(initial, workspace));
    let id = state.next_session_id.fetch_add(1, Ordering::Relaxed);
    state.sessions.write().unwrap().insert(id, session);
    Ok(id)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn shell_session_run(
    state: tauri::State<'_, ShellState>,
    registry: tauri::State<'_, WorkspaceRegistry>,
    id: u32,
    command: String,
    cwd: Option<String>,
    timeout_secs: Option<u64>,
    workspace: Option<WorkspaceEnv>,
    call_id: Option<String>,
) -> Result<SessionRunOutput, String> {
    let session = state
        .sessions
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| "no shell session".to_string())?;
    let effective_workspace = workspace
        .clone()
        .unwrap_or_else(|| session.workspace.clone());
    authorize_spawn_cwd(&registry, cwd.as_deref(), &effective_workspace)?;
    let duration = timeout_duration(timeout_secs);
    let cancellation = match call_id.as_deref() {
        Some(id) => session.begin_call(id)?,
        None => session.cancellation(),
    };
    tauri::async_runtime::spawn_blocking(move || {
        let result = session.run(command, cwd, workspace, duration, cancellation);
        if let Some(id) = call_id {
            session.finish_call(&id);
        }
        result
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub fn shell_session_cancel(
    state: tauri::State<ShellState>,
    id: u32,
    call_id: String,
) -> Result<(), String> {
    let session = state
        .sessions
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| "no shell session".to_string())?;
    session.cancel_call(&call_id)
}

#[tauri::command]
pub fn shell_session_close(state: tauri::State<ShellState>, id: u32) -> Result<(), String> {
    if let Some(session) = state.sessions.write().unwrap().remove(&id) {
        session.close();
    }
    Ok(())
}

pub(crate) fn build_oneshot_command(
    command: &str,
    #[cfg_attr(not(windows), allow(unused_variables))] workspace: &WorkspaceEnv,
    #[cfg_attr(not(windows), allow(unused_variables))] cwd: Option<&str>,
) -> Result<Command, String> {
    #[cfg(windows)]
    if let WorkspaceEnv::Wsl { distro } = workspace {
        validate_wsl_distro_name(distro)?;
        let mut cmd = Command::new("wsl.exe");
        cmd.arg("-d").arg(distro);
        if let Some(cwd) = cwd.filter(|s| !s.is_empty()) {
            cmd.arg("--cd").arg(cwd);
        }
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        cmd.arg("--exec")
            .arg("setsid")
            .arg("--wait")
            .arg("env")
            .arg(format!("RCODE_SHELL_TOKEN={token}"))
            .arg("sh")
            .arg("-lc")
            .arg(command);
        cmd.env("RCODE_WSL_TREE_DISTRO", distro)
            .env("RCODE_WSL_TREE_TOKEN", token);
        return Ok(cmd);
    }
    #[cfg(unix)]
    {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(command);
        for (key, value) in crate::modules::workspace::appimage_env_overrides() {
            match value {
                Some(v) => {
                    cmd.env(key, v);
                }
                None => {
                    cmd.env_remove(key);
                }
            }
        }
        Ok(cmd)
    }
    #[cfg(windows)]
    {
        let shell = crate::modules::pty::shell_init::windows_shell_path();
        let mut cmd = Command::new(&shell);
        let is_cmd = shell
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("cmd.exe"))
            .unwrap_or(false);
        if is_cmd {
            cmd.arg("/C").arg(command);
        } else {
            cmd.arg("-NoProfile").arg("-Command").arg(command);
        }
        Ok(cmd)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn run(cmd: &str, timeout_secs: u64) -> CommandOutput {
        run_blocking_inner(
            cmd.into(),
            None,
            WorkspaceEnv::Local,
            Duration::from_secs(timeout_secs),
        )
        .expect("run")
    }

    #[test]
    fn run_blocking_captures_stdout_and_zero_exit() {
        let out = run("printf 'hello\\n'", 5);
        assert_eq!(out.stdout, "hello\n");
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(!out.truncated);
    }

    #[test]
    fn run_blocking_captures_stderr_and_nonzero_exit() {
        let out = run("printf 'oops\\n' >&2; exit 3", 5);
        assert!(out.stderr.contains("oops"));
        assert_eq!(out.exit_code, Some(3));
    }

    #[test]
    fn run_blocking_times_out_long_running_command() {
        let out = run("sleep 10", 1);
        assert!(out.timed_out);
        assert_eq!(out.exit_code, None);
    }

    #[test]
    fn run_blocking_truncates_huge_output() {
        let big = MAX_OUTPUT_BYTES + 4096;
        let out = run(&format!("head -c {big} /dev/zero"), 10);
        assert!(out.truncated);
        assert!(out.stdout.len() <= MAX_OUTPUT_BYTES);
    }

    #[test]
    fn build_oneshot_command_uses_sh_minus_c_on_unix() {
        let cmd = build_oneshot_command("echo hi", &WorkspaceEnv::Local, None).unwrap();
        assert_eq!(cmd.get_program(), "/bin/sh");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, vec!["-c", "echo hi"]);
    }
}
