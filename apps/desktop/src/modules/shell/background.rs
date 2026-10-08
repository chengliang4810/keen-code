use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use super::process::{read_pipe, DrainControl, ProcessTree, DRAIN_TIMEOUT, PROCESS_POLL};
use super::ringbuffer::BoundedRingBuffer;
use crate::modules::workspace::{resolve_path, WorkspaceEnv};

const RING_CAP: usize = 4 * 1024 * 1024;
const DEFAULT_PAGE_BYTES: usize = 64 * 1024;
const MAX_PAGE_BYTES: usize = 128 * 1024;
const MAX_RUNNING: usize = 16;
const MAX_HISTORY: usize = 16;
const TOTAL_LOG_BUDGET: usize = 64 * 1024 * 1024;
const HISTORY_TTL: Duration = Duration::from_secs(3600);

struct BackgroundState {
    buffer: Mutex<BoundedRingBuffer>,
    drain: DrainControl,
    readers_finished: AtomicUsize,
    exited: AtomicBool,
    exit_code: AtomicI32,
    exit_unknown: AtomicBool,
    completed_at: Mutex<Option<Instant>>,
    completion: Condvar,
}

pub struct BackgroundProc {
    pub command: String,
    pub cwd: Option<String>,
    pub started_at_ms: u64,
    process: Arc<ProcessTree>,
    state: Arc<BackgroundState>,
}

#[derive(Serialize)]
pub struct BackgroundLogResponse {
    pub bytes: String,
    pub next_offset: u64,
    pub dropped: u64,
    pub exited: bool,
    pub exit_code: Option<i32>,
    pub has_more: bool,
}

#[derive(Serialize)]
pub struct BackgroundProcInfo {
    pub handle: u32,
    pub command: String,
    pub cwd: Option<String>,
    pub started_at_ms: u64,
    pub exited: bool,
    pub exit_code: Option<i32>,
}

impl BackgroundProc {
    fn exit_status(&self) -> (bool, Option<i32>) {
        let exited = self.state.exited.load(Ordering::Acquire);
        let code = if exited && !self.state.exit_unknown.load(Ordering::Acquire) {
            Some(self.state.exit_code.load(Ordering::Acquire))
        } else {
            None
        };
        (exited, code)
    }

    pub fn read_logs(&self, since: u64) -> BackgroundLogResponse {
        self.read_logs_page(since, None)
    }

    pub fn read_logs_page(&self, since: u64, max_bytes: Option<usize>) -> BackgroundLogResponse {
        let limit = max_bytes
            .unwrap_or(DEFAULT_PAGE_BYTES)
            .clamp(4, MAX_PAGE_BYTES);
        let (exited, exit_code) = self.exit_status();
        let (bytes, next_offset, dropped, has_more) = self
            .state
            .buffer
            .lock()
            .unwrap()
            .read_utf8_page(since, limit, exited);
        BackgroundLogResponse {
            bytes: String::from_utf8_lossy(&bytes).into_owned(),
            next_offset,
            dropped,
            exited,
            exit_code,
            has_more,
        }
    }

    pub fn kill(&self) {
        self.process.terminate();
        self.state.drain.finish();
        let completed = self.state.completed_at.lock().unwrap();
        let _ = self.state.completion.wait_timeout_while(
            completed,
            DRAIN_TIMEOUT + PROCESS_POLL,
            |time| time.is_none(),
        );
    }

    pub fn info(&self, handle: u32) -> BackgroundProcInfo {
        let (exited, exit_code) = self.exit_status();
        BackgroundProcInfo {
            handle,
            command: self.command.clone(),
            cwd: self.cwd.clone(),
            started_at_ms: self.started_at_ms,
            exited,
            exit_code,
        }
    }

    fn completed_at(&self) -> Option<Instant> {
        *self.state.completed_at.lock().unwrap()
    }
}

impl Drop for BackgroundProc {
    fn drop(&mut self) {
        self.process.terminate();
        self.state.drain.stop();
        *self.state.buffer.lock().unwrap() = BoundedRingBuffer::new(0);
    }
}

#[derive(Default)]
pub(super) struct BackgroundRegistry {
    processes: HashMap<u32, Arc<BackgroundProc>>,
    closed: bool,
}

impl BackgroundRegistry {
    fn prune(&mut self, reserve: bool) -> Result<(), String> {
        let entries: Vec<_> = self
            .processes
            .iter()
            .map(|(id, process)| (*id, process.completed_at()))
            .collect();
        for id in retention_plan(&entries, reserve, self.closed, Instant::now())? {
            self.processes.remove(&id);
        }
        Ok(())
    }

    pub(super) fn spawn(
        &mut self,
        id: u32,
        command: String,
        cwd: Option<String>,
        workspace: WorkspaceEnv,
    ) -> Result<(), String> {
        self.prune(true)?;
        let process = spawn(command, cwd, workspace)?;
        self.processes.insert(id, process);
        Ok(())
    }

    pub(super) fn get(&mut self, handle: u32) -> Result<Arc<BackgroundProc>, String> {
        self.prune(false)?;
        self.processes
            .get(&handle)
            .cloned()
            .ok_or_else(|| "background handle expired or not found".into())
    }

    pub(super) fn list(&mut self) -> Result<Vec<BackgroundProcInfo>, String> {
        self.prune(false)?;
        let mut output: Vec<_> = self
            .processes
            .iter()
            .map(|(handle, process)| process.info(*handle))
            .collect();
        output.sort_by_key(|process| process.handle);
        Ok(output)
    }

    pub(super) fn shutdown(&mut self) {
        self.closed = true;
        for process in self.processes.values() {
            process.process.terminate();
            process.state.drain.stop();
        }
        self.processes.clear();
    }
}

