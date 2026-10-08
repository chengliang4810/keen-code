use std::time::Duration;

use gpui::{
    AnyElement, App, Bounds, Context, InteractiveElement, IntoElement, Modifiers, MouseButton,
    ParentElement, Pixels, Point, Render, Styled, TestAppContext, VisualTestContext, Window, div,
    point, px,
};

use super::Drawer;
use crate::theme::Theme;

type Build = Box<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// One element in a column that a padded block measures by content.
struct Narrow(Build);

impl Render for Narrow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().w(px(280.0)).flex().flex_col().child(
            div().p_5().child(
                div()
                    .flex()
                    .flex_col()
                    .child((self.0)(window, cx))
                    .child("A line long enough to wrap in a narrow card, so the column fills it."),
            ),
        )
    }
}

/// The width of `selector` in that column; a root that fills it takes all 240px.
pub(crate) fn narrow_width(
    cx: &mut TestAppContext,
    selector: &'static str,
    build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
) -> Pixels {
    let (_, cx) = cx.add_window_view(|_, _| Narrow(Box::new(build)));
    cx.run_until_parked();
    cx.debug_bounds(selector).expect(selector).size.width
}

/// A drawer holding a body the test can find.
struct Pulled;

impl Render for Pulled {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            Drawer::new("drawer", |_, _| {})
                .child(div().debug_selector(|| "drawer-body".into()).h(px(120.0))),
        )
    }
}

fn body(cx: &mut VisualTestContext) -> Bounds<Pixels> {
    cx.debug_bounds("drawer-body").expect("the body")
}

/// Opens a drawer and waits out its entrance.
fn opened(cx: &mut TestAppContext, reduced: bool) -> &mut VisualTestContext {
    cx.update(|cx| {
        Theme::init(cx);
        Theme::update(cx, |theme| theme.reduced_motion = reduced);
    });
    let (_, cx) = cx.add_window_view(|_, _| Pulled);
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(if reduced { 10 } else { 250 }));
    cx.update(|window, _| window.refresh());
    cx
}

/// Presses the grab and drags it down by `dy`.
fn pull(cx: &mut VisualTestContext, dy: f32) -> Point<Pixels> {
    let rest = body(cx).origin;
    let grab = point(rest.x + px(10.0), rest.y - px(28.0));
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::none());
    for step in [4.0, 8.0, dy] {
        let at = point(grab.x, grab.y + px(step));
        cx.simulate_mouse_move(at, MouseButton::Left, Modifiers::none());
    }
    point(grab.x, grab.y + px(dy))
}

#[gpui::test]
fn a_resting_drawer_reaches_the_bottom(cx: &mut TestAppContext) {
    let cx = opened(cx, true);
    let panel = cx.debug_bounds("drawer-panel").expect("the panel");
    let bottom = cx.update(|window, _| window.viewport_size().height);
    assert_eq!(
        body(cx).size.height,
        px(120.0),
        "the panel's 0.85 cap leaves a resting body whole"
    );
    assert_eq!(
        panel.bottom(),
        bottom,
        "the panel reaches the window's bottom"
    );
}

#[gpui::test]
fn a_pulled_drawer_follows_the_pointer(cx: &mut TestAppContext) {
    let cx = opened(cx, true);
    let at = pull(cx, 8.0);
    let held = body(cx).origin.y;
    cx.simulate_mouse_move(
        at + point(px(0.0), px(40.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    assert_eq!(
        body(cx).origin.y - held,
        px(40.0),
        "the panel moves with the pull"
    );
}

#[gpui::test]
fn a_released_drawer_glides_back(cx: &mut TestAppContext) {
    let cx = opened(cx, false);
    let rest = body(cx).origin.y;
    let at = pull(cx, 40.0);
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    let lift = body(cx).origin.y - rest;
    assert!(
        lift > px(20.0),
        "the panel glides back from {lift:?} below rest"
    );
}
