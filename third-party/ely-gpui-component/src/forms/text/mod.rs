mod actions;
mod edit;
mod element;
mod geometry;
mod highlight;
mod ime;

use std::{ops::Range, rc::Rc, time::Duration};

use gpui::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, Pixels, Point, SharedString,
    Subscription, Task, TextAlign, Window, WrappedLine,
};

pub(crate) use actions::{Backspace, Down, Enter, Redo, Submit, Undo, Up, bind_keys};
pub use edit::History;
use edit::Snapshot;
pub(crate) use edit::{from_utf16, to_utf16};
pub use highlight::Highlight;

use crate::theme::{ActiveTheme, Theme};

/// What a text input tells its owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Changed,
    /// Enter in one line, or the submit key in several.
    Submit,
    Focus,
    Blur,
}

type Highlighter = Rc<dyn Fn(&str, &App) -> Vec<(Range<usize>, Highlight)>>;
type Filter = Rc<dyn Fn(char) -> bool>;
pub(crate) type Fit = Rc<dyn Fn(&str, Range<usize>, &str) -> (String, usize)>;

/// The last shaped text: one entry per hard line, with its display offset.
pub(crate) struct Layout {
    pub lines: Rc<[(usize, WrappedLine)]>,
    pub bounds: Bounds<Pixels>,
    pub line_height: Pixels,
    /// The lines show the placeholder, not content.
    pub placeholder: bool,
    /// 单行对齐偏移同时用于绘制、光标、选择和 IME，不能只改变文字绘制。
    pub text_offset: Point<Pixels>,
}

const BULLET: char = '•';
const BLINK: Duration = Duration::from_millis(530);

/// Editable text: one line or many, with selection, undo, IME and clipboard.
pub struct TextInput {
    focus: FocusHandle,
    text: String,
    placeholder: SharedString,
    label: SharedString,
    text_align: TextAlign,
    selection: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    composing: Option<Snapshot>,
    rows: Option<(usize, usize)>,
    masked: bool,
    filter: Option<Filter>,
    fit: Option<Fit>,
    max_len: Option<usize>,
    highlighter: Option<Highlighter>,
    disabled: bool,
    history: History<Snapshot>,
    /// The selection the last edit replaced.
    edited_from: Range<usize>,
    pub(crate) layout: Option<Layout>,
    pub(crate) scroll: Point<Pixels>,
    selecting: bool,
    caret_on: bool,
    /// Blink runs only while focused, or unseen carets redraw forever.
    focused: bool,
    // 仅在 reduced-motion 值变化时重建计时器，避免普通主题更新重置闪烁相位。
    blink_reduced_motion: bool,
    blink_epoch: u64,
    _blink: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<InputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextInput {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle().tab_stop(true);
        let subscriptions = vec![
            cx.on_focus(&focus, window, |input, _, cx| {
                input.focused = true;
                input.restart_blink(cx);
                cx.emit(InputEvent::Focus);
            }),
            cx.on_blur(&focus, window, |input, _, cx| {
                input.focused = false;
                input.selecting = false;
                input._blink = None;
                cx.emit(InputEvent::Blur);
                cx.notify();
            }),
            cx.observe_global_in::<Theme>(window, |input, _, cx| {
                input.sync_reduced_motion(cx);
            }),
        ];
        Self {
            focus,
            text: String::new(),
            placeholder: SharedString::default(),
            label: SharedString::default(),
            text_align: TextAlign::Left,
            selection: 0..0,
            reversed: false,
            marked: None,
            composing: None,
            rows: None,
            masked: false,
            filter: None,
            fit: None,
            max_len: None,
            highlighter: None,
            disabled: false,
            history: History::default(),
            edited_from: 0..0,
            layout: None,
            scroll: Point::default(),
            selecting: false,
            caret_on: true,
            focused: false,
            blink_reduced_motion: cx.theme().reduced_motion,
            blink_epoch: 0,
            _blink: None,
            _subscriptions: subscriptions,
        }
    }

