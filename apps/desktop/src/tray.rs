//! NativeHost 系统托盘模型与跨平台窗口动作。
//!
//! 菜单状态和动作在这里保持平台无关；实际 Windows Shell/AppKit 调用由
//! `NativeTrayHost` 实现。这样 GPUI root 可以把同一 typed 菜单投影到原生窗口，
//! 不需要旧的 Tauri 菜单或字符串事件。

use serde::{Deserialize, Serialize};

/// 菜单项标识：显示主窗口。
pub const MENU_SHOW: &str = "tray:show";
/// 菜单项标识：新建对话。
pub const MENU_NEW_CHAT: &str = "tray:new-chat";
/// 菜单项标识：退出应用。
pub const MENU_QUIT: &str = "tray:quit";
/// 会话菜单项标识前缀，其后为 Session 稳定标识。
pub const MENU_SESSION_PREFIX: &str = "tray:session:";

/// 托盘菜单固定项文案。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuLabels {
    pub new_chat: String,
    pub show: String,
    pub quit: String,
}

/// 托盘菜单中的一个会话入口。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TraySessionEntry {
    pub id: String,
    pub title: String,
}

/// NativeHost 投影到原生托盘的完整菜单模型。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuPayload {
    pub labels: TrayMenuLabels,
    pub sessions: Vec<TraySessionEntry>,
}

/// 托盘平台适配器。Windows 实现调用 Shell_NotifyIcon/Win32 菜单；没有真实
/// 托盘后端的平台在编译期裁剪菜单、隐藏和角标入口。
pub trait NativeTrayHost: Send + Sync {
    fn show_main_window(&self);
    #[cfg(windows)]
    fn hide_main_window(&self) -> Result<(), String>;
    #[cfg(windows)]
    fn set_dock_badge(&self, count: Option<u32>) -> Result<(), String>;
    #[cfg(windows)]
    fn close_to_tray_enabled(&self) -> bool;
    fn request_exit(&self) -> Result<(), String>;
}

/// 托盘菜单项对应的应用动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    NewChat,
    OpenSession(String),
    Quit,
}

/// 隐藏主窗口并移除平台菜单栏/任务栏身份。
#[cfg(windows)]
pub fn hide_main_window<H: NativeTrayHost>(host: &H) -> Result<(), String> {
    host.hide_main_window()
}

/// 将未读数量映射到平台角标；0 必须移除角标。
#[cfg(windows)]
pub fn set_badge<H: NativeTrayHost>(host: &H, count: u32) -> Result<(), String> {
    host.set_dock_badge((count > 0).then_some(count))
}

/// 按托盘动作执行原生窗口和退出行为。
pub fn handle_action<H: NativeTrayHost + NativeTrayNavigation>(
    host: &H,
    action: TrayAction,
) -> Result<(), String> {
    match action {
        TrayAction::Show => host.show_main_window(),
        TrayAction::NewChat => {
            host.show_main_window();
            host.open_new_chat();
        }
        TrayAction::OpenSession(session_id) => {
            validate_session_menu_id(&session_id)?;
            host.show_main_window();
            host.open_session(&session_id);
        }
        TrayAction::Quit => {
            host.show_main_window();
            host.request_exit()?
        }
    }
    Ok(())
}

/// 托盘事件需要的两个 UI 导航动作由 NativeHost 直接投影到 GPUI。
pub trait NativeTrayNavigation {
    fn open_new_chat(&self);
    fn open_session(&self, session_id: &str);
}

/// 处理主窗口关闭手势：按设置隐藏到托盘，否则走统一退出入口。
pub fn app_close_window<H: NativeTrayHost>(host: &H) -> Result<(), String> {
    #[cfg(windows)]
    {
        if !host.close_to_tray_enabled() {
            host.request_exit()?;
            return Ok(());
        }
        hide_main_window(host)
    }
    #[cfg(not(windows))]
    {
        host.request_exit()
    }
}

/// 菜单项标识映射为动作；未知标识必须忽略。
pub fn classify_menu_id(id: &str) -> Option<TrayAction> {
    if id == MENU_SHOW {
        return Some(TrayAction::Show);
    }
    if id == MENU_NEW_CHAT {
        return Some(TrayAction::NewChat);
    }
    if id == MENU_QUIT {
        return Some(TrayAction::Quit);
    }
    id.strip_prefix(MENU_SESSION_PREFIX)
        .filter(|session_id| !session_id.is_empty())
        .map(|session_id| TrayAction::OpenSession(session_id.to_owned()))
}

pub fn session_menu_id(session_id: &str) -> String {
    format!("{MENU_SESSION_PREFIX}{session_id}")
}

fn validate_session_menu_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err("托盘会话标识无效".to_owned());
    }
    Ok(())
}

/// Windows 菜单用 `&` 标记助记符，用户内容中的 `&` 必须转义后原样显示。
pub fn menu_text(value: &str) -> String {
    #[cfg(windows)]
    {
        value.replace('&', "&&")
    }
    #[cfg(not(windows))]
    {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_tray_menu_items() {
        assert_eq!(classify_menu_id(MENU_SHOW), Some(TrayAction::Show));
        assert_eq!(classify_menu_id(MENU_NEW_CHAT), Some(TrayAction::NewChat));
        assert_eq!(classify_menu_id(MENU_QUIT), Some(TrayAction::Quit));
        assert_eq!(
            classify_menu_id(&session_menu_id("session-1")),
            Some(TrayAction::OpenSession("session-1".to_owned()))
        );
    }

    #[test]
    fn ignores_unknown_and_empty_menu_items() {
        assert_eq!(classify_menu_id("tray:unknown"), None);
        assert_eq!(classify_menu_id(MENU_SESSION_PREFIX), None);
        assert_eq!(classify_menu_id(""), None);
    }

    #[test]
    fn menu_text_is_stable_for_user_content() {
        let text = menu_text("A & B");
        #[cfg(windows)]
        assert_eq!(text, "A && B");
        #[cfg(not(windows))]
        assert_eq!(text, "A & B");
    }

    #[test]
    fn rejects_unsafe_session_actions() {
        assert!(validate_session_menu_id(" session").is_err());
        assert!(validate_session_menu_id("session\n1").is_err());
        assert!(validate_session_menu_id("session-1").is_ok());
    }
}
