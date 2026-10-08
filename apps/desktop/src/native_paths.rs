//! 原生宿主使用的固定路径集合。
//!
//! 路径在宿主启动时一次解析并作为显式依赖传入各领域服务。这样配置、项目和
//! 资源服务不需要持有窗口句柄，也不会在后台线程隐式重新发现用户目录。

use anyhow::{Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// 开发运行时与正式安装版使用不同的数据根，避免共享会话和运行时状态。
pub(crate) const KEENCODE_HOME_NAME: &str = if cfg!(debug_assertions) {
    ".keencode-dev"
} else {
    ".keencode"
};

/// 原生宿主启动时解析出的本机路径。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativePaths {
    /// KeenCode 私有配置、会话、扩展和日志的根目录。
    pub data_root: PathBuf,
    /// 当前用户主目录，用于没有平台 API 的纯 Rust 服务。
    pub home_dir: PathBuf,
    /// 当前用户文档目录，用于新项目的默认父目录。
    pub documents_dir: PathBuf,
}

impl NativePaths {
    /// 按当前进程环境解析正式宿主路径；测试可通过显式开关隔离数据根。
    pub fn discover() -> Result<Self> {
        // `dirs` 在 Windows 使用 Known Folder，在 Unix 使用平台约定，不能用
        // `home/Documents` 猜测本机的重定向或本地化文档目录。
        let home_dir = dirs::home_dir()
            .or_else(|| {
                std::env::var_os("USERPROFILE")
                    .or_else(|| std::env::var_os("HOME"))
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
            })
            .context("无法确定当前用户主目录")?;
        let data_root =
            benchmark_root_from_environment().unwrap_or_else(|| home_dir.join(KEENCODE_HOME_NAME));
        let documents_dir = dirs::document_dir().unwrap_or_else(|| home_dir.join("Documents"));
        Ok(Self::new(data_root, home_dir, documents_dir))
    }

    /// 由调用方明确提供路径，供 NativeHost 和确定性测试使用。
    pub fn new(data_root: PathBuf, home_dir: PathBuf, documents_dir: PathBuf) -> Self {
        Self {
            data_root,
            home_dir,
            documents_dir,
        }
    }

    /// 由用户主目录和数据根构造路径；文档目录遵循本机默认 `Documents` 目录。
    pub fn from_home_and_data_root(home_dir: PathBuf, data_root: PathBuf) -> Self {
        let documents_dir = home_dir.join("Documents");
        Self::new(data_root, home_dir, documents_dir)
    }

    /// 为不需要用户目录语义的临时服务从数据根构造路径。
    pub fn from_data_root(data_root: PathBuf) -> Self {
        let home_dir = data_root
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| data_root.clone());
        Self::from_home_and_data_root(home_dir, data_root)
    }
}

/// 测试与离线验收使用显式开关指定隔离数据目录；普通启动忽略该变量。
fn benchmark_root_from_environment() -> Option<PathBuf> {
    let enabled = std::env::var_os("KEENCODE_BENCHMARK");
    let path = std::env::var_os("KEENCODE_BENCHMARK_DATA_DIR");
    benchmark_root_from_values(enabled.as_deref(), path)
}

fn benchmark_root_from_values(enabled: Option<&OsStr>, path: Option<OsString>) -> Option<PathBuf> {
    (enabled == Some(OsStr::new("1")))
        .then_some(path)
        .flatten()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::{KEENCODE_HOME_NAME, NativePaths, benchmark_root_from_values};
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

    #[test]
    fn discovery_preserves_development_data_root_name() {
        let paths = NativePaths::from_home_and_data_root(
            PathBuf::from("D:/Users/demo"),
            PathBuf::from("D:/Users/demo").join(KEENCODE_HOME_NAME),
        );
        assert_eq!(
            paths.data_root,
            PathBuf::from("D:/Users/demo").join(KEENCODE_HOME_NAME)
        );
        assert_eq!(
            paths.documents_dir,
            PathBuf::from("D:/Users/demo/Documents")
        );
    }

    #[test]
    fn benchmark_root_requires_explicit_switch_and_non_empty_path() {
        let isolated = OsString::from("D:/bench/keencode");
        assert_eq!(
            benchmark_root_from_values(Some(OsStr::new("1")), Some(isolated.clone())),
            Some(PathBuf::from(isolated))
        );
        assert_eq!(
            benchmark_root_from_values(Some(OsStr::new("0")), Some(OsString::from("D:/bench"))),
            None
        );
        assert_eq!(
            benchmark_root_from_values(Some(OsStr::new("1")), Some(OsString::new())),
            None
        );
        assert_eq!(
            benchmark_root_from_values(Some(OsStr::new("1")), None),
            None
        );
    }

    #[test]
    fn explicit_paths_are_not_reinterpreted() {
        let paths = NativePaths::new(
            PathBuf::from("D:/bench/data"),
            PathBuf::from("D:/Users/demo"),
            PathBuf::from("D:/Users/demo/文档"),
        );
        assert_eq!(paths.data_root, PathBuf::from("D:/bench/data"));
        assert_eq!(paths.home_dir, PathBuf::from("D:/Users/demo"));
        assert_eq!(paths.documents_dir, PathBuf::from("D:/Users/demo/文档"));
    }
}
