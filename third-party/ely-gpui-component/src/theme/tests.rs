use std::time::Duration;

use gpui::{TestAppContext, hsla};

use super::{ActiveTheme, CodeRenderSettings, CodeSyntaxTheme, Mode, Palette, Syntax, Theme};

/// An owner's palette serves its own mode; the other keeps Ely's, and none brings Ely's back.
#[gpui::test]
fn an_owners_palette_serves_its_mode_alone(cx: &mut TestAppContext) {
    cx.update(Theme::init);
    let mut own = Palette::light(false);
    own.accent = hsla(0.6, 0.5, 0.5, 1.0);
    own.syntax = CodeRenderSettings::default().syntax(Mode::Light);
    cx.update(|cx| Theme::set_palette(Mode::Light, Some(own.clone()), cx));
    assert_eq!(cx.read(|cx| cx.theme().palette()), own);
    cx.update(|cx| Theme::set_mode(Mode::Dark, cx));
    let mut expected_dark = Palette::dark(false);
    expected_dark.syntax = CodeRenderSettings::default().syntax(Mode::Dark);
    assert_eq!(cx.read(|cx| cx.theme().palette()), expected_dark);
    cx.update(|cx| {
        Theme::set_mode(Mode::Light, cx);
        Theme::set_palette(Mode::Light, None, cx);
    });
    let mut expected_light = Palette::light(false);
    expected_light.syntax = CodeRenderSettings::default().syntax(Mode::Light);
    assert_eq!(cx.read(|cx| cx.theme().palette()), expected_light);
}

/// A mode set at once keeps its palette through fades.
#[gpui::test]
fn a_mode_set_at_once_skips_the_fade(cx: &mut TestAppContext) {
    cx.update(|cx| {
        Theme::init(cx);
        Theme::update(cx, |theme| theme.reduced_motion = true);
        Theme::set_mode(Mode::Dark, cx);
        Theme::set_mode_now(Mode::Light, cx);
    });
    let mut expected_light = Palette::light(false);
    expected_light.syntax = CodeRenderSettings::default().syntax(Mode::Light);
    assert_eq!(cx.read(|cx| cx.theme().colors.clone()), expected_light);
    std::thread::sleep(Duration::from_millis(2));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    assert!(!cx.read(|cx| cx.theme().is_dark()));
    assert_eq!(cx.read(|cx| cx.theme().colors.clone()), expected_light);
}

/// Every color of the palette and of code answers to its field's name, the quiet fills too.
#[test]
fn every_color_answers_to_its_name() {
    let mut palette = Palette::light(false);
    for (ix, name) in Palette::NAMES.iter().enumerate() {
        *palette.token_mut(name) = hsla(ix as f32 / 64.0, 0.5, 0.5, 1.0);
    }
    assert_eq!(Palette::NAMES.len(), 36, "every color of the palette");
    let at = |name: &str| {
        Palette::NAMES
            .iter()
            .position(|each| *each == name)
            .expect("a name")
    };
    assert_eq!(
        palette.success_subtle,
        hsla(at("success_subtle") as f32 / 64.0, 0.5, 0.5, 1.0)
    );
    assert_eq!(palette.ink, hsla(at("ink") as f32 / 64.0, 0.5, 0.5, 1.0));
    for name in Syntax::NAMES {
        *palette.syntax.token_mut(name) = hsla(0.4, 0.2, 0.3, 1.0);
    }
    assert_eq!(Syntax::NAMES.len(), 13, "every color of code");
    assert_eq!(palette.syntax.comment, hsla(0.4, 0.2, 0.3, 1.0));
}

#[test]
fn code_syntax_catalog_keeps_github_tokens_distinct_from_ely() {
    let themes = super::syntax_themes();
    assert_eq!(themes.len(), 5);
    assert_eq!(themes[0].name, "GitHub Light");
    assert_eq!(themes[1].name, "GitHub Dark");
    assert_eq!(themes[2].name, "Ely");
    assert_ne!(themes[0].light.keyword, themes[2].light.keyword);
    assert_ne!(themes[1].dark.keyword, themes[2].dark.keyword);
    assert_eq!(CodeSyntaxTheme::GitHubLight.name(), "GitHub Light");
}

#[gpui::test]
fn code_render_settings_update_active_and_custom_syntax(cx: &mut TestAppContext) {
    cx.update(Theme::init);
    let settings = CodeRenderSettings {
        light_theme: CodeSyntaxTheme::Paper,
        dark_theme: CodeSyntaxTheme::Quiet,
        ..CodeRenderSettings::default()
    };
    cx.update(|cx| {
        Theme::set_palette(Mode::Light, Some(Palette::light(false)), cx);
        Theme::set_code_render_settings(settings.clone(), cx);
    });
    assert_eq!(
        cx.read(|cx| cx.theme().colors.syntax.clone()),
        settings.syntax(Mode::Light)
    );
    assert_eq!(
        cx.read(|cx| cx.theme().palette().syntax),
        settings.syntax(Mode::Light)
    );
}

#[test]
#[should_panic(expected = "no palette color glow")]
fn an_unknown_color_fails() {
    Palette::light(false).token_mut("glow");
}

#[test]
fn glass_lets_the_blur_through_until_high_contrast() {
    for (plain, strong) in [
        (Palette::light(false), Palette::light(true)),
        (Palette::dark(false), Palette::dark(true)),
    ] {
        assert!(
            plain.glass.a > 0.0 && plain.glass.a < 1.0,
            "{:?}",
            plain.glass
        );
        assert_eq!(strong.glass, strong.bg);
    }
}
