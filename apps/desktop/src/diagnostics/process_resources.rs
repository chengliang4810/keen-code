//! 平台进程资源采样适配器。
//!
//! Windows 通过 ToolHelp 枚举当前宿主进程下的 WebView2 后代，再用进程句柄读取
//! 工作集（RSS）、PrivateUsage、PagefileUsage 和累计 CPU 时间。其他平台不猜测
//! 兼容 API，明确返回 `None`，由面板显示为“未报告”。

/// 一次宿主进程及其 WebView2 后代的聚合资源摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProcessResourceSample {
    /// 相邻采样间隔内的进程 CPU 占用百分比；首个样本没有基线时为 `None`。
    pub cpu_percent: Option<f64>,
    /// 所有可读取进程的工作集总量，即 RSS 近似值。
    pub resident_bytes: Option<u64>,
    /// 所有可读取进程的 PrivateUsage 总量。
    pub private_bytes: Option<u64>,
    /// 所有可读取进程的 PagefileUsage 总量，即 Windows 提交内存近似值。
    pub virtual_bytes: Option<u64>,
    /// 成功读取的宿主/WebView2 进程数量；平台不支持时为 `None`。
    pub process_count: Option<u64>,
}

/// 按需读取平台进程资源；不创建常驻线程，生命周期由前端采样控制器决定。
#[derive(Default)]
pub struct ProcessResourceSampler {
    platform: PlatformSampler,
}

impl ProcessResourceSampler {
    /// 读取一份新的聚合资源摘要。
    pub fn sample(&mut self) -> ProcessResourceSample {
        self.platform.sample()
    }
}

#[cfg(not(target_os = "windows"))]
#[derive(Default)]
struct PlatformSampler;

