//! WebView2 浏览器进程的孤儿清理(仅 Windows)。
//!
//! 宿主被强制终止或崩溃时,msedgewebview2.exe 可能作为孤儿残留:它不随宿主
//! 进程树终止,还会占住用户数据目录与调试端口,甚至让下次启动因环境选项
//! 不一致而创建失败。本模块在桌面启动早期、任何 WebView 创建之前,按命令行
//! 指纹识别属于本应用的残留浏览器进程并终止。
//!
//! 安全边界:只清理"父进程不是存活的 keencode-desktop.exe"的候选,因此
//! 并行运行中的其他桌面实例(单实例插件生效前残留的场景)不受影响。

/// 本应用 WebView2 的进程名;WebView2 运行时会把宿主 exe 名写进命令行,
/// 用户数据目录由 Tauri 标识符派生。
#[cfg(windows)]
const WEBVIEW_PROCESS_NAME: &str = "msedgewebview2.exe";
#[cfg(windows)]
const DESKTOP_PROCESS_NAME: &str = "keencode-desktop.exe";
#[cfg(windows)]
const COMMAND_FINGERPRINT: &str = "--webview-exe-name=keencode-desktop.exe";
#[cfg(windows)]
const DATA_DIR_FINGERPRINT: &str = "com.keencode.desktop\\EBWebView";

use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(windows)]
use sysinfo::{Pid, Process, System};

static SWEPT_COUNT: AtomicUsize = AtomicUsize::new(0);

/// 上次 [`cleanup_stale_webviews`] 终止的进程数;供诊断日志在 setup 阶段补记。
pub fn take_swept_count() -> usize {
    SWEPT_COUNT.swap(0, Ordering::AcqRel)
}

#[cfg(windows)]
pub fn cleanup_stale_webviews() {
    let swept = sweep::cleanup_stale_webviews();
    SWEPT_COUNT.store(swept, Ordering::Release);
}

#[cfg(not(windows))]
pub fn cleanup_stale_webviews() {}

#[cfg(windows)]
mod sweep {
    use std::ffi::OsStr;
    use sysinfo::{Pid, Process, ProcessRefreshKind, RefreshKind, System, UpdateKind};

    use super::{has_live_desktop_parent, is_webview_candidate};

    /// 终止上次异常退出遗留的 WebView2 浏览器进程,返回成功终止的数量。
    pub(super) fn cleanup_stale_webviews() -> usize {
        let own_pid = std::process::id();
        // 只刷新进程名与命令行:枚举一次性、无轮询,避免冷启动预算外的开销。
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_processes(ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always)),
        );
        let stale: Vec<Pid> = system
            .processes()
            .values()
            .filter(|process| {
                let command = process.cmd().join(OsStr::new(" "));
                is_webview_candidate(
                    &process.name().to_string_lossy(),
                    &command.to_string_lossy(),
                ) && !has_live_desktop_parent(&system, process.pid(), own_pid)
            })
            .map(Process::pid)
            .collect();
        stale
            .into_iter()
            .filter(|pid| system.process(*pid).is_some_and(Process::kill))
            .count()
    }
}

/// 命中本应用 WebView2 指纹:进程名匹配,且命令行带宿主 exe 名或本应用
/// 用户数据目录特征。其他应用的 WebView2 不会命中。
#[cfg(windows)]
fn is_webview_candidate(process_name: &str, command: &str) -> bool {
    process_name.eq_ignore_ascii_case(WEBVIEW_PROCESS_NAME)
        && (command.contains(COMMAND_FINGERPRINT) || command.contains(DATA_DIR_FINGERPRINT))
}

/// 父进程是本进程或仍存活的另一桌面实例时返回 true(保留)。
#[cfg(windows)]
fn has_live_desktop_parent(system: &System, pid: Pid, own_pid: u32) -> bool {
    let Some(parent_pid) = system.process(pid).and_then(Process::parent) else {
        return false;
    };
    if parent_pid.as_u32() == own_pid {
        return true;
    }
    system.process(parent_pid).is_some_and(|parent| {
        parent
            .name()
            .to_string_lossy()
            .eq_ignore_ascii_case(DESKTOP_PROCESS_NAME)
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::{COMMAND_FINGERPRINT, DATA_DIR_FINGERPRINT, is_webview_candidate};

    #[test]
    fn 指纹只命中本应用的webview进程() {
        let command = format!(
            "\"C:\\Program Files\\x86\\msedgewebview2.exe\" --embedded-browser-webview=1 \
             {COMMAND_FINGERPRINT} --user-data-dir=\"C:\\Users\\u\\AppData\\Local\\{DATA_DIR_FINGERPRINT}\""
        );

        assert!(is_webview_candidate("msedgewebview2.exe", &command));
        assert!(is_webview_candidate("MSEDGEWEBVIEW2.EXE", &command));
    }

    #[test]
    fn 其他应用与宿主自身的进程不命中() {
        let other_app =
            "--user-data-dir=\"C:\\Users\\u\\AppData\\Local\\com.other.app\\EBWebView\"";
        let host_command = format!("--type=gpu-process {COMMAND_FINGERPRINT}");

        assert!(!is_webview_candidate("msedgewebview2.exe", other_app));
        assert!(!is_webview_candidate("keencode-desktop.exe", &host_command));
    }
}
