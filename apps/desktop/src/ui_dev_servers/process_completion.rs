//! command-group的Windows完成端口可先收到创建/退出消息；停止成功还需等待真实进程句柄。
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

pub(super) struct ProcessHandle(HANDLE);
// Windows内核等待句柄可在线程间转移；此包装只等待和关闭，不执行进程操作。
unsafe impl Send for ProcessHandle {}
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(super) fn capture(root: u32) -> Result<Vec<ProcessHandle>, String> {
    let system = sysinfo::System::new_with_specifics(
        sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing()),
    );
    let parents: HashMap<_, _> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            process
                .parent()
                .map(|parent| (pid.as_u32(), parent.as_u32()))
        })
        .collect();
    let mut handles = Vec::new();
    for pid in system
        .processes()
        .keys()
        .map(|pid| pid.as_u32())
        .filter(|pid| super::local_servers::owned_by(*pid, root, &parents))
    {
        if handles.len() >= 2048 {
            return Err("开发脚本后代进程超过等待上限".into());
        }
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            let cause = std::io::Error::last_os_error();
            // PID已在快照后消失时无需再等待；访问失败不能伪装为已经退出。
            if cause.raw_os_error() == Some(87) {
                continue;
            }
            return Err(super::error("无法取得开发脚本退出等待句柄", cause));
        }
        handles.push(ProcessHandle(handle));
    }
    Ok(handles)
}

pub(super) fn wait(handles: Vec<ProcessHandle>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    for handle in handles {
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u32::MAX as u128) as u32;
        let result = unsafe { WaitForSingleObject(handle.0, timeout) };
        if result != WAIT_OBJECT_0 {
            return Err("开发脚本后代进程退出尚未完成".into());
        }
    }
    Ok(())
}
