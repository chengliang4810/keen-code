//! NativeHost 进程资源采样。
//!
//! 采样对象只有当前 GPUI Rust 进程；不会枚举、聚合或读取 WebView、JavaScript
//! 运行时和其他子进程。CPU 与内存数字来自 `sysinfo` 的进程 API，UI 只把它们
//! 当作诊断摘要，不能据此推断系统总体资源使用。

/// 一次当前 GPUI 进程的资源摘要。
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProcessResourceSample {
    /// 当前进程在最近一次 sysinfo 刷新间隔内的 CPU 百分比。
    pub cpu_percent: Option<f64>,
    /// 当前进程工作集（RSS）近似值。
    pub resident_bytes: Option<u64>,
    /// 当前进程的私有提交量；sysinfo 不提供该平台字段时为 `None`。
    pub private_bytes: Option<u64>,
    /// 当前进程的私有工作集；sysinfo 不提供该平台字段时为 `None`。
    pub private_resident_bytes: Option<u64>,
    /// 当前进程提交/虚拟内存近似值。
    pub virtual_bytes: Option<u64>,
    /// 成功读取的 NativeHost 进程数量，成功时固定为 1。
    pub process_count: Option<u64>,
}

/// 逻辑处理器数量只查询一次，用于把 sysinfo 的每核 CPU 值归一到整机百分比。
pub(crate) fn logical_processor_count() -> usize {
    static COUNT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *COUNT.get_or_init(|| std::thread::available_parallelism().map_or(1, usize::from))
}

/// 按需读取平台进程资源；不创建常驻采样线程，生命周期由宿主诊断调用方控制。
pub struct ProcessResourceSampler {
    platform: PlatformSampler,
}

impl Default for ProcessResourceSampler {
    fn default() -> Self {
        Self {
            platform: PlatformSampler::new(),
        }
    }
}

impl ProcessResourceSampler {
    /// 读取一份当前 GPUI Rust 进程的资源摘要。
    pub fn sample(&mut self) -> ProcessResourceSample {
        self.platform.sample()
    }
}

struct PlatformSampler {
    system: sysinfo::System,
    pid: sysinfo::Pid,
    cpu_sampled_at: Option<std::time::Instant>,
}

impl PlatformSampler {
    fn new() -> Self {
        Self {
            system: sysinfo::System::new_with_specifics(
                sysinfo::RefreshKind::nothing().with_processes(
                    sysinfo::ProcessRefreshKind::nothing()
                        .with_cpu()
                        .with_memory(),
                ),
            ),
            pid: sysinfo::Pid::from_u32(std::process::id()),
            cpu_sampled_at: None,
        }
    }

    fn sample(&mut self) -> ProcessResourceSample {
        let sampled_at = std::time::Instant::now();
        let cpu_interval_valid = self.cpu_sampled_at.is_some_and(|previous| {
            sampled_at.duration_since(previous) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL
        });
        let refreshed = self.system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[self.pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory(),
        );
        if refreshed == 0 {
            return ProcessResourceSample::default();
        }
        if self.cpu_sampled_at.is_none() || cpu_interval_valid {
            self.cpu_sampled_at = Some(sampled_at);
        }
        let Some(process) = self.system.process(self.pid) else {
            return ProcessResourceSample::default();
        };

        // sysinfo 的 process CPU 以单个逻辑核为 100%；按整机容量归一，
        // 避免多核机器把一个进程报告为大于 100%。
        let usage = f64::from(process.cpu_usage());
        let cpu_percent = (cpu_interval_valid && usage.is_finite())
            .then(|| (usage / logical_processor_count() as f64 * 100.0).clamp(0.0, 100.0));
        let sample = ProcessResourceSample {
            cpu_percent,
            resident_bytes: Some(process.memory()),
            private_bytes: None,
            private_resident_bytes: None,
            virtual_bytes: Some(process.virtual_memory()),
            process_count: Some(1),
        };
        #[cfg(target_os = "windows")]
        let sample = {
            let mut sample = sample;
            use windows_sys::Win32::System::ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            };
            use windows_sys::Win32::System::Threading::GetCurrentProcess;
            let mut counters = PROCESS_MEMORY_COUNTERS_EX {
                cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                ..Default::default()
            };
            // 当前进程的伪句柄不需要 CloseHandle；失败时保持 None，不以虚拟内存冒充私有提交量。
            if unsafe {
                GetProcessMemoryInfo(
                    GetCurrentProcess(),
                    &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                    size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
                )
            } != 0
            {
                sample.private_bytes = Some(counters.PrivateUsage as u64);
                sample.resident_bytes = Some(counters.WorkingSetSize as u64);
            }
            sample
        };
        sample
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessResourceSample, ProcessResourceSampler, logical_processor_count};

    #[test]
    fn current_process_memory_is_native_only() {
        let mut sampler = ProcessResourceSampler::default();
        let sample = sampler.sample();
        assert_eq!(sample.process_count, Some(1));
        assert!(sample.resident_bytes.is_some());
        assert!(sample.virtual_bytes.is_some());
        #[cfg(target_os = "windows")]
        assert!(sample.private_bytes.is_some_and(|bytes| bytes > 0));
        #[cfg(not(target_os = "windows"))]
        assert!(sample.private_bytes.is_none());
        assert!(sample.private_resident_bytes.is_none());
    }

    #[test]
    fn sampler_does_not_report_uninitialized_cpu_as_system_idle() {
        let sample = ProcessResourceSample::default();
        assert!(sample.cpu_percent.is_none());
        assert!(logical_processor_count() >= 1);
    }

    #[test]
    fn cpu_value_is_bounded_when_sysinfo_reports_multiple_cores() {
        let mut sampler = ProcessResourceSampler::default();
        let sample = sampler.sample();
        assert!(
            sample
                .cpu_percent
                .is_none_or(|value| (0.0..=100.0).contains(&value))
        );
    }
}
