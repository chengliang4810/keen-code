//! 设置页统一的分组和行布局。
//!
//! Ely 的通用设置组件面向多种应用，默认间距和字号不符合 KeenCode 的 ZCode
//! 设置页基线，因此在设置模块内保留相同的构建 API，并集中承载本地视觉令牌。

use ely_gpui_component::{forms::SwitchStyle, theme::ActiveTheme};
use gpui::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce, Role,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::native_ui::style::{UiTextSize, settings_card_color, ui_text_size};

/// 当前补偿只由 Windows DPI192 的同条件实图证明；其他 DPI 保留正常绘制，
/// 避免把一物理像素的差值推广成未经验证的固定逻辑像素偏移。
fn paint_snap_offset(window: &Window) -> gpui::Pixels {
    if cfg!(target_os = "windows") && (window.scale_factor() - 2.0).abs() < f32::EPSILON {
        px(0.5)
    } else {
        px(0.0)
    }
}

/// 固定来源 Input/Select 的高度独立于 Ely density；文字仍使用唯一界面字号令牌。
pub(super) fn settings_input_style(
    theme: &ely_gpui_component::theme::Theme,
    large: bool,
    select: bool,
) -> ely_gpui_component::forms::InputStyle {
    ely_gpui_component::forms::InputStyle {
        height: Some(px(if large { 32.0 } else { 28.0 })),
        padding_left: Some(px(if large { 12.0 } else { 8.0 })),
        padding_right: Some(px(if select {
            if large { 8.0 } else { 4.0 }
        } else if large {
            12.0
        } else {
            8.0
        })),
        text_size: Some(ui_text_size(theme, UiTextSize::Base)),
        background: Some(theme.colors.sunken),
        border: Some(theme.colors.border),
        radius: Some(px(if large { 8.0 } else { 6.0 })),
        gap: Some(px(if select { 6.0 } else { 8.0 })),
    }
}

/// 固定来源 Switch 的几何和语义颜色；Ely 默认值继续服务于其他调用方。
pub(super) fn settings_switch_style(theme: &ely_gpui_component::theme::Theme) -> SwitchStyle {
    SwitchStyle {
        width: Some(px(32.0)),
        height: Some(px(18.0)),
        padding: Some(px(1.0)),
        border_width: Some(px(0.0)),
        rail_off: Some(theme.colors.accent.alpha(0.3)),
        knob: Some(theme.colors.on_accent),
        knob_shadow: Some(false),
    }
}

pub(super) fn settings_select_indicator(
    theme: &ely_gpui_component::theme::Theme,
) -> impl IntoElement {
    // 固定来源 Select 由 lucide-react 1.17.0 的 ChevronDown 提供 1.5px provider 描边；
    // Native 单独承载相同 path，避免修改 Ely 共享的 2px 图标资产。
    ely_gpui_component::primitives::Icon::from_path("native/icons/chevron-down.svg")
        .size(ely_gpui_component::theme::IconSize::Sm)
        // 来源 Select 的 foreground-subtle 是 60% 语义；Native 的对应令牌是 fg_muted。
        .color(theme.colors.fg_muted)
}

/// 来源数字输入的单位使用 text-ui-lg，不能继承 Ely Caption 的 12px 字号。
pub(super) fn settings_unit_label(theme: &ely_gpui_component::theme::Theme) -> impl IntoElement {
    div()
        .text_size(ui_text_size(theme, UiTextSize::Lg))
        .text_color(theme.colors.fg_subtle)
        .child("px")
}

/// 设置页的小节标题和说明显式覆盖 Ely 的 Heading/Caption 默认字号，保持来源
/// 的 16px/14px 层级，同时继续使用统一的界面字号比例。
pub(crate) fn settings_heading(
    text: impl Into<SharedString>,
    theme: &ely_gpui_component::theme::Theme,
) -> impl IntoElement {
    div()
        .text_size(ui_text_size(theme, UiTextSize::Lg))
        .line_height(ui_text_size(theme, UiTextSize::Lg) * 1.5)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.colors.fg)
        .child(text.into())
}

