use super::{SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*};
use ely_gpui_component::forms::HotkeyInput;
use gpui::{AnyElement, App, IntoElement, Keystroke, ParentElement, Styled, Window, div, px};

fn parse_hotkey(value: &str) -> Option<Keystroke> {
    Keystroke::parse(value).ok()
}

fn control(
    id: &'static str,
    label: &'static str,
    current: &KeyboardSettings,
    value: &str,
    dispatch: SettingsCommandHandler,
    update: impl Fn(&mut KeyboardSettings, String) + 'static,
) -> AnyElement {
    let initial = current.clone();
    HotkeyInput::new(id, parse_hotkey(value))
        .label(label)
        .on_change(move |stroke, window, cx| {
            let Some(stroke) = stroke else {
                return;
            };
            let mut next = initial.clone();
            update(&mut next, stroke.unparse());
            dispatch(SettingsCommand::SaveKeyboard(next), window, cx);
        })
        .into_any_element()
}

pub(super) fn render(
    current: &KeyboardSettings,
    dispatch: SettingsCommandHandler,
    _window: &mut Window,
    _cx: &mut App,
) -> AnyElement {
    let composer = control(
        "settings-keyboard-composer-submit",
        "发送消息快捷键",
        current,
        &current.composer_submit,
        dispatch.clone(),
        |settings, value| settings.composer_submit = value,
    );
    let new_chat = control(
        "settings-keyboard-new-chat",
        "新建会话快捷键",
        current,
        &current.new_chat,
        dispatch.clone(),
        |settings, value| settings.new_chat = value,
    );
    let search = control(
        "settings-keyboard-search",
        "搜索快捷键",
        current,
        &current.search,
        dispatch.clone(),
        |settings, value| settings.search = value,
    );
    let quit = control(
        "settings-keyboard-quit",
        "退出快捷键",
        current,
        &current.quit,
        dispatch,
        |settings, value| settings.quit = value,
    );

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            SettingsSection::new("输入区")
                .description("自定义 Composer 的发送按键；Shift+Enter 始终保留为换行。")
                .row(
                    SettingsRow::new("发送消息")
                        .description("按下此键发送当前输入。")
                        .control(div().w(px(232.0)).max_w_full().child(composer)),
                ),
        )
        .child(
            SettingsSection::new("窗口快捷键")
                .description("这些快捷键在当前窗口内生效，并与文本编辑快捷键保持隔离。")
                .row(
                    SettingsRow::new("新建会话")
                        .control(div().w(px(232.0)).max_w_full().child(new_chat)),
                )
                .row(
                    SettingsRow::new("搜索").control(div().w(px(232.0)).max_w_full().child(search)),
                )
                .row(SettingsRow::new("退出").control(div().w(px(232.0)).max_w_full().child(quit))),
        )
        .into_any_element()
}
