use super::section::{
    settings_input_style, settings_select_indicator, settings_switch_style, settings_unit_label,
};
use super::{SettingsCommandHandler, SettingsRow, SettingsSection, contracts::*};
use crate::native_ui::style::{UiTextSize, code_preview_palette, ui_text_size};
use ely_gpui_component::{
    buttons::Button,
    forms::{Choice, Combobox, Input, InputEvent, RadioGroup, Switch, TextInput},
    primitives::{Icon, IconName},
    theme::{
        ActiveTheme, CodeSyntaxTheme as RuntimeCodeSyntaxTheme, ControlSize, IconSize, Mode,
        Palette, Syntax, Theme,
    },
};
use gpui::{
    AnyElement, App, AppContext, Context, ElementId, Entity, FontStyle, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled, StyledText,
    Subscription, Window, div, px, relative,
};
use std::ops::Range;

const FONT_SIZE_MIN: u8 = 11;
const FONT_SIZE_MAX: u8 = 20;
const CODE_FONT_SIZE_MIN: u8 = 12;
const CODE_FONT_SIZE_MAX: u8 = 20;

const CODE_PREVIEW: &str = r##"const themePreview: ThemeConfig = {
  surface: "sidebar",
  accent: "#339CFF",
  contrast: 45,
};"##;

fn normalize_font_family(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "系统默认".to_owned()
    } else {
        value.to_owned()
    }
}

fn appearance_value(value: AppearanceMode) -> &'static str {
    match value {
        AppearanceMode::System => "system",
        AppearanceMode::Light => "light",
        AppearanceMode::Dark => "dark",
    }
}

fn appearance_from_value(value: &str) -> Option<AppearanceMode> {
    match value {
        "system" => Some(AppearanceMode::System),
        "light" => Some(AppearanceMode::Light),
        "dark" => Some(AppearanceMode::Dark),
        _ => None,
    }
}

fn appearance_icon(value: AppearanceMode) -> IconName {
    match value {
        AppearanceMode::System => IconName::Monitor,
        AppearanceMode::Light => IconName::Sun,
        AppearanceMode::Dark => IconName::Moon,
    }
}

fn code_theme_value(value: CodeSyntaxTheme) -> &'static str {
    match value {
        CodeSyntaxTheme::GitHubLight => "github-light",
        CodeSyntaxTheme::GitHubDark => "github-dark",
        CodeSyntaxTheme::Ely => "ely",
        CodeSyntaxTheme::Quiet => "quiet",
        CodeSyntaxTheme::Paper => "paper",
    }
}

fn code_theme_from_value(value: &str) -> Option<CodeSyntaxTheme> {
    match value {
        "github-light" => Some(CodeSyntaxTheme::GitHubLight),
        "github-dark" => Some(CodeSyntaxTheme::GitHubDark),
        "ely" => Some(CodeSyntaxTheme::Ely),
        "quiet" => Some(CodeSyntaxTheme::Quiet),
        "paper" => Some(CodeSyntaxTheme::Paper),
        _ => None,
    }
}

fn code_theme_label(value: CodeSyntaxTheme) -> &'static str {
    match value {
        CodeSyntaxTheme::GitHubLight => "GitHub Light",
        CodeSyntaxTheme::GitHubDark => "GitHub Dark",
        CodeSyntaxTheme::Ely => "Ely",
        CodeSyntaxTheme::Quiet => "Quiet",
        CodeSyntaxTheme::Paper => "Paper",
    }
}

fn code_theme_choices() -> [Choice; 5] {
    [
        Choice::new("github-light", "GitHub Light"),
        Choice::new("github-dark", "GitHub Dark"),
        Choice::new("ely", "Ely"),
        Choice::new("quiet", "Quiet"),
        Choice::new("paper", "Paper"),
    ]
}

fn code_patch(code: CodeAppearanceSettings) -> GeneralPatch {
    GeneralPatch {
        code: Some(code),
        ..GeneralPatch::default()
    }
}

