//! 进程级 PATH 覆盖：桌面后端在启动时后台捕获登录 Shell 的 PATH 后写入，
//! 命令类工具在解析程序名与构造子进程环境时统一读取。
//!
//! macOS 从 Finder/Dock 启动的打包应用只继承 launchd 的最小 PATH，且非交互
//! Shell 不加载用户 rc 配置；覆盖值让 Bash、Git、LSP、MCP 等工具在不修改
//! 进程自身环境的前提下拿到完整的用户 PATH。

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::sync::OnceLock;

static OVERLAY_PATH: OnceLock<OsString> = OnceLock::new();

/// 写入进程级 PATH 覆盖。仅首次调用生效；空值被忽略并返回 false。
pub fn set_path_overlay(path: OsString) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    OVERLAY_PATH.set(path).is_ok()
}

/// 已生效的 PATH 覆盖；尚未写入时返回 None。
pub fn path_overlay() -> Option<&'static OsString> {
    OVERLAY_PATH.get()
}

/// 程序解析与子进程应使用的 PATH：优先覆盖值，否则继承进程自身 PATH。
pub fn effective_path() -> OsString {
    OVERLAY_PATH
        .get()
        .cloned()
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default()
}

/// 在有效 PATH 中解析程序名；绝对路径或含路径分隔符的名字按原样返回，
/// 相对名字依次匹配 PATH 目录下的可执行文件，找不到时返回 NotFound，
/// 与按候选回退的 spawn 语义保持一致。
pub fn resolve_program(program: &OsStr) -> io::Result<OsString> {
    resolve_program_in_path(program, &effective_path())
}

/// 按给定 PATH 解析程序名，独立保留解析规则以便在不修改进程级覆盖的情况下测试。
fn resolve_program_in_path(program: &OsStr, effective: &OsStr) -> io::Result<OsString> {
    let path = Path::new(program);
    if path.is_absolute() || has_path_separator(program) {
        return Ok(program.to_os_string());
    }
    for directory in std::env::split_paths(effective) {
        if directory.as_os_str().is_empty() {
            continue;
        }
        let candidate = directory.join(path);
        if candidate.is_file() {
            return Ok(candidate.into_os_string());
        }
        #[cfg(windows)]
        if path.extension().is_none() {
            // Windows 的 Command::new 会按 PATHEXT 补全后缀；自定义 PATH
            // 解析器也必须覆盖 git.exe、工具.cmd 等无后缀调用。
            let pathext = std::env::var_os("PATHEXT")
                .unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
            for extension in pathext.to_string_lossy().split(';') {
                if extension.is_empty() {
                    continue;
                }
                let mut name = program.to_os_string();
                name.push(extension);
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Ok(candidate.into_os_string());
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("有效 PATH 中未找到程序 {program:?}"),
    ))
}

fn has_path_separator(program: &OsStr) -> bool {
    let text = program.to_string_lossy();
    if cfg!(windows) {
        text.contains('/') || text.contains('\\')
    } else {
        text.contains('/')
    }
}

/// 把有效 PATH 写入子进程环境；调用方已显式设置 PATH 时不覆盖。
pub fn apply_to_std_command(command: &mut std::process::Command) {
    let Some(overlay) = OVERLAY_PATH.get() else {
        return;
    };
    let explicit = command
        .get_envs()
        .any(|(name, _)| name == OsStr::new("PATH"));
    if !explicit {
        command.env("PATH", overlay);
    }
}

/// tokio Command 版本的 [`apply_to_std_command`]。
pub fn apply_to_tokio_command(command: &mut tokio::process::Command) {
    apply_to_std_command(command.as_std_mut());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_program_passes_through_paths_with_separators() {
        let absolute = OsString::from("/bin/keencode-not-checked-at-resolution");
        assert_eq!(resolve_program(&absolute).unwrap(), absolute);
        let relative = OsString::from("./keencode-relative-tool");
        assert_eq!(resolve_program(&relative).unwrap(), relative);
    }

    #[test]
    fn resolve_program_reports_not_found_for_missing_names() {
        let error = resolve_program(OsStr::new("keencode-definitely-missing-probe")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(windows)]
    #[test]
    fn resolve_program_uses_pathext_for_extensionless_names() {
        let directory = tempfile::tempdir().expect("应创建 PATH 探针目录");
        let executable = directory.path().join("keencode-path-probe.exe");
        std::fs::write(&executable, b"probe").expect("应写入 PATH 探针");
        let effective = std::env::join_paths([directory.path()]).expect("应构造测试 PATH");

        let resolved = resolve_program_in_path(OsStr::new("keencode-path-probe"), &effective)
            .expect("Windows PATHEXT 应解析 .exe 探针");
        assert!(
            resolved
                .to_string_lossy()
                .eq_ignore_ascii_case(&executable.to_string_lossy())
        );
    }

    #[test]
    fn apply_to_std_command_respects_explicit_path() {
        let mut command = std::process::Command::new("true");
        command.env("PATH", "/explicit");
        apply_to_std_command(&mut command);
        let (_, value) = command
            .get_envs()
            .find(|(name, _)| *name == OsStr::new("PATH"))
            .expect("PATH 应存在");
        assert_eq!(value, Some(OsStr::new("/explicit")));
    }

    #[test]
    fn set_path_overlay_is_once_and_effective() {
        let mut entries = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .collect::<Vec<_>>();
        entries.push(std::env::temp_dir().join("keencode-overlay-unit-marker"));
        let overlay = std::env::join_paths(entries.iter()).unwrap();
        assert!(set_path_overlay(overlay.clone()));
        assert!(!set_path_overlay(OsString::from("/ignored")));
        assert_eq!(path_overlay(), Some(&overlay));
        assert_eq!(effective_path(), overlay);
    }
}
