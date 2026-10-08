use gpui::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*,
};

use crate::{
    layout::{Collapsible, seeded::use_seeded},
    primitives::Disclosure,
    theme::{ActiveTheme, ControlSize, TextSize},
};

/// Labels and their values, one pair a row with the labels in a quiet column, or stacked with each label above its value. A value short of room drops under its label.
#[derive(IntoElement, Default)]
pub struct DescriptionList {
    items: Vec<(SharedString, AnyElement)>,
    stacked: bool,
    lined: bool,
}

impl DescriptionList {
    pub fn new() -> Self {
        Self::default()
    }

    /// A label, and its value: text, or any element such as a badge.
    pub fn item(mut self, label: impl Into<SharedString>, value: impl IntoElement) -> Self {
        self.items.push((label.into(), value.into_any_element()));
        self
    }

    /// Each label above its value, for narrow places.
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// A hairline between rows.
    pub fn lined(mut self) -> Self {
        self.lined = true;
        self
    }
}

impl RenderOnce for DescriptionList {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = &theme.colors;
        let (stacked, lined, last) = (self.stacked, self.lined, self.items.len().saturating_sub(1));
        div()
            .flex()
            .flex_col()
            .text_size(theme.text_size(TextSize::Base))
            .children(
                self.items
                    .into_iter()
                    .enumerate()
                    .map(|(ix, (label, value))| {
                        let label = div().text_color(colors.fg_muted).child(label);
                        div()
                            .flex()
                            .when(stacked, |row| row.flex_col().gap_0p5())
                            .when(!stacked, |row| {
                                row.flex_wrap().items_end().gap_x_4().gap_y_0p5()
                            })
                            .py_2()
                            .when(lined && ix < last, |row| {
                                row.border_b_1().border_color(colors.border)
                            })
                            .child(if stacked {
                                label.text_size(theme.text_size(TextSize::Sm))
                            } else {
                                label.flex_none().w(theme.label_width())
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_1()
                                    .when(stacked, |value| value.min_w_0())
                                    .when(!stacked, |value| value.min_w(theme.label_width() * 0.5))
                                    .text_color(colors.fg)
                                    .child(div().min_w_0().child(value)),
                            )
                    }),
            )
    }
}

/// A titled group of a `PropertyGrid`, open unless `folded`.
pub struct PropertyGroup {
    title: SharedString,
    open: bool,
    rows: Vec<(SharedString, AnyElement)>,
}

impl PropertyGroup {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            open: true,
            rows: Vec::new(),
        }
    }

    /// Starts folded.
    pub fn folded(mut self) -> Self {
        self.open = false;
        self
    }

    /// A property's name and the editor that changes it.
    pub fn row(mut self, name: impl Into<SharedString>, editor: impl IntoElement) -> Self {
        self.rows.push((name.into(), editor.into_any_element()));
        self
    }
}

/// An inspector's table: groups that fold, each row a name and its editor. The owner supplies the editors and keeps the values.
#[derive(IntoElement)]
pub struct PropertyGrid {
    id: ElementId,
    groups: Vec<PropertyGroup>,
}

impl PropertyGrid {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            groups: Vec::new(),
        }
    }

    pub fn group(mut self, group: PropertyGroup) -> Self {
        self.groups.push(group);
        self
    }
}

impl RenderOnce for PropertyGrid {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let folds: Vec<_> = self
            .groups
            .iter()
            .enumerate()
            .map(|(ix, group)| {
                use_seeded(
                    (self.id.clone(), format!("fold-{ix}")),
                    group.open,
                    window,
                    cx,
                )
            })
            .collect();
        let theme = cx.theme();
        let colors = &theme.colors;
        let row_height = theme.control_height(ControlSize::Md);
        let id = self.id.clone();
        div()
            .id(self.id)
            .flex()
            .flex_col()
            .text_size(theme.text_size(TextSize::Sm))
            .children(self.groups.into_iter().enumerate().map(
                |(ix, PropertyGroup { title, rows, .. })| {
                    let shown = folds[ix].read(cx).value;
                    let (toggle, key) = (folds[ix].clone(), title.clone());
                    let chevron = Disclosure::new((id.clone(), format!("chevron-{ix}")), shown);
                    div()
                        .when(ix > 0, |group| {
                            group.border_t_1().border_color(colors.border)
                        })
                        .child(
                            div()
                                .id((id.clone(), format!("group-{ix}")))
                                .flex()
                                .items_center()
                                .gap_1()
                                .h(row_height)
                                .px_2()
                                .cursor_pointer()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(colors.fg)
                                .hover(|style| style.bg(colors.hover))
                                .on_click(move |_, _, cx| {
                                    log::info!(
                                        "property grid: {key} {}",
                                        if shown { "folded" } else { "opened" }
                                    );
                                    toggle.update(cx, |fold, cx| {
                                        fold.value = !shown;
                                        cx.notify();
                                    })
                                })
                                .child(chevron)
                                .child(title),
                        )
                        .child(
                            Collapsible::new((id.clone(), format!("rows-{ix}")), shown).child(
                                div()
                                    .pb_1()
                                    .children(rows.into_iter().map(|(name, editor)| {
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .min_h(row_height)
                                            .pl_6()
                                            .pr_2()
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .w(theme.label_width() * 0.75)
                                                    .text_color(colors.fg_muted)
                                                    .child(name),
                                            )
                                            .child(div().flex_1().min_w_0().child(editor))
                                    })),
                            ),
                        )
                },
            ))
    }
}