fn runtime_code_theme(value: CodeSyntaxTheme) -> RuntimeCodeSyntaxTheme {
    match value {
        CodeSyntaxTheme::GitHubLight => RuntimeCodeSyntaxTheme::GitHubLight,
        CodeSyntaxTheme::GitHubDark => RuntimeCodeSyntaxTheme::GitHubDark,
        CodeSyntaxTheme::Ely => RuntimeCodeSyntaxTheme::Ely,
        CodeSyntaxTheme::Quiet => RuntimeCodeSyntaxTheme::Quiet,
        CodeSyntaxTheme::Paper => RuntimeCodeSyntaxTheme::Paper,
    }
}

fn preview_highlights(text: &str, syntax: &Syntax) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut spans = Vec::new();
    let mut add_token = |token: &str, color| {
        spans.extend(text.match_indices(token).map(|(start, _)| {
            (
                start..start + token.len(),
                HighlightStyle {
                    color: Some(color),
                    ..Default::default()
                },
            )
        }));
    };

    add_token("const", syntax.keyword);
    add_token("themePreview", syntax.variable);
    add_token("ThemeConfig", syntax.type_name);
    add_token("surface", syntax.property);
    add_token("accent", syntax.property);
    add_token("contrast", syntax.property);
    add_token("45", syntax.number);
    for punctuation in ["{", "}", ":", ";", ","] {
        add_token(punctuation, syntax.punctuation);
    }

    let mut string_search_start = 0;
    while let Some(relative_start) = text[string_search_start..].find('"') {
        let start = string_search_start + relative_start;
        let Some(relative_end) = text[start + 1..].find('"') else {
            break;
        };
        let end = start + relative_end + 2;
        spans.push((
            start..end,
            HighlightStyle {
                color: Some(syntax.string),
                ..Default::default()
            },
        ));
        string_search_start = end;
    }
    if let Some(start) = text.find("//") {
        spans.push((
            start..text[start..].find('\n').unwrap_or(text.len() - start),
            HighlightStyle {
                color: Some(syntax.comment),
                font_style: Some(FontStyle::Italic),
                ..Default::default()
            },
        ));
    }
    spans
}

fn code_preview(
    mode: Mode,
    settings: &CodeAppearanceSettings,
    theme: &Theme,
    preview_palette: &Palette,
    is_active: bool,
    id: impl Into<ElementId>,
) -> AnyElement {
    let active_palette = &theme.colors;
    let header_text_size = ui_text_size(theme, UiTextSize::Base);
    let badge_text_size = ui_text_size(theme, UiTextSize::Xs);
    let header_line_height = px(20.0 * theme.font_scale);
    let badge_line_height = px(16.0 * theme.font_scale);
    let id = id.into();
    let selected = match mode {
        Mode::Light => settings.light_theme,
        Mode::Dark => settings.dark_theme,
    };
    let syntax = runtime_code_theme(selected).syntax(mode);
    let code_view = div()
        .id((id.clone(), "code"))
        .flex_1()
        .min_w_0()
        .font_family(settings.font_family.clone())
        .text_size(px(settings.font_size_px))
        .line_height(relative(1.55))
        .text_color(syntax.variable)
        .child(
            StyledText::new(CODE_PREVIEW)
                .with_highlights(preview_highlights(CODE_PREVIEW, &syntax)),
        );
    let code_view = if settings.wrap_long_lines {
        code_view.whitespace_normal()
    } else {
        code_view.overflow_x_scroll().whitespace_nowrap()
    };
    let line_numbers = div()
        .flex()
        .flex_col()
        .w(px(20.0))
        .flex_none()
        .text_color(preview_palette.fg_muted)
        .font_family(settings.font_family.clone())
        .text_size(px(settings.font_size_px))
        .line_height(relative(1.55))
        .children((1..=CODE_PREVIEW.lines().count()).map(|line| {
            div()
                .text_align(gpui::TextAlign::Right)
                .child(format!("{line}"))
        }));
    let body = div()
        .flex()
        .items_start()
        .gap_3()
        .w_full()
        .min_w_0()
        .bg(preview_palette.bg)
        .text_color(preview_palette.fg)
        .px_3()
        .py_2();
    let body = if settings.show_line_numbers {
        body.child(line_numbers).child(code_view)
    } else {
        body.child(code_view)
    };
    let mode_label = match mode {
        Mode::Light => "浅色预览",
        Mode::Dark => "深色预览",
    };
    let badge_label = if is_active {
        "当前生效"
    } else {
        match mode {
            Mode::Light => "浅色",
            Mode::Dark => "深色",
        }
    };
    let badge_background = if is_active {
        active_palette.active
    } else {
        active_palette.surface
    };
    let badge_foreground = if is_active {
        active_palette.fg
    } else {
        active_palette.fg_muted
    };
    div()
        .id(id)
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .rounded(px(12.0))
        .border_1()
        .border_color(active_palette.border)
        .bg(active_palette.sunken)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(active_palette.border)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_size(header_text_size)
                                .line_height(header_line_height)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(active_palette.fg)
                                .child(mode_label),
                        )
                        .child(
                            div()
                                .text_size(header_text_size)
                                .line_height(header_line_height)
                                .text_color(active_palette.fg_muted)
                                .child(code_theme_label(selected)),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .rounded(px(6.0))
                        .px(px(10.0))
                        .py(px(4.0))
                        .text_size(badge_text_size)
                        .line_height(badge_line_height)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(badge_foreground)
                        .bg(badge_background)
                        .child(badge_label),
                ),
        )
        .child(body)
        .into_any_element()
}

