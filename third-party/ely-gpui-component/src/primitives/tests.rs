use std::{cell::RefCell, rc::Rc};

use gpui::{
    App, Context, Entity, FocusHandle, IntoElement, Modifiers, ParentElement, Render, RenderOnce,
    Styled, TestAppContext, Window, anchored, div, point, prelude::*, px,
};

use super::raise;
use crate::theme::Theme;

/// A raised square holding a raised button, under a cover drawn after it in the same layer.
struct Stack {
    heard: Vec<&'static str>,
}

impl Render for Stack {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (inner, cover) = (cx.entity(), cx.entity());
        let button = div()
            .id("inner")
            .size(px(100.0))
            .occlude()
            .on_click(move |_, _, cx| inner.update(cx, |stack, _| stack.heard.push("inner")));
        let sheet = div()
            .id("cover")
            .absolute()
            .top_0()
            .left_0()
            .size(px(200.0))
            .on_click(move |_, _, cx| cover.update(cx, |stack, _| stack.heard.push("cover")));
        div().size_full().child(raise(
            "sheet",
            anchored().position(point(px(0.0), px(0.0))).child(
                div()
                    .relative()
                    .size(px(200.0))
                    .child(raise(
                        "button",
                        anchored().position(point(px(50.0), px(50.0))).child(button),
                    ))
                    .child(sheet),
            ),
        ))
    }
}

#[gpui::test]
fn what_is_raised_inside_a_raise_lies_over_it(cx: &mut TestAppContext) {
    cx.update(Theme::init);
    let (view, cx) = cx.add_window_view(|_, _| Stack { heard: Vec::new() });
    cx.run_until_parked();
    cx.simulate_click(point(px(100.0), px(100.0)), Modifiers::none());
    assert_eq!(
        view.read_with(cx, |stack, _| stack.heard.clone()),
        ["inner"]
    );
}

/// A focusable box whose focus handle lives in keyed state; each draw tells the test which one it drew.
#[derive(IntoElement)]
struct Held {
    seen: Rc<RefCell<Vec<Entity<FocusHandle>>>>,
}

impl RenderOnce for Held {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let kept = window.use_keyed_state("kept", cx, |_, cx| cx.focus_handle());
        self.seen.borrow_mut().push(kept.clone());
        let handle = kept.read(cx).clone();
        div().id("held").size(px(40.0)).track_focus(&handle)
    }
}

/// A raised sheet holding a raised tip that comes and goes, then a raised box that must stay itself.
struct Nest {
    tip: bool,
    seen: Rc<RefCell<Vec<Entity<FocusHandle>>>>,
}

impl Render for Nest {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let tip = self.tip.then(|| {
            raise(
                "tip",
                anchored()
                    .position(point(px(0.0), px(0.0)))
                    .child(div().child("tip")),
            )
        });
        let held = Held {
            seen: self.seen.clone(),
        };
        div().size_full().child(raise(
            "sheet",
            anchored().position(point(px(0.0), px(0.0))).child(
                div().size(px(200.0)).children(tip).child(raise(
                    "held",
                    anchored().position(point(px(50.0), px(50.0))).child(held),
                )),
            ),
        ))
    }
}

#[gpui::test]
fn a_raise_keeps_its_state_while_another_comes_before_it(cx: &mut TestAppContext) {
    cx.update(Theme::init);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let (view, cx) = cx.add_window_view(|_, _| Nest {
        tip: false,
        seen: seen.clone(),
    });
    cx.run_until_parked();
    let first = seen.borrow().last().cloned().expect("the box drew");
    let handle = first.read_with(cx, |handle, _| handle.clone());
    cx.update(|window, cx| window.focus(&handle, cx));
    view.update(cx, |nest, cx| {
        nest.tip = true;
        cx.notify();
    });
    cx.run_until_parked();
    let last = seen.borrow().last().cloned().expect("the box drew again");
    assert_eq!(
        last.entity_id(),
        first.entity_id(),
        "the box kept its state"
    );
    assert!(
        cx.update(|window, _| handle.is_focused(window)),
        "the box kept focus"
    );
}
