//! 后台捕获登录 Shell 的 PATH 并写入命令工具的进程级覆盖。
//!
//! macOS 从 Finder/Dock 启动的打包应用只继承 launchd 的最小 PATH
//! （`/usr/bin:/bin:/usr/sbin:/sbin`），且 Bash 工具的非交互 Shell 不会
//! 加载 `~/.zshrc` 等配置，导致 Bash、Git、LSP、MCP 工具找不到 node 等
//! 用户级命令。[`begin_login_shell_path_capture`] 在 `run()` 最早启动
//! 后台线程，不阻塞窗口启动；捕获完成后一次性写入
//! `keencode_tools::set_path_overlay`，之后所有命令类子进程与程序解析
//! 立即受益。会话创建时可调用 [`wait_for_capture_applied`] 做有界等待，
//! 保证首个会话即生效。

use std::sync::{Arc, Condvar, Mutex, Once, OnceLock};
use std::time::Duration;

/// 登录 Shell 捕获的整体超时；超时后放弃捕获，仅用静态目录兜底。
/// 后台执行不阻塞启动，因此允许覆盖慢 rc 配置的 Shell。
#[cfg(not(windows))]
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// `env -0` 输出的前哨兵；只有它之后的内容才参与解析，
/// 用于跳过 rc 配置向 stdout 打印的噪音。
#[cfg(not(windows))]
const ENV_MARKER: &str = "__KEENCODE_ENV_BEGIN__";

/// 捕获线程的完成信号；写入端与等待端分属不同线程。
struct CaptureSignal {
    finished: Mutex<bool>,
    condvar: Condvar,
}

static CAPTURE_SIGNAL: OnceLock<Arc<CaptureSignal>> = OnceLock::new();

/// 有界等待只消费一次，避免每个会话都重复付出等待预算。
static WAIT_ONCE: Once = Once::new();

/// 启动后台登录 Shell PATH 捕获；重复调用无效果。Windows 直接返回。
pub fn begin_login_shell_path_capture() {
    #[cfg(windows)]
    {
        // Windows 的 PATH 由系统注册表下发，不存在 launchd 最小化问题。
    }
    #[cfg(not(windows))]
    {
        let signal = Arc::new(CaptureSignal {
            finished: Mutex::new(false),
            condvar: Condvar::new(),
        });
        // set 成功即获得启动权；重复调用在此早退。
        if CAPTURE_SIGNAL.set(Arc::clone(&signal)).is_err() {
            return;
        }
        let spawn_result = std::thread::Builder::new()
            .name("keencode-shell-env".to_owned())
            .spawn({
                let signal = Arc::clone(&signal);
                move || {
                    let applied = capture_and_store_overlay();
                    let mut finished = signal.finished.lock().expect("捕获信号锁应可用");
                    *finished = true;
                    signal.condvar.notify_all();
                    if applied {
                        tracing::info!("登录 Shell PATH 覆盖已写入");
                    }
                }
            });
        if spawn_result.is_err() {
            // 线程创建极端罕见地失败：立即放行等待端，避免空转等待。
            let mut finished = signal.finished.lock().expect("捕获信号锁应可用");
            *finished = true;
            signal.condvar.notify_all();
        }
    }
}

/// 有界等待后台捕获收尾；只生效一次，超时后不再等待。
pub fn wait_for_capture_applied(max_wait: Duration) {
    WAIT_ONCE.call_once(|| {
        let Some(signal) = CAPTURE_SIGNAL.get() else {
            return;
        };
        let mut finished = signal.finished.lock().expect("捕获信号锁应可用");
        while !*finished {
            let (guard, timeout) = signal
                .condvar
                .wait_timeout(finished, max_wait)
                .expect("捕获信号锁应可用");
            finished = guard;
            if timeout.timed_out() {
                return;
            }
        }
    });
}

#[cfg(not(windows))]
fn capture_and_store_overlay() -> bool {
    let started = std::time::Instant::now();
    let current = current_path_entries();
    if !should_capture_login_shell(&current) {
        return false;
    }
    let captured = capture_login_shell_path_with_timeout(CAPTURE_TIMEOUT).unwrap_or_default();
    let static_extra = static_path_entries();
    let merged = merge_path_entries(&captured, &current, &static_extra);
    if merged == current {
        return false;
    }
    let Ok(joined) = std::env::join_paths(merged.iter()) else {
        return false;
    };
    let applied = keencode_tools::set_path_overlay(joined);
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        captured = !captured.is_empty(),
        entries = merged.len(),
        "登录 Shell PATH 捕获完成",
    );
    applied
}

