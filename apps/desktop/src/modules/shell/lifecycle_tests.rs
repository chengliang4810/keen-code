use super::*;
use std::path::Path;

#[cfg(windows)]
fn quoted_path(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn long_descendant_command(path: &Path) -> String {
    #[cfg(windows)]
    {
        format!("$child = Start-Process cmd.exe -ArgumentList '/C', 'ping -n 60 127.0.0.1 > nul' -NoNewWindow -PassThru; Set-Content -LiteralPath '{}' -Value $child.Id; Wait-Process -Id $child.Id", quoted_path(path))
    }
    #[cfg(unix)]
    {
        format!(
            "sleep 60 & child=$!; printf '%s' \"$child\" > '{}'; wait \"$child\"",
            path.to_string_lossy().replace('\'', "'\\''")
        )
    }
}

fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse::<u32>() {
                return pid;
            }
        }
        assert!(Instant::now() < deadline, "test descendant did not start");
        thread::sleep(PROCESS_POLL);
    }
}

pub(super) fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
        };
        let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if process.is_null() {
            return false;
        }
        let result = WaitForSingleObject(process, 0) == WAIT_TIMEOUT;
        CloseHandle(process);
        result
    }
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}

fn assert_exited(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_alive(pid) && Instant::now() < deadline {
        thread::sleep(PROCESS_POLL);
    }
    assert!(!process_alive(pid), "owned descendant {pid} survived");
}

#[test]
fn timeout_finishes_with_live_pipe_descendant_and_kills_tree() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("child.pid");
    let command = long_descendant_command(&pid_file);
    let started = Instant::now();
    let output =
        run_blocking_inner(command, None, WorkspaceEnv::Local, Duration::from_secs(2)).unwrap();
    assert!(output.timed_out);
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid = wait_for_pid(&pid_file);
    assert_exited(pid);
}

#[test]
fn cancellation_cleans_running_descendants_but_not_independent_background() {
    let directory = tempfile::tempdir().unwrap();
    let background_pid_file = directory.path().join("background.pid");
    let mut command = build_oneshot_command(
        &long_descendant_command(&background_pid_file),
        &WorkspaceEnv::Local,
        None,
    )
    .unwrap();
    let background = ProcessTree::spawn(&mut command, &CancellationToken::new()).unwrap();
    let background_pid = wait_for_pid(&background_pid_file);
    let foreground_pid_file = directory.path().join("foreground.pid");
    let foreground = long_descendant_command(&foreground_pid_file);
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let worker = thread::spawn(move || {
        run_blocking_cancel(
            foreground,
            None,
            WorkspaceEnv::Local,
            Duration::from_secs(30),
            token,
        )
    });
    let foreground_pid = wait_for_pid(&foreground_pid_file);
    let started = Instant::now();
    cancellation.cancel();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_exited(foreground_pid);
    assert!(process_alive(background_pid));
    background.terminate();
    assert_exited(background_pid);
    let _ = background.child().wait();
}

#[test]
fn pre_cancelled_command_does_not_create_file() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("must-not-exist");
    #[cfg(windows)]
    let command = format!(
        "Set-Content -LiteralPath '{}' -Value 'unexpected'",
        quoted_path(&file)
    );
    #[cfg(unix)]
    let command = format!("touch '{}'", file.to_string_lossy().replace('\'', "'\\''"));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(run_blocking_cancel(
        command,
        None,
        WorkspaceEnv::Local,
        Duration::from_secs(2),
        cancellation
    )
    .is_err());
    assert!(!file.exists());
}

#[test]
fn session_retains_cwd_and_close_cancels_queued_call() {
    let directory = tempfile::tempdir().unwrap();
    let next = directory.path().join("next");
    std::fs::create_dir(&next).unwrap();
    let session = Arc::new(ShellSession::new(
        directory.path().to_string_lossy().into_owned(),
        WorkspaceEnv::Local,
    ));
    #[cfg(windows)]
    let command = "Set-Location next; Write-Output 'moved'";
    #[cfg(unix)]
    let command = "cd next; printf moved";
    let output = session
        .run(
            command.into(),
            None,
            None,
            Duration::from_secs(5),
            session.cancellation(),
        )
        .unwrap();
    assert!(output.stdout.contains("moved"));
    assert_eq!(
        std::fs::canonicalize(session.current_cwd()).unwrap(),
        std::fs::canonicalize(next).unwrap()
    );
    let cancellation = session.begin_call("queued").unwrap();
    let execution = session.reserve_execution();
    let other = Arc::clone(&session);
    let worker = thread::spawn(move || {
        other.run(
            "echo skipped".into(),
            None,
            None,
            Duration::from_secs(5),
            cancellation,
        )
    });
    session.close();
    assert!(worker.join().unwrap().is_err());
    drop(execution);
}
