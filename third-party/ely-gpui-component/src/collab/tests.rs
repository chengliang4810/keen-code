use gpui::{
    AppContext as _, Context, Entity, Focusable, IntoElement, ParentElement, Render, Styled,
    TestAppContext, VisualTestContext, Window, div,
};

use super::{FollowMode, Peer, Reply};
use crate::{
    forms,
    forms::{Input, TextInput},
    theme::Theme,
};

/// A field outside a followed view and one inside it, with what they asked: replies sent, follows stopped.
struct Desk {
    outside: Entity<TextInput>,
    field: Entity<TextInput>,
    following: bool,
    sent: usize,
    stopped: usize,
}

impl Render for Desk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (send, stop) = (cx.entity(), cx.entity());
        let peer = self.following.then(|| Peer::new("ada", "Ada", 0));
        div().size_full().child(Input::new(&self.outside)).child(
            FollowMode::new("follow", peer, move |_, cx| {
                stop.update(cx, |desk, _| desk.stopped += 1)
            })
            .child(Reply::new("reply", &self.field, move |_, cx| {
                send.update(cx, |desk, _| desk.sent += 1)
            })),
        )
    }
}

fn open(following: bool, cx: &mut TestAppContext) -> (Entity<Desk>, &mut VisualTestContext) {
    cx.update(|cx| {
        Theme::init(cx);
        forms::bind_keys(cx);
    });
    let (desk, cx) = cx.add_window_view(move |window, cx| Desk {
        outside: cx.new(|cx| TextInput::new(window, cx)),
        field: cx.new(|cx| TextInput::new(window, cx)),
        following,
        sent: 0,
        stopped: 0,
    });
    cx.update(|window, cx| {
        window.activate_window();
        let field = desk.read(cx).field.clone();
        window.focus(&field.focus_handle(cx), cx);
    });
    cx.run_until_parked();
    (desk, cx)
}

#[gpui::test]
fn enter_sends_a_reply_only_with_words(cx: &mut TestAppContext) {
    let (desk, cx) = open(false, cx);
    cx.simulate_keystrokes("space enter");
    assert_eq!(desk.read_with(cx, |desk, _| desk.sent), 0, "blank stays");
    cx.simulate_input("agreed");
    cx.simulate_keystrokes("enter");
    assert_eq!(desk.read_with(cx, |desk, _| desk.sent), 1);
}

#[gpui::test]
fn escape_stops_following_only_while_following(cx: &mut TestAppContext) {
    let (desk, cx) = open(true, cx);
    cx.simulate_keystrokes("escape");
    assert_eq!(desk.read_with(cx, |desk, _| desk.stopped), 1);
    desk.update(cx, |desk, cx| {
        desk.following = false;
        cx.notify();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    assert_eq!(desk.read_with(cx, |desk, _| desk.stopped), 1);
}

#[gpui::test]
fn following_begun_from_outside_still_stops_on_escape(cx: &mut TestAppContext) {
    let (desk, cx) = open(false, cx);
    cx.update(|window, cx| {
        let outside = desk.read(cx).outside.focus_handle(cx);
        window.focus(&outside, cx);
    });
    desk.update(cx, |desk, cx| {
        desk.following = true;
        cx.notify();
    });
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    assert_eq!(desk.read_with(cx, |desk, _| desk.stopped), 1);
    let back = cx.update(|window, cx| desk.read(cx).outside.focus_handle(cx).is_focused(window));
    assert!(back, "focus returns to where it was");
}
