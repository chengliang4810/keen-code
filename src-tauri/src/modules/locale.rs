//! 只切换菜单显示文字，保留菜单 ID、系统动作和退出保护。

fn validate_locale(locale: &str) -> Result<(), String> {
    match locale {
        "en-US" | "zh-CN" => Ok(()),
        _ => Err("unsupported interface locale".to_string()),
    }
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn menu_text(text: &str, locale: &str) -> String {
    const LABELS: &[(&str, &str)] = &[
        ("File", "文件"),
        ("Edit", "编辑"),
        ("View", "视图"),
        ("Window", "窗口"),
        ("Help", "帮助"),
        ("About", "关于"),
        ("Services", "服务"),
        ("Hide Others", "隐藏其他应用"),
        ("Show All", "显示全部"),
        ("Quit", "退出"),
        ("Hide", "隐藏"),
        ("Undo", "撤销"),
        ("Redo", "重做"),
        ("Cut", "剪切"),
        ("Copy", "复制"),
        ("Paste", "粘贴"),
        ("Select All", "全选"),
        ("Minimize", "最小化"),
        ("Zoom", "缩放"),
        ("Maximize", "最大化"),
        ("Close Window", "关闭窗口"),
        ("Close", "关闭"),
        ("Enter Full Screen", "进入全屏"),
        ("Exit Full Screen", "退出全屏"),
        ("Toggle Full Screen", "切换全屏"),
        ("Full Screen", "全屏"),
        ("Bring All to Front", "全部置于前台"),
    ];
    for &(english, chinese) in LABELS {
        if text == english || text == chinese {
            return if locale == "zh-CN" { chinese } else { english }.to_string();
        }
    }
    // 系统生成的应用名保留原样，中英文往返切换不会积累翻译后的前缀。
    for (english, chinese) in [("About ", "关于 "), ("Hide ", "隐藏 "), ("Quit ", "退出 ")] {
        if let Some(name) = text
            .strip_prefix(english)
            .or_else(|| text.strip_prefix(chinese))
        {
            return format!(
                "{}{name}",
                if locale == "zh-CN" { chinese } else { english }
            );
        }
    }
    text.to_string()
}

#[tauri::command]
pub fn ui_set_locale(app: tauri::AppHandle, locale: String) -> Result<(), String> {
    validate_locale(&locale)?;
    #[cfg(target_os = "macos")]
    if let Some(menu) = app.menu() {
        localize_items(menu.items().map_err(|e| e.to_string())?, &locale)
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    Ok(())
}

#[cfg(target_os = "macos")]
fn localize_items<R: tauri::Runtime>(
    items: Vec<tauri::menu::MenuItemKind<R>>,
    locale: &str,
) -> tauri::Result<()> {
    use tauri::menu::MenuItemKind;
    for item in items {
        match item {
            MenuItemKind::Submenu(item) => {
                item.set_text(menu_text(&item.text()?, locale))?;
                localize_items(item.items()?, locale)?;
            }
            MenuItemKind::MenuItem(item) => item.set_text(menu_text(&item.text()?, locale))?,
            MenuItemKind::Predefined(item) => {
                let text = item.text()?;
                // 分隔符没有标题，不调用其不支持的标题更新操作。
                if !text.is_empty() {
                    item.set_text(menu_text(&text, locale))?;
                }
            }
            MenuItemKind::Check(item) => item.set_text(menu_text(&item.text()?, locale))?,
            MenuItemKind::Icon(item) => item.set_text(menu_text(&item.text()?, locale))?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{menu_text, validate_locale};

    #[test]
    fn accepts_only_resolved_interface_locales() {
        assert!(validate_locale("en-US").is_ok());
        assert!(validate_locale("zh-CN").is_ok());
        assert!(validate_locale("system").is_err());
        assert!(validate_locale("zh-CN/../../").is_err());
    }

    #[test]
    fn menu_labels_switch_in_both_directions_without_changing_application_names() {
        for original in [
            "Copy",
            "Quit RCode",
            "About rcode",
            "Hide Others",
            "Enter Full Screen",
        ] {
            let chinese = menu_text(original, "zh-CN");
            assert_ne!(chinese, original);
            assert_eq!(menu_text(&chinese, "en-US"), original);
            assert_eq!(menu_text(&chinese, "zh-CN"), chinese);
        }
        assert_eq!(menu_text("RCode", "zh-CN"), "RCode");
        assert_eq!(menu_text("", "zh-CN"), "");
    }
}
