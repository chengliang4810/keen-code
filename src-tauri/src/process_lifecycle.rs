//! Tauri 桌面层的外部进程适配入口。
//!
//! 短生命周期命令的 Tokio、管道排空、总体 deadline、进程树清理和任务回收统一由
//! `peri_middlewares::process_lifecycle` 实现；本模块只保留桌面层的同步签名、平台
//! 创建选项和既有中文错误映射，避免在两个 crate 中维护相同的生命周期状态机。

use peri_agent::agent::async_tasks::{configure_std_command, kill_process_group};
use peri_middlewares::process_lifecycle::{
    ProcessLifecycleError, run_short_lived_command_blocking,
};
use std::io;
use std::process::{Command, Output};
use std::time::Duration;

/// Tauri Git/代理命令沿用的每个输出流 8 MiB 上限。
const TAURI_OUTPUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// 向已有的进程组发送强制终止信号；终端 PTY 生命周期仍使用此独立入口。
///
/// Unix 下调用方必须保证根进程是独立进程组组长；Windows 下由 peri 的
/// `taskkill /T /F` 回退实现进程树终止。
pub(crate) fn terminate_process_tree(pid: u32) {
    kill_process_group(pid, "KILL");
}

/// 在固定时限内执行标准库外部命令，并复用 Peri 的共享生命周期 runner。
///
/// 桌面层继续接收 `std::process::Command` 和 `Result<Output, String>`，而命令的
/// Tokio 转换、stdout/stderr 持续排空、读取错误传播、总体 deadline、Windows Job
/// Object/taskkill、Unix 进程组和所有失败路径的任务回收均由共享入口负责。
pub(crate) fn run_std_command_with_timeout(
    mut command: Command,
    label: &str,
    timeout: Duration,
) -> Result<Output, String> {
    // 即使调用方没有通过 new_std_command 构造命令，也统一应用桌面平台创建选项。
    configure_std_command(&mut command);
    run_short_lived_command_blocking(command, timeout)
        .map_err(|error| map_process_lifecycle_error(label, timeout, error))
}

/// 将共享 runner 的结构化错误转换为 Tauri 既有的中文错误契约。
fn map_process_lifecycle_error(
    label: &str,
    timeout: Duration,
    error: ProcessLifecycleError,
) -> String {
    match error {
        ProcessLifecycleError::Timeout => {
            format!("{label}执行超时（{:.1} 秒）", timeout.as_secs_f64())
        }
        ProcessLifecycleError::Io(error) if error.kind() == io::ErrorKind::FileTooLarge => {
            format!(
                "{label}超过 {} MB 读取上限",
                TAURI_OUTPUT_LIMIT_BYTES / (1024 * 1024)
            )
        }
        ProcessLifecycleError::Io(error) => format!("{label}执行失败：{error}"),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::run_std_command_with_timeout;
    #[cfg(unix)]
    use peri_agent::agent::async_tasks::new_std_command;
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    /// 超时适配必须交给共享 runner 清理 Unix 进程组，而不是只等待根 shell。
    #[cfg(unix)]
    #[test]
    fn timeout_terminates_child_process_group() {
        let directory = tempfile::tempdir().expect("创建外部命令测试目录");
        let marker = directory.path().join("child-survived");
        let marker_text = marker.to_string_lossy().into_owned();
        let mut command = new_std_command("sh");
        command.args(["-c", r#"(sleep 1; touch "$MARKER") & wait"#]);
        command.env("MARKER", marker_text);

        let started = Instant::now();
        let result =
            run_std_command_with_timeout(command, "测试外部命令", Duration::from_millis(100));
        assert!(result.is_err(), "超时命令必须返回错误");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "超时路径不能退化为等待整个命令结束"
        );

        std::thread::sleep(Duration::from_millis(1_200));
        assert!(!marker.exists(), "子进程不应在根进程终止后继续运行");
    }

    /// 根进程退出后，继承管道的后代 drain 仍必须受同一个总体 deadline 限制。
    #[cfg(unix)]
    #[test]
    fn normal_drain_uses_overall_deadline() {
        let directory = tempfile::tempdir().expect("创建外部命令测试目录");
        let marker = directory.path().join("grandchild-survived");
        let marker_text = marker.to_string_lossy().into_owned();
        let mut command = new_std_command("sh");
        command.args(["-c", r#"(sleep 1; touch "$MARKER") & exit 0"#]);
        command.env("MARKER", marker_text);

        let started = Instant::now();
        let result =
            run_std_command_with_timeout(command, "测试外部命令", Duration::from_millis(100));
        assert!(result.is_err(), "继承管道未关闭时必须返回 drain 错误");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "drain 不应在总体 deadline 之外为每个管道额外等待"
        );

        std::thread::sleep(Duration::from_millis(1_200));
        assert!(!marker.exists(), "继承管道的后代不应在清理后继续运行");
    }
}
