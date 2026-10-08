use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window,
    canvas, div, point, prelude::*, size,
};

use super::graph::{GraphRow, Half, lanes};
use crate::{
    data_display::{Avatar, Tag, Tone},
    theme::{ActiveTheme, AvatarSize, ControlSize, IconSize, Radius, TextSize},
    typography::Ellipsis,
};

/// A commit as lists show it: id, parents first-parent first, subject, author, when, and the refs on it.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub id: SharedString,
    pub parents: Vec<SharedString>,
    pub subject: SharedString,
    pub author: SharedString,
    pub when: SharedString,
    pub refs: Vec<SharedString>,
}

type OnPick = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// One commit in a row: its author's initials, its subject and the refs on it, its short id, and when.
#[derive(IntoElement)]
pub struct CommitItem {
    id: ElementId,
    commit: Commit,
    selected: bool,
    on_pick: Option<OnPick>,
}

impl CommitItem {
    pub fn new(id: impl Into<ElementId>, commit: Commit) -> Self {
        Self {
            id: id.into(),
            commit,
            selected: false,
            on_pick: None,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
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

impl RenderOnce for CommitItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let commit = self.commit;
        let short: String = commit.id.chars().take(7).collect();
        let (pick, picked) = (self.on_pick, commit.id.clone());
        div()
            .id(self.id.clone())
            .flex_1()
            .min_w_0()
            .h(theme.control_height(ControlSize::Lg))
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .rounded(theme.radius(Radius::Sm))
            .text_size(theme.text_size(TextSize::Sm))
            .when(self.selected, |row| row.bg(colors.active))
            .when(!self.selected, |row| row.hover(|row| row.bg(colors.hover)))
            .when_some(pick, |row, pick| {
                row.cursor_pointer()
                    .on_click(move |_, window, cx| pick(&picked, window, cx))
            })
            .child(
                Avatar::new((self.id.clone(), "author"), commit.author.clone())
                    .size(AvatarSize::Xs),
            )
            .children(commit.refs.iter().enumerate().map(|(ix, name)| {
                let tone = if name.starts_with('v') {
                    Tone::Warning
                } else if name.contains('/') {
                    Tone::Neutral
                } else {
                    Tone::Info
                };
                Tag::new((self.id.clone(), format!("ref-{ix}")), name.clone()).tone(tone)
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(colors.fg)
                    .child(Ellipsis::new(commit.subject)),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(colors.fg_muted)
                    .child(commit.author),
            )
            .child(
                div()
                    .flex_none()
                    .font_family(theme.mono_family.clone())
                    .text_color(colors.fg_subtle)
                    .child(short),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(colors.fg_subtle)
                    .child(commit.when),
            )
    }
}

/// Commits newest first, one to a row; a press picks one.
#[derive(IntoElement)]
pub struct CommitList {
    id: ElementId,
    commits: Vec<Commit>,
    selected: Option<SharedString>,
    on_pick: Option<OnPick>,
    graph: bool,
}

impl CommitList {
    pub fn new(id: impl Into<ElementId>, commits: impl IntoIterator<Item = Commit>) -> Self {
        Self {
            id: id.into(),
            commits: commits.into_iter().collect(),
            selected: None,
            on_pick: None,
            graph: false,
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

    /// Draws the lanes beside the rows, as a commit graph.
    pub fn graph(mut self) -> Self {
        self.graph = true;
        self
    }
}

/// Paints one row's lanes, `span` lanes wide so every row lines up: strokes into and out of its commit, lanes passing it, and the commit's dot.
fn lanes_cell(
    row: GraphRow,
    span: usize,
    lane: Pixels,
    height: Pixels,
    stroke: Pixels,
    palette: [Hsla; 8],
) -> AnyElement {
    let width = lane * span as f32;
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window, _| {
            let x = |at: usize| bounds.origin.x + lane * (at as f32 + 0.5);
            let (top, middle, bottom) = (
                bounds.origin.y,
                bounds.origin.y + height / 2.0,
                bounds.origin.y + height,
            );
            for piece in &row.strokes {
                let (from, to) = match piece.half {
                    Half::Top => (point(x(piece.from), top), point(x(piece.to), middle)),
                    Half::Bottom => (point(x(piece.from), middle), point(x(piece.to), bottom)),
                    Half::Through => (point(x(piece.from), top), point(x(piece.to), bottom)),
                };
                let color = palette[piece.from.max(piece.to) % palette.len()];
                let mut path = PathBuilder::stroke(stroke);
                path.move_to(from);
                path.line_to(to);
                match path.build() {
                    Ok(path) => window.paint_path(path, color),
                    Err(error) => log::error!("commit graph: a stroke failed to build: {error:#}"),
                }
            }
            let dot = lane * 0.45;
            let center = point(x(row.lane), middle);
            window.paint_quad(
                gpui::fill(
                    Bounds::new(center - point(dot / 2.0, dot / 2.0), size(dot, dot)),
                    palette[row.lane % palette.len()],
                )
                .corner_radii(dot / 2.0),
            );
        },
    )
    .flex_none()
    .w(width)
    .h(height)
    .into_any_element()
}

impl RenderOnce for CommitList {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let rem = window.rem_size();
        let height = theme.control_height(ControlSize::Lg).to_pixels(rem);
        let lane = theme.icon_size(IconSize::Md).to_pixels(rem);
        let stroke = theme.chart().hairline * 1.5;
        let palette = theme.colors.chart;
        let graph: Vec<Option<GraphRow>> = if self.graph {
            let parents: Vec<Vec<&str>> = self
                .commits
                .iter()
                .map(|commit| commit.parents.iter().map(|id| id.as_ref()).collect())
                .collect();
            lanes(
                self.commits
                    .iter()
                    .zip(&parents)
                    .map(|(commit, parents)| (commit.id.as_ref(), parents.as_slice())),
            )
            .into_iter()
            .map(Some)
            .collect()
        } else {
            vec![None; self.commits.len()]
        };
        let span = graph
            .iter()
            .flatten()
            .map(|row| row.width)
            .max()
            .unwrap_or(0);
        div()
            .flex()
            .flex_col()
            .children(self.commits.into_iter().zip(graph).map(|(commit, lanes)| {
                let selected = self.selected.as_ref() == Some(&commit.id);
                let item = CommitItem::new((self.id.clone(), commit.id.clone()), commit)
                    .selected(selected);
                let item = match &self.on_pick {
                    Some(pick) => {
                        let pick = pick.clone();
                        item.on_pick(move |id, window, cx| pick(id, window, cx))
                    }
                    None => item,
                };
                div()
                    .flex()
                    .items_center()
                    .children(lanes.map(|row| lanes_cell(row, span, lane, height, stroke, palette)))
                    .child(item)
            }))
    }
}
