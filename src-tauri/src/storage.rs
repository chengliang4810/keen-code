//! KeenCode 本地持久化目录。
//!
//! 所有 Rust 后端配置、会话、扩展和日志统一写入当前用户主目录下的
//! `.keencode`，不再使用各平台的应用配置或应用数据目录。

use anyhow::{Context, Result};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// KeenCode 在用户主目录下使用的唯一持久化目录名。
const KEENCODE_HOME_NAME: &str = ".keencode";

/// 返回当前用户唯一的 KeenCode 持久化根目录。
pub(crate) fn root_dir(app: &AppHandle) -> Result<PathBuf> {
    let home = app.path().home_dir().context("无法确定当前用户目录")?;
    Ok(root_dir_from_home(home))
}

/// 将已经解析的用户主目录转换为 KeenCode 持久化根目录。
fn root_dir_from_home(home: PathBuf) -> PathBuf {
    home.join(KEENCODE_HOME_NAME)
}

/// 在 Tauri 启动前从当前进程环境解析 Windows 用户目录。
#[cfg(target_os = "windows")]
pub(crate) fn root_dir_before_start() -> Result<PathBuf> {
    let user_profile = std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("无法从 USERPROFILE 确定当前用户目录")?;
    Ok(root_dir_from_home(user_profile))
}

#[cfg(test)]
mod tests {
    use super::{KEENCODE_HOME_NAME, root_dir_from_home};
    use std::path::PathBuf;

    /// 持久化根目录名必须保持为当前唯一的 `.keencode`。
    #[test]
    fn uses_single_keencode_home_name() {
        assert_eq!(KEENCODE_HOME_NAME, ".keencode");
        assert_eq!(
            root_dir_from_home(PathBuf::from("/Users/demo")),
            PathBuf::from("/Users/demo/.keencode")
        );
    }
}
