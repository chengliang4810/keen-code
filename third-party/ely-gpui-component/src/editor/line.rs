use std::ops::Range;

use gpui::{
    AnyElement, Context, FontWeight, HighlightStyle, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Stateful, Styled, StyledText, UnderlineStyle,
    Window, canvas, div, prelude::*,
};

use super::{
    cursor::Selection,
    gutter::Cell,
    layout::{Shown, stack},
    state::{CodeEditor, CursorShape},
    syntax,
};
use crate::{
    charts::tint,
    theme::{ActiveTheme, Radius},
};

impl CodeEditor {
    /// A line's own styles: syntax colors, rainbow brackets, problems, washes, matches and the matched pair.
    fn styles(
        &self,
        line: usize,
        range: &Range<usize>,
        cx: &Context<Self>,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let colors = &cx.theme().colors;
        let local = |span: &Range<usize>| {
            span.start.max(range.start) - range.start..span.end.min(range.end) - range.start
        };
        let touches = |span: &Range<usize>| {
            span.start < range.end.max(range.start + 1) && range.start < span.end
        };
        let full = self.row_range(line);
        let mut spans: Vec<(Range<usize>, HighlightStyle)> =
            syntax::colors(&self.buffer.text()[full.clone()], cx)
                .into_iter()
                .filter_map(|(span, style)| {
                    let span = full.start + span.start..full.start + span.end;
                    let start = span.start.max(range.start);
                    let end = span.end.min(range.end);
                    (start < end).then_some((start - range.start..end - range.start, style))
                })
                .collect();
        let brackets = self
            .frame
            .brackets
            .iter()
            .filter(|bracket| range.contains(&bracket.offset));
        for bracket in brackets {
            let at = bracket.offset - range.start;
            let color = if self.options.rainbow {
                tint(colors, bracket.depth % 3)
            } else {
                colors.syntax.punctuation
            };
            spans.retain(|(span, _)| *span != (at..at + 1));
            spans.push((at..at + 1, color.into()));
        }
        for span in self.frame.occurrences.iter().filter(|span| touches(span)) {
            spans.push((
                local(span),
                HighlightStyle {
                    background_color: Some(colors.selection.opacity(0.45)),
                    ..HighlightStyle::default()
                },
            ));
        }
        for (span, color) in self
            .marks
            .backgrounds
            .iter()
            .filter(|(span, _)| touches(span))
        {
            spans.push((
                local(span),
                HighlightStyle {
                    background_color: Some(*color),
                    ..HighlightStyle::default()
                },
            ));
        }
        for diagnostic in self
            .marks
            .diagnostics
            .iter()
            .filter(|diagnostic| touches(&diagnostic.range))
        {
            spans.push((
                local(&diagnostic.range),
                HighlightStyle {
                    underline: Some(UnderlineStyle {
                        thickness: cx.theme().underline_thickness(),
                        color: Some(diagnostic.severity.color(colors)),
                        wavy: true,
                    }),
                    ..HighlightStyle::default()
                },
            ));
        }
        if let Some((open, close)) = self.frame.matched {
            for at in [open, close].into_iter().filter(|at| range.contains(at)) {
                let at = at - range.start;
                spans.push((
                    at..at + 1,
                    HighlightStyle {
                        background_color: Some(colors.active),
                        font_weight: Some(FontWeight::BOLD),
                        ..HighlightStyle::default()
                    },
                ));
            }
        }
        stack(spans)
    }

    /// The columns guides stand at on a line: every tab stop inside its indent, or the next line's when it is blank.
    fn guides(&self, line: usize) -> Vec<usize> {
        let indent = (line..self.buffer.lines())
            .find(|at| !self.buffer.line(*at).trim().is_empty())
            .map_or(0, |at| self.buffer.indent(at));
        (0..indent).step_by(self.options.tab).collect()
    }

