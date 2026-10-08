use std::rc::Rc;

use gpui::{
    AnyElement, App, Div, ElementId, FontWeight, Hsla, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, prelude::*, relative,
};

use similar::DiffableStr;

use super::merging::{Region, RegionKind, Take, conflicts, resolve, result};
use crate::{
    buttons::{Button, ButtonVariant},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Palette, Radius, TextSize},
    typography::{LEADING, format},
};

type OnTake = Rc<dyn Fn(usize, Take, &mut Window, &mut App)>;
type OnText = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// A column of code lines, padded to `rows` so neighbors line up.
fn column(lines: &[String], rows: usize, wash: Option<Hsla>) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .px_2()
        .when_some(wash, |column, wash| column.bg(wash))
        .children((0..rows).map(|ix| {
            let line = lines
                .get(ix)
                .map_or("", |line| line.trim_end_matches(['\r', '\n']));
            div()
                .whitespace_nowrap()
                .overflow_hidden()
                .line_height(relative(LEADING))
                .child(line.to_string())
        }))
}

/// The three buttons that settle a conflict.
fn takes(
    id: &ElementId,
    conflict: usize,
    on_take: &Option<OnTake>,
    words: [&'static str; 3],
) -> Div {
    let [ours, theirs, both] = words;
    div().flex().gap_1().children(
        [
            (Take::Ours, ours),
            (Take::Theirs, theirs),
            (Take::Both, both),
        ]
        .map(|(take, words)| {
            let on_take = on_take.clone();
            Button::new((id.clone(), format!("take-{conflict}-{take:?}")), words)
                .variant(ButtonVariant::Ghost)
                .size(ControlSize::Sm)
                .when_some(on_take, |button, on_take| {
                    button.on_click(move |_, window, cx| on_take(conflict, take, window, cx))
                })
        }),
    )
}

/// Three versions side by side, ours, the base and theirs, region by region: clean changes merged and marked, each conflict held with a choice of side; the result below.
#[derive(IntoElement)]
pub struct ThreeWayMerge {
    id: ElementId,
    labels: [SharedString; 3],
    regions: Rc<Vec<Region>>,
    takes: Vec<Option<Take>>,
    on_take: Option<OnTake>,
}

impl ThreeWayMerge {
    /// `labels` name ours, the base and theirs.
    pub fn new(id: impl Into<ElementId>, labels: [&str; 3], regions: Rc<Vec<Region>>) -> Self {
        Self {
            id: id.into(),
            labels: labels.map(|label| label.to_string().into()),
            regions,
            takes: Vec::new(),
            on_take: None,
        }
    }

    /// The side each conflict took so far, in order.
    pub fn takes(mut self, takes: impl IntoIterator<Item = Option<Take>>) -> Self {
        self.takes = takes.into_iter().collect();
        self
    }

    pub fn on_take(
        mut self,
        handler: impl Fn(usize, Take, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_take = Some(Rc::new(handler));
        self
    }
}

fn washes(kind: RegionKind, colors: &Palette) -> [Option<Hsla>; 3] {
    let changed = colors.success.opacity(0.1);
    match kind {
        RegionKind::Unchanged => [None, None, None],
        RegionKind::Ours => [Some(changed), None, None],
        RegionKind::Theirs => [None, None, Some(changed)],
        RegionKind::Both => [Some(changed), None, Some(changed)],
        RegionKind::Conflict => [Some(colors.warning.opacity(0.14)); 3],
    }
}

impl RenderOnce for ThreeWayMerge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let (merged, open) = result(&self.regions, &self.takes);
        let total = self
            .regions
            .iter()
            .filter(|region| region.kind == RegionKind::Conflict)
            .count();
        let mut conflict = 0;
        let blocks: Vec<AnyElement> = self
            .regions
            .iter()
            .map(|region| {
                let rows = region
                    .ours
                    .len()
                    .max(region.base.len())
                    .max(region.theirs.len());
                let [ours, base, theirs] = washes(region.kind, &colors);
                let block = div().flex().flex_col().child(
                    div()
                        .flex()
                        .gap_px()
                        .child(column(&region.ours, rows, ours))
                        .child(column(&region.base, rows, base))
                        .child(column(&region.theirs, rows, theirs)),
                );
                if region.kind != RegionKind::Conflict {
                    return block.into_any_element();
                }
                let at = conflict;
                conflict += 1;
                let settled = self.takes.get(at).copied().flatten();
                block
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .border_b_1()
                            .border_color(colors.border)
                            .font_family(theme.font_family.clone())
                            .child(
                                Icon::new(IconName::TriangleAlert)
                                    .size(IconSize::Sm)
                                    .color(colors.warning),
                            )
                            .child(div().flex_1().text_color(colors.fg_muted).child(
                                match settled {
                                    Some(take) => {
                                        format!("Conflict {} took {}", at + 1, word(take))
                                    }
                                    None => format!("Conflict {}", at + 1),
                                },
                            ))
                            .child(takes(
                                &self.id,
                                at,
                                &self.on_take,
                                ["Take ours", "Take theirs", "Take both"],
                            )),
                    )
                    .into_any_element()
            })
            .collect();
        let header = |label: &SharedString| {
            div()
                .flex_1()
                .px_2()
                .py_1()
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.fg_muted)
                .child(label.clone())
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(theme.text_size(TextSize::Sm))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(theme.radius(Radius::Md))
                    .border_1()
                    .border_color(colors.border)
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .gap_px()
                            .bg(colors.hover)
                            .children(self.labels.iter().map(header)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .font_family(theme.mono_family.clone())
                            .text_size(theme.text_size(TextSize::Xs))
                            .children(blocks),
                    ),
            )
            .child(div().text_color(colors.fg_muted).child(if open == 0 {
                format!(
                    "Merged: {}, all settled.",
                    format::plural(total as u64, "conflict", "conflicts")
                )
            } else {
                format!(
                    "{} of {} still open.",
                    open,
                    format::plural(total as u64, "conflict", "conflicts")
                )
            }))
            .child(
                div()
                    .p_2()
                    .rounded(theme.radius(Radius::Md))
                    .bg(colors.sunken)
                    .font_family(theme.mono_family.clone())
                    .text_size(theme.text_size(TextSize::Xs))
                    .text_color(colors.fg)
                    .children(merged.tokenize_lines().into_iter().map(|line| {
                        let line = line.trim_end_matches(['\r', '\n']);
                        let marker = ["<<<<<<<", "=======", ">>>>>>>"]
                            .iter()
                            .any(|mark| line.starts_with(mark));
                        div()
                            .whitespace_nowrap()
                            .when(marker, |row| row.text_color(colors.warning))
                            .child(line.to_string())
                    })),
            )
    }
}

