use std::rc::Rc;

use gpui::{
    App, ElementId, InteractiveElement, IntoElement, Keystroke, MouseButton, ParentElement,
    RenderOnce, Role, SharedString, Styled, Window, div, prelude::*,
};

use crate::{
    primitives::{FocusRing, Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
    typography::KbdCombo,
};

type OnChange = Rc<dyn Fn(Option<Keystroke>, &mut Window, &mut App)>;

/// Records one shortcut, then lets go. Backspace clears; Escape leaves.
#[derive(IntoElement)]
pub struct HotkeyInput {
    id: ElementId,
    value: Option<Keystroke>,
    label: SharedString,
    on_change: Option<OnChange>,
}

impl HotkeyInput {
    pub fn new(id: impl Into<ElementId>, value: Option<Keystroke>) -> Self {
        Self {
            id: id.into(),
            value,
            label: SharedString::default(),
            on_change: None,
        }
    }

    /// 辅助技术读取的快捷键录制控件名称；名称不会绘制到控件内部。
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = label.into();
        self
    }

    pub fn on_change(
        mut self,
        handler: impl Fn(Option<Keystroke>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for HotkeyInput {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window
            .use_keyed_state(self.id.clone(), cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let recording = focus.is_focused(window);
        let theme = cx.theme();
        let colors = &theme.colors;
        let on_change = self.on_change;
        let label = self.label.clone();
        let click_focus = focus.clone();
        div()
            .id(self.id)
            .role(Role::Button)
            .when(!label.is_empty(), |field| field.aria_label(label))
            .track_focus(&focus)
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .h(theme.control_height(ControlSize::Md))
            .px(theme.control_padding(ControlSize::Md))
            .rounded(theme.radius(Radius::Md))
            .border_1()
            .border_color(colors.border_strong)
            .bg(colors.surface)
            .focus_ring(cx)
            .cursor_pointer()
            .text_size(theme.text_size(TextSize::Base))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&click_focus, cx);
            })
            .on_key_down(move |event, window, cx| {
                cx.stop_propagation();
                let stroke = &event.keystroke;
                let bare = !stroke.modifiers.modified();
                let next = match stroke.key.as_str() {
                    "escape" if bare => {
                        window.blur(cx);
                        return;
                    }
                    "backspace" | "delete" if bare => None,
                    _ => Some(Keystroke {
                        key_char: None,
                        ..stroke.clone()
                    }),
                };
                log::info!(
                    "hotkey input: {}",
                    next.as_ref()
                        .map_or("cleared".into(), |stroke| stroke.unparse())
                );
                window.blur(cx);
                if let Some(on_change) = &on_change {
                    on_change(next, window, cx);
                }
            })
            .map(|field| match (&self.value, recording) {
                (_, true) => field.child(
                    div()
                        .text_color(colors.fg_subtle)
                        .child("Press a shortcut…"),
                ),
                (Some(stroke), false) => field.child(KbdCombo::new(&stroke.unparse())),
                (None, false) => field.child(div().text_color(colors.fg_subtle).child("None")),
            })
            .child(
                Icon::new(IconName::Keyboard)
                    .size(IconSize::Sm)
                    .color(colors.fg_subtle),
            )
    }
}
