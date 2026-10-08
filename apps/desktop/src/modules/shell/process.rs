use std::io::{self, Read};
use std::process::{ChildStderr, ChildStdout, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::thread;
use std::time::{Duration, Instant};

use shared_child::SharedChild;

pub(super) const PROCESS_POLL: Duration = Duration::from_millis(20);
pub(super) const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct ProcessTree {
    child: Arc<SharedChild>,
    stopped: AtomicBool,
    #[cfg(windows)]
    job: crate::modules::proc::job::ProcessJob,
    #[cfg(windows)]
    wsl: Option<rcode_tools::WslProcessTree>,
}

impl ProcessTree {
    pub(super) fn spawn(
        command: &mut Command,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<Arc<Self>, String> {
        if cancellation.is_cancelled() {
            return Err("shell command cancelled".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000 | 0x0000_0004);
        }
        let child = Arc::new(SharedChild::spawn(command).map_err(|error| error.to_string())?);
        #[cfg(windows)]
        let job = match crate::modules::proc::job::ProcessJob::create_for(child.id()) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot supervise shell process tree: {error}"));
            }
        };
        let tree = Arc::new(Self {
            child,
            stopped: AtomicBool::new(false),
            #[cfg(windows)]
            job,
            #[cfg(windows)]
            wsl: rcode_tools::WslProcessTree::from_command(command),
        });
        if cancellation.is_cancelled() {
            tree.terminate();
            let _ = tree.child.wait();
            return Err("shell command cancelled".into());
        }
        #[cfg(windows)]
        if let Err(error) = resume_suspended_process(tree.child.id()) {
            tree.terminate();
            let _ = tree.child.wait();
            return Err(format!("cannot start supervised shell process: {error}"));
        }
        Ok(tree)
    }

    pub(super) fn child(&self) -> Arc<SharedChild> {
        Arc::clone(&self.child)
    }

    pub(super) fn terminate(&self) {
        if self.stopped.swap(true, Ordering::AcqRel) {
            return;
        }
        #[cfg(windows)]
        if let Some(wsl) = &self.wsl {
            if let Err(error) = wsl.terminate() {
                log::warn!("WSL shell process-tree termination failed: {error}");
            }
        }
        #[cfg(windows)]
        if let Err(error) = self.job.terminate() {
            log::warn!("shell process-tree termination failed: {error}");
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
    }

    pub(super) fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(windows)]
fn resume_suspended_process(pid: u32) -> io::Result<()> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut present = Thread32First(snapshot, &mut entry) != 0;
        let mut result = Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no shell startup thread",
        ));
        while present {
            if entry.th32OwnerProcessID == pid {
                let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if handle.is_null() {
                    result = Err(io::Error::last_os_error());
                } else {
                    let resumed = ResumeThread(handle);
                    result = if resumed == u32::MAX {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    };
                    CloseHandle(handle);
                }
                break;
            }
            present = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        result
    }
}

pub(super) trait OutputPipe: Read + Send + 'static {
    fn available(&self) -> io::Result<Option<usize>>;
}

#[cfg(windows)]
fn available_windows(pipe: &impl std::os::windows::io::AsRawHandle) -> io::Result<Option<usize>> {
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, ERROR_PIPE_NOT_CONNECTED};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let mut available = 0;
    if unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_PIPE_NOT_CONNECTED as i32)
        {
            return Ok(None);
        }
        return Err(error);
    }
    Ok(Some(available as usize))
}

#[cfg(unix)]
fn available_unix(pipe: &impl std::os::unix::io::AsRawFd) -> io::Result<Option<usize>> {
    let mut descriptor = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut descriptor, 1, PROCESS_POLL.as_millis() as i32) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::Interrupted {
            Ok(Some(0))
        } else {
            Err(error)
        };
    }
    if ready == 0 {
        return Ok(Some(0));
    }
    if descriptor.revents & libc::POLLIN != 0 {
        return Ok(Some(8192));
    }
    if descriptor.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
        return Ok(None);
    }
    Ok(Some(0))
}

macro_rules! output_pipe {
    ($pipe:ty) => {
        impl OutputPipe for $pipe {
            fn available(&self) -> io::Result<Option<usize>> {
                #[cfg(windows)]
                {
                    available_windows(self)
                }
                #[cfg(unix)]
                {
                    available_unix(self)
                }
            }
        }
    };
}
output_pipe!(ChildStdout);
output_pipe!(ChildStderr);

pub(super) struct DrainControl {
    stop: AtomicBool,
    deadline: Mutex<Option<Instant>>,
}

impl DrainControl {
    pub(super) fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            deadline: Mutex::new(None),
        }
    }

    pub(super) fn finish(&self) {
        self.deadline
            .lock()
            .unwrap()
            .get_or_insert_with(|| Instant::now() + DRAIN_TIMEOUT);
    }

    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
            || self
                .deadline
                .lock()
                .unwrap()
                .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

pub(super) fn read_pipe(
    mut pipe: impl OutputPipe,
    control: &DrainControl,
    mut on_bytes: impl FnMut(&[u8]),
) -> io::Result<bool> {
    let mut buffer = [0u8; 8192];
    while !control.stopped() {
        match pipe.available()? {
            None => return Ok(true),
            Some(0) => {
                #[cfg(windows)]
                thread::sleep(PROCESS_POLL);
            }
            Some(available) => {
                let length = available.min(buffer.len());
                match pipe.read(&mut buffer[..length]) {
                    Ok(0) => return Ok(true),
                    Ok(count) => on_bytes(&buffer[..count]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(false)
}