    /// Several lines: grows from `min` rows to `max`, then scrolls.
    pub fn multi_line(mut self, min: usize, max: usize) -> Self {
        assert!(0 < min && min <= max, "text rows {min}..={max} are empty");
        self.rows = Some((min, max));
        self
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }

    /// The name assistive technology reads; the placeholder otherwise.
    pub fn label(mut self, text: impl Into<SharedString>) -> Self {
        self.label = text.into();
        self
    }

    /// Shows bullets instead of the text, and keeps it off the clipboard.
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// Drops typed or pasted characters `allow` rejects.
    pub fn filter(mut self, allow: impl Fn(char) -> bool + 'static) -> Self {
        self.filter = Some(Rc::new(allow));
        self
    }

    /// Longest text, in characters.
    pub fn max_len(mut self, len: usize) -> Self {
        self.max_len = Some(len);
        self
    }

    /// Colors spans of the text as it changes.
    pub fn highlighter(
        mut self,
        highlight: impl Fn(&str, &App) -> Vec<(Range<usize>, Highlight)> + 'static,
    ) -> Self {
        self.highlighter = Some(Rc::new(highlight));
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_masked(&self) -> bool {
        self.masked
    }

    pub fn is_multi_line(&self) -> bool {
        self.rows.is_some()
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    pub fn rows(&self) -> Option<(usize, usize)> {
        self.rows
    }

    pub fn placeholder_text(&self) -> &SharedString {
        &self.placeholder
    }

    pub fn label_text(&self) -> &SharedString {
        &self.label
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn cursor(&self) -> usize {
        if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = text.into();
        cx.notify();
    }

    pub fn set_label(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.label = text.into();
        cx.notify();
    }

    pub fn set_masked(&mut self, masked: bool, cx: &mut Context<Self>) {
        self.masked = masked;
        cx.notify();
    }

    pub fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        self.disabled = disabled;
        self.focus = self.focus.clone().tab_stop(!disabled);
        cx.notify();
    }

    /// Replaces all text and puts the caret at its end. Not an undo step.
    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = self.admit(&text.into(), 0);
        let text = match &self.fit {
            Some(fit) => fit(&self.text, 0..self.text.len(), &text).0,
            None => text,
        };
        self.selection = text.len()..text.len();
        self.text = text;
        self.marked = None;
        self.composing = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    /// Reshapes every edit, as a mask does: text, replaced range and typed text in; text and caret out.
    pub(crate) fn set_fit(&mut self, fit: Fit) {
        self.fit = Some(fit);
    }

    /// Replaces the selection, as typing would. An undo step.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.replace(self.selection.clone(), text, false, cx);
    }

    pub fn select(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        assert!(
            range.start <= range.end && range.end <= self.text.len(),
            "selection {range:?} is outside {} bytes",
            self.text.len()
        );
        self.selection = range;
        self.reversed = false;
        self.restart_blink(cx);
    }

    /// Keeps what `filter` and `max_len` allow of `incoming`.
    fn admit(&self, incoming: &str, keep: usize) -> String {
        let allowed: String = incoming
            .chars()
            .filter(|ch| self.filter.as_ref().is_none_or(|allow| allow(*ch)))
            .collect();
        match self.max_len {
            Some(max) => allowed.chars().take(max.saturating_sub(keep)).collect(),
            None => allowed,
        }
    }

    /// The selection the last edit replaced, for an owner's undo to go back to.
    pub(crate) fn edited_from(&self) -> Range<usize> {
        self.edited_from.clone()
    }

    pub(crate) fn replace(
        &mut self,
        range: Range<usize>,
        incoming: &str,
        typing: bool,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        self.edited_from = self.selection.clone();
        let incoming = if self.rows.is_none() {
            incoming.replace(['\n', '\r'], " ")
        } else {
            incoming.replace("\r\n", "\n")
        };
        let kept = self.text.chars().count() - self.text[range.clone()].chars().count();
        let incoming = self.admit(&incoming, kept);
        if incoming.is_empty() && range.is_empty() {
            return;
        }
        let composed = self.composing.take();
        let before = composed.clone().unwrap_or_else(|| self.snapshot());
        let caret = match &self.fit {
            Some(fit) => {
                let (base, range) = match &composed {
                    Some(start) => (start.text.as_str(), start.selection.clone()),
                    None => (self.text.as_str(), range),
                };
                let (text, caret) = fit(base, range, &incoming);
                self.text = text;
                caret
            }
            None => {
                self.text.replace_range(range.clone(), &incoming);
                range.start + incoming.len()
            }
        };
        self.selection = caret..caret;
        self.reversed = false;
        self.marked = None;
        self.commit(before, typing, cx);
        self.restart_blink(cx);
    }

    /// Makes `before` one undo step, if the text moved on from it.
    fn commit(&mut self, before: Snapshot, typing: bool, cx: &mut Context<Self>) {
        if before.text != self.text {
            self.history.record(before, typing);
            cx.emit(InputEvent::Changed);
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
        }
    }

    pub(crate) fn restore(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.text = snapshot.text;
        self.selection = snapshot.selection;
        self.reversed = false;
        self.marked = None;
        self.composing = None;
        cx.emit(InputEvent::Changed);
        self.restart_blink(cx);
    }

    pub(crate) fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selection = offset..offset;
        self.reversed = false;
        self.restart_blink(cx);
    }

