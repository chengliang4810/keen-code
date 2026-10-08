use gpui::{
    AbsoluteLength, App, ClickEvent, ElementId, IntoElement, MouseButton, Pixels, RenderOnce, Role,
    SharedString, Window, div, prelude::*, px,
};

use super::button::{ButtonVariant, ClickHandler, label_size, tone};
use crate::{
    i18n,
    primitives::{FocusRing, Icon, IconName, Tooltip},
    theme::{ActiveTheme, ControlSize, Radius},
};

/// Square button holding one icon.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    variant: ButtonVariant,
    size: ControlSize,
    side_length: Option<AbsoluteLength>,
    /// 可选的背景内缩；根节点仍保留完整命中范围，只收缩视觉背景。
    background_inset: Pixels,
    disabled: bool,
    disabled_opacity: f32,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    pub fn new(id: impl Into<ElementId>, icon: IconName) -> Self {
        Self {
            id: id.into(),
            icon,
            variant: ButtonVariant::Ghost,
            size: ControlSize::default(),
            side_length: None,
            background_inset: Pixels::ZERO,
            disabled: false,
            disabled_opacity: 0.45,
            tooltip: None,
            on_click: None,
        }
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ControlSize) -> Self {
        self.size = size;
        self
    }

    /// 固定来源控件可独立于 Ely density 指定正方形边长；未设置时沿用主题尺寸。
    pub fn side_length(mut self, side_length: Pixels) -> Self {
        self.side_length = Some(side_length.into());
        self
    }

    /// 固定来源控件可将背景向内收缩；根节点的尺寸、边框和交互命中范围保持不变。
    pub fn background_inset(mut self, inset: Pixels) -> Self {
        self.background_inset = inset.max(Pixels::ZERO);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 固定来源可指定禁用态透明度；未设置时保留 Ely 默认的 45%。
    pub fn disabled_opacity(mut self, opacity: f32) -> Self {
        self.disabled_opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Names what the icon does, on hover.
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let tone = tone(self.variant, &theme.colors);
        let (_, icon_size) = label_size(self.size);
        let side = self
            .side_length
            .unwrap_or_else(|| theme.control_height(self.size).into());
        let name: SharedString = self
            .tooltip
            .clone()
            .unwrap_or_else(|| self.icon.name().into());
        let inset_enabled = self.background_inset != Pixels::ZERO;
        let group_name = inset_enabled.then(|| format!("ely-icon-button:{id}", id = self.id));
        let background = group_name.as_ref().map(|group_name| {
            let radius = theme.radius(Radius::Md).to_pixels(window.rem_size());
            let inner_radius = if self.background_inset < radius {
                radius - self.background_inset
            } else {
                Pixels::ZERO
            };
            div()
                // 按压样式需要稳定状态 ID；可访问按钮和点击仍由外层节点承载。
                .id("background")
                .absolute()
                .inset(self.background_inset)
                // Taffy 的绝对 inset 会先扣除父节点固定的 1px border；
                // 只抵消 border，才能让任意 inset 保持其公开 API 语义。
                .m(-px(1.0))
                .rounded(inner_radius)
                .bg(tone.bg)
                .when(!self.disabled, |el| {
                    el.group_hover(group_name.clone(), |style| style.bg(tone.hover))
                        .group_active(group_name.clone(), |style| style.bg(tone.pressed))
                })
        });

        div()
            .when_some(group_name, |el, group| el.group(group))
            .id(self.id)
            .role(Role::Button)
            .aria_label(name)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(side)
            .rounded(theme.radius(Radius::Md))
            .border_1()
            .border_color(tone.border)
            .when(!inset_enabled, |el| el.bg(tone.bg))
            .when_some(background, |el, background| el.relative().child(background))
            .child(Icon::new(self.icon).size(icon_size).color(tone.fg))
            .when_some(self.tooltip, |el, text| el.tooltip(Tooltip::text(text)))
            .map(|el| {
                if self.disabled {
                    return el
                        .aria_description(i18n::text(cx, "state.unavailable", &[]))
                        .opacity(self.disabled_opacity)
                        .cursor_not_allowed();
                }
                el.cursor_pointer()
                    .tab_index(0)
                    .when(!inset_enabled, |el| {
                        el.hover(|style| style.bg(tone.hover))
                            .active(|style| style.bg(tone.pressed))
                    })
                    .focus_ring(cx)
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .when_some(self.on_click, |el, handler| el.on_click(handler))
            })
    }
}