/// 登录 Shell 捕获只在进程 PATH 看起来是 launchd 最小集（不超过 4 项）
/// 时执行；从终端启动的应用 PATH 已完整，捕获纯属重复开销。
#[cfg(not(windows))]
fn should_capture_login_shell(current: &[std::path::PathBuf]) -> bool {
    current.len() <= 4
}

#[cfg(not(windows))]
fn current_path_entries() -> Vec<std::path::PathBuf> {
    std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default()
}

/// 首个出现者优先合并去重；丢弃空项（POSIX 中 `::` 表示当前目录，
/// 在工具环境下有歧义且不安全）。
#[cfg(not(windows))]
fn merge_path_entries(
    captured: &[std::path::PathBuf],
    current: &[std::path::PathBuf],
    static_extra: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    let mut merged = Vec::with_capacity(captured.len() + current.len() + static_extra.len());
    let mut seen = std::collections::HashSet::new();
    for entry in captured.iter().chain(current).chain(static_extra) {
        if entry.as_os_str().is_empty() {
            continue;
        }
        if seen.insert(entry.as_os_str().to_os_string()) {
            merged.push(entry.clone());
        }
    }
    merged
}

/// 常见安装目录兜底；只保留真实存在的目录。
#[cfg(not(windows))]
fn static_path_entries() -> Vec<std::path::PathBuf> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    static_path_entries_for(home.as_deref())
}

#[cfg(not(windows))]
fn static_path_entries_for(home: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    let mut candidates = vec![
        std::path::PathBuf::from("/opt/homebrew/bin"),
        std::path::PathBuf::from("/opt/homebrew/sbin"),
        std::path::PathBuf::from("/usr/local/bin"),
        std::path::PathBuf::from("/usr/local/sbin"),
    ];
    if let Some(home) = home {
        candidates.push(home.join(".cargo/bin"));
        candidates.push(home.join(".local/bin"));
        candidates.push(home.join("bin"));
    }
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(not(windows))]
fn login_shell() -> Option<std::ffi::OsString> {
    match std::env::var_os("SHELL") {
        Some(shell) if !shell.is_empty() => Some(shell),
        _ => {
            let fallback = if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/bash"
            };
            std::path::Path::new(fallback)
                .exists()
                .then(|| fallback.into())
        }
    }
}

#[cfg(not(windows))]
fn capture_login_shell_path_with_timeout(timeout: Duration) -> Option<Vec<std::path::PathBuf>> {
    let shell = login_shell()?;
    let stdout_path = capture_temp_file_path()?;
    let script = format!("printf '%s\\n' {ENV_MARKER}; env -0");

    // stdout 写入临时文件而不是管道：环境过大时管道缓冲会填满，
    // 子进程写阻塞导致读、等双卡死；文件没有这个问题。
    let stdout_file = std::fs::File::create(&stdout_path).ok()?;
    let spawn_result = std::process::Command::new(&shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(stdout_file))
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = match spawn_result {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_file(&stdout_path);
            tracing::debug!(%error, "登录 Shell 环境捕获进程启动失败");
            return None;
        }
    };
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&stdout_path);
                tracing::debug!("登录 Shell 环境捕获超时，跳过");
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&stdout_path);
                tracing::debug!(%error, "登录 Shell 状态轮询失败，跳过环境捕获");
                return None;
            }
        }
    };
    let stdout = std::fs::read(&stdout_path);
    let _ = std::fs::remove_file(&stdout_path);
    if !status.success() {
        tracing::debug!("登录 Shell 环境捕获命令失败，跳过");
        return None;
    }
    parse_captured_path(&stdout.ok()?)
}

#[cfg(not(windows))]
fn capture_temp_file_path() -> Option<std::path::PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(keencode_tools::long_form_temp_dir().join(format!(
        "keencode-shell-env-{}-{nanos}.tmp",
        std::process::id()
    )))
}

