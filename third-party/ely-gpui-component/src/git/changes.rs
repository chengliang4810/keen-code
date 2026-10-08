use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*,
};

use super::badges::GitStatusBadge;
use crate::{
    buttons::{Button, ButtonVariant, IconButton},
    forms::{Checkbox, Input, Submit, TextInput},
    lists::GitStatus,
    primitives::IconName,
    theme::{ActiveTheme, ControlSize, Radius, TextSize},
    typography::{Ellipsis, tabular},
};

/// A changed file: its path, its status, and the lines it added and removed.
#[derive(Clone, Debug, PartialEq)]
pub struct Changed {
    pub path: SharedString,
    pub status: GitStatus,
    pub added: usize,
    pub removed: usize,
}

/// What a changes list asks for a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeAction {
    Stage,
    Unstage,
    Discard,
    Open,
}

type OnAction = Rc<dyn Fn(&SharedString, ChangeAction, &mut Window, &mut App)>;
type OnAll = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// Files changed in the work tree, the staged apart: each with its status, its folder and what it changed; open, discard, stage or unstage it, or all at once.
#[derive(IntoElement)]
pub struct ChangesList {
    id: ElementId,
    staged: Vec<Changed>,
    unstaged: Vec<Changed>,
    on_action: Option<OnAction>,
    on_all: Option<OnAll>,
}

impl ChangesList {
    pub fn new(
        id: impl Into<ElementId>,
        staged: impl IntoIterator<Item = Changed>,
        unstaged: impl IntoIterator<Item = Changed>,
    ) -> Self {
        Self {
            id: id.into(),
            staged: staged.into_iter().collect(),
            unstaged: unstaged.into_iter().collect(),
            on_action: None,
            on_all: None,
        }
    }

    pub fn on_action(
        mut self,
        handler: impl Fn(&SharedString, ChangeAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }

    /// Called with true to stage every change, false to unstage every staged one.
    pub fn on_all(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_all = Some(Rc::new(handler));
        self
    }
}

impl ChangesList {
    fn row(&self, file: &Changed, staged: bool, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let (folder, name) = match file.path.rsplit_once('/') {
            Some((folder, name)) => (folder.to_string(), name.to_string()),
            None => (String::new(), file.path.to_string()),
        };
        let group = SharedString::from(format!("change-{staged}-{}", file.path));
        let action = |key: &str, icon, words: &'static str, act: ChangeAction| {
            let (on_action, path) = (self.on_action.clone(), file.path.clone());
            IconButton::new(
                (self.id.clone(), format!("{key}-{staged}-{}", file.path)),
                icon,
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .tooltip(words)
            .when_some(on_action, |button, on_action| {
                button.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    on_action(&path, act, window, cx)
                })
            })
        };
        let (open, path) = (self.on_action.clone(), file.path.clone());
        div()
            .id((self.id.clone(), format!("row-{staged}-{}", file.path)))
            .group(group.clone())
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded(theme.radius(Radius::Sm))
            .cursor_pointer()
            .hover(|row| row.bg(colors.hover))
            .when_some(open, |row, open| {
                row.on_click(move |_, window, cx| open(&path, ChangeAction::Open, window, cx))
            })
            .child(GitStatusBadge::new(
                (self.id.clone(), format!("status-{staged}-{}", file.path)),
                file.status,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .text_color(colors.fg)
                            .when(file.status == GitStatus::Deleted, |name| {
                                name.line_through()
                            })
                            .child(name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(colors.fg_subtle)
                            .child(Ellipsis::new(folder)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .opacity(0.)
                    .group_hover(group, |actions| actions.opacity(1.))
                    .child(action(
                        "open",
                        IconName::FileText,
                        "Open file",
                        ChangeAction::Open,
                    ))
                    .when(!staged, |actions| {
                        actions.child(action(
                            "discard",
                            IconName::RotateCcw,
                            "Discard changes",
                            ChangeAction::Discard,
                        ))
                    })
                    .child(if staged {
                        action("unstage", IconName::Minus, "Unstage", ChangeAction::Unstage)
                    } else {
                        action("stage", IconName::Plus, "Stage", ChangeAction::Stage)
                    }),
            )
            .child(
                tabular(div().flex_none().text_color(colors.fg_subtle))
                    .child(format!("+{} −{}", file.added, file.removed)),
            )
            .into_any_element()
    }

    fn section(
        &self,
        title: &'static str,
        files: &[Changed],
        staged: bool,
        cx: &App,
    ) -> Option<AnyElement> {
        if files.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let on_all = self.on_all.clone();
        let (icon, words) = if staged {
            (IconName::Minus, "Unstage all")
        } else {
            (IconName::Plus, "Stage all")
        };
        Some(
            div()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_muted)
                        .child(div().flex_1().font_weight(FontWeight::MEDIUM).child(title))
                        .child(tabular(div()).child(files.len().to_string()))
                        .child(
                            IconButton::new((self.id.clone(), format!("all-{staged}")), icon)
                                .variant(ButtonVariant::Ghost)
                                .size(ControlSize::Sm)
                                .tooltip(words)
                                .when_some(on_all, |button, on_all| {
                                    button
                                        .on_click(move |_, window, cx| on_all(!staged, window, cx))
                                }),
                        ),
                )
                .children(files.iter().map(|file| self.row(file, staged, cx)))
                .into_any_element(),
        )
    }
}

