use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, Styled, Window, div,
};

use crate::{
    buttons::{Button, ButtonVariant},
    forms::{Choice, Run},
    motion::ProgressBar,
    navigation::Wizard,
    theme::{ActiveTheme, ControlSize, TextSize},
};

type OnStep = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A step on the way in: its key, its title and a line on what it is for.
#[derive(Clone, Debug, PartialEq)]
pub struct OnboardingStep {
    pub key: SharedString,
    pub title: SharedString,
    pub body: SharedString,
}

/// A first-run flow on `navigation::Wizard`: "Step 2 of 4" over a bar in as many parts, the step's title, line and content, then Back and Next; Next is Finish on the last step. With `on_skip`, Skip for now leaves early.
#[derive(IntoElement)]
pub struct OnboardingWizard {
    id: ElementId,
    steps: Vec<OnboardingStep>,
    current: usize,
    content: Option<AnyElement>,
    ready: bool,
    on_step: Option<OnStep>,
    on_finish: Option<Run>,
    on_skip: Option<Run>,
}

impl OnboardingWizard {
    pub fn new(
        id: impl Into<ElementId>,
        steps: impl IntoIterator<Item = OnboardingStep>,
        current: usize,
    ) -> Self {
        let steps: Vec<OnboardingStep> = steps.into_iter().collect();
        assert!(
            steps.len() >= 2,
            "an onboarding wizard needs two steps, got {}",
            steps.len()
        );
        assert!(
            current < steps.len(),
            "step {current} is past the last of {}",
            steps.len()
        );
        Self {
            id: id.into(),
            steps,
            current,
            content: None,
            ready: true,
            on_step: None,
            on_finish: None,
            on_skip: None,
        }
    }

    /// The current step's content, under its title and line.
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// Whether Next may run; the step's fields decide.
    pub fn ready(mut self, ready: bool) -> Self {
        self.ready = ready;
        self
    }

    /// Runs with the step Back or Next leads to.
    pub fn on_step(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_step = Some(Rc::new(handler));
        self
    }

    pub fn on_finish(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_finish = Some(Rc::new(handler));
        self
    }

    /// Shows Skip for now.
    pub fn on_skip(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_skip = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for OnboardingWizard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self.id;
        let on_step = self
            .on_step
            .unwrap_or_else(|| panic!("onboarding wizard {id:?} has no on_step"));
        let on_finish = self
            .on_finish
            .unwrap_or_else(|| panic!("onboarding wizard {id:?} has no on_finish"));
        let theme = cx.theme();
        let (current, count) = (self.current, self.steps.len());
        let reached = current + 1;
        let place = format!("Step {reached} of {count}");
        let skip = self.on_skip.map(|run| {
            Button::new((id.clone(), "skip"), "Skip for now")
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .on_click(move |_, window, cx| {
                    log::info!("onboarding wizard: skipped at step {current}");
                    run(window, cx)
                })
        });
        let head = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .debug_selector({
                        let place = place.clone();
                        move || format!("onboarding-{place}")
                    })
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(theme.colors.fg_muted)
                    .child(place),
            )
            .children(skip.map(|skip| div().flex_none().child(skip)));
        let step = &self.steps[current];
        let words = div().flex().child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(theme.text_size(TextSize::Lg))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(step.title.clone()),
                )
                .child(
                    div()
                        .mt_1()
                        .text_color(theme.colors.fg_muted)
                        .child(step.body.clone()),
                ),
        );
        let wizard = Wizard::new(
            (id.clone(), "wizard"),
            self.steps
                .iter()
                .map(|step| Choice::new(step.key.clone(), step.title.clone())),
            current,
        )
        .headless()
        .ready(self.ready)
        .content(
            div()
                .flex()
                .flex_col()
                .gap_5()
                .child(words)
                .children(self.content),
        )
        .on_step(move |to, window, cx| on_step(to, window, cx))
        .on_finish(move |window, cx| on_finish(window, cx));
        div()
            .flex()
            .flex_col()
            .gap_6()
            .child(
                div().flex().flex_col().gap_2().child(head).child(
                    ProgressBar::new((id.clone(), "progress"), reached as f32 / count as f32)
                        .segments(count),
                ),
            )
            .child(wizard)
    }
}