/// 带标题、说明和设置行的紧凑面板。
#[derive(IntoElement)]
pub(crate) struct SettingsSection {
    title: SharedString,
    description: Option<SharedString>,
    show_header: bool,
    rows: Vec<SettingsRow>,
    title_rounding_rows: usize,
}

impl SettingsSection {
    pub(crate) fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            show_header: true,
            rows: Vec::new(),
            title_rounding_rows: 0,
        }
    }

    pub(crate) fn description(mut self, text: impl Into<SharedString>) -> Self {
        self.description = Some(text.into());
        self
    }

    /// General 页的来源实现直接连续渲染卡片；保留同一行构建 API，隐藏额外标题和说明。
    pub(crate) fn without_header(mut self) -> Self {
        self.show_header = false;
        self
    }

    pub(crate) fn row(mut self, mut row: SettingsRow) -> Self {
        row.title_rounding_ordinal = self.title_rounding_rows;
        if row.compensates_title_rounding() {
            self.title_rounding_rows += 1;
        }
        self.rows.push(row);
        self
    }
}

impl RenderOnce for SettingsSection {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let paint_offset = paint_snap_offset(window);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let card_color = settings_card_color(theme);
        let section_title = self.title.clone();
        let header = self.show_header.then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .id((
                            gpui::ElementId::from("settings-section-title"),
                            section_title.clone(),
                        ))
                        .text_size(ui_text_size(theme, UiTextSize::Lg))
                        .line_height(ui_text_size(theme, UiTextSize::Lg) * 1.5)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.fg)
                        // 小节标题是设置页的可访问导航锚点，不能只依赖视觉文本。
                        .role(Role::Heading)
                        .aria_label(section_title.clone())
                        .child(section_title.clone()),
                )
                .children(self.description.map(|text| {
                    div()
                        .id((
                            gpui::ElementId::from("settings-section-description"),
                            section_title.clone(),
                        ))
                        // Windows 的文字绘制基线比来源 Chromium 低半个逻辑像素；
                        // 相对定位只校准绘制位置，不改变小节和卡片的累计布局高度。
                        .relative()
                        .top(-paint_offset)
                        .role(Role::Label)
                        .aria_value(text.clone())
                        .text_size(ui_text_size(theme, UiTextSize::Base))
                        .line_height(px(24.0))
                        .text_color(colors.fg_muted)
                        .child(text)
                }))
                .into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .children(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w_full()
                    .border_1()
                    .border_color(colors.border)
                    .bg(card_color)
                    .rounded(px(12.0))
                    .overflow_hidden()
                    .children(self.rows.into_iter().enumerate().map(|(index, row)| {
                        div()
                            .when(index > 0, |view| {
                                view.border_t_1().border_color(colors.border)
                            })
                            .child(row)
                    })),
            )
    }
}

/// 设置面板中的一行，控件在宽度允许时与标签并排，窄窗口下自然换行。
#[derive(IntoElement)]
pub(crate) struct SettingsRow {
    title: SharedString,
    description: Option<SharedString>,
    /// 需要占据整行的补充控件，例如来源设置中的宽输入框。
    detail: Option<AnyElement>,
    control: Option<AnyElement>,
    control_layout: SettingsControlLayout,
    /// 同一卡片内普通说明行的顺序，用于分配来源分数行高的累计物理像素误差。
    title_rounding_ordinal: usize,
}

#[derive(Clone, Copy, Default)]
enum SettingsControlLayout {
    #[default]
    Default,
    Wide,
}

