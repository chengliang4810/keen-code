use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, WindowHandle, div, prelude::*,
};

use super::{Hosted, open_hosted};
use crate::{
    buttons::{Button, ButtonVariant, IconButton},
    layout::ScrollArea,
    motion::ProgressBar,
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
    typography::{Caption, Label, Title},
};

/// Where an update stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UpdateState {
    Available,
    /// 0 to 1.
    Downloading(f32),
    Ready,
}

type Action = Rc<dyn Fn(&mut Window, &mut App)>;

/// Handlers for the update flow. Unset ones hide their button.
#[derive(Clone, Default)]
struct Actions {
    download: Option<Action>,
    restart: Option<Action>,
    later: Option<Action>,
}

fn progress_line(state: UpdateState) -> Option<AnyElement> {
    match state {
        UpdateState::Downloading(value) => {
            Some(ProgressBar::new("update-progress", value).into_any_element())
        }
        _ => None,
    }
}

fn buttons(state: UpdateState, actions: &Actions, size: ControlSize) -> Vec<Button> {
    let wire = |button: Button, action: &Option<Action>| {
        action.clone().map(|action| {
            button
                .size(size)
                .on_click(move |_, window, cx| action(window, cx))
        })
    };
    let later = wire(
        Button::new("update-later", "Later").variant(ButtonVariant::Ghost),
        &actions.later,
    );
    let main = match state {
        UpdateState::Available => wire(
            Button::new("update-download", "Download").primary(),
            &actions.download,
        ),
        UpdateState::Downloading(_) => None,
        UpdateState::Ready => wire(
            Button::new("update-restart", "Restart to update").primary(),
            &actions.restart,
        ),
    };
    later.into_iter().chain(main).collect()
}

macro_rules! update_actions {
    () => {
        /// Starts the download while `Available`.
        pub fn on_download(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
            self.actions.download = Some(Rc::new(handler));
            self
        }

        /// Relaunches once `Ready`.
        pub fn on_restart(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
            self.actions.restart = Some(Rc::new(handler));
            self
        }

        pub fn on_later(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
            self.actions.later = Some(Rc::new(handler));
            self
        }
    };
}

/// A slim bar that offers a new version, shows the download, then asks to restart.
#[derive(IntoElement, Clone)]
pub struct UpdateBanner {
    id: ElementId,
    version: SharedString,
    state: UpdateState,
    actions: Actions,
}

impl UpdateBanner {
    pub fn new(
        id: impl Into<ElementId>,
        version: impl Into<SharedString>,
        state: UpdateState,
    ) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            state,
            actions: Actions::default(),
        }
    }

    update_actions!();
}

impl RenderOnce for UpdateBanner {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let message = match self.state {
            UpdateState::Available => format!("Version {} is available.", self.version),
            UpdateState::Downloading(value) => {
                format!("Downloading {}… {}%", self.version, (value * 100.0).round())
            }
            UpdateState::Ready => format!("Version {} is ready.", self.version),
        };
        let dismiss = self.actions.later.clone();
        div()
            .id(self.id)
            .relative()
            .flex()
            .items_center()
            .gap_3()
            .py_2()
            .pl_4()
            .pr_2()
            .bg(theme.colors.surface)
            .border_b_1()
            .border_color(theme.colors.border)
            .text_size(theme.text_size(TextSize::Sm))
            .child(
                Icon::new(IconName::Download)
                    .size(IconSize::Sm)
                    .color(theme.colors.fg_muted),
            )
            .child(div().flex_1().child(message))
            .children(buttons(
                self.state,
                &Actions {
                    later: None,
                    ..self.actions.clone()
                },
                ControlSize::Sm,
            ))
            .when_some(dismiss, |banner, dismiss| {
                banner.child(
                    IconButton::new("update-dismiss", IconName::X)
                        .size(ControlSize::Sm)
                        .on_click(move |_, window, cx| dismiss(window, cx)),
                )
            })
            .when_some(progress_line(self.state), |banner, line| {
                banner.child(div().absolute().left_0().right_0().bottom_0().child(line))
            })
    }
}

/// The update window: what changed, and what to do.
#[derive(IntoElement, Clone)]
pub struct UpdateDialog {
    name: SharedString,
    current: SharedString,
    version: SharedString,
    notes: Vec<SharedString>,
    state: UpdateState,
    actions: Actions,
}

impl UpdateDialog {
    pub fn new(
        name: impl Into<SharedString>,
        current: impl Into<SharedString>,
        version: impl Into<SharedString>,
        state: UpdateState,
    ) -> Self {
        Self {
            name: name.into(),
            current: current.into(),
            version: version.into(),
            notes: Vec::new(),
            state,
            actions: Actions::default(),
        }
    }

    /// One line of release notes.
    pub fn note(mut self, line: impl Into<SharedString>) -> Self {
        self.notes.push(line.into());
        self
    }

    pub fn state(mut self, state: UpdateState) -> Self {
        self.state = state;
        self
    }

    update_actions!();

    pub fn open(self, cx: &mut App) -> anyhow::Result<WindowHandle<Hosted<UpdateDialog>>> {
        let size = cx.theme().dialog_window();
        open_hosted("Software Update", size, self, cx)
    }
}

impl RenderOnce for UpdateDialog {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = &theme.colors;
        let headline = match self.state {
            UpdateState::Ready => format!("{} {} is ready", self.name, self.version),
            _ => format!("{} {} is available", self.name, self.version),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_4()
            .px_8()
            .pb_6()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Title::new(headline))
                    .child(Caption::new(format!("You have {}.", self.current))),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(theme.radius(Radius::Lg))
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.sunken)
                    .child(
                        ScrollArea::new("update-notes").size_full().child(
                            div().flex().flex_col().gap_2().p_4().children(
                                self.notes.into_iter().map(|line| {
                                    div()
                                        .flex()
                                        .gap_2()
                                        .child(Label::new("·"))
                                        .child(Label::new(line))
                                }),
                            ),
                        ),
                    ),
            )
            .when_some(progress_line(self.state), |dialog, line| dialog.child(line))
            .child(div().flex().justify_end().gap_2().children(buttons(
                self.state,
                &self.actions,
                ControlSize::Md,
            )))
    }
}
