use std::rc::Rc;

use gpui::{
    App, ElementId, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*,
};
use jiff::Timestamp;

use super::{
    peers::Peer,
    reactions::{Reaction, Reactions},
};
use crate::{
    buttons::{Button, ButtonVariant, SegmentedControl},
    feedback::EmptyState,
    forms::{Enter, Input, OnFlag, OnValue, Run, TextInput},
    overlays::Popover,
    primitives::IconName,
    theme::{ActiveTheme, AvatarSize, ControlSize, Radius, TextSize},
    typography::RelativeTime,
};

/// A comment: who wrote it, when, what, and the reactions it has.
#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub author: Peer,
    pub at: Timestamp,
    pub body: SharedString,
    pub reactions: Vec<Reaction>,
}

/// A conversation on a document: its key, the words it is about, its comments in order, and whether it is resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    pub key: SharedString,
    pub quote: Option<SharedString>,
    pub comments: Vec<Comment>,
    pub resolved: bool,
}

type OnReact = Rc<dyn Fn(usize, &SharedString, &mut Window, &mut App)>;
type OnThreadFlag = Rc<dyn Fn(&SharedString, bool, &mut Window, &mut App)>;
type OnThreadReact = Rc<dyn Fn(&SharedString, usize, &SharedString, &mut Window, &mut App)>;

/// A field to answer in and a button to send; Enter sends too, never an empty answer.
#[derive(IntoElement)]
pub struct Reply {
    id: ElementId,
    field: Entity<TextInput>,
    on_send: Run,
}

impl Reply {
    pub fn new(
        id: impl Into<ElementId>,
        field: &Entity<TextInput>,
        on_send: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            field: field.clone(),
            on_send: Rc::new(on_send),
        }
    }
}

impl RenderOnce for Reply {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let empty = self.field.read(cx).text().trim().is_empty();
        let (send, enter) = (self.on_send.clone(), self.on_send);
        div()
            .flex()
            .items_center()
            .gap_2()
            .capture_action(move |_: &Enter, window, cx| {
                cx.stop_propagation();
                if !empty {
                    log::info!("reply: sent");
                    enter(window, cx);
                }
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.field).size(ControlSize::Sm)),
            )
            .child(
                Button::new((self.id.clone(), "send"), "Reply")
                    .variant(ButtonVariant::Primary)
                    .size(ControlSize::Sm)
                    .disabled(empty)
                    .on_click(move |_, window, cx| {
                        log::info!("reply: sent");
                        send(window, cx)
                    }),
            )
    }
}

/// A conversation on a document: the words it is about, each comment with its author, time and reactions, a reply below, and resolve or reopen above; a resolved thread quiets down and takes no replies.
#[derive(IntoElement)]
pub struct CommentThread {
    id: ElementId,
    thread: Thread,
    active: bool,
    reply: Option<(Entity<TextInput>, Run)>,
    on_resolve: Option<OnFlag>,
    on_react: Option<OnReact>,
}

impl CommentThread {
    pub fn new(id: impl Into<ElementId>, thread: Thread) -> Self {
        assert!(
            !thread.comments.is_empty(),
            "thread {} has no comment",
            thread.key
        );
        Self {
            id: id.into(),
            thread,
            active: false,
            reply: None,
            on_resolve: None,
            on_react: None,
        }
    }

    /// Marks the thread the reader is on.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Answers in `field`; `on_send` takes its text.
    pub fn reply(
        mut self,
        field: &Entity<TextInput>,
        on_send: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.reply = Some((field.clone(), Rc::new(on_send)));
        self
    }

    /// Gets true to resolve, false to reopen.
    pub fn on_resolve(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_resolve = Some(Rc::new(handler));
        self
    }

