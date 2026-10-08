//! ZCode 3.14.3 的桌面视觉令牌；固定来源为 29628c9。
//!
//! 颜色字面量只在此处定义，组件仍通过 Ely Theme 和语义角色取色。
//! GPUI 使用逻辑像素，不能把高 DPI 截图的物理像素直接作为布局尺寸。

use ely_gpui_component::theme::{Mode, Palette, Theme};
use gpui::{App, BoxShadow, Font, FontFallbacks, Hsla, Pixels, SharedString, font, px, rgb};
use std::sync::LazyLock;

const DEFAULT_CJK_FALLBACK_FAMILIES: [&str; 2] = ["Microsoft YaHei UI", "Microsoft YaHei"];

/// CSS Backgrounds 将 blur 半径定义为 Gaussian 标准差的两倍；固定 GPUI
/// shader 直接把 `BoxShadow.blur_radius` 当作标准差，因此这里保留来源
/// CSS 数值并在适配边界完成一次转换。
fn css_blur_radius_to_gpui_sigma(blur_radius: Pixels) -> Pixels {
    blur_radius * 0.5
}

/// 来源输入壳的 Tailwind `shadow-xl/5`：两层阴影都使用主题 shadow 的 5% alpha。
/// 按逻辑像素绘制，由 GPUI 处理 DPI；Ely 的浮层阴影比这个工作台输入壳更深。
pub(crate) fn composer_shadow(colors: &Palette) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.0), px(20.0), colors.shadow.alpha(0.05))
            .blur_radius(css_blur_radius_to_gpui_sigma(px(25.0)))
            .spread_radius(px(-5.0)),
        BoxShadow::new(px(0.0), px(8.0), colors.shadow.alpha(0.05))
            .blur_radius(css_blur_radius_to_gpui_sigma(px(10.0)))
            .spread_radius(px(-6.0)),
    ]
}

/// Composer 外壳需要不透明底色，避免 GPUI 的阴影与半透明 surface 在同一节点内叠加。
/// 这里保留主题语义，并复现 `surface` 覆盖 `bg` 后的实际画布颜色。
pub(crate) fn composer_surface_background(colors: &Palette) -> Hsla {
    colors.bg.blend(colors.surface).alpha(1.0)
}

/// GPUI 接收单个真实字体族名称；Windows 上的等宽默认字体固定为 Consolas，
/// 中文由显式 fallback 承载，避免把 CSS 的 `monospace` 或 `.SystemUIFont`
/// 当作等宽字体解析。
pub const DEFAULT_MONO_FONT_FAMILY: &str = if cfg!(target_os = "windows") {
    "Consolas"
} else {
    "JetBrains Mono"
};

/// 返回系统 UI 字体的 GPUI 名称；设置文件仍保存用户看到的“系统默认”。
pub fn default_ui_font_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "Segoe UI"
    } else {
        ".SystemUIFont"
    }
}

fn cjk_fallbacks() -> FontFallbacks {
    // 字体 fallback 在每帧都会使用；共享不可变表，避免重复分配字符串和 Vec。
    static FALLBACKS: LazyLock<FontFallbacks> = LazyLock::new(|| {
        FontFallbacks::from_fonts(
            DEFAULT_CJK_FALLBACK_FAMILIES
                .iter()
                .map(|family| (*family).to_owned())
                .collect(),
        )
    });
    FALLBACKS.clone()
}

/// 构造界面字体。只有系统默认字体使用固定的中文 fallback；显式自定义字体
/// 必须保持原有族名和自身 fallback 语义。
pub fn ui_font(family: impl Into<SharedString>) -> Font {
    let family = family.into();
    let mut resolved = font(family.clone());
    if matches!(family.as_str(), "系统默认" | ".SystemUIFont" | "Segoe UI") {
        resolved.fallbacks = Some(cjk_fallbacks());
    }
    resolved
}

/// 构造终端、代码和工具详情使用的等宽字体。默认 Windows 字体是 Consolas，
/// 不使用 `.SystemUIFont`，这样中文输出不会回落到比例 UI 字体。
pub fn mono_font(family: impl Into<SharedString>) -> Font {
    let family = family.into();
    let mut resolved = font(family.clone());
    if family.as_str() == DEFAULT_MONO_FONT_FAMILY {
        resolved.fallbacks = Some(cjk_fallbacks());
    }
    resolved
}

