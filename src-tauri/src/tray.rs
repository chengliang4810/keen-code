//! 常驻的系统托盘 / macOS 菜单栏图标。
//!
//! 托盘图标在启动时创建并常驻，菜单项由前端按当前界面语言投影后推送，
//! 菜单里的会话列表同样来自前端已确定标题、归档状态的可展示会话投影。
//! 窗口关闭时按设置隐藏窗口并移除 macOS Dock 图标，应用继续在后台运行。

use serde::{Deserialize, Serialize};
#[cfg(not(target_os = "macos"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager,
    menu::{Menu, MenuBuilder, MenuEvent},
    tray::TrayIconBuilder,
};

/// 唯一托盘图标标识。
const TRAY_ID: &str = "keencode-tray";
/// 菜单项标识：显示主窗口。
const MENU_SHOW: &str = "tray:show";
/// 菜单项标识：新建对话。
const MENU_NEW_CHAT: &str = "tray:new-chat";
/// 菜单项标识：退出应用。
const MENU_QUIT: &str = "tray:quit";
/// 会话菜单项标识前缀，其后为 Session 稳定标识。
const MENU_SESSION_PREFIX: &str = "tray:session:";

/// 前端收到后新建一个对话。
const EVENT_NEW_CHAT: &str = "app://tray-new-chat";
/// 前端收到后打开托盘菜单中选中的会话。
const EVENT_OPEN_SESSION: &str = "app://tray-open-session";

/// macOS 菜单栏模板图标：单色加透明度，由系统按菜单栏明暗自动着色。
#[cfg(target_os = "macos")]
const TRAY_ICON: tauri::image::Image<'static> = tauri::include_image!("./icons/tray-macos.png");
/// 非 macOS 平台使用的系统托盘图标；不随菜单栏明暗自动着色。
#[cfg(not(target_os = "macos"))]
const TRAY_ICON: tauri::image::Image<'static> = tauri::include_image!("./icons/tray-windows.png");

/// 托盘菜单固定项在当前界面语言下的文案。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuLabels {
    /// 新建对话。
    pub new_chat: String,
    /// 显示窗口。
    pub show: String,
    /// 退出应用。
    pub quit: String,
}

/// 托盘菜单中的一个会话入口。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraySessionEntry {
    /// Session 稳定标识。
    pub id: String,
    /// 当前界面上显示的会话标题。
    pub title: String,
}

/// 前端推送的完整托盘菜单投影。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayMenuPayload {
    /// 固定项文案。
    pub labels: TrayMenuLabels,
    /// 需要出现在菜单里的最近会话，已由前端裁剪到固定条数。
    pub sessions: Vec<TraySessionEntry>,
}

impl TrayMenuPayload {
    /// 前端完成首次投影前的兜底菜单；文案跟随已保存的界面语言。
    fn fallback(language: crate::app_settings::InterfaceLanguage) -> Self {
        let (new_chat, show, quit) = match language {
            crate::app_settings::InterfaceLanguage::SimplifiedChinese => {
                ("新建对话", "显示窗口", "退出 KeenCode")
            }
            crate::app_settings::InterfaceLanguage::TraditionalChinese => {
                ("新增對話", "顯示視窗", "結束 KeenCode")
            }
            crate::app_settings::InterfaceLanguage::English => {
                ("New conversation", "Show KeenCode", "Quit KeenCode")
            }
        };
        Self {
            labels: TrayMenuLabels {
                new_chat: new_chat.to_owned(),
                show: show.to_owned(),
                quit: quit.to_owned(),
            },
            sessions: Vec::new(),
        }
    }
}

/// 托盘菜单项对应的应用动作。
#[derive(Clone, Debug, PartialEq, Eq)]
enum TrayAction {
    /// 显示主窗口。
    Show,
    /// 显示主窗口并新建对话。
    NewChat,
    /// 显示主窗口并打开指定会话。
    OpenSession(String),
    /// 按统一退出入口请求退出。
    Quit,
}

/// 前端在托盘菜单中选中一个会话时收到的载荷。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrayOpenSessionPayload {
    /// 被选中的 Session 稳定标识。
    session_id: String,
}

/// 创建常驻托盘图标；失败不阻断应用启动。
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    // 设置读取失败时回退首次启动默认语言，菜单随后由前端按当前语言覆盖。
    let language = crate::app_settings::get(app)
        .map(|settings| settings.interface_language)
        .unwrap_or_default();
    let menu = build_menu(app, &TrayMenuPayload::fallback(language))?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(TRAY_ICON)
        .tooltip("KeenCode")
        .menu(&menu)
        .on_menu_event(handle_menu_event);
    #[cfg(target_os = "macos")]
    {
        // macOS 惯例：左键单击直接弹出菜单，菜单首项即「显示窗口」。
        builder = builder.icon_as_template(true).show_menu_on_left_click(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Windows 托盘惯例：左键单击显示窗口，右键弹出菜单。
        builder = builder
            .show_menu_on_left_click(false)
            .on_tray_icon_event(handle_tray_icon_event);
    }
    builder.build(app)?;
    Ok(())
}

/// 用前端投影替换当前托盘菜单。
pub fn apply_menu(app: &AppHandle, payload: &TrayMenuPayload) -> Result<(), String> {
    let tray = app
        .tray_by_id(TRAY_ID)
        .ok_or_else(|| "系统托盘尚未创建".to_owned())?;
    let menu = build_menu(app, payload).map_err(|error| error.to_string())?;
    tray.set_menu(Some(menu)).map_err(|error| error.to_string())
}

