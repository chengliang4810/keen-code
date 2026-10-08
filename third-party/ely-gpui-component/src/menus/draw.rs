use gpui::{
    AnyElement, App, Div, ElementId, Entity, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Role, Stateful, Styled, Toggled, div, prelude::*,
};

use super::{
    menu::{Open, at_depth, choose, mark, measure},
    model::{Entry, Kind, Menu, MenuItem},
};
use crate::{
    forms::{Run, surface},
    navigation::marked,
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, Radius, TextSize},
    typography::KbdCombo,
};

fn row(item: &MenuItem, ix: usize, current: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let colors = &theme.colors;
    let fg = if item.disabled {
        colors.fg_disabled
    } else {
        colors.fg
    };
    let (lead, lead_color) = match &item.kind {
        Kind::Check(true) => (Some(IconName::Check), fg),
        Kind::Radio(true) => (Some(IconName::Dot), fg),
        _ => (item.icon, if item.disabled { fg } else { colors.fg_muted }),
    };
    let role = match &item.kind {
        Kind::Check(_) => Role::MenuItemCheckBox,
        Kind::Radio(_) => Role::MenuItemRadio,
        _ => Role::MenuItem,
    };
    let toggled = match &item.kind {
        Kind::Check(true) | Kind::Radio(true) => Some(Toggled::True),
        Kind::Check(false) | Kind::Radio(false) => Some(Toggled::False),
        _ => None,
    };
    div()
        .id(("row", ix))
        .role(role)
        .aria_label(item.label.clone())
        .aria_selected(current)
        .when_some(toggled, |row, toggled| row.aria_toggled(toggled))
        .relative()
        .flex()
        .items_center()
        .gap_2()
        .h(theme.control_height(ControlSize::Md))
        .px_2()
        .rounded(theme.radius(Radius::Md))
        .text_color(fg)
        .when(current && !item.disabled, |row| row.bg(colors.hover))
        .when(!item.disabled, |row| row.cursor_pointer())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(theme.icon_size(IconSize::Sm))
                .when_some(lead, |slot, icon| {
                    slot.child(Icon::new(icon).size(IconSize::Sm).color(lead_color))
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(if item.hits.is_empty() {
                    item.label.clone().into_any_element()
                } else {
                    marked(item.label.clone(), item.hits.clone(), cx)
                }),
        )
        .when_some(item.keys.clone(), |row, keys| {
            row.child(
                div()
                    .flex_none()
                    .pl_4()
                    .text_color(colors.fg_subtle)
                    .child(KbdCombo::new(&keys)),
            )
        })
        .when(matches!(item.kind, Kind::Sub(_)), |row| {
            row.child(
                Icon::new(IconName::ChevronRight)
                    .size(IconSize::Xs)
                    .color(colors.fg_subtle),
            )
        })
}

pub(super) fn panel(
    id: &ElementId,
    menu: &Menu,
    state: &Entity<Open>,
    depth: usize,
    close: &Run,
    head: Option<AnyElement>,
    cx: &App,
) -> Stateful<Div> {
    let open = state.read(cx);
    let here = at_depth(menu, &open.levels, depth).expect("settled levels follow submenu rows");
    let at = open.levels[depth].at;
    let theme = cx.theme();
    let colors = &theme.colors;
    let rows = here
        .entries
        .iter()
        .enumerate()
        .map(|(ix, entry)| match entry {
            Entry::Separator => div().h_px().my_1().bg(colors.border).into_any_element(),
            Entry::Heading(title) => div()
                .px_2()
                .pt_1p5()
                .pb_1()
                .text_size(theme.text_size(TextSize::Xs))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.fg_subtle)
                .child(title.clone())
                .into_any_element(),
            Entry::Item(item) => {
                let marked = at == Some(ix);
                let sub = matches!(item.kind, Kind::Sub(_));
                let (hover, click, close, menu) =
                    (state.clone(), state.clone(), close.clone(), menu.clone());
                let hover_menu = menu.clone();
                row(item, ix, marked, cx)
                    .when(!item.disabled, |row| {
                        row.on_mouse_move(move |_, _, cx| {
                            let open = hover.read(cx);
                            let Some(level) = open.levels.get(depth) else {
                                return;
                            };
                            let shown = open.levels.len() > depth + 1;
                            if level.at != Some(ix) || (sub && !shown) {
                                mark(&hover, &hover_menu, depth, ix, true, cx);
                            }
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            move |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                choose(&click, &menu, depth, ix, &close, window, cx);
                            },
                        )
                    })
                    .when(marked && sub, |row| {
                        row.child(measure(state.clone(), move |open, bounds| {
                            let level = &mut open.levels[depth];
                            let changed = level.row != bounds;
                            level.row = bounds;
                            changed
                        }))
                    })
                    .into_any_element()
            }
        });
    surface((id.clone(), format!("panel-{depth}")), cx)
        .relative()
        .min_w(theme.menu_width())
        .p_1()
        .flex()
        .flex_col()
        .children(head)
        .children(rows)
        .child(measure(state.clone(), move |open, bounds| {
            let level = &mut open.levels[depth];
            let changed = level.panel != bounds;
            level.panel = bounds;
            changed
        }))
}