fn density_value_name(value: UiDensity) -> &'static str {
    match value {
        UiDensity::Compact => "compact",
        UiDensity::Standard => "standard",
        UiDensity::Comfortable => "comfortable",
    }
}

fn density_from_value(value: &str) -> Option<UiDensity> {
    match value {
        "compact" => Some(UiDensity::Compact),
        "standard" => Some(UiDensity::Standard),
        "comfortable" => Some(UiDensity::Comfortable),
        _ => None,
    }
}

fn parse_font_size(text: &str) -> Option<f32> {
    let value = text.trim().parse::<u8>().ok()?;
    (FONT_SIZE_MIN..=FONT_SIZE_MAX)
        .contains(&value)
        .then_some(f32::from(value))
}

fn format_font_size(value: f32) -> String {
    format!("{value:.0}")
}

/// 字体大小保留普通文本输入语义；提交或失焦时才确认，无效草稿恢复到已确认值。
struct FontSizeDraft {
    input: Entity<TextInput>,
    committed: f32,
    focused: bool,
    dispatch: SettingsCommandHandler,
    _subscription: Subscription,
}

impl FontSizeDraft {
    fn new(
        initial: f32,
        dispatch: SettingsCommandHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(window, cx)
                .text_align(gpui::TextAlign::Right)
                .placeholder(format!("{FONT_SIZE_MIN}-{FONT_SIZE_MAX}"))
                .filter(|character| character.is_ascii_digit());
            input.set_text(format_font_size(initial), cx);
            input
        });
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |draft, input, event, window, cx| match *event {
                    InputEvent::Focus => draft.focused = true,
                    InputEvent::Blur | InputEvent::Submit => {
                        let next =
                            parse_font_size(input.read(cx).text()).unwrap_or(draft.committed);
                        let changed = (next - draft.committed).abs() > f32::EPSILON;
                        draft.committed = next;
                        draft.focused = matches!(*event, InputEvent::Submit);
                        input.update(cx, |input, cx| {
                            input.set_text(format_font_size(next), cx);
                        });
                        if changed {
                            (draft.dispatch)(
                                SettingsCommand::SaveAppearance(GeneralPatch {
                                    font_size_px: Some(next),
                                    ..GeneralPatch::default()
                                }),
                                window,
                                cx,
                            );
                        }
                    }
                    InputEvent::Changed => {}
                },
            );
        Self {
            input,
            committed: initial,
            focused: false,
            dispatch,
            _subscription: subscription,
        }
    }

    fn sync(&mut self, confirmed: f32, dispatch: SettingsCommandHandler, cx: &mut Context<Self>) {
        self.dispatch = dispatch;
        if self.focused {
            return;
        }
        let expected = format_font_size(confirmed);
        if (self.committed - confirmed).abs() <= f32::EPSILON
            && self.input.read(cx).text() == expected
        {
            return;
        }
        self.committed = confirmed;
        self.input
            .update(cx, |input, cx| input.set_text(expected, cx));
    }
}

