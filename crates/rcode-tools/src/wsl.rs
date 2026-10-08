//! WSL 命令的 Linux 进程树回收，供宿主和共享 Shell 工具复用。

use std::process::Command;

/// 用继承的唯一环境标识定位 WSL 内部的命令及其后代。
#[derive(Clone)]
pub struct WslProcessTree {
    distro: String,
    token: String,
}

impl WslProcessTree {
    /// 从经过宿主授权的命令中提取监督标识。
    pub fn from_command(command: &Command) -> Option<Self> {
        let environment: std::collections::HashMap<_, _> = command.get_envs().collect();
        Some(Self {
            distro: environment
                .get(std::ffi::OsStr::new("RCODE_WSL_TREE_DISTRO"))?
                .as_ref()?
                .to_str()?
                .into(),
            token: environment
                .get(std::ffi::OsStr::new("RCODE_WSL_TREE_TOKEN"))?
                .as_ref()?
                .to_str()?
                .into(),
        })
    }

    /// 在有界监督命令中终止标识匹配的 Linux 进程组与脱离进程组的后代。
    pub fn terminate(&self) -> Result<(), String> {
        if self.distro.is_empty()
            || self.distro.len() > 128
            || self.distro.starts_with('-')
            || !self
                .distro
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
            || self.token.len() != 32
            || !self.token.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid WSL supervision identity".into());
        }
        #[cfg(not(windows))]
        {
            Err("WSL is only available on Windows".into())
        }
        #[cfg(windows)]
        {
            use command_group::CommandGroup;
            use std::time::{Duration, Instant};
            let script = format!(
                "for entry in /proc/[0-9]*/environ; do if grep -zFxq 'RCODE_SHELL_TOKEN={}' \"$entry\" 2>/dev/null; then pid=${{entry#/proc/}}; pid=${{pid%/environ}}; /bin/kill -KILL -- -\"$pid\" 2>/dev/null; /bin/kill -KILL \"$pid\" 2>/dev/null; fi; done; exit 0",
                self.token
            );
            let mut command = Command::new("wsl.exe");
            command
                .args(["-d", &self.distro, "--exec", "sh", "-c", &script])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            let mut group = command.group();
            group.creation_flags(0x0800_0000);
            let mut child = group.spawn().map_err(|error| error.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                    let _ = child.kill();
                    return if status.success() {
                        Ok(())
                    } else {
                        Err("WSL tree cleanup failed".into())
                    };
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("WSL tree cleanup deadline exceeded".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