    pub(crate) fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.reversed = !self.reversed;
            self.selection = self.selection.end..self.selection.start;
        }
        self.restart_blink(cx);
    }

    /// reduced-motion 设置变化时重建插入符计时器。
    fn sync_reduced_motion(&mut self, cx: &mut Context<Self>) {
        let reduced_motion = cx.theme().reduced_motion;
        if self.blink_reduced_motion == reduced_motion {
            return;
        }
        self.blink_reduced_motion = reduced_motion;
        self.restart_blink(cx);
    }

    /// 单行输入的内容对齐；多行仍采用左对齐，避免改变换行与垂直导航语义。
    pub fn text_align(mut self, align: TextAlign) -> Self {
        self.text_align = align;
        self
    }

    /// Shows the caret now and restarts its blink.
    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.caret_on = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        let still = cx.theme().reduced_motion;
        self._blink = (!still && self.focused).then(|| {
            cx.spawn(async move |input, cx| {
                loop {
                    cx.background_executor().timer(BLINK).await;
                    let alive = input.update(cx, |input, cx| {
                        if input.blink_epoch == epoch {
                            input.caret_on = !input.caret_on;
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        return;
                    }
                }
            })
        });
        cx.notify();
    }

    /// What the element shows: bullets when masked.
    pub(crate) fn display_text(&self) -> String {
        if self.masked {
            BULLET.to_string().repeat(self.grapheme_count())
        } else {
            self.text.clone()
        }
    }

    fn grapheme_count(&self) -> usize {
        unicode_segmentation::UnicodeSegmentation::graphemes(self.text.as_str(), true).count()
    }

    pub(crate) fn display_offset(&self, offset: usize) -> usize {
        if self.masked {
            edit::masked_offset(&self.text, offset, BULLET)
        } else {
            offset
        }
    }

    pub(crate) fn content_offset(&self, display: usize) -> usize {
        if self.masked {
            edit::unmasked_offset(&self.text, display, BULLET)
        } else {
            display
        }
    }

    pub(crate) fn highlights(&self, cx: &App) -> Vec<(Range<usize>, Highlight)> {
        match (&self.highlighter, self.masked) {
            (Some(highlight), false) => highlight(&self.text, cx),
            _ => Vec::new(),
        }
    }

    pub(crate) fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub(crate) fn selecting(&self) -> bool {
        self.selecting
    }

    pub(crate) fn caret_on(&self) -> bool {
        self.caret_on
    }

    pub(crate) fn focus(&self) -> &FocusHandle {
        &self.focus
    }
}
