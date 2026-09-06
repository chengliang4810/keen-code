//! stdio MCP 服务进程树的跨平台生命周期边界。

use crate::error::McpError;

/// 在创建子进程前准备、在创建后接管并在释放时终止整棵进程树。
pub(crate) struct ProcessTree {
    #[cfg(unix)]
    process_group: Option<i32>,
    #[cfg(windows)]
    job_handle: isize,
}

impl ProcessTree {
    /// 配置新进程组或创建 Windows Job Object。
    pub(crate) fn prepare(command: &mut tokio::process::Command) -> Result<Self, McpError> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.as_std_mut().process_group(0);
            Ok(Self {
                process_group: None,
            })
        }
        #[cfg(windows)]
        {
            use std::mem::{size_of, zeroed};
            use std::os::windows::process::CommandExt as _;
            use std::ptr::null;
            use windows_sys::Win32::System::JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };
            use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

            command.as_std_mut().creation_flags(CREATE_SUSPENDED);

            // SAFETY: 传入空安全描述符和名称来创建当前进程私有 Job；返回句柄由 Drop 关闭。
            let job = unsafe { CreateJobObjectW(null(), null()) };
            if job.is_null() {
                return Err(McpError::Transport(format!(
                    "创建 stdio MCP Windows Job Object 失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            // SAFETY: 该 POD 结构允许全零初始化，随后仅设置文档定义的 LimitFlags。
            let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
            information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: job 是有效句柄，information 在调用期间存活，长度与结构类型一致。
            let configured = unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&raw const information).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                // SAFETY: job 是本函数刚创建且尚未转移所有权的有效句柄。
                unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
                return Err(McpError::Transport(format!(
                    "配置 stdio MCP Windows Job Object 失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self {
                job_handle: job as isize,
            })
        }
    }

    /// 把刚创建的子进程接管到独立进程组或 Job Object。
    pub(crate) fn attach(&mut self, child: &tokio::process::Child) -> Result<(), McpError> {
        #[cfg(unix)]
        {
            let process_id = child
                .id()
                .ok_or_else(|| McpError::Transport("stdio MCP 子进程缺少 PID".to_owned()))?;
            self.process_group = Some(process_id as i32);
            Ok(())
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
            let process = child.raw_handle().ok_or_else(|| {
                McpError::Transport("stdio MCP 子进程缺少 Windows 句柄".to_owned())
            })?;
            // SAFETY: 两个句柄均有效；Job 句柄由 self 独占并活到子进程退出之后。
            if unsafe { AssignProcessToJobObject(self.job_handle as _, process as _) } == 0 {
                return Err(McpError::Transport(format!(
                    "把 stdio MCP 子进程加入 Windows Job Object 失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
    }

    /// Windows 在 Job 接管完成后恢复主线程；其他平台无需额外操作。
    pub(crate) fn resume(&self, child: &tokio::process::Child) -> Result<(), McpError> {
        #[cfg(not(windows))]
        {
            let _ = (self, child);
            Ok(())
        }
        #[cfg(windows)]
        {
            use std::mem::size_of;
            use windows_sys::Win32::Foundation::{CloseHandle, FALSE, INVALID_HANDLE_VALUE};
            use windows_sys::Win32::System::Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First,
                Thread32Next,
            };
            use windows_sys::Win32::System::Threading::{
                OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
            };

            let process_id = child
                .id()
                .ok_or_else(|| McpError::Transport("stdio MCP 子进程缺少 PID".to_owned()))?;
            // SAFETY: 创建只读线程快照；返回句柄在本分支所有路径关闭。
            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(McpError::Transport(format!(
                    "枚举 stdio MCP 主线程失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            let mut entry = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..THREADENTRY32::default()
            };
            // SAFETY: snapshot 有效且 entry 的尺寸字段已经按 API 要求设置。
            let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
            let mut thread_id = None;
            while has_entry {
                if entry.th32OwnerProcessID == process_id {
                    thread_id = Some(entry.th32ThreadID);
                    break;
                }
                // SAFETY: snapshot 与 entry 在循环期间保持有效。
                has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
            }
            // SAFETY: snapshot 是本函数创建的有效句柄且只关闭一次。
            unsafe { CloseHandle(snapshot) };
            let thread_id = thread_id
                .ok_or_else(|| McpError::Transport("找不到已挂起的 stdio MCP 主线程".to_owned()))?;
            // SAFETY: thread_id 来自目标进程的线程快照，仅请求恢复权限且不继承句柄。
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, FALSE, thread_id) };
            if thread.is_null() {
                return Err(McpError::Transport(format!(
                    "打开已挂起的 stdio MCP 主线程失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            // SAFETY: thread 是具有 THREAD_SUSPEND_RESUME 权限的有效线程句柄。
            let previous_count = unsafe { ResumeThread(thread) };
            // SAFETY: thread 是本函数打开的有效句柄且只关闭一次。
            unsafe { CloseHandle(thread) };
            if previous_count == u32::MAX {
                return Err(McpError::Transport(format!(
                    "恢复 stdio MCP 主线程失败：{}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
    }

    /// 立即终止进程组或 Job Object 中仍然存活的全部进程。
    pub(crate) fn terminate(&self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            // SAFETY: 负 PID 按 POSIX 表示目标进程组；信号仅作用于为子进程创建的独立组。
            unsafe {
                libc::kill(-process_group, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        {
            // SAFETY: self 持有有效 Job 句柄；终止码仅用于被清理的子进程。
            unsafe {
                windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job_handle as _, 1);
            }
        }
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.terminate();
        #[cfg(windows)]
        {
            // SAFETY: job_handle 由 prepare 创建且只在这里关闭一次。
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.job_handle as _);
            }
        }
    }
}