fn parse_code_font_size(text: &str) -> Option<f32> {
    let value = text.trim().parse::<u8>().ok()?;
    (CODE_FONT_SIZE_MIN..=CODE_FONT_SIZE_MAX)
        .contains(&value)
        .then_some(f32::from(value))
}

/// 代码字号沿用普通文本输入的提交语义；无效草稿失焦时恢复到完整已确认代码设置。
struct CodeFontSizeDraft {
    input: Entity<TextInput>,
    code: CodeAppearanceSettings,
    focused: bool,
    dispatch: SettingsCommandHandler,
    _subscription: Subscription,
}

impl CodeFontSizeDraft {
    fn new(
        initial: &CodeAppearanceSettings,
        dispatch: SettingsCommandHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = TextInput::new(window, cx)
                .text_align(gpui::TextAlign::Right)
                .label("代码字号")
                .placeholder(format!("{CODE_FONT_SIZE_MIN}-{CODE_FONT_SIZE_MAX}"))
                .filter(|character| character.is_ascii_digit());
            input.set_text(format_font_size(initial.font_size_px), cx);
            input
        });
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |draft, input, event, window, cx| match *event {
                    InputEvent::Focus => draft.focused = true,
                    InputEvent::Blur | InputEvent::Submit => {
                        let next = parse_code_font_size(input.read(cx).text())
                            .unwrap_or(draft.code.font_size_px);
                        let changed = (next - draft.code.font_size_px).abs() > f32::EPSILON;
                        let mut code = draft.code.clone();
                        code.font_size_px = next;
                        draft.code = code.clone();
                        draft.focused = matches!(*event, InputEvent::Submit);
                        input.update(cx, |input, cx| {
                            input.set_text(format_font_size(next), cx);
                        });
                        if changed {
                            (draft.dispatch)(
                                SettingsCommand::SaveAppearance(code_patch(code)),
                                window,
                                cx,
                            );
                        }
                    }
                    InputEvent::Changed => {}
                },
            );
        Self {
            input,
            code: initial.clone(),
            focused: false,
            dispatch,
            _subscription: subscription,
        }
    }

    fn sync(
        &mut self,
        confirmed: &CodeAppearanceSettings,
        dispatch: SettingsCommandHandler,
        cx: &mut Context<Self>,
    ) {
        self.dispatch = dispatch;
        self.code = confirmed.clone();
        if self.focused {
            return;
        }
        let expected = format_font_size(confirmed.font_size_px);
        if self.input.read(cx).text() == expected {
            return;
        }
        self.input
            .update(cx, |input, cx| input.set_text(expected, cx));
    }
}

