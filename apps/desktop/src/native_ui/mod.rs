//! GPUI/Ely 原生工作台的数据服务。
//!
//! 这些模块只暴露本地 Rust API，不携带 Tauri command、AppHandle 或前端协议
//! 适配。窗口销毁只会丢弃订阅者，后台 PTY/开发进程由 `NativeWorkbench` 持有。

pub mod assets;
pub mod chat;
pub mod composer;
pub mod main_workbench;
pub mod model;
pub mod navigation;
pub mod settings;
pub mod sidebar;
pub mod style;
pub mod workbench;

pub use assets::NativeAssets;
pub use main_workbench::{NativeUi, NativeUiPage};
