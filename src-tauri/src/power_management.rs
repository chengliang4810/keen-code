//! macOS 与 Windows 的空闲睡眠抑制状态。

use anyhow::{Context, Result};
use std::sync::Mutex;

/// 持有唯一的系统空闲睡眠抑制句柄。
pub struct PowerManagement {
    /// 句柄存在时，系统不会因为用户空闲自动进入睡眠。
    inhibitor: Mutex<Option<keepawake::KeepAwake>>,
}

impl PowerManagement {
    /// 构造尚未阻止空闲睡眠的电源管理状态。
    pub fn new() -> Self {
        Self {
            inhibitor: Mutex::new(None),
        }
    }

    /// 按当前设置创建或释放系统空闲睡眠抑制句柄。
    pub fn set_keep_awake(&self, enabled: bool) -> Result<()> {
        let mut inhibitor = self.inhibitor.lock().expect("电源管理状态锁已损坏");
        if enabled {
            if inhibitor.is_some() {
                return Ok(());
            }
            let handle = keepawake::Builder::default()
                .idle(true)
                .display(false)
                .sleep(false)
                .reason("KeenCode 正在保持任务运行")
                .create()
                .context("无法阻止系统因空闲进入睡眠")?;
            *inhibitor = Some(handle);
        } else {
            *inhibitor = None;
        }
        Ok(())
    }

    /// 返回当前进程是否持有空闲睡眠抑制句柄。
    #[cfg(test)]
    fn is_enabled(&self) -> bool {
        self.inhibitor
            .lock()
            .expect("电源管理状态锁已损坏")
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::PowerManagement;

    /// 新建状态不得在用户开启前阻止系统睡眠。
    #[test]
    fn starts_disabled() {
        assert!(!PowerManagement::new().is_enabled());
    }

    /// 当前平台必须能够创建并释放真实的空闲睡眠抑制句柄。
    #[test]
    fn enables_and_releases_idle_sleep_inhibitor() {
        let management = PowerManagement::new();
        management.set_keep_awake(true).expect("创建空闲睡眠抑制");
        assert!(management.is_enabled());
        management.set_keep_awake(false).expect("释放空闲睡眠抑制");
        assert!(!management.is_enabled());
    }
}
