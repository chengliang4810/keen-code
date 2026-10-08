//! 插件核心不依赖窗口、Tauri 或真实用户配置。

mod http_response;
mod path_utils;
pub mod plugins;
mod storage;
mod workspace;

pub use plugins::*;