#[cfg(not(windows))]
fn parse_captured_path(stdout: &[u8]) -> Option<Vec<std::path::PathBuf>> {
    use std::os::unix::ffi::OsStrExt as _;

    let marker = format!("{ENV_MARKER}\n");
    let start = stdout
        .windows(marker.len())
        .position(|window| window == marker.as_bytes())?
        + marker.len();
    let mut found = None;
    for entry in stdout[start..].split(|byte| *byte == 0) {
        let Some(equals) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (key, value) = (&entry[..equals], &entry[equals + 1..]);
        if !is_valid_env_key(key) {
            continue;
        }
        if key == b"PATH" {
            found = Some(std::env::split_paths(std::ffi::OsStr::from_bytes(value)).collect());
        }
    }
    found
}

#[cfg(not(windows))]
fn is_valid_env_key(key: &[u8]) -> bool {
    let Some((first, rest)) = key.split_first() else {
        return false;
    };
    (first.is_ascii_alphabetic() || *first == b'_')
        && rest
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

#[cfg(not(windows))]
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entries(values: &[&str]) -> Vec<PathBuf> {
        values.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn merge_prefers_captured_order_and_dedupes() {
        let captured = entries(&["/Users/u/.nvm/versions/node/v24/bin", "/opt/homebrew/bin"]);
        let current = entries(&["/usr/bin", "/bin", "/opt/homebrew/bin"]);
        let static_extra = entries(&["/opt/homebrew/bin", "/usr/local/bin"]);
        let merged = merge_path_entries(&captured, &current, &static_extra);
        assert_eq!(
            merged,
            entries(&[
                "/Users/u/.nvm/versions/node/v24/bin",
                "/opt/homebrew/bin",
                "/usr/bin",
                "/bin",
                "/usr/local/bin",
            ])
        );
    }

    #[test]
    fn merge_drops_empty_entries() {
        let current = entries(&["", "/usr/bin"]);
        let merged = merge_path_entries(&[], &current, &[]);
        assert_eq!(merged, entries(&["/usr/bin"]));
    }

    #[test]
    fn parse_skips_noise_before_marker_and_picks_path() {
        let stdout = b"p10k instant prompt junk\n__KEENCODE_ENV_BEGIN__\nHOME=/Users/u\0PATH=/usr/bin:/bin\0broken-no-equals\0";
        let parsed = parse_captured_path(stdout).expect("应解析出 PATH");
        assert_eq!(parsed, entries(&["/usr/bin", "/bin"]));
    }

    #[test]
    fn parse_rejects_invalid_entries_and_missing_marker() {
        assert_eq!(parse_captured_path(b"1BAD=x\0PATH=/bin\0"), None);
        assert_eq!(parse_captured_path(b"PATH=/bin\0"), None);
    }

    #[test]
    fn env_key_validation_follows_shell_rules() {
        assert!(is_valid_env_key(b"PATH"));
        assert!(is_valid_env_key(b"_PRIVATE_X1"));
        assert!(!is_valid_env_key(b"1BAD"));
        assert!(!is_valid_env_key(b"HAS SPACE"));
        assert!(!is_valid_env_key(b""));
    }

    #[test]
    fn static_entries_include_existing_home_dirs() {
        let home =
            std::env::temp_dir().join(format!("keencode-static-test-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".cargo/bin")).expect("应创建临时目录");
        let result = static_path_entries_for(Some(&home));
        let _ = std::fs::remove_dir_all(&home);
        assert!(result.contains(&home.join(".cargo/bin")));
        assert!(!result.contains(&home.join(".local/bin")));
    }

    #[test]
    fn capture_gate_matches_launchd_minimal_path() {
        assert!(should_capture_login_shell(&entries(&[
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ])));
        assert!(!should_capture_login_shell(&entries(&[
            "/opt/homebrew/bin",
            "/Users/u/.nvm/versions/node/v24/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
            "/usr/sbin",
            "/sbin",
        ])));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn login_shell_capture_includes_system_paths() {
        let captured = capture_login_shell_path_with_timeout(Duration::from_secs(20))
            .expect("macOS 登录 Shell 应能捕获 PATH");
        let joined = captured
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");
        assert!(joined.contains("/usr/bin"), "实际捕获：{joined}");
        assert!(joined.contains("/bin"), "实际捕获：{joined}");
    }
}
