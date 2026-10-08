//! 原生 GPUI 设置窗口。
//!
//! `SettingsPanel` 是一个可嵌入 `gpui_chat` 的面板实体。所有持久化和执行操作都
//! 通过 [`NativeSettingsService`] 进入 NativeHost；设置 UI 不直接读写磁盘，也不
//! 使用旧 Tauri/JavaScript 适配层。

mod agents;
mod appearance;
mod automation;
mod cache;
pub mod contracts;
mod diagnostics;
mod domain;
mod general;
mod hooks;
mod keyboard;
pub(crate) mod native_keybindings;
pub(crate) mod native_settings;
mod panel;
mod providers;
mod resources;
mod section;
mod usage;
mod workflows;

use std::rc::Rc;

use gpui::{App, Window};

pub use contracts::*;
pub use domain::NativeSettingsRuntimeDomain;
pub use general::apply_general_settings;
pub use native_keybindings::{NativeKeybindingAction, NativeKeybindingsState};
pub use native_settings::{
    NativeSettingsAdapter, NativeSettingsDomain, NativeSettingsRuntimePort,
    NativeSettingsSubscriptionHandle, NativeSettingsUpdatePort,
};
pub use panel::{SettingsPanel, SettingsPanelHandle};
pub(crate) use section::{SettingsRow, SettingsSection, settings_heading};

/// 页面控件只负责发出 typed command，Panel 负责把命令交给 NativeHost。
pub(crate) type SettingsCommandHandler = Rc<dyn Fn(SettingsCommand, &mut Window, &mut App)>;