pub(super) fn render(
    current: &GeneralSettings,
    dispatch: SettingsCommandHandler,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let theme_input = window.use_keyed_state("settings-appearance-theme", cx, |window, cx| {
        TextInput::new(window, cx).label("界面主题")
    });
    let theme_dispatch = dispatch.clone();
    let theme_picker = Combobox::new(
        "settings-appearance-theme",
        &theme_input,
        [
            Choice::new("light", "浅色").icon(IconName::Sun),
            Choice::new("dark", "深色").icon(IconName::Moon),
            Choice::new("system", "跟随系统").icon(IconName::Monitor),
        ],
    )
    .selected(appearance_value(current.appearance))
    .size(ControlSize::Lg)
    .input_style(settings_input_style(cx.theme(), true, true))
    .indicator(settings_select_indicator(cx.theme()))
    .prefix(Icon::new(appearance_icon(current.appearance)).size(IconSize::Sm))
    .on_change(move |value, window, cx| {
        let Some(next) = appearance_from_value(value.as_ref()) else {
            return;
        };
        theme_dispatch(
            SettingsCommand::SaveAppearance(GeneralPatch {
                appearance: Some(next),
                ..GeneralPatch::default()
            }),
            window,
            cx,
        );
    });
    let theme_control = div().w(px(192.0)).max_w_full().child(theme_picker);

    let font = window.use_keyed_state("settings-appearance-font", cx, |window, cx| {
        let mut field = TextInput::new(window, cx).placeholder("字体族");
        field.set_text(current.font_family.clone(), cx);
        field
    });
    let font_input = font.clone();
    let font_dispatch = dispatch.clone();
    let font_change = move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
        let value = normalize_font_family(font_input.read(cx).text());
        font_dispatch(
            SettingsCommand::SaveAppearance(GeneralPatch {
                font_family: Some(value),
                ..GeneralPatch::default()
            }),
            window,
            cx,
        );
    };

    let font_size = window.use_keyed_state("settings-appearance-font-size", cx, |window, cx| {
        FontSizeDraft::new(current.font_size_px, dispatch.clone(), window, cx)
    });
    font_size.update(cx, |draft, cx| {
        draft.sync(current.font_size_px, dispatch.clone(), cx)
    });
    let font_size_input = font_size.read(cx).input.clone();
    let font_size_control = div().w(px(112.0)).max_w_full().child(
        Input::new(&font_size_input)
            .style(settings_input_style(cx.theme(), false, false))
            .suffix(settings_unit_label(cx.theme())),
    );

    let light_theme_input =
        window.use_keyed_state("settings-appearance-code-light-theme", cx, |window, cx| {
            TextInput::new(window, cx).label("浅色代码主题")
        });
    let light_theme_dispatch = dispatch.clone();
    let light_theme_code = current.code.clone();
    let light_theme = Combobox::new(
        "settings-appearance-code-light-theme",
        &light_theme_input,
        code_theme_choices(),
    )
    .selected(code_theme_value(current.code.light_theme))
    .size(ControlSize::Lg)
    .input_style(settings_input_style(cx.theme(), true, true))
    .indicator(settings_select_indicator(cx.theme()))
    .on_change(move |value, window, cx| {
        let Some(next) = code_theme_from_value(value.as_ref()) else {
            return;
        };
        let mut code = light_theme_code.clone();
        code.light_theme = next;
        light_theme_dispatch(
            SettingsCommand::SaveAppearance(code_patch(code)),
            window,
            cx,
        );
    });
    let light_theme_control = div().w(px(192.0)).max_w_full().child(light_theme);

    let dark_theme_input =
        window.use_keyed_state("settings-appearance-code-dark-theme", cx, |window, cx| {
            TextInput::new(window, cx).label("深色代码主题")
        });
    let dark_theme_dispatch = dispatch.clone();
    let dark_theme_code = current.code.clone();
    let dark_theme = Combobox::new(
        "settings-appearance-code-dark-theme",
        &dark_theme_input,
        code_theme_choices(),
    )
    .selected(code_theme_value(current.code.dark_theme))
    .size(ControlSize::Lg)
    .input_style(settings_input_style(cx.theme(), true, true))
    .indicator(settings_select_indicator(cx.theme()))
    .on_change(move |value, window, cx| {
        let Some(next) = code_theme_from_value(value.as_ref()) else {
            return;
        };
        let mut code = dark_theme_code.clone();
        code.dark_theme = next;
        dark_theme_dispatch(
            SettingsCommand::SaveAppearance(code_patch(code)),
            window,
            cx,
        );
    });
    let dark_theme_control = div().w(px(192.0)).max_w_full().child(dark_theme);

    let initial_code = current.code.clone();
    let code_font_size_dispatch = dispatch.clone();
    let code_font_size = window.use_keyed_state(
        "settings-appearance-code-font-size",
        cx,
        move |window, cx| {
            CodeFontSizeDraft::new(&initial_code, code_font_size_dispatch.clone(), window, cx)
        },
    );
    code_font_size.update(cx, |draft, cx| {
        draft.sync(&current.code, dispatch.clone(), cx)
    });
    let code_font_size_input = code_font_size.read(cx).input.clone();
    let code_font_size_control = div().w(px(112.0)).max_w_full().child(
        Input::new(&code_font_size_input)
            .style(settings_input_style(cx.theme(), false, false))
            .suffix(settings_unit_label(cx.theme())),
    );

    let line_numbers_dispatch = dispatch.clone();
    let line_numbers_code = current.code.clone();
    let show_line_numbers = Switch::new(
        "settings-appearance-code-line-numbers",
        current.code.show_line_numbers,
    )
    .style(settings_switch_style(cx.theme()))
    .aria_label("显示行号")
    .on_change(move |next, window, cx| {
        let mut code = line_numbers_code.clone();
        code.show_line_numbers = next;
        line_numbers_dispatch(
            SettingsCommand::SaveAppearance(code_patch(code)),
            window,
            cx,
        );
    });

    let wrap_lines_dispatch = dispatch.clone();
    let wrap_lines_code = current.code.clone();
    let wrap_long_lines = Switch::new(
        "settings-appearance-code-wrap-lines",
        current.code.wrap_long_lines,
    )
    .style(settings_switch_style(cx.theme()))
    .aria_label("长行自动换行")
    .on_change(move |next, window, cx| {
        let mut code = wrap_lines_code.clone();
        code.wrap_long_lines = next;
        wrap_lines_dispatch(
            SettingsCommand::SaveAppearance(code_patch(code)),
            window,
            cx,
        );
    });

    let active_theme = cx.theme();
    let active_mode = active_theme.mode();
    let active_palette = active_theme.palette();
    let preview_header_text_size = ui_text_size(active_theme, UiTextSize::Base);
    let preview_heading_line_height = px(20.0 * active_theme.font_scale);
    let preview_description_line_height = px(24.0 * active_theme.font_scale);
    let light_preview_palette = code_preview_palette(Mode::Light);
    let dark_preview_palette = code_preview_palette(Mode::Dark);
    let code_preview_control = div()
        .flex()
        .flex_wrap()
        .gap_4()
        .w_full()
        .min_w_0()
        // 来源在宽屏下使用两列；每张卡保留最小可读宽度，窄窗口时自然换行。
        .child(
            div()
                .flex_1()
                .min_w(px(280.0))
                .max_w_full()
                .child(code_preview(
                    Mode::Light,
                    &current.code,
                    active_theme,
                    &light_preview_palette,
                    active_mode == Mode::Light,
                    "settings-appearance-code-preview-light",
                )),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(280.0))
                .max_w_full()
                .child(code_preview(
                    Mode::Dark,
                    &current.code,
                    active_theme,
                    &dark_preview_palette,
                    active_mode == Mode::Dark,
                    "settings-appearance-code-preview-dark",
                )),
        );
    let code_preview_section = div()
        .flex()
        .flex_col()
        .gap_4()
        .w_full()
        .min_w_0()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(preview_header_text_size)
                        .line_height(preview_heading_line_height)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(active_palette.fg)
                        .child("代码预览"),
                )
                .child(
                    div()
                        .text_size(preview_header_text_size)
                        .line_height(preview_description_line_height)
                        .text_color(active_palette.fg_muted)
                        .child(
                            "同时预览浅色与深色代码主题，当前界面使用的主题会标记为“当前生效”。",
                        ),
                ),
        )
        .child(code_preview_control);

    let density_dispatch = dispatch.clone();
    let density = RadioGroup::new(
        "settings-appearance-density",
        [
            Choice::new("compact", "紧凑"),
            Choice::new("standard", "标准"),
            Choice::new("comfortable", "舒适"),
        ],
    )
    .selected(density_value_name(current.density))
    .horizontal()
    .on_change(move |value, window, cx| {
        let Some(next) = density_from_value(value.as_ref()) else {
            return;
        };
        density_dispatch(
            SettingsCommand::SaveAppearance(GeneralPatch {
                density: Some(next),
                ..GeneralPatch::default()
            }),
            window,
            cx,
        );
    });

    let motion_dispatch = dispatch;
    let reduced_motion = Switch::new("settings-appearance-reduced-motion", current.reduced_motion)
        .style(settings_switch_style(cx.theme()))
        .aria_label("减少动效")
        .on_change(move |next, window, cx| {
            motion_dispatch(
                SettingsCommand::SaveAppearance(GeneralPatch {
                    reduced_motion: Some(next),
                    ..GeneralPatch::default()
                }),
                window,
                cx,
            )
        });

    div()
        .flex()
        .flex_col()
        .gap_8()
        .child(
            SettingsSection::new("界面设置")
                .description("设置应用主题和界面文字大小。")
                .row(
                    SettingsRow::new("界面主题")
                        .description("选择浅色、深色或跟随系统主题。")
                        .control(theme_control),
                )
                .row(
                    SettingsRow::new("界面字号")
                        .description("调整应用界面的文字大小，图标和布局尺寸不受影响。")
                        .control(font_size_control),
                ),
        )
        .child(
            SettingsSection::new("代码设置")
                .description("设置代码内容的主题、字号和显示方式，不受界面字号影响。")
                .row(
                    SettingsRow::new("浅色代码主题")
                        .description("浅色界面下代码内容使用的高亮主题。")
                        .control(light_theme_control),
                )
                .row(
                    SettingsRow::new("深色代码主题")
                        .description("深色界面下代码内容使用的高亮主题。")
                        .control(dark_theme_control),
                )
                .row(
                    SettingsRow::new("显示行号")
                        .description("在代码内容和差异视图中显示行号。")
                        .control(show_line_numbers),
                )
                .row(
                    SettingsRow::new("长行自动换行")
                        .description("代码内容过长时自动换行。")
                        .control(wrap_long_lines),
                )
                .row(
                    SettingsRow::new("代码字号")
                        .description("调整代码块、文件预览和差异视图的默认字号。")
                        .control(code_font_size_control),
                ),
        )
        .child(code_preview_section)
        .child(
            SettingsSection::new("其他界面设置")
                .description("调整界面字体、密度和动效。")
                .row(
                    SettingsRow::new("界面字体")
                        .wide()
                        .description("输入系统中可用的字体族名称；留空时使用系统默认字体。")
                        .control(
                            div()
                                .flex()
                                .w_full()
                                .min_w_0()
                                .max_w_full()
                                .gap_2()
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&font)
                                            .size(ControlSize::Lg)
                                            .style(settings_input_style(cx.theme(), true, false)),
                                    ),
                                )
                                .child(
                                    Button::new("settings-appearance-font-apply", "应用")
                                        .size(ControlSize::Lg)
                                        .on_click(font_change),
                                ),
                        ),
                )
                .row(SettingsRow::new("界面密度").wide().control(density))
                .row(SettingsRow::new("减少动效").control(reduced_motion)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        appearance_from_value, appearance_value, code_theme_from_value, code_theme_value,
        density_from_value, density_value_name,
    };
    use crate::native_ui::settings::contracts::{AppearanceMode, CodeSyntaxTheme, UiDensity};

    #[test]
    fn appearance_values_round_trip() {
        for value in [
            AppearanceMode::System,
            AppearanceMode::Light,
            AppearanceMode::Dark,
        ] {
            assert_eq!(appearance_from_value(appearance_value(value)), Some(value));
        }
        for value in [
            UiDensity::Compact,
            UiDensity::Standard,
            UiDensity::Comfortable,
        ] {
            assert_eq!(density_from_value(density_value_name(value)), Some(value));
        }
        for value in [
            CodeSyntaxTheme::GitHubLight,
            CodeSyntaxTheme::GitHubDark,
            CodeSyntaxTheme::Ely,
            CodeSyntaxTheme::Quiet,
            CodeSyntaxTheme::Paper,
        ] {
            assert_eq!(code_theme_from_value(code_theme_value(value)), Some(value));
        }
    }
}