fn word(take: Take) -> &'static str {
    match take {
        Take::Ours => "ours",
        Take::Theirs => "theirs",
        Take::Both => "both",
    }
}

/// A file with conflict markers, each conflict shown as the current and the incoming change, with actions to accept either or both.
#[derive(IntoElement)]
pub struct ConflictResolver {
    id: ElementId,
    text: SharedString,
    on_resolve: Option<OnText>,
}

impl ConflictResolver {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            on_resolve: None,
        }
    }

    /// Called with the whole text once a conflict is settled.
    pub fn on_resolve(mut self, handler: impl Fn(String, &mut Window, &mut App) + 'static) -> Self {
        self.on_resolve = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ConflictResolver {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let found = conflicts(&self.text);
        let text = self.text.to_string();
        let on_take: Option<OnTake> = self.on_resolve.map(|on_resolve| {
            Rc::new(
                move |ix: usize, take: Take, window: &mut Window, cx: &mut App| {
                    on_resolve(resolve(&text, ix, take), window, cx)
                },
            ) as OnTake
        });
        let (current, incoming) = (colors.success.opacity(0.1), colors.info.opacity(0.1));
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, line) in self.text.tokenize_lines().into_iter().enumerate() {
            let line = line.trim_end_matches(['\r', '\n']);
            if let Some(at) = found.iter().position(|conflict| conflict.start == ix) {
                rows.push(
                    takes(
                        &self.id,
                        at,
                        &on_take,
                        ["Accept current", "Accept incoming", "Accept both"],
                    )
                    .py_0p5()
                    .font_family(theme.font_family.clone())
                    .into_any_element(),
                );
            }
            let wash = found.iter().find_map(|conflict| {
                if ix == conflict.start {
                    Some(current.opacity(2.0))
                } else if ix > conflict.start && ix < conflict.middle {
                    Some(current)
                } else if ix > conflict.middle && ix < conflict.end {
                    Some(incoming)
                } else if ix == conflict.end {
                    Some(incoming.opacity(2.0))
                } else {
                    None
                }
            });
            let marker = found
                .iter()
                .any(|conflict| [conflict.start, conflict.middle, conflict.end].contains(&ix));
            rows.push(
                div()
                    .px_2()
                    .whitespace_nowrap()
                    .when_some(wash, |row, wash| row.bg(wash))
                    .when(marker, |row| row.text_color(colors.fg_muted))
                    .child(line.to_string())
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .rounded(theme.radius(Radius::Md))
            .border_1()
            .border_color(colors.border)
            .py_1()
            .font_family(theme.mono_family.clone())
            .text_size(theme.text_size(TextSize::Xs))
            .text_color(colors.fg)
            .children(rows)
    }
}