impl SettingsRow {
    pub(crate) fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            detail: None,
            control: None,
            control_layout: SettingsControlLayout::Default,
            title_rounding_ordinal: 0,
        }
    }

    pub(crate) fn description(mut self, text: impl Into<SharedString>) -> Self {
        self.description = Some(text.into());
        self
    }

    /// 将补充控件放在标题和说明下方，保持来源设置行的输入区域布局。
    pub(crate) fn detail(mut self, detail: impl IntoElement) -> Self {
        self.detail = Some(detail.into_any_element());
        self
    }

    pub(crate) fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// 文本区域或复合控件使用更宽的固定控制列，避免被普通设置行压缩到不可用。
    pub(crate) fn wide(mut self) -> Self {
        self.control_layout = SettingsControlLayout::Wide;
        self
    }

    fn compensates_title_rounding(&self) -> bool {
        self.description.is_some()
            && self.detail.is_none()
            && matches!(self.control_layout, SettingsControlLayout::Default)
    }
}

impl RenderOnce for SettingsRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let paint_offset = paint_snap_offset(window);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let control_width = match self.control_layout {
            SettingsControlLayout::Default => 192.0,
            SettingsControlLayout::Wide => 280.0,
        };
        let title_text_size = ui_text_size(theme, UiTextSize::Base);
        let title_line_height = title_text_size * 1.625;
        // Text 元素在物理像素上 snap 行高，DPR2 的 22.75px 会变为 22.5px；
        // padding 同样会先 snap，不能用两侧各 0.125px 的补偿保留分数。
        // 按卡片累计边界把误差分配到底部：默认行距交替 151/152 物理像素，
        // 标题和说明仍自然换行，复杂控件主导高度的行不参与这项补偿。
        let title_line_height_compensation = if self.compensates_title_rounding() {
            let error_in_device_pixels =
                f32::from(title_line_height - window.pixel_snap(title_line_height))
                    * window.scale_factor();
            let ordinal = self.title_rounding_ordinal as f32;
            px(((ordinal + 1.0) * error_in_device_pixels).floor()
                - (ordinal * error_in_device_pixels).floor())
                / window.scale_factor()
        } else {
            px(0.0)
        };
        let row_title = self.title.clone();
        let main = div()
            .flex()
            .min_w_0()
            .items_center()
            .flex_wrap()
            .gap_x_4()
            .gap_y(px(16.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id((
                                gpui::ElementId::from("settings-row-label"),
                                self.title.clone(),
                            ))
                            .role(Role::Label)
                            // AccessKit 将 Label 的 value 映射为 Windows UIA Name。
                            .aria_value(self.title.clone())
                            .text_size(title_text_size)
                            .line_height(title_line_height)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.fg)
                            .child(self.title),
                    )
                    .children(self.description.map(|text| {
                        div()
                            .id((gpui::ElementId::from("settings-row-description"), row_title))
                            .mt_1()
                            .relative()
                            .top(-paint_offset)
                            // 行说明是状态和诊断文案；显式作为 Label 的 value 暴露，
                            // 避免 UIA 只读取标题而丢失原因和约束。
                            .role(Role::Label)
                            .aria_value(text.clone())
                            .text_size(ui_text_size(theme, UiTextSize::Base))
                            .line_height(px(24.0))
                            .text_color(colors.fg_muted)
                            .child(text)
                    })),
            )
            .children(self.control.map(|control| {
                div()
                    .flex_shrink_1()
                    .min_w_0()
                    .w(px(control_width))
                    .max_w_full()
                    .flex()
                    .justify_end()
                    // 来源居中控件的半像素 snap 与 GPUI 不同；
                    // 保留行高，在当前 Windows DPI192 下补齐一物理像素。
                    .relative()
                    .top(paint_offset)
                    .child(control)
            }));

        div()
            .flex()
            .flex_col()
            .min_w_0()
            .px_4()
            .pt(px(12.0))
            .pb(px(12.0) + title_line_height_compensation)
            .child(main)
            .when_some(self.detail, |row, detail| {
                row.child(div().mt_3().w_full().max_w_full().child(detail))
            })
    }
}
