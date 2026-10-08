use gpui::{
    AnyElement, Entity, IntoElement, Modifiers, MouseButton, ParentElement, SharedString, Styled,
    TestAppContext, point, px,
};
use jiff::civil::date;

use super::{Desk, desk, heard, note, press, settle, tab_to};
use crate::{
    mail::Label,
    project::{
        IssueCard, KanbanBoard, KanbanCard, KanbanColumn, LabelManager, Person, Status, Task,
        TaskDetailPanel,
    },
};

fn card(key: &str) -> KanbanCard {
    let task = Task::new(SharedString::from(key.to_string()), format!("Task {key}"));
    let face =
        IssueCard::new(SharedString::from(format!("issue-{key}")), task).today(date(2026, 9, 27));
    KanbanCard::new(SharedString::from(key.to_string()), face)
}

fn board(_: &Desk, owner: Entity<Desk>) -> AnyElement {
    let (moved, opened) = (owner.clone(), owner);
    KanbanBoard::new("board")
        .column(
            KanbanColumn::new("todo", "Todo")
                .card(card("a"))
                .card(card("b")),
        )
        .column(KanbanColumn::new("doing", "In progress").card(card("c")))
        .on_move(move |key, column, index, _, cx| {
            note(&moved, format!("move {key} {column} {index}"), cx)
        })
        .on_open(move |key, _, cx| note(&opened, format!("open {key}"), cx))
        .into_any_element()
}

/// Presses at `from`, drags in steps to `to`, and lets go there.
fn drag(
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
    cx: &mut gpui::VisualTestContext,
) {
    cx.simulate_mouse_move(from, None, Modifiers::none());
    cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    for step in 1..=6 {
        let share = step as f32 / 6.0;
        let at = point(
            from.x + (to.x - from.x) * share,
            from.y + (to.y - from.y) * share,
        );
        cx.simulate_mouse_move(at, Some(MouseButton::Left), Modifiers::none());
        settle(cx);
    }
    cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    settle(cx);
}

#[gpui::test]
fn a_card_dragged_to_another_column_lands_where_it_is_let_go(cx: &mut TestAppContext) {
    let (host, cx) = desk(board, cx);
    let from = cx.debug_bounds("card a").expect("card a draws").center();
    let lane = cx.debug_bounds("column doing").expect("the column draws");
    drag(from, point(lane.center().x, lane.bottom() - px(8.0)), cx);
    assert_eq!(
        heard(&host, cx),
        ["move a doing 1"],
        "below the column's one card"
    );
}

#[gpui::test]
fn a_card_let_go_in_its_own_place_does_not_move(cx: &mut TestAppContext) {
    let (host, cx) = desk(board, cx);
    let from = cx.debug_bounds("card a").expect("card a draws").center();
    drag(from, point(from.x, from.y + px(6.0)), cx);
    assert!(heard(&host, cx).is_empty(), "{:?}", heard(&host, cx));
}

#[gpui::test]
fn option_arrows_move_the_focused_card_and_enter_opens_it(cx: &mut TestAppContext) {
    let (host, cx) = desk(board, cx);
    tab_to(1, cx);
    press("down", cx);
    press("alt-down", cx);
    press("alt-right", cx);
    press("enter", cx);
    assert_eq!(
        heard(&host, cx),
        ["move a todo 1", "move a doing 0", "open a"]
    );
}

fn narrow(desk: &Desk, owner: Entity<Desk>) -> AnyElement {
    gpui::div()
        .w(px(300.0))
        .child(board(desk, owner))
        .into_any_element()
}

#[gpui::test]
fn a_focused_card_past_the_fold_scrolls_into_view(cx: &mut TestAppContext) {
    let (_, cx) = desk(narrow, cx);
    tab_to(3, cx);
    let board = cx.debug_bounds("kanban-board").expect("the board draws");
    let card = cx.debug_bounds("card c").expect("card c draws");
    assert!(
        board.left() <= card.left() && card.right() <= board.right(),
        "card c at {card:?} shows inside the board at {board:?}"
    );
}

