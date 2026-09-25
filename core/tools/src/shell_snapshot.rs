//! 会话级 Shell 环境快照：让非交互命令继承用户登录 Shell 的环境与别名。
//!
//! 背景：`bash -lc` 只读取 `~/.bash_profile` 一类的登录脚本，而用户日常依赖的
//! PATH 追加（nvm、pyenv、pnpm）与别名常写在 `~/.bashrc` 里；Windows 上 Git
//! Bash 与 PowerShell 的环境来源更分散。结果是"用户终端里能跑的命令，Agent
//! 跑不了"。
//!
//! 做法：本 Session 首次执行命令前，用一次登录 Shell 把 `export -p` 与
//! `alias -p` 的输出固化成一份脚本；之后每条命令先 source 该脚本再执行原命令。
//! 快照只读取一次，避免每条命令都付一次 rc 加载成本。
//!
//! 失败即降级：快照生成失败、文件缺失或不可读时按原始命令执行，命令本身不会
//! 因为快照机制而失败。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::environment::ToolEnvironment;

/// 快照文件所在目录名（位于 Session 工件目录下）。
const SNAPSHOT_DIRECTORY: &str = "shell-snapshot";

/// 快照文件名。
const SNAPSHOT_FILE: &str = "env.sh";

/// 生成快照时允许等待的最长时间。
const SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 把命令包装为"先复放快照，再执行原命令"。
///
/// 快照不存在时先尝试生成一次；生成失败则直接返回原命令。
pub(crate) fn wrap_command(environment: &Arc<ToolEnvironment>, command: &str) -> String {
    let Some(snapshot) = ensure_snapshot(environment) else {
        return command.to_owned();
    };
    let snapshot = snapshot.to_string_lossy().replace('\\', "/");
    // 快照缺失时静默跳过（`[ -f ]` 判断），不让 source 失败污染命令退出码；
    // 用户命令原样保留在其后，`$?` 反映的是用户命令的结果。
    format!("if [ -f '{snapshot}' ]; then . '{snapshot}' >/dev/null 2>&1 || true; fi\n{command}")
}

/// 返回本 Session 的快照路径，必要时先生成一次。
fn ensure_snapshot(environment: &Arc<ToolEnvironment>) -> Option<PathBuf> {
    if let Some(existing) = environment.shell_snapshot_path() {
        return existing.exists().then_some(existing);
    }
    let path = snapshot_path(environment);
    match generate_snapshot(environment, &path) {
        Ok(()) => {
            environment.set_shell_snapshot_path(path.clone());
            Some(path)
        }
        Err(_) => {
            // 生成失败不缓存失败结果：下一次命令可以重试（例如登录 Shell 首次
            // 初始化较慢导致超时）。
            None
        }
    }
}

/// 返回本 Session 快照的固定路径。
fn snapshot_path(environment: &Arc<ToolEnvironment>) -> PathBuf {
    environment
        .artifact_directory()
        .join(SNAPSHOT_DIRECTORY)
        .join(SNAPSHOT_FILE)
}

/// 用一次登录 Shell 导出环境变量与别名到快照文件。
///
/// 只导出 `export -p` 与 `alias -p`：函数与 Shell 选项不导出，避免把用户
/// 交互式配置里的行为（如 `set -o vi`、提示符、`cd` 钩子）带进非交互执行。
fn generate_snapshot(environment: &Arc<ToolEnvironment>, path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let script = "export -p; echo '---ALIASES---'; alias -p 2>/dev/null || true";
    let output = run_login_shell(environment, script)?;
    if !output.status.success() {
        return Err(std::io::Error::other("登录 Shell 快照导出失败"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let rendered = render_snapshot(&text);
    if rendered.trim().is_empty() {
        return Err(std::io::Error::other("登录 Shell 未导出任何环境"));
    }
    std::fs::write(path, rendered)
}

/// 用当前平台可用的登录 Shell 执行一段脚本并返回输出。
fn run_login_shell(
    environment: &Arc<ToolEnvironment>,
    script: &str,
) -> std::io::Result<std::process::Output> {
    let mut last_error = None;
    for program in shell_candidates() {
        let mut command = std::process::Command::new(&program);
        command
            .arg("-lc")
            .arg(script)
            .current_dir(environment.working_directory())
            .stdin(std::process::Stdio::null());
        // 没有 wait-timeout 依赖时用线程 + channel 实现超时等待，避免登录
        // Shell 卡在交互式提示上时阻塞整个 Turn。
        match run_with_timeout(command, SNAPSHOT_TIMEOUT) {
            Ok(output) => return Ok(output),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("未找到可用的 Shell")))
}

/// 在独立线程中等待子进程结束，超时则放弃等待并返回错误。
fn run_with_timeout(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let child = command.spawn()?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "登录 Shell 快照超时",
        )),
    }
}

/// 返回当前平台可尝试的登录 Shell。
fn shell_candidates() -> Vec<std::ffi::OsString> {
    #[cfg(windows)]
    {
        vec![
            std::ffi::OsString::from("bash.exe"),
            std::ffi::OsString::from(r"C:\Program Files\Git\bin\bash.exe"),
            std::ffi::OsString::from(r"C:\Program Files\Git\usr\bin\bash.exe"),
        ]
    }
    #[cfg(not(windows))]
    {
        vec![std::ffi::OsString::from("bash")]
    }
}

/// 把登录 Shell 的输出整理成可复放的快照脚本。
///
/// `export -p` 在部分 Shell 下会输出 `declare -x`，统一改写为 `export`，
/// 使快照在 dash/POSIX sh 下同样可 source。别名段整体保留。
fn render_snapshot(raw: &str) -> String {
    let mut lines = Vec::new();
    lines.push("# KeenCode Shell 环境快照（会话内生成，可安全删除）".to_owned());
    let mut in_aliases = false;
    for line in raw.lines() {
        if line.contains("---ALIASES---") {
            in_aliases = true;
            continue;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        if in_aliases {
            // 只保留别名定义，忽略 alias 输出的其他内容。
            if trimmed.starts_with("alias ") {
                lines.push(trimmed.to_owned());
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("declare -x ") {
            lines.push(format!("export {rest}"));
        } else if trimmed.starts_with("export ") {
            lines.push(trimmed.to_owned());
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::render_snapshot;

    #[test]
    fn snapshot_rewrites_declare_into_export() {
        let raw = "declare -x PATH=\"/usr/bin\"\ndeclare -x HOME=\"/root\"\n---ALIASES---\nalias ll='ls -la'\nalias gs='git status'\n";
        let rendered = render_snapshot(raw);
        assert!(rendered.contains("export PATH=\"/usr/bin\""));
        assert!(rendered.contains("export HOME=\"/root\""));
        assert!(rendered.contains("alias ll='ls -la'"));
        assert!(rendered.contains("alias gs='git status'"));
        assert!(!rendered.contains("declare -x"));
    }

    #[test]
    fn snapshot_keeps_posix_export_form_and_drops_unrelated_lines() {
        let raw = "export PATH=\"/bin\"\nsome noise line\n---ALIASES---\nnot an alias\n";
        let rendered = render_snapshot(raw);
        assert!(rendered.contains("export PATH=\"/bin\""));
        assert!(!rendered.contains("noise"));
        assert!(!rendered.contains("not an alias"));
    }
}
