use gpui::{
    App, ElementId, Entity, InteractiveElement, IntoElement, ParentElement, RenderOnce, Styled,
    Window, div,
};
use jiff::civil::DateTime;

use super::{
    super::{Input, TextInput},
    CronRule, zoned_now,
};
use crate::{
    buttons::{Button, ButtonVariant},
    theme::{ActiveTheme, ControlSize, TextSize},
};

const PRESETS: [(&str, &str); 5] = [
    ("Hourly", "0 * * * *"),
    ("Daily at 9:00", "0 9 * * *"),
    ("Weekdays at 9:00", "0 9 * * 1-5"),
    ("Mondays at 9:00", "0 9 * * 1"),
    ("Monthly", "0 0 1 * *"),
];

/// A five-field cron rule, read back in words with its next runs.
#[derive(IntoElement)]
pub struct CronEditor {
    id: ElementId,
    state: Entity<TextInput>,
    now: Option<DateTime>,
    runs: usize,
}

impl CronEditor {
    pub fn new(id: impl Into<ElementId>, state: &Entity<TextInput>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            now: None,
            runs: 3,
        }
    }

    /// The moment next runs count from. Defaults to the system's clock.
    pub fn now(mut self, at: DateTime) -> Self {
        self.now = Some(at);
        self
    }

    /// How many next runs to list.
    pub fn runs(mut self, count: usize) -> Self {
        assert!(count > 0, "list at least one run");
        self.runs = count;
        self
    }
}

impl RenderOnce for CronEditor {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let text = self.state.read(cx).text().trim().to_string();
        let rule = (!text.is_empty()).then(|| CronRule::parse(&text));
        let now = self.now.unwrap_or_else(|| zoned_now().datetime());
        let theme = cx.theme();
        let colors = &theme.colors;
        let presets = PRESETS.iter().enumerate().map(|(ix, (label, preset))| {
            let state = self.state.clone();
            Button::new(("preset", ix), *label)
                .variant(ButtonVariant::Outline)
                .size(ControlSize::Sm)
                .on_click(move |_, _, cx| {
                    log::info!("cron editor: preset {preset}");
                    state.update(cx, |input, cx| input.set_text(*preset, cx));
                })
        });
        let (words, color, runs) = match &rule {
            None => (
                "Five fields: minute, hour, day of month, month, weekday.".to_string(),
                colors.fg_subtle,
                None,
            ),
            Some(Err(problem)) => (problem.clone(), colors.danger, None),
            Some(Ok(rule)) => {
                let next = rule.next(now, self.runs);
                let runs = if next.is_empty() {
                    "No run in the next five years.".to_string()
                } else {
                    let times: Vec<String> = next
                        .iter()
                        .map(|at| at.strftime("%a %b %-d, %H:%M").to_string())
                        .collect();
                    format!("Next: {}", times.join(" \u{00b7} "))
                };
                (rule.describe(), colors.fg, Some(runs))
            }
        };
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().flex_wrap().gap_2().children(presets))
            .child(
                div()
                    .font_family(theme.mono_family.clone())
                    .child(Input::new(&self.state).invalid(matches!(rule, Some(Err(_))))),
            )
            .child(
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(color)
                    .child(words),
            )
            .children(runs.map(|runs| {
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.fg_muted)
                    .child(runs)
            }))
    }
}
