use std::rc::Rc;

use gpui::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*,
};

use super::{badges::DiffStat, commits::Commit};
use crate::{
    theme::{ActiveTheme, Radius, TextSize},
    typography::Ellipsis,
};

type OnPick = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// A file's commits newest first on a line through them: subject, author and when, and what each changed; a press picks one.
#[derive(IntoElement)]
pub struct FileHistory {
    id: ElementId,
    commits: Vec<(Commit, (usize, usize))>,
    selected: Option<SharedString>,
    on_pick: Option<OnPick>,
}

impl FileHistory {
    /// Each commit with the lines it added and removed in the file.
    pub fn new(
        id: impl Into<ElementId>,
        commits: impl IntoIterator<Item = (Commit, (usize, usize))>,
    ) -> Self {
        Self {
            id: id.into(),
            commits: commits.into_iter().collect(),
            selected: None,
            on_pick: None,
        }
    }

    pub fn selected(mut self, id: impl Into<SharedString>) -> Self {
        self.selected = Some(id.into());
        self
    }

    pub fn on_pick(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for FileHistory {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let last = self.commits.len().saturating_sub(1);
        div()
            .flex()
            .flex_col()
            .text_size(theme.text_size(TextSize::Sm))
            .children(self.commits.into_iter().enumerate().map(
                |(ix, (commit, (added, removed)))| {
                    let selected = self.selected.as_ref() == Some(&commit.id);
                    let (pick, id) = (self.on_pick.clone(), commit.id.clone());
                    let short: String = commit.id.chars().take(7).collect();
                    div()
                        .id((self.id.clone(), format!("commit-{ix}")))
                        .flex()
                        .gap_3()
                        .px_2()
                        .rounded(theme.radius(Radius::Sm))
                        .when(selected, |row| row.bg(colors.active))
                        .when(!selected, |row| row.hover(|row| row.bg(colors.hover)))
                        .when_some(pick, |row, pick| {
                            row.cursor_pointer()
                                .on_click(move |_, window, cx| pick(&id, window, cx))
                        })
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(div().w_0().h_2().when(ix > 0, |line| {
                                    line.border_l_1().border_color(colors.border)
                                }))
                                .child(
                                    div()
                                        .size_2()
                                        .rounded_full()
                                        .border_1()
                                        .border_color(if selected {
                                            colors.focus
                                        } else {
                                            colors.border_strong
                                        })
                                        .bg(if ix == 0 { colors.focus } else { colors.bg }),
                                )
                                .child(div().w_0().flex_1().when(ix < last, |line| {
                                    line.border_l_1().border_color(colors.border)
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .py_1p5()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_color(colors.fg)
                                        .child(Ellipsis::new(commit.subject.clone())),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_size(theme.text_size(TextSize::Xs))
                                        .text_color(colors.fg_subtle)
                                        .child(commit.author.clone())
                                        .child(commit.when.clone())
                                        .child(
                                            div()
                                                .font_family(theme.mono_family.clone())
                                                .child(short),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .py_1p5()
                                .child(DiffStat::new(added, removed)),
                        )
                },
            ))
    }
}
