use std::rc::Rc;

use gpui::{
    Animation, AnimationExt, App, ElementId, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, Pixels, RenderOnce, Role, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::*, transparent_black,
};

use super::options::OnFlag;
use crate::{
    i18n, motion,
    primitives::tab_stop,
    theme::{ActiveTheme, Elevation, Mix, TextSize},
};

/// 可选样式覆盖只在调用方显式传入时生效，保持其他 Ely 调用方的密度和颜色行为。
#[derive(Clone, Copy, Default)]
pub struct SwitchStyle {
    /// 轨道的总宽度和高度（包含内边距与边框）；不设置时使用 Ely 主题尺寸。
    pub width: Option<Pixels>,
    pub height: Option<Pixels>,
    /// 轨道内边距与边框宽度；不设置时保留 Ely 的 `p-0.5` 和 1px 边框。
    pub padding: Option<Pixels>,
    pub border_width: Option<Pixels>,
    /// 关闭状态的轨道颜色和 thumb 颜色；开启轨道仍使用主题 accent。
    pub rail_off: Option<Hsla>,
    pub knob: Option<Hsla>,
    /// 是否保留 Ely 默认的 thumb 阴影；设置页按来源控件关闭该阴影，避免阴影改变开关外接几何。
    pub knob_shadow: Option<bool>,
}

/// An on-off switch. The thumb slides over with a small overshoot.
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    on: bool,
    label: Option<SharedString>,
    aria_label: Option<SharedString>,
    disabled: bool,
    on_change: Option<OnFlag>,
    style: SwitchStyle,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, on: bool) -> Self {
        Self {
            id: id.into(),
            on,
            label: None,
            aria_label: None,
            disabled: false,
            on_change: None,
            style: SwitchStyle::default(),
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// 仅设置无障碍名称，不渲染可见标签。
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = Some(label.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 覆盖轨道几何和语义颜色；未提供的字段继续使用 Ely 默认值。
    pub fn style(mut self, style: SwitchStyle) -> Self {
        self.style = style;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = tab_stop(
            (self.id.clone(), "focus").into(),
            !self.disabled,
            window,
            cx,
        );
        let changes = motion::changes((self.id.clone(), "changes"), self.on, window, cx);
        let focused = focus.is_focused(window);
        let theme = cx.theme();
        let colors = &theme.colors;
        let track = theme.switch_track();
        let width = self
            .style
            .width
            .unwrap_or_else(|| track.width.to_pixels(window.rem_size()));
        let height = self
            .style
            .height
            .unwrap_or_else(|| track.height.to_pixels(window.rem_size()));
        let travel = width - height;
        let (off, on) = (
            self.style.rail_off.unwrap_or(colors.border_strong),
            colors.accent,
        );
        let knob_off = self.style.knob.unwrap_or(colors.fg_muted);
        let knob_on = self.style.knob.unwrap_or(colors.on_accent);
        let (from, to, fill_from, fill_to, knob_from, knob_to) = if self.on {
            (0.0, 1.0, off, on, knob_off, knob_on)
        } else {
            (1.0, 0.0, on, off, knob_on, knob_off)
        };
        let thumb = div()
            .h_full()
            .map(|mut thumb| {
                thumb.style().aspect_ratio = Some(1.0);
                thumb
            })
            .rounded_full()
            .bg(knob_to)
            .when(self.style.knob_shadow.unwrap_or(true), |thumb| {
                thumb.shadow(theme.elevation(Elevation::Raised))
            });
        let mut rail = div()
            .flex()
            .flex_none()
            .items_center()
            .w(width)
            .h(height)
            .p_0p5()
            .rounded_full()
            .border_1()
            .border_color(if focused {
                colors.focus
            } else {
                transparent_black()
            })
            .bg(fill_to)
            .when(self.disabled, |rail| rail.opacity(0.5));
        if let Some(padding) = self.style.padding {
            rail = rail.p(padding);
        }
        if let Some(border_width) = self.style.border_width {
            rail = rail.border(border_width);
        }
        let rail = if changes == 0 {
            rail.child(thumb.ml(travel * to)).into_any_element()
        } else {
            let duration = motion::duration(motion::BASE, cx);
            rail.child(thumb.with_animation(
                ("switch-thumb", changes),
                Animation::new(duration),
                move |thumb, t| {
                    thumb
                        .ml(travel * motion::lerp(from, to, motion::spring(t)))
                        .bg(knob_from.mix(&knob_to, t))
                },
            ))
            .with_animation(
                ("switch-fill", changes),
                Animation::new(duration),
                move |rail, t| rail.bg(fill_from.mix(&fill_to, t)),
            )
            .into_any_element()
        };
        let (id, next, on_change) = (self.id.clone(), !self.on, self.on_change);
        let aria_label = self.aria_label.clone().or_else(|| self.label.clone());
        let toggle = Rc::new(move |window: &mut Window, cx: &mut App| {
            log::info!("switch {id:?}: {}", if next { "on" } else { "off" });
            if let Some(on_change) = &on_change {
                on_change(next, window, cx);
            }
        });
        let mouse_focus = focus.clone();
        div()
            .id(self.id)
            .role(Role::Switch)
            .aria_toggled(self.on.into())
            .when_some(aria_label, |row, label| row.aria_label(label))
            .when(self.disabled, |row| {
                row.aria_description(i18n::text(cx, "state.unavailable", &[]))
            })
            .track_focus(&focus)
            .flex()
            .items_center()
            .gap_2()
            .text_size(theme.text_size(TextSize::Base))
            .text_color(if self.disabled {
                colors.fg_disabled
            } else {
                colors.fg
            })
            .when(!self.disabled, |row| {
                let click_toggle = toggle.clone();
                row.cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        window.focus(&mouse_focus, cx);
                        window.prevent_default();
                    })
                    // GPUI 会把已聚焦控件的 Space/Enter keyup 合成为 ClickEvent；
                    // 不在 keydown 再切换，避免一次按键触发两次状态变更。
                    .on_click(move |_, window, cx| click_toggle(window, cx))
            })
            .child(rail)
            .children(self.label.map(|label| {
                div()
                    .debug_selector(|| "switch-label".into())
                    .flex_1()
                    .min_w_0()
                    .child(label)
            }))
    }
}
