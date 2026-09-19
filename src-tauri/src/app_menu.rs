//! macOS 应用菜单栏（屏幕顶部菜单）的本地化构建与「重启 KeenCode」入口。
//!
//! macOS 会在应用启动时自动生成一条应用菜单栏。Tauri 的默认菜单由上游固定为
//! 英文，既不随界面语言变化，也没有重启入口。这里接管整条菜单：按当前界面语言
//! 构建应用菜单、文件、编辑、显示、窗口与帮助子菜单，并在应用菜单中加入
//! 「重启 KeenCode」。菜单文案属于原生界面且不含动态内容，因此与托盘兜底文案
//! 一样直接在后端维护；界面语言变化时由 `settings_set` 触发热重建。

use tauri::{
    AppHandle, Manager,
    menu::{
        AboutMetadata, HELP_SUBMENU_ID, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
        WINDOW_SUBMENU_ID,
    },
};

use crate::app_settings::InterfaceLanguage;

/// 菜单项标识：重启应用。
const MENU_RESTART: &str = "app:restart";

/// 一条应用菜单在当前界面语言下的全部文案。
struct AppMenuLabels {
    about: &'static str,
    services: &'static str,
    hide: &'static str,
    hide_others: &'static str,
    restart: &'static str,
    quit: &'static str,
    file: &'static str,
    close_window: &'static str,
    edit: &'static str,
    undo: &'static str,
    redo: &'static str,
    cut: &'static str,
    copy: &'static str,
    paste: &'static str,
    select_all: &'static str,
    view: &'static str,
    fullscreen: &'static str,
    window: &'static str,
    minimize: &'static str,
    maximize: &'static str,
    help: &'static str,
}

impl AppMenuLabels {
    /// 列出全部字段，供测试确认没有任何空文案。
    #[cfg(test)]
    fn all(&self) -> [&'static str; 21] {
        [
            self.about,
            self.services,
            self.hide,
            self.hide_others,
            self.restart,
            self.quit,
            self.file,
            self.close_window,
            self.edit,
            self.undo,
            self.redo,
            self.cut,
            self.copy,
            self.paste,
            self.select_all,
            self.view,
            self.fullscreen,
            self.window,
            self.minimize,
            self.maximize,
            self.help,
        ]
    }
}

/// 当前界面语言下的整条菜单文案；术语沿用各语言的 macOS 惯例。
fn labels(language: InterfaceLanguage) -> AppMenuLabels {
    match language {
        InterfaceLanguage::SimplifiedChinese => AppMenuLabels {
            about: "关于 KeenCode",
            services: "服务",
            hide: "隐藏 KeenCode",
            hide_others: "隐藏其他",
            restart: "重启 KeenCode",
            quit: "退出 KeenCode",
            file: "文件",
            close_window: "关闭窗口",
            edit: "编辑",
            undo: "撤销",
            redo: "重做",
            cut: "剪切",
            copy: "拷贝",
            paste: "粘贴",
            select_all: "全选",
            view: "显示",
            fullscreen: "进入全屏幕",
            window: "窗口",
            minimize: "最小化",
            maximize: "缩放",
            help: "帮助",
        },
        InterfaceLanguage::TraditionalChinese => AppMenuLabels {
            about: "關於 KeenCode",
            services: "服務",
            hide: "隱藏 KeenCode",
            hide_others: "隱藏其他",
            restart: "重新啟動 KeenCode",
            quit: "結束 KeenCode",
            file: "檔案",
            close_window: "關閉視窗",
            edit: "編輯",
            undo: "還原",
            redo: "重做",
            cut: "剪下",
            copy: "拷貝",
            paste: "貼上",
            select_all: "全選",
            view: "檢視",
            fullscreen: "進入全螢幕",
            window: "視窗",
            minimize: "最小化",
            maximize: "縮放",
            help: "說明",
        },
        InterfaceLanguage::English => AppMenuLabels {
            about: "About KeenCode",
            services: "Services",
            hide: "Hide KeenCode",
            hide_others: "Hide Others",
            restart: "Restart KeenCode",
            quit: "Quit KeenCode",
            file: "File",
            close_window: "Close Window",
            edit: "Edit",
            undo: "Undo",
            redo: "Redo",
            cut: "Cut",
            copy: "Copy",
            paste: "Paste",
            select_all: "Select All",
            view: "View",
            fullscreen: "Enter Full Screen",
            window: "Window",
            minimize: "Minimize",
            maximize: "Zoom",
            help: "Help",
        },
    }
}

/// 用指定界面语言重建并安装应用菜单；界面语言变化时调用。
pub fn apply(app: &AppHandle, language: InterfaceLanguage) {
    match build_for_language(app, language) {
        Ok(menu) => {
            if let Err(error) = app.set_menu(menu) {
                tracing::warn!(%error, "重建 macOS 应用菜单失败");
            }
        }
        Err(error) => tracing::warn!(%error, "构建 macOS 应用菜单失败"),
    }
}