impl RenderOnce for ChangesList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let sections = [
            self.section("Staged changes", &self.staged, true, cx),
            self.section("Changes", &self.unstaged, false, cx),
        ];
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(theme.text_size(TextSize::Sm))
            .children(sections.into_iter().flatten())
    }
}

/// Characters a subject reads well within, and the most it should take.
const SUBJECT: (usize, usize) = (50, 72);

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

/// A commit message: its subject counted against fifty, a body, and the commit button, which waits for a subject and something staged. Cmd-Enter commits; amend takes the last commit's place.
#[derive(IntoElement)]
pub struct CommitInput {
    id: ElementId,
    subject: Entity<TextInput>,
    body: Entity<TextInput>,
    branch: SharedString,
    staged: usize,
    amend: bool,
    on_amend: Option<OnAll>,
    on_commit: Option<Run>,
}

impl CommitInput {
    pub fn new(
        id: impl Into<ElementId>,
        subject: &Entity<TextInput>,
        body: &Entity<TextInput>,
        branch: impl Into<SharedString>,
        staged: usize,
    ) -> Self {
        Self {
            id: id.into(),
            subject: subject.clone(),
            body: body.clone(),
            branch: branch.into(),
            staged,
            amend: false,
            on_amend: None,
            on_commit: None,
        }
    }

    pub fn amend(mut self, amend: bool) -> Self {
        self.amend = amend;
        self
    }

    pub fn on_amend(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_amend = Some(Rc::new(handler));
        self
    }

    pub fn on_commit(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_commit = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CommitInput {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let length = self.subject.read(cx).text().chars().count();
        let ready = length > 0 && (self.staged > 0 || self.amend);
        let count = match length {
            length if length > SUBJECT.1 => colors.danger,
            length if length > SUBJECT.0 => colors.warning,
            _ => colors.fg_subtle,
        };
        let commit = self.on_commit.filter(|_| ready);
        let submit = commit.clone();
        let label = if self.amend {
            format!("Amend {}", self.branch)
        } else {
            format!("Commit to {}", self.branch)
        };
        let amend =
            Checkbox::new((self.id.clone(), "amend"), self.amend).label("Amend last commit");
        let amend = match self.on_amend {
            Some(on_amend) => amend.on_change(move |on, window, cx| on_amend(on, window, cx)),
            None => amend,
        };
        div()
            .id(self.id.clone())
            .flex()
            .flex_col()
            .gap_2()
            .text_size(theme.text_size(TextSize::Sm))
            .capture_action(move |_: &Submit, window, cx| {
                if let Some(submit) = &submit {
                    cx.stop_propagation();
                    submit(window, cx)
                }
            })
            .child(
                Input::new(&self.subject).suffix(
                    tabular(div().text_color(count)).child(format!("{length}/{}", SUBJECT.0)),
                ),
            )
            .child(Input::new(&self.body))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(amend))
                    .child(
                        Button::new((self.id.clone(), "commit"), label)
                            .variant(ButtonVariant::Primary)
                            .size(ControlSize::Sm)
                            .disabled(!ready)
                            .when_some(commit, |button, commit| {
                                button.on_click(move |_, window, cx| commit(window, cx))
                            }),
                    ),
            )
    }
}