fn labels(_: &Desk, owner: Entity<Desk>) -> AnyElement {
    let (renamed, made) = (owner.clone(), owner);
    LabelManager::new(
        "labels",
        [Label::new("bug", "Bug", 3), Label::new("site", "Site", 1)],
    )
    .on_rename(move |key, name, _, cx| note(&renamed, format!("rename {key} {name}"), cx))
    .on_create(move |name, _, cx| note(&made, format!("make {name}"), cx))
    .into_any_element()
}

#[gpui::test]
fn a_label_keeps_a_name_of_its_own(cx: &mut TestAppContext) {
    let (host, cx) = desk(labels, cx);
    tab_to(2, cx);
    press("enter", cx);
    cx.simulate_input("site");
    press("enter", cx);
    assert!(
        cx.debug_bounds("refused bug").is_some(),
        "a taken name is refused"
    );
    tab_to(2, cx);
    press("enter", cx);
    cx.simulate_input("Defect");
    press("enter", cx);
    tab_to(7, cx);
    cx.simulate_input("bug");
    press("enter", cx);
    assert!(
        cx.debug_bounds("refused +new").is_some(),
        "a new label cannot take one"
    );
    cx.simulate_input(" report");
    press("enter", cx);
    assert_eq!(heard(&host, cx), ["rename bug Defect", "make bug report"]);
}

fn panel(_: &Desk, owner: Entity<Desk>) -> AnyElement {
    let task = Task::new("brief", "Write the brief").status(Status::Todo);
    TaskDetailPanel::new("panel", task, [Person::new("ana", "Ana Lima")])
        .on_change(move |task, _, cx| {
            let said = format!("{} {:?} {:?}", task.title, task.status, task.priority);
            note(&owner, said, cx)
        })
        .into_any_element()
}

#[gpui::test]
fn the_panel_hands_on_the_task_after_each_edit(cx: &mut TestAppContext) {
    let (host, cx) = desk(panel, cx);
    tab_to(1, cx);
    press("enter", cx);
    cx.simulate_input("   ");
    press("enter", cx);
    tab_to(1, cx);
    press("enter", cx);
    cx.simulate_input("Client brief");
    press("enter", cx);
    tab_to(2, cx);
    cx.simulate_keystrokes("down down enter");
    settle(cx);
    tab_to(3, cx);
    cx.simulate_keystrokes("down up enter");
    settle(cx);
    assert_eq!(
        heard(&host, cx),
        [
            "Client brief Todo None",
            "Write the brief InProgress None",
            "Write the brief Todo Low"
        ],
        "a blank title is not taken"
    );
}

fn roadmap(_: &Desk, _: Entity<Desk>) -> AnyElement {
    crate::project::Roadmap::new("roadmap", date(2026, 9, 1), 2)
        .lane(
            "Design",
            [
                crate::project::Initiative::new(
                    "early",
                    "Early",
                    date(2026, 7, 1),
                    date(2026, 8, 30),
                ),
                crate::project::Initiative::new(
                    "half",
                    "Half",
                    date(2026, 10, 1),
                    date(2026, 10, 31),
                ),
            ],
        )
        .into_any_element()
}

#[gpui::test]
fn a_roadmap_draws_only_what_falls_in_its_months(cx: &mut TestAppContext) {
    let (_, cx) = desk(roadmap, cx);
    assert!(
        cx.debug_bounds("initiative early").is_none(),
        "it ended before the span"
    );
    let map = cx.debug_bounds("roadmap").expect("the roadmap draws");
    let half = cx.debug_bounds("initiative half").expect("October draws");
    let share = (half.left() - map.left()) / map.size.width;
    assert!(
        (share - 30.0 / 61.0).abs() < 0.01,
        "October starts 30 of 61 days in: {share}"
    );
    let end = (half.right() - map.left()) / map.size.width;
    assert!(
        (end - 1.0).abs() < 0.01,
        "October runs through its last day: {end}"
    );
}