    /// Gets a comment's index and the emoji to toggle on it.
    pub fn on_react(
        mut self,
        handler: impl Fn(usize, &SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_react = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CommentThread {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let resolved = self.thread.resolved;
        let resolve = self.on_resolve.clone().map(|resolve| {
            Button::new(
                (self.id.clone(), "resolve"),
                if resolved { "Reopen" } else { "Resolve" },
            )
            .variant(ButtonVariant::Ghost)
            .size(ControlSize::Sm)
            .icon(if resolved {
                IconName::RotateCcw
            } else {
                IconName::Check
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                log::info!("comment thread: resolved {}", !resolved);
                resolve(!resolved, window, cx)
            })
        });
        let comments = self
            .thread
            .comments
            .iter()
            .enumerate()
            .map(|(ix, comment)| {
                let react = self.on_react.clone();
                let reactions = (!comment.reactions.is_empty() || react.is_some()).then(|| {
                    let reactions = Reactions::new(
                        (self.id.clone(), format!("reactions-{ix}")),
                        comment.reactions.clone(),
                    );
                    match react {
                        Some(react) => reactions
                            .on_toggle(move |emoji, window, cx| react(ix, emoji, window, cx)),
                        None => reactions,
                    }
                });
                div()
                    .flex()
                    .gap_2()
                    .child(
                        comment
                            .author
                            .avatar((self.id.clone(), format!("author-{ix}")), AvatarSize::Xs),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(comment.author.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(theme.text_size(TextSize::Xs))
                                            .text_color(colors.fg_subtle)
                                            .child(RelativeTime::new(
                                                (self.id.clone(), format!("when-{ix}")),
                                                comment.at,
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(if resolved { colors.fg_muted } else { colors.fg })
                                    .child(comment.body.clone()),
                            )
                            .children(reactions),
                    )
            });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(theme.radius(Radius::Lg))
            .border_1()
            .border_color(if self.active {
                colors.focus
            } else {
                colors.border
            })
            .bg(colors.surface)
            .text_size(theme.text_size(TextSize::Sm))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .children(self.thread.quote.clone().map(|quote| {
                                div()
                                    .pl_2()
                                    .border_l_2()
                                    .border_color(colors.border_strong)
                                    .text_color(colors.fg_muted)
                                    .italic()
                                    .child(quote)
                            })),
                    )
                    .children(resolve),
            )
            .children(comments)
            .when(!resolved, |thread| {
                thread.children(self.reply.map(|(field, send)| {
                    Reply::new((self.id.clone(), "reply"), &field, move |window, cx| {
                        send(window, cx)
                    })
                }))
            })
    }
}

/// A comment's mark beside the words it is about, with how many comments it holds; a press opens the thread in a bubble over the page.
#[derive(IntoElement)]
pub struct CommentMarker {
    id: ElementId,
    thread: Thread,
    reply: Option<(Entity<TextInput>, Run)>,
    on_resolve: Option<OnFlag>,
}

impl CommentMarker {
    pub fn new(id: impl Into<ElementId>, thread: Thread) -> Self {
        Self {
            id: id.into(),
            thread,
            reply: None,
            on_resolve: None,
        }
    }

    pub fn reply(
        mut self,
        field: &Entity<TextInput>,
        on_send: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.reply = Some((field.clone(), Rc::new(on_send)));
        self
    }

    pub fn on_resolve(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_resolve = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CommentMarker {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let prose = cx.theme().prose_width();
        let count = self.thread.comments.len().to_string();
        let (id, thread, reply, resolve) =
            (self.id.clone(), self.thread, self.reply, self.on_resolve);
        Popover::new(self.id, count, move |_, _| {
            let mut bubble = CommentThread::new((id.clone(), "thread"), thread);
            if let Some((field, send)) = reply {
                bubble = bubble.reply(&field, move |window, cx| send(window, cx));
            }
            if let Some(resolve) = resolve {
                bubble = bubble.on_resolve(move |on, window, cx| resolve(on, window, cx));
            }
            div().w(prose).child(bubble)
        })
        .icon(IconName::MessageSquare)
        .variant(ButtonVariant::Ghost)
        .size(ControlSize::Sm)
    }
}

/// Every conversation on a document down a column, the open ones or the resolved: a press picks one, and the one picked takes replies.
#[derive(IntoElement)]
pub struct CommentSidebar {
    id: ElementId,
    threads: Vec<Thread>,
    active: Option<SharedString>,
    reply: Option<(Entity<TextInput>, Run)>,
    on_select: Option<OnValue>,
    on_resolve: Option<OnThreadFlag>,
    on_react: Option<OnThreadReact>,
}

impl CommentSidebar {
    pub fn new(id: impl Into<ElementId>, threads: impl IntoIterator<Item = Thread>) -> Self {
        Self {
            id: id.into(),
            threads: threads.into_iter().collect(),
            active: None,
            reply: None,
            on_select: None,
            on_resolve: None,
            on_react: None,
        }
    }

    /// The thread picked, by key, and what a press on another asks.
    pub fn active(
        mut self,
        active: Option<SharedString>,
        on_select: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.active = active;
        self.on_select = Some(Rc::new(on_select));
        self
    }

    /// Answers the picked thread in `field`.
    pub fn reply(
        mut self,
        field: &Entity<TextInput>,
        on_send: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.reply = Some((field.clone(), Rc::new(on_send)));
        self
    }

    /// Gets a thread's key and true to resolve it, false to reopen.
    pub fn on_resolve(
        mut self,
        handler: impl Fn(&SharedString, bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_resolve = Some(Rc::new(handler));
        self
    }

    /// Gets a thread's key, a comment's index and the emoji to toggle on it.
    pub fn on_react(
        mut self,
        handler: impl Fn(&SharedString, usize, &SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_react = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for CommentSidebar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let showing = window.use_keyed_state((self.id.clone(), "resolved"), cx, |_, _| false);
        let resolved = *showing.read(cx);
        let open = self
            .threads
            .iter()
            .filter(|thread| !thread.resolved)
            .count();
        let done = self.threads.len() - open;
        let filter = SegmentedControl::new(
            (self.id.clone(), "filter"),
            if resolved { "resolved" } else { "open" },
        )
        .size(ControlSize::Sm)
        .segment("open", format!("Open {open}"), None)
        .segment("resolved", format!("Resolved {done}"), None)
        .on_change(move |value, _, cx| {
            showing.update(cx, |showing, cx| {
                *showing = value.as_ref() == "resolved";
                cx.notify();
            })
        });
        let shown: Vec<Thread> = self
            .threads
            .into_iter()
            .filter(|thread| thread.resolved == resolved)
            .collect();
        let empty = shown.is_empty().then(|| {
            EmptyState::new(
                (self.id.clone(), "empty"),
                IconName::MessageSquare,
                if resolved {
                    "Nothing resolved yet"
                } else {
                    "No open comments"
                },
            )
        });
        let threads = shown.into_iter().map(|thread| {
            let key = thread.key.clone();
            let active = self.active.as_ref() == Some(&key);
            let mut item = CommentThread::new((self.id.clone(), format!("thread-{key}")), thread)
                .active(active);
            if active && let Some((field, send)) = &self.reply {
                let send = send.clone();
                item = item.reply(field, move |window, cx| send(window, cx));
            }
            if let Some(resolve) = self.on_resolve.clone() {
                let key = key.clone();
                item = item.on_resolve(move |on, window, cx| resolve(&key, on, window, cx));
            }
            if let Some(react) = self.on_react.clone() {
                let key = key.clone();
                item =
                    item.on_react(move |ix, emoji, window, cx| react(&key, ix, emoji, window, cx));
            }
            let select = self.on_select.clone();
            div()
                .id((self.id.clone(), format!("pick-{key}")))
                .when_some(select.filter(|_| !active), |row, select| {
                    row.on_click(move |_, window, cx| {
                        log::info!("comment sidebar: {key}");
                        select(&key, window, cx)
                    })
                })
                .child(item)
        });
        div()
            .id(self.id.clone())
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .overflow_y_scroll()
            .child(filter)
            .children(empty)
            .children(threads)
    }
}