/// 处理应用菜单点击；当前只有「重启」一个自定义动作。
pub fn handle_menu_event(app: &AppHandle, event: MenuEvent) {
    if event.id().as_ref() == MENU_RESTART {
        request_restart(app);
    }
}

/// 停止全部任务、刷新本地记录后重启进程，复用统一退出清理入口。
fn request_restart(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = crate::app_exit::prepare_for_exit(&app).await {
            tracing::error!(%error, "重启前清理失败，将强制重启");
            app.state::<crate::app_exit::ExitState>().approve();
        }
        app.restart();
    });
}

/// 组装指定界面语言的完整应用菜单。
fn build_for_language(
    app: &AppHandle,
    language: InterfaceLanguage,
) -> tauri::Result<Menu<tauri::Wry>> {
    let labels = labels(language);
    let package = app.package_info();
    let config = app.config();
    let about_metadata = AboutMetadata {
        name: Some(package.name.clone()),
        version: Some(package.version.to_string()),
        copyright: config.bundle.copyright.clone(),
        authors: config
            .bundle
            .publisher
            .clone()
            .map(|publisher| vec![publisher]),
        ..Default::default()
    };

    // 首个菜单即 macOS 应用菜单，标题沿用应用名（品牌名不本地化）。
    let app_submenu = Submenu::with_items(
        app,
        package.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(app, Some(labels.about), Some(about_metadata))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, Some(labels.services))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, Some(labels.hide))?,
            &PredefinedMenuItem::hide_others(app, Some(labels.hide_others))?,
            &PredefinedMenuItem::separator(app)?,
            // 重启与退出同组，退出保持菜单末项以遵循 macOS 惯例。
            &MenuItem::with_id(app, MENU_RESTART, labels.restart, true, None::<&str>)?,
            &PredefinedMenuItem::quit(app, Some(labels.quit))?,
        ],
    )?;

    let file_submenu = Submenu::with_items(
        app,
        labels.file,
        true,
        &[&PredefinedMenuItem::close_window(
            app,
            Some(labels.close_window),
        )?],
    )?;

    let edit_submenu = Submenu::with_items(
        app,
        labels.edit,
        true,
        &[
            &PredefinedMenuItem::undo(app, Some(labels.undo))?,
            &PredefinedMenuItem::redo(app, Some(labels.redo))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, Some(labels.cut))?,
            &PredefinedMenuItem::copy(app, Some(labels.copy))?,
            &PredefinedMenuItem::paste(app, Some(labels.paste))?,
            &PredefinedMenuItem::select_all(app, Some(labels.select_all))?,
        ],
    )?;

    let view_submenu = Submenu::with_items(
        app,
        labels.view,
        true,
        &[&PredefinedMenuItem::fullscreen(
            app,
            Some(labels.fullscreen),
        )?],
    )?;

    // 窗口与帮助子菜单保留 Tauri 固定标识，macOS 需要据此注册系统窗口/帮助菜单。
    let window_submenu = Submenu::with_id_and_items(
        app,
        WINDOW_SUBMENU_ID,
        labels.window,
        true,
        &[
            &PredefinedMenuItem::minimize(app, Some(labels.minimize))?,
            &PredefinedMenuItem::maximize(app, Some(labels.maximize))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, Some(labels.close_window))?,
        ],
    )?;

    let help_submenu = Submenu::with_id(app, HELP_SUBMENU_ID, labels.help, true)?;

    Menu::with_items(
        app,
        &[
            &app_submenu,
            &file_submenu,
            &edit_submenu,
            &view_submenu,
            &window_submenu,
            &help_submenu,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 应用菜单标识必须稳定，供事件分类与测试断言使用。
    #[test]
    fn restart_menu_id_is_stable() {
        assert_eq!(MENU_RESTART, "app:restart");
    }

    /// 三种语言的每个字段都必须有可见文案，避免出现空白菜单项。
    #[test]
    fn every_label_is_non_empty_for_all_languages() {
        for language in [
            InterfaceLanguage::SimplifiedChinese,
            InterfaceLanguage::TraditionalChinese,
            InterfaceLanguage::English,
        ] {
            for value in labels(language).all() {
                assert!(!value.trim().is_empty(), "{language:?} 存在空菜单文案");
            }
        }
    }

    /// 重启项文案随界面语言变化，且三种语言各不相同。
    #[test]
    fn restart_label_follows_interface_language() {
        assert_eq!(
            labels(InterfaceLanguage::SimplifiedChinese).restart,
            "重启 KeenCode"
        );
        assert_eq!(
            labels(InterfaceLanguage::TraditionalChinese).restart,
            "重新啟動 KeenCode"
        );
        assert_eq!(
            labels(InterfaceLanguage::English).restart,
            "Restart KeenCode"
        );
    }
}