#[cfg(not(target_os = "windows"))]
impl PlatformSampler {
    fn sample(&mut self) -> ProcessResourceSample {
        ProcessResourceSample::default()
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::ProcessResourceSample;
    use std::collections::HashMap;
    use std::mem::size_of;
    use std::time::Instant;
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    #[derive(Default)]
    pub(super) struct PlatformSampler {
        previous_cpu_ticks: HashMap<u32, u64>,
        previous_at: Option<Instant>,
    }

    impl PlatformSampler {
        pub(super) fn sample(&mut self) -> ProcessResourceSample {
            let now = Instant::now();
            let process_ids = tracked_process_ids();
            let mut current_cpu_ticks = HashMap::new();
            let mut resident_bytes = None;
            let mut private_bytes = None;
            let mut virtual_bytes = None;
            let mut process_count = 0u64;

            for process_id in process_ids {
                let Some(usage) = query_process(process_id) else {
                    continue;
                };
                process_count = process_count.saturating_add(1);
                if let Some(cpu_ticks) = usage.cpu_ticks {
                    current_cpu_ticks.insert(process_id, cpu_ticks);
                }
                add_bytes(&mut resident_bytes, usage.resident_bytes);
                add_bytes(&mut private_bytes, usage.private_bytes);
                add_bytes(&mut virtual_bytes, usage.virtual_bytes);
            }

            let cpu_percent = self.previous_at.and_then(|previous_at| {
                if current_cpu_ticks.is_empty() || self.previous_cpu_ticks.is_empty() {
                    return None;
                }
                let elapsed_100ns = now.duration_since(previous_at).as_secs_f64() * 10_000_000.0;
                if elapsed_100ns <= 0.0 {
                    return None;
                }
                let mut comparable_processes = 0u64;
                let delta_ticks = current_cpu_ticks
                    .iter()
                    .fold(0u64, |total, (pid, current)| {
                        let Some(previous) = self.previous_cpu_ticks.get(pid).copied() else {
                            return total;
                        };
                        comparable_processes = comparable_processes.saturating_add(1);
                        total.saturating_add(current.saturating_sub(previous))
                    });
                if comparable_processes == 0 {
                    return None;
                }
                Some((delta_ticks as f64 / elapsed_100ns * 100.0).max(0.0))
            });

            self.previous_cpu_ticks = current_cpu_ticks;
            self.previous_at = Some(now);
            ProcessResourceSample {
                cpu_percent,
                resident_bytes,
                private_bytes,
                virtual_bytes,
                process_count: (process_count > 0).then_some(process_count),
            }
        }
    }

    #[derive(Default)]
    struct ProcessEntry {
        process_id: u32,
        parent_process_id: u32,
        executable: String,
    }

    struct ProcessUsage {
        cpu_ticks: Option<u64>,
        resident_bytes: Option<u64>,
        private_bytes: Option<u64>,
        virtual_bytes: Option<u64>,
    }

    fn tracked_process_ids() -> Vec<u32> {
        let root_process_id = std::process::id();
        let entries = enumerate_processes();
        let parents = entries
            .iter()
            .map(|entry| (entry.process_id, entry.parent_process_id))
            .collect::<HashMap<_, _>>();
        let mut process_ids = Vec::with_capacity(entries.len().min(32));
        process_ids.push(root_process_id);
        for entry in entries {
            if entry.process_id == root_process_id
                || !entry.executable.eq_ignore_ascii_case("msedgewebview2.exe")
                || !is_descendant(entry.process_id, root_process_id, &parents)
            {
                continue;
            }
            process_ids.push(entry.process_id);
        }
        process_ids.sort_unstable();
        process_ids.dedup();
        process_ids
    }

    fn enumerate_processes() -> Vec<ProcessEntry> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
            return Vec::new();
        }
        let mut entries = Vec::new();
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) != 0 };
        while has_entry {
            entries.push(ProcessEntry {
                process_id: entry.th32ProcessID,
                parent_process_id: entry.th32ParentProcessID,
                executable: String::from_utf16_lossy(
                    &entry.szExeFile[..entry
                        .szExeFile
                        .iter()
                        .position(|value| *value == 0)
                        .unwrap_or(entry.szExeFile.len())],
                ),
            });
            has_entry = unsafe { Process32NextW(snapshot, &mut entry) != 0 };
        }
        unsafe {
            CloseHandle(snapshot);
        }
        entries
    }

    fn is_descendant(process_id: u32, root_process_id: u32, parents: &HashMap<u32, u32>) -> bool {
        let mut current = process_id;
        for _ in 0..64 {
            if current == root_process_id {
                return true;
            }
            let Some(parent) = parents.get(&current).copied() else {
                return false;
            };
            if parent == current || parent == 0 {
                return false;
            }
            current = parent;
        }
        false
    }

    fn query_process(process_id: u32) -> Option<ProcessUsage> {
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
                0,
                process_id,
            )
        };
        if handle.is_null() {
            return None;
        }

        let mut creation_time = FILETIME::default();
        let mut exit_time = FILETIME::default();
        let mut kernel_time = FILETIME::default();
        let mut user_time = FILETIME::default();
        let has_cpu = unsafe {
            GetProcessTimes(
                handle,
                &mut creation_time,
                &mut exit_time,
                &mut kernel_time,
                &mut user_time,
            ) != 0
        };

        let mut counters = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..Default::default()
        };
        let has_memory = unsafe {
            GetProcessMemoryInfo(
                handle,
                (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX)
                    .cast::<PROCESS_MEMORY_COUNTERS>(),
                size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ) != 0
        };
        unsafe {
            CloseHandle(handle);
        }

        if !has_cpu && !has_memory {
            return None;
        }
        Some(ProcessUsage {
            cpu_ticks: has_cpu
                .then(|| filetime_value(kernel_time).saturating_add(filetime_value(user_time))),
            resident_bytes: has_memory.then_some(counters.WorkingSetSize as u64),
            private_bytes: has_memory.then_some(counters.PrivateUsage as u64),
            virtual_bytes: has_memory.then_some(counters.PagefileUsage as u64),
        })
    }

    fn filetime_value(value: FILETIME) -> u64 {
        (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
    }

    fn add_bytes(target: &mut Option<u64>, value: Option<u64>) {
        let Some(value) = value else {
            return;
        };
        *target = Some(target.unwrap_or_default().saturating_add(value));
    }

    #[cfg(test)]
    mod tests {
        use super::PlatformSampler;

        #[test]
        fn current_process_memory_is_readable() {
            let mut sampler = PlatformSampler::default();
            let first = sampler.sample();
            assert!(first.process_count.unwrap_or_default() >= 1);
            assert!(first.resident_bytes.is_some());
            assert!(first.private_bytes.is_some());
            assert!(first.virtual_bytes.is_some());
        }

        #[test]
        fn cpu_sample_requires_a_baseline() {
            let mut sampler = PlatformSampler::default();
            assert!(sampler.sample().cpu_percent.is_none());
            std::thread::sleep(std::time::Duration::from_millis(2));
            assert!(sampler.sample().cpu_percent.is_some());
        }
    }
}

#[cfg(target_os = "windows")]
use windows::PlatformSampler;

#[cfg(not(target_os = "windows"))]
#[cfg(test)]
mod tests {
    use super::{ProcessResourceSample, ProcessResourceSampler};

    #[test]
    fn unsupported_platform_reports_unknown_values() {
        let mut sampler = ProcessResourceSampler::default();
        assert_eq!(sampler.sample(), ProcessResourceSample::default());
    }
}
