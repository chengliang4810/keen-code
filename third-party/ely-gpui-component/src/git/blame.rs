use std::{ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, StyledText, Window, div,
    prelude::*, relative,
};

use crate::{
    data_display::Avatar,
    editor::code_colors,
    theme::{ActiveTheme, AvatarSize, TextSize},
    typography::{Ellipsis, LEADING},
};

/// Who last changed lines: the commit, its author, when and its subject, and its age from 0, newest, to 1, oldest.
#[derive(Clone, Debug, PartialEq)]
pub struct Blame {
    pub commit: SharedString,
    pub author: SharedString,
    pub when: SharedString,
    pub subject: SharedString,
    pub age: f32,
}

/// Consecutive lines from one change, as the change's index and the lines.
pub(crate) fn runs(owners: &[usize]) -> Vec<(usize, Range<usize>)> {
    let mut out: Vec<(usize, Range<usize>)> = Vec::new();
    for (line, owner) in owners.iter().enumerate() {
        match out.last_mut() {
            Some((last, lines)) if last == owner => lines.end = line + 1,
            _ => out.push((*owner, line..line + 1)),
        }
    }
    out
}

/// Who last changed a line, set after its code in quiet words: the author, when, and the subject.
#[derive(IntoElement)]
pub struct GitBlameAnnotation {
    blame: Blame,
}

impl GitBlameAnnotation {
    pub fn new(blame: Blame) -> Self {
        Self { blame }
    }
}

impl RenderOnce for GitBlameAnnotation {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors.clone();
        div()
            .flex_none()
            .pl_6()
            .whitespace_nowrap()
            .text_color(colors.fg_subtle)
            .child(format!(
                "{}, {} · {}",
                self.blame.author, self.blame.when, self.blame.subject
            ))
    }
}

type OnCommit = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// Code with who last changed each run of lines beside it: author, when and subject on a run's first line, a bar that fades with age. The current line shows its annotation; a press on a run picks its commit.
#[derive(IntoElement)]
pub struct BlameView {
    id: ElementId,
    code: SharedString,
    owners: Vec<usize>,
    blames: Vec<Blame>,
    current: Option<usize>,
    on_commit: Option<OnCommit>,
}

impl BlameView {
    /// `owners` holds, for each line of `code`, its entry in `blames`.
    pub fn new(
        id: impl Into<ElementId>,
        code: impl Into<SharedString>,
        owners: impl IntoIterator<Item = usize>,
        blames: impl IntoIterator<Item = Blame>,
    ) -> Self {
        let code: SharedString = code.into();
        let owners: Vec<usize> = owners.into_iter().collect();
        let blames: Vec<Blame> = blames.into_iter().collect();
        assert_eq!(
            owners.len(),
            code.lines().count(),
            "every line has an owner"
        );
        assert!(
            owners.iter().all(|owner| *owner < blames.len()),
            "every owner is a blame"
        );
        Self {
            id: id.into(),
            code,
            owners,
            blames,
            current: None,
            on_commit: None,
        }
    }

    /// The line with the cursor, from zero.
    pub fn current(mut self, line: usize) -> Self {
        self.current = Some(line);
        self
    }

    pub fn on_commit(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_commit = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for BlameView {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let lines: Vec<&str> = self.code.lines().collect();
        let digits = lines.len().to_string().len();
        let blocks: Vec<AnyElement> = runs(&self.owners)
            .into_iter()
            .map(|(owner, span)| {
                let blame = &self.blames[owner];
                let fade = 1.0 - blame.age.clamp(0.0, 1.0) * 0.8;
                let pick = self.on_commit.clone();
                let commit = blame.commit.clone();
                let gutter = div()
                    .id((self.id.clone(), format!("run-{}", span.start)))
                    .flex_none()
                    .w_64()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_2()
                    .border_l_2()
                    .border_color(colors.accent.opacity(fade))
                    .font_family(theme.font_family.clone())
                    .text_size(theme.text_size(TextSize::Xs))
                    .when_some(pick, |gutter, pick| {
                        gutter
                            .cursor_pointer()
                            .hover(|gutter| gutter.bg(colors.hover))
                            .on_click(move |_, window, cx| pick(&commit, window, cx))
                    })
                    .child(
                        Avatar::new(
                            (self.id.clone(), format!("author-{}", span.start)),
                            blame.author.clone(),
                        )
                        .size(AvatarSize::Xs),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(colors.fg)
                                            .child(blame.author.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_color(colors.fg_subtle)
                                            .child(blame.when.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(colors.fg_muted)
                                    .child(Ellipsis::new(blame.subject.clone())),
                            ),
                    );
                let code = div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .line_height(relative(LEADING))
                    .flex()
                    .flex_col()
                    .children(span.clone().map(|line| {
                        let text = lines[line].to_string();
                        let styles = code_colors(&text, cx);
                        div()
                            .flex()
                            .gap_3()
                            .whitespace_nowrap()
                            .when(self.current == Some(line), |row| row.bg(colors.hover))
                            .child(
                                div()
                                    .flex_none()
                                    .w(theme.text_size(TextSize::Xs) * (digits as f32 * 0.62))
                                    .text_right()
                                    .text_color(colors.fg_subtle)
                                    .child((line + 1).to_string()),
                            )
                            .child(StyledText::new(text).with_highlights(styles))
                            .when(self.current == Some(line), |row| {
                                row.child(GitBlameAnnotation::new(blame.clone()))
                            })
                    }));
                div()
                    .flex()
                    .border_t_1()
                    .border_color(colors.border)
                    .child(gutter)
                    .child(code)
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .font_family(theme.mono_family.clone())
            .text_size(theme.text_size(TextSize::Xs))
            .text_color(colors.fg)
            .children(blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_from_one_change_run_together() {
        assert_eq!(runs(&[0, 0, 1, 0, 0, 0]), [(0, 0..2), (1, 2..3), (0, 3..6)]);
        assert!(runs(&[]).is_empty());
    }
}