fn retention_plan(
    entries: &[(u32, Option<Instant>)],
    reserve: bool,
    closed: bool,
    now: Instant,
) -> Result<Vec<u32>, String> {
    let mut expired = Vec::new();
    let mut history = Vec::new();
    let mut running = 0;
    for (id, completed) in entries {
        match completed {
            Some(time) if now.saturating_duration_since(*time) >= HISTORY_TTL => expired.push(*id),
            Some(time) => history.push((*id, *time)),
            None => running += 1,
        }
    }
    if reserve && (closed || running >= MAX_RUNNING) {
        return Err("background process capacity reached".into());
    }
    history.sort_by_key(|(_, time)| *time);
    let capacity = (TOTAL_LOG_BUDGET / RING_CAP).saturating_sub(usize::from(reserve));
    let remove = (running + history.len())
        .saturating_sub(capacity)
        .max(history.len().saturating_sub(MAX_HISTORY));
    if remove > history.len() {
        return Err("background log capacity reached".into());
    }
    expired.extend(history.into_iter().take(remove).map(|(id, _)| id));
    Ok(expired)
}

pub fn spawn(
    command: String,
    cwd: Option<String>,
    workspace: WorkspaceEnv,
) -> Result<Arc<BackgroundProc>, String> {
    let trimmed = command.trim().to_string();
    if trimmed.is_empty() {
        return Err("empty command".into());
    }
    if trimmed.len() > 64 * 1024 || cwd.as_ref().is_some_and(|path| path.len() > 8192) {
        return Err("background command metadata too large".into());
    }
    if let Some(ref directory) = cwd {
        if !resolve_path(directory, &workspace).is_dir() {
            return Err(format!("cwd is not a directory: {directory}"));
        }
    }
    let mut command = super::build_oneshot_command(&trimmed, &workspace, cwd.as_deref())?;
    if let (WorkspaceEnv::Local, Some(ref directory)) = (&workspace, &cwd) {
        command.current_dir(directory);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let process = ProcessTree::spawn(&mut command, &tokio_util::sync::CancellationToken::new())?;
    let child = process.child();
    let stdout = child
        .take_stdout()
        .ok_or_else(|| "no stdout pipe".to_string())?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| "no stderr pipe".to_string())?;
    let state = Arc::new(BackgroundState {
        buffer: Mutex::new(BoundedRingBuffer::new(RING_CAP)),
        drain: DrainControl::new(),
        readers_finished: AtomicUsize::new(0),
        exited: AtomicBool::new(false),
        exit_code: AtomicI32::new(0),
        exit_unknown: AtomicBool::new(false),
        completed_at: Mutex::new(None),
        completion: Condvar::new(),
    });
    let started_at_ms = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let output = Arc::new(BackgroundProc {
        command: trimmed,
        cwd,
        started_at_ms,
        process,
        state: Arc::clone(&state),
    });
    spawn_reader(stdout, Arc::clone(&state));
    spawn_reader(stderr, Arc::clone(&state));
    let process = Arc::downgrade(&output.process);
    thread::spawn(move || {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    match status.code() {
                        Some(code) => state.exit_code.store(code, Ordering::Release),
                        None => state.exit_unknown.store(true, Ordering::Release),
                    }
                    break;
                }
                Ok(None) => thread::sleep(PROCESS_POLL),
                Err(_) => {
                    state.exit_unknown.store(true, Ordering::Release);
                    break;
                }
            }
        }
        if let Some(process) = process.upgrade() {
            process.terminate();
        }
        state.drain.finish();
        let deadline = Instant::now() + DRAIN_TIMEOUT + PROCESS_POLL;
        while state.readers_finished.load(Ordering::Acquire) != 2 && Instant::now() < deadline {
            thread::sleep(PROCESS_POLL);
        }
        state.drain.stop();
        let mut completed = state.completed_at.lock().unwrap();
        state.exited.store(true, Ordering::Release);
        *completed = Some(Instant::now());
        state.completion.notify_all();
    });
    Ok(output)
}

fn spawn_reader(pipe: impl super::process::OutputPipe, state: Arc<BackgroundState>) {
    thread::spawn(move || {
        if let Err(error) = read_pipe(pipe, &state.drain, |bytes| {
            state.buffer.lock().unwrap().push(bytes)
        }) {
            log::warn!("background output read failed: {error}");
        }
        state.readers_finished.fetch_add(1, Ordering::Release);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_never_evicts_running_processes() {
        let now = Instant::now();
        let active: Vec<_> = (0..MAX_RUNNING).map(|id| (id as u32, None)).collect();
        assert!(retention_plan(&active, true, false, now).is_err());
        assert_eq!(
            retention_plan(&active, false, false, now).unwrap(),
            Vec::<u32>::new()
        );
        assert!(retention_plan(&[], true, true, now).is_err());
    }

    #[test]
    fn exited_history_is_pruned_oldest_first_to_reserved_total_byte_budget() {
        let now = Instant::now();
        let entries: Vec<_> = (0..100)
            .map(|id| (id, Some(now - Duration::from_secs(100 - id as u64))))
            .collect();
        let removed = retention_plan(&entries, true, false, now).unwrap();
        assert_eq!(removed, (0..85).collect::<Vec<_>>());
        let retained_with_new = entries.len() - removed.len() + 1;
        assert!(retained_with_new * RING_CAP <= TOTAL_LOG_BUDGET);
        assert!(retained_with_new <= MAX_HISTORY);
    }

    #[test]
    fn ttl_expires_only_finished_handles_and_recent_history_survives() {
        let now = Instant::now();
        let entries = [(1, None), (2, Some(now - HISTORY_TTL)), (3, Some(now))];
        assert_eq!(
            retention_plan(&entries, false, false, now).unwrap(),
            vec![2]
        );
    }
}
