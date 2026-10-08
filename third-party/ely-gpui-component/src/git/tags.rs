use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div,
};

use super::refs::{Run, action, listed};
use crate::{
    primitives::IconName,
    theme::{ActiveTheme, TextSize},
};

type OnStash = Rc<dyn Fn(usize, StashAction, &mut Window, &mut App)>;

/// A tag: its name, its message when annotated, the commit it names, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct GitTag {
    pub name: SharedString,
    pub message: Option<SharedString>,
    pub commit: SharedString,
    pub when: SharedString,
}

/// Tags newest first, each with its message and the commit it names.
#[derive(IntoElement)]
pub struct TagList {
    id: ElementId,
    tags: Vec<GitTag>,
}

impl TagList {
    pub fn new(id: impl Into<ElementId>, tags: impl IntoIterator<Item = GitTag>) -> Self {
        Self {
            id: id.into(),
            tags: tags.into_iter().collect(),
        }
    }
}

impl RenderOnce for TagList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let rows: Vec<AnyElement> = self
            .tags
            .iter()
            .map(|tag| {
                let short: String = tag.commit.chars().take(7).collect();
                listed(
                    (self.id.clone(), format!("tag-{}", tag.name)).into(),
                    IconName::Tag,
                    div()
                        .text_color(colors.fg)
                        .child(tag.name.clone())
                        .into_any_element(),
                    tag.message.clone().unwrap_or_default(),
                    vec![short.into(), tag.when.clone()],
                    Vec::new(),
                    cx,
                )
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .text_size(theme.text_size(TextSize::Sm))
            .children(rows)
    }
}

/// A stash: its message, the branch it came from, and when.
#[derive(Clone, Debug, PartialEq)]
pub struct Stash {
    pub message: SharedString,
    pub branch: SharedString,
    pub when: SharedString,
}

/// What a stash list does to a stash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StashAction {
    Apply,
    Pop,
    Drop,
}

/// Stashes newest first, as `stash@{n}`: apply one, pop it, or drop it.
#[derive(IntoElement)]
pub struct StashList {
    id: ElementId,
    stashes: Vec<Stash>,
    on_action: Option<OnStash>,
}

impl StashList {
    pub fn new(id: impl Into<ElementId>, stashes: impl IntoIterator<Item = Stash>) -> Self {
        Self {
            id: id.into(),
            stashes: stashes.into_iter().collect(),
            on_action: None,
        }
    }

    pub fn on_action(
        mut self,
        handler: impl Fn(usize, StashAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for StashList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let rows: Vec<AnyElement> = self
            .stashes
            .iter()
            .enumerate()
            .map(|(ix, stash)| {
                let act = |key: &str, icon, words: &'static str, what: StashAction| {
                    let run = self.on_action.clone().map(|on_action| {
                        Rc::new(move |window: &mut Window, cx: &mut App| {
                            on_action(ix, what, window, cx)
                        }) as Run
                    });
                    action(
                        (self.id.clone(), format!("{key}-{ix}")).into(),
                        icon,
                        words,
                        run,
                    )
                };
                listed(
                    (self.id.clone(), format!("stash-{ix}")).into(),
                    IconName::Archive,
                    div()
                        .font_family(theme.mono_family.clone())
                        .text_color(colors.fg)
                        .child(format!("stash@{{{ix}}}"))
                        .into_any_element(),
                    format!("{} · on {}", stash.message, stash.branch).into(),
                    vec![stash.when.clone()],
                    vec![
                        act("apply", IconName::Download, "Apply", StashAction::Apply),
                        act("pop", IconName::ArrowUp, "Pop", StashAction::Pop),
                        act("drop", IconName::Trash2, "Drop", StashAction::Drop),
                    ],
                    cx,
                )
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .text_size(theme.text_size(TextSize::Sm))
            .children(rows)
    }
}
