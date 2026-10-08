use gpui::{
    Context, IntoElement, KeyBinding, KeyUpEvent, Keystroke, ParentElement, Render, Styled,
    TestAppContext, VisualTestContext, Window, div, px,
};

use super::DiffViewer;
use crate::{forms, primitives::FocusNext, theme::Theme};

/// A diff whose first stretch folds, and the folds asked open.
struct Folded {
    opened: Vec<usize>,
}

impl Render for Folded {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let old: String = (0..20).map(|ix| format!("line {ix}\n")).collect();
        let new = old.replace("line 18", "line eighteen");
        div().w(px(480.0)).h(px(320.0)).child(
            DiffViewer::new("diff", "a.rs", &old, &new)
                .headless()
                .on_open(move |ix, _, cx| view.update(cx, |folded, _| folded.opened.push(ix))),
        )
    }
}

fn settle(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

#[gpui::test]
fn a_fold_opens_from_the_keyboard(cx: &mut TestAppContext) {
    cx.update(|cx| {
        Theme::init(cx);
        forms::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("tab", FocusNext, None)]);
    });
    let (view, cx) = cx.add_window_view(|_, _| Folded { opened: Vec::new() });
    cx.update(|window, _| window.activate_window());
    settle(cx);
    cx.update(|window, cx| window.focus_next(cx));
    settle(cx);
    cx.simulate_keystrokes("enter");
    settle(cx);
    cx.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("enter").expect("a key"),
    });
    settle(cx);
    assert_eq!(view.read_with(cx, |folded, _| folded.opened.clone()), [0]);
}