/// 构造代码内容使用的字体；代码字体设置与界面字体、终端字体相互独立。
pub fn code_font(theme: &Theme) -> Font {
    mono_font(theme.code.font_family.clone())
}

/// 返回代码内容的固定逻辑像素字号，不叠加界面字号缩放。
pub fn code_text_size(theme: &Theme) -> Pixels {
    px(theme.code.font_size_px)
}

/// 来源设置页的页面标题为 30px；仍随用户确认的界面字号比例缩放。
pub fn page_title_size(theme: &ely_gpui_component::theme::Theme) -> Pixels {
    px(30.0 * theme.font_scale)
}

/// 来源设置卡片使用 bg-card：浅色为白色，深色为 #2b2b2b。
/// Ely 的 sunken 在当前固定 palette 中与该值一致；卡片不能改用半透明 surface。
pub fn settings_card_color(theme: &Theme) -> Hsla {
    theme.colors.sunken
}

/// 代码预览的内容面板按目标模式取固定 ZCode palette，不能复用活动模式或 Ely 默认暖色。
pub(crate) fn code_preview_palette(mode: Mode) -> Palette {
    zcode_palette(mode)
}

pub const SIDEBAR_WIDTH: f32 = 264.0;
pub const DESKTOP_INSET: f32 = 4.0;
/// 来源 WorkspaceHeader 的 h-12 同时定义拖拽区与会话顶部占位。
pub const TITLEBAR_HEIGHT: f32 = 48.0;
pub const CONTENT_MAX_WIDTH: f32 = 896.0;
pub const COMPOSER_MAX_WIDTH: f32 = 896.0;
pub const EMPTY_COMPOSER_MAX_WIDTH: f32 = 672.0;
/// 来源 Windows Chromium 的稳定滚动槽占 15 个逻辑像素；消息与 dock 共用留白。
pub const CONVERSATION_SCROLL_GUTTER: f32 = if cfg!(target_os = "windows") {
    15.0
} else {
    0.0
};

/// 来源 container query 以会话列本身的宽度判断断点，不以整个窗口判断。
/// 普通聊天和输入区共用此结果，避免侧栏或状态面板变化时两列错位。
pub fn conversation_content_width(available_width: f32) -> Pixels {
    let width = if available_width >= 1280.0 {
        (available_width - 384.0).min(1152.0)
    } else if available_width >= 864.0 {
        (available_width - 96.0).min(COMPOSER_MAX_WIDTH)
    } else {
        available_width
    };
    px(width.max(0.0))
}

/// Shell 中的背景角色不能合并：Windows 画布、聊天正文和输入卡片并非同一颜色。
#[derive(Clone, Copy)]
pub struct ShellColors {
    pub canvas: Hsla,
    pub sidebar: Hsla,
    pub content: Hsla,
    pub header: Hsla,
    pub panel: Hsla,
    pub input: Hsla,
}

impl ShellColors {
    pub fn from_theme(theme: &Theme) -> Self {
        let dark = theme.mode() == Mode::Dark;
        let canvas = color(if dark { 0x2b2b2b } else { 0xececee });
        Self {
            canvas,
            // 固定来源的 Windows 侧栏透明，实际继承窗口画布。
            sidebar: if cfg!(target_os = "windows") {
                canvas
            } else {
                color(if dark { 0x161616 } else { 0xf0f0f0 })
            },
            content: theme.colors.bg,
            header: color(if dark { 0x202020 } else { 0xffffff }),
            panel: color(if dark { 0x202020 } else { 0xffffff }),
            input: theme.colors.sunken,
        }
    }
}

/// 与来源 text-ui-* 对应；字号设置沿用唯一 Theme.font_scale。
#[derive(Clone, Copy)]
pub enum UiTextSize {
    Xs,
    Sm,
    Caption,
    Base,
    Lg,
    Xl,
}

pub fn ui_text_size(theme: &Theme, size: UiTextSize) -> Pixels {
    let offset = match size {
        UiTextSize::Xs => -4.0,
        UiTextSize::Sm => -2.0,
        UiTextSize::Caption => -1.0,
        UiTextSize::Base => 0.0,
        UiTextSize::Lg => 2.0,
        UiTextSize::Xl => 4.0,
    };
    // 来源 CSS 是基准字号加偏移，不能再给小字应用独立缩放。
    px(14.0 * theme.font_scale + offset)
}

fn color(hex: u32) -> Hsla {
    rgb(hex).into()
}