/// 恢复主窗口并显示 macOS Dock 图标。
pub fn show_main_window(app: &AppHandle) {
    // 先恢复 Dock/前台身份，否则 accessory 状态下窗口无法被真正聚焦。
    set_dock_visibility(app, true);
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// 隐藏主窗口并移除 macOS Dock 图标，应用继续后台常驻。
pub fn hide_main_window(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "主窗口不存在".to_owned())?;
    window.hide().map_err(|error| error.to_string())?;
    set_dock_visibility(app, false);
    Ok(())
}

/// 切换 macOS Dock 图标可见性；其他平台没有等价概念。
fn set_dock_visibility(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    if let Err(error) = app.set_dock_visibility(visible) {
        tracing::warn!(%error, visible, "切换 Dock 图标可见性失败");
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

/// 按前端投影构造托盘菜单。
fn build_menu(app: &AppHandle, payload: &TrayMenuPayload) -> tauri::Result<Menu<tauri::Wry>> {
    let mut builder = MenuBuilder::new(app)
        .text(MENU_NEW_CHAT, menu_text(&payload.labels.new_chat))
        .text(MENU_SHOW, menu_text(&payload.labels.show));
    if !payload.sessions.is_empty() {
        builder = builder.separator();
        for session in &payload.sessions {
            builder = builder.text(session_menu_id(&session.id), menu_text(&session.title));
        }
    }
    builder
        .separator()
        .text(MENU_QUIT, menu_text(&payload.labels.quit))
        .build()
}

/// 会话菜单项标识。
fn session_menu_id(session_id: &str) -> String {
    format!("{MENU_SESSION_PREFIX}{session_id}")
}

/// 把菜单项标识映射为托盘动作；未知标识忽略。
fn classify_menu_id(id: &str) -> Option<TrayAction> {
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

/// Windows 菜单用 `&` 标记助记符，用户内容中的 `&` 必须转义后原样显示。
fn menu_text(value: &str) -> String {
    #[cfg(windows)]
    {
        value.replace('&', "&&")
    }
    #[cfg(not(windows))]
    {
        value.to_owned()
    }
}

/// 处理托盘菜单点击。
fn handle_menu_event(app: &AppHandle, event: MenuEvent) {
    match classify_menu_id(event.id().as_ref()) {
        Some(TrayAction::Show) => show_main_window(app),
        Some(TrayAction::NewChat) => {
            show_main_window(app);
            if let Err(error) = app.emit(EVENT_NEW_CHAT, ()) {
                tracing::error!(%error, "通知前端新建对话失败");
            }
        }
        Some(TrayAction::OpenSession(session_id)) => {
            show_main_window(app);
            let payload = TrayOpenSessionPayload { session_id };
            if let Err(error) = app.emit(EVENT_OPEN_SESSION, payload) {
                tracing::error!(%error, "通知前端打开托盘会话失败");
            }
        }
        Some(TrayAction::Quit) => {
            // 退出确认对话框必须可见，因此先恢复窗口再走统一退出入口。
            show_main_window(app);
            if let Err(error) = crate::app_exit::request_exit(app) {
                tracing::error!(%error, "托盘退出请求失败");
            }
        }
        None => {}
    }
}

/// 非 macOS 平台按系统托盘惯例用左键单击显示窗口。
#[cfg(not(target_os = "macos"))]
fn handle_tray_icon_event(tray: &TrayIcon, event: TrayIconEvent) {
    if matches!(
        event,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
    ) {
        show_main_window(tray.app_handle());
    }
}

#[tauri::command]
/// 用当前界面语言的投影替换托盘菜单。
pub fn tray_set_menu(app: AppHandle, menu: TrayMenuPayload) -> Result<(), String> {
    apply_menu(&app, &menu)
}

#[tauri::command]
/// 处理主窗口的关闭手势：常驻设置下隐藏到托盘，否则走统一退出入口。
pub fn app_close_window(app: AppHandle) -> Result<(), String> {
    if !close_to_tray_enabled(&app) {
        crate::app_exit::request_exit(&app)?;
        return Ok(());
    }
    hide_main_window(&app)
}

/// 读取关闭窗口时的常驻策略；设置读取失败按直接退出处理，避免窗口无法关闭。
fn close_to_tray_enabled(app: &AppHandle) -> bool {
    crate::app_settings::get(app)
        .map(|settings| settings.close_to_tray)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个固定菜单项映射到唯一动作，会话项从标识中还原 Session。
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

    /// 未知标识和空会话标识都必须被忽略，不得触发任何窗口动作。
    #[test]
    fn ignores_unknown_and_empty_menu_items() {
        assert_eq!(classify_menu_id("tray:unknown"), None);
        assert_eq!(classify_menu_id(MENU_SESSION_PREFIX), None);
        assert_eq!(classify_menu_id(""), None);
    }

    /// 前端投影的字段名与界面 camelCase 契约一致。
    #[test]
    fn menu_payload_uses_camel_case_fields() {
        let value = serde_json::to_value(TrayMenuPayload {
            labels: TrayMenuLabels {
                new_chat: "New".to_owned(),
                show: "Show".to_owned(),
                quit: "Quit".to_owned(),
            },
            sessions: vec![TraySessionEntry {
                id: "session-1".to_owned(),
                title: "Title".to_owned(),
            }],
        })
        .expect("托盘菜单投影应可序列化");
        assert_eq!(value["labels"]["newChat"], "New");
        assert_eq!(value["sessions"][0]["id"], "session-1");
    }
}