    /// Dots where the line has spaces and arrows for tabs, spaces elsewhere, so they fall on the text's columns.
    fn whitespace(
        &self,
        line: usize,
        range: &Range<usize>,
        shown: &Shown,
        cx: &Context<Self>,
    ) -> String {
        let text = &self.buffer.text()[range.clone()];
        let notes = self.notes_in(line, range, cx);
        let mut out = String::new();
        let mut note = notes.iter().peekable();
        for (at, ch) in text.char_indices() {
            while let Some((_, words, _)) = note.next_if(|(place, ..)| *place <= at) {
                out.extend(std::iter::repeat_n(' ', words.chars().count()));
            }
            out.push(match ch {
                ' ' => '·',
                '\t' => '→',
                _ => ' ',
            });
        }
        debug_assert!(out.chars().count() <= shown.text.chars().count());
        out
    }

    pub(crate) fn line_row(
        &self,
        base: Stateful<gpui::Div>,
        row: usize,
        line: usize,
        range: Range<usize>,
        continuation: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let (advance, height) = (self.frame.advance, self.frame.line);
        let text = &self.buffer.text()[range.clone()];
        let shown = Shown::new(
            text,
            self.notes_in(line, &range, cx),
            self.styles(line, &range, cx),
        );
        let x = |column: usize| advance * (column + 1) as f32;
        let focused = self.focus.is_focused(window);
        let primary = self.primary();
        let current = primary.is_empty() && self.buffer.line_of(primary.head) == line;
        let column_of = |offset: usize| {
            let offset = offset.clamp(range.start, range.end);
            let column = self.buffer.text()[range.start..offset].chars().count();
            shown.column(text, column)
        };
        let last_segment = range.end == self.row_range(line).end;
        let selections = self.selections.iter().filter_map(|selection| {
            let span = selection.range();
            if span.is_empty() || span.end <= range.start || span.start >= range.end {
                return None;
            }
            let start = column_of(span.start.max(range.start));
            let past = span.end > range.end;
            let end = if past {
                column_of(range.end) + 1
            } else {
                column_of(span.end.min(range.end))
            };
            (end > start).then(|| {
                div()
                    .absolute()
                    .top_0()
                    .left(x(start))
                    .w(advance * (end - start) as f32)
                    .h(height)
                    .bg(if focused {
                        colors.selection
                    } else {
                        colors.selection.opacity(0.5)
                    })
            })
        });
        let active = self
            .frame
            .folds
            .iter()
            .filter(|(header, end)| {
                let at = self.buffer.line_of(primary.head);
                *header < at && at <= *end
            })
            .map(|(header, _)| self.buffer.indent(*header))
            .next_back();
        let guides = if self.options.guides {
            self.guides(line)
        } else {
            Default::default()
        };
        let rulers = self.options.rulers.iter().map(|column| {
            div()
                .absolute()
                .top_0()
                .left(x(*column))
                .h(height)
                .w_0()
                .border_l_1()
                .border_color(colors.border.opacity(0.6))
        });
        let guides = guides.into_iter().map(|column| {
            let strong = active == Some(column);
            div()
                .absolute()
                .top_0()
                .left(x(column))
                .h(height)
                .w_0()
                .border_l_1()
                .border_color(if strong {
                    colors.border_strong
                } else {
                    colors.border.opacity(0.6)
                })
        });
        let carets = self
            .selections
            .iter()
            .filter(|selection| self.buffer.line_of(selection.head) == line)
            .filter(|selection| {
                (range.start..range.end).contains(&selection.head)
                    || (last_segment && selection.head == range.end)
            })
            .filter(|_| focused && self.caret_on)
            .map(|selection| {
                let left = x(column_of(selection.head));
                let caret = theme.caret_width().to_pixels(window.rem_size());
                let mark = div().absolute().left(left);
                match self.options.cursor {
                    CursorShape::Line => mark.top_0().w(caret).h(height).bg(colors.focus),
                    CursorShape::Block => mark
                        .top_0()
                        .w(advance)
                        .h(height)
                        .bg(colors.focus.opacity(0.35)),
                    CursorShape::Underline => mark
                        .top(height - caret * 1.5)
                        .w(advance)
                        .h(caret * 1.5)
                        .bg(colors.focus),
                }
            });
        let dots = self.options.whitespace.then(|| {
            div()
                .absolute()
                .top_0()
                .left(x(0))
                .h(height)
                .line_height(height)
                .whitespace_nowrap()
                .text_color(colors.fg_subtle.opacity(0.6))
                .child(self.whitespace(line, &range, &shown, cx))
        });
        let folded = (!continuation && last_segment && self.folded.contains(&line)).then(|| {
            let entity = cx.entity();
            div()
                .id(("unfold", line))
                .absolute()
                .top_0()
                .left(x(shown.text.chars().count() + 1))
                .h(height)
                .flex()
                .items_center()
                .child(
                    div()
                        .px_1()
                        .rounded(theme.radius(Radius::Sm))
                        .bg(colors.hover)
                        .text_color(colors.fg_muted)
                        .line_height(height * 0.8)
                        .child("⋯"),
                )
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.prevent_default();
                    entity.update(cx, |editor, cx| editor.toggle_fold(line, cx));
                })
        });
        let added = self
            .marks
            .hunks
            .iter()
            .any(|hunk| (hunk.line..hunk.line + hunk.added).contains(&line));
        let entity = cx.entity();
        let left = x(0);
        let press_range = range.clone();
        let drag_range = range.clone();
        let code = div()
            .id(("code", row))
            .relative()
            .flex_1()
            .h_full()
            .overflow_hidden()
            .when(current && focused, |code| code.bg(colors.hover))
            .when(added, |code| code.bg(colors.success.opacity(0.08)))
            .children(selections)
            .children(guides)
            .children(rulers)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(left)
                    .h(height)
                    .line_height(height)
                    .whitespace_nowrap()
                    .text_color(colors.syntax.variable)
                    .child(
                        StyledText::new(shown.text.clone()).with_highlights(shown.styles.clone()),
                    ),
            )
            .children(dots)
            .children(folded)
            .children(carets)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let origin = bounds.origin.x + left;
                        entity.update(cx, |editor, _| {
                            if editor.metrics.left != origin {
                                editor.metrics.left = origin;
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |editor, event: &MouseDownEvent, window, cx| {
                    editor.press(line, press_range.clone(), event, window, cx)
                }),
            )
            .on_mouse_move(cx.listener(move |editor, event: &MouseMoveEvent, _, cx| {
                editor.drag(line, drag_range.clone(), event, cx)
            }));
        base.child(self.gutter(
            if continuation {
                Cell::Blank
            } else {
                Cell::Line(line)
            },
            row,
            cx,
        ))
        .child(code)
        .into_any_element()
    }

    /// A press on a line: one caret, a word on a double press, the line on a triple; Shift extends, Alt adds a caret.
    pub(crate) fn press(
        &mut self,
        line: usize,
        range: Range<usize>,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let at = self.offset_in_range(line, range, event.position.x, cx);
        let chosen = match event.click_count {
            2 => {
                let word = self.buffer.word_at(at);
                vec![Selection {
                    anchor: word.start,
                    head: word.end,
                    goal: None,
                }]
            }
            3.. => {
                let range = self.buffer.line_range(line);
                vec![Selection {
                    anchor: range.start,
                    head: self.buffer.next(range.end),
                    goal: None,
                }]
            }
            _ if event.modifiers.shift => {
                let mut all = self.selections.clone();
                let primary = all.last_mut().expect("an editor keeps a cursor");
                primary.head = at;
                primary.goal = None;
                all
            }
            _ if event.modifiers.alt => {
                let mut all = self.selections.clone();
                all.push(Selection::caret(at));
                all
            }
            _ => {
                self.dragging = true;
                vec![Selection::caret(at)]
            }
        };
        self.set_selections(chosen, cx);
    }

    /// A drag over a line stretches the primary selection to where the pointer is.
    pub(crate) fn drag(
        &mut self,
        line: usize,
        range: Range<usize>,
        event: &MouseMoveEvent,
        cx: &mut Context<Self>,
    ) {
        if !self.dragging || !event.dragging() {
            return;
        }
        let at = self.offset_in_range(line, range, event.position.x, cx);
        let mut all = self.selections.clone();
        let primary = all.last_mut().expect("an editor keeps a cursor");
        if primary.head == at {
            return;
        }
        primary.head = at;
        primary.goal = None;
        self.set_selections(all, cx);
    }
}