/// 装配精确的 ZAI light/dark palette，启动时取消 Ely 的过渡帧。
pub fn install_palettes(cx: &mut App) {
    Theme::set_palette(Mode::Light, Some(zcode_palette(Mode::Light)), cx);
    Theme::set_palette(Mode::Dark, Some(zcode_palette(Mode::Dark)), cx);
}

fn zcode_palette(mode: Mode) -> Palette {
    let dark = mode == Mode::Dark;
    let mut palette = if dark {
        Palette::dark(false)
    } else {
        Palette::light(false)
    };
    let tone = color(if dark { 0xffffff } else { 0x0d0d0d });
    palette.bg = color(if dark { 0x161616 } else { 0xf8f8f8 });
    // 来源 bg-surface 是半透明叠层，输入和浮层才是实色；合并三者会抬高卡片对比度。
    palette.surface = tone.alpha(if dark { 0.05 } else { 0.03 });
    palette.sunken = color(if dark { 0x2b2b2b } else { 0xffffff });
    palette.overlay = palette.sunken;
    palette.hover = tone.alpha(0.05);
    // Chromium 将深色 10% alpha 量化为 26/255；复现同一选中态色阶，
    // 避免在 #2b2b2b 侧栏上输出 #404040 而非来源的 #414141。
    palette.active = tone.alpha(if dark { 26.0 / 255.0 } else { 0.05 });
    // Chromium 将 CSS 的 10% alpha 量化成 8 位的 26/255；保留该精度，
    // 避免 GPUI 的浮点混合让深色卡片边界和滚动条各暗一个 RGB 等级。
    palette.border = tone.alpha(26.0 / 255.0);
    palette.border_strong = tone.alpha(0.15);
    palette.fg = color(if dark { 0xd4d4d4 } else { 0x262626 });
    palette.fg_muted = palette.fg.alpha(0.6);
    palette.fg_subtle = palette.fg.alpha(if dark { 0.3 } else { 0.4 });
    palette.fg_disabled = palette.fg_subtle;
    palette.accent = color(if dark { 0xffffff } else { 0x000000 });
    palette.accent_hover = palette.accent.alpha(0.85);
    palette.on_accent = color(if dark { 0x000000 } else { 0xffffff });
    palette.link = color(if dark { 0x4099ff } else { 0x0b7fff });
    palette.focus = palette.border_strong;
    palette.selection = palette.link.alpha(if dark { 0.28 } else { 0.22 });
    palette.success = color(if dark { 0x46bf72 } else { 0x1e8a3e });
    palette.warning = color(if dark { 0xff8a30 } else { 0xe07b00 });
    palette.danger = color(if dark { 0xff5c5c } else { 0xe03131 });
    palette.info = palette.link;
    palette.success_subtle = palette.success.alpha(0.12);
    palette.warning_subtle = palette.warning.alpha(0.12);
    palette.danger_subtle = palette.danger.alpha(0.12);
    palette.info_subtle = color(if dark { 0x001d3d } else { 0xebf4ff });
    palette.backdrop = color(0x000000).alpha(0.4);
    palette.glass = palette.surface;
    palette.shadow = color(0x000000);
    palette.tooltip_bg = color(if dark { 0x2b2b2b } else { 0xf0f0f0 });
    palette.tooltip_fg = color(if dark { 0xf8f8f8 } else { 0x0d0d0d });
    palette.chart = [
        palette.info,
        palette.success,
        color(if dark { 0x7b5ce5 } else { 0x9e77ed }),
        palette.danger,
        palette.warning,
        color(if dark { 0x42c8c8 } else { 0x0aa7a7 }),
        palette.link,
        palette.fg_muted,
    ];
    palette.ansi = if dark {
        [
            0x363636, 0xff5c5c, 0x46bf72, 0xff8a30, 0x4099ff, 0x7b5ce5, 0x42c8c8, 0xadadad,
            0x747474, 0xff9999, 0x87d9a4, 0xffb26b, 0x80beff, 0xa888f2, 0x8ee5e5, 0xf8f8f8,
        ]
    } else {
        [
            0x5c5c5c, 0xe03131, 0x1e8a3e, 0xe07b00, 0x0b7fff, 0x9e77ed, 0x0aa7a7, 0xadadad,
            0x888888, 0xe03131, 0x1e8a3e, 0xe07b00, 0x0066dd, 0x9e77ed, 0x0aa7a7, 0x0d0d0d,
        ]
    }
    .map(color);
    palette
}
