use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, EntityInputHandler, FontStyle, HighlightStyle, Pixels,
    Point, UTF16Selection, Window, point,
};

use super::{
    cursor::Selection,
    layout::{Shown, displayed_range},
    state::CodeEditor,
};
use crate::{
    forms::{from_utf16, to_utf16},
    theme::ActiveTheme,
};

impl CodeEditor {
    /// Notes set into a line: inlay hints, and the ghost text's first line.
    pub(crate) fn notes(&self, line: usize, cx: &App) -> Vec<(usize, String, HighlightStyle)> {
        let colors = &cx.theme().colors;
        let range = self.row_range(line);
        let hint = HighlightStyle {
            color: Some(colors.fg_subtle),
            background_color: Some(colors.hover),
            ..HighlightStyle::default()
        };
        let mut notes: Vec<(usize, String, HighlightStyle)> = self
            .marks
            .hints
            .iter()
            .filter(|hint| range.contains(&hint.offset) || hint.offset == range.end)
            .map(|note| (note.offset - range.start, note.text.to_string(), hint))
            .collect();
        if let Some(ghost) = &self.marks.ghost
            && self.buffer.line_of(ghost.offset) == line
        {
            let first = ghost.text.split('\n').next().unwrap_or_default();
            let style = HighlightStyle {
                color: Some(colors.fg_subtle),
                font_style: Some(FontStyle::Italic),
                ..HighlightStyle::default()
            };
            notes.push((ghost.offset - range.start, first.to_string(), style));
        }
        notes
    }

    /// 过滤到一个视觉段，换行边界上的 note 只归入后一个段。
    pub(crate) fn notes_in(
        &self,
        line: usize,
        range: &Range<usize>,
        cx: &App,
    ) -> Vec<(usize, String, HighlightStyle)> {
        let full = self.row_range(line);
        let last = range.end == full.end;
        self.notes(line, cx)
            .into_iter()
            .filter_map(|(at, text, style)| {
                let at = full.start + at;
                (at >= range.start && (at < range.end || (last && at == range.end))).then_some((
                    at - range.start,
                    text,
                    style,
                ))
            })
            .collect()
    }

    /// A line's bytes as its row shows them: a multi-line ghost cuts the rest to follow its last line.
    pub(crate) fn row_range(&self, line: usize) -> Range<usize> {
        displayed_range(&self.buffer, &self.marks, line)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn row_text(&self, line: usize) -> &str {
        &self.buffer.text()[self.row_range(line)]
    }

    /// Ends a composition: all of it, from its first mark, is one undo step.
    fn end_composition(&mut self) {
        if let Some(start) = self.composing.take()
            && start.text != self.buffer.text()
        {
            self.history.record(start, false);
        }
    }

    /// The offset under a window point, when a row of code is there.
    pub(crate) fn offset_at(&self, position: Point<Pixels>, cx: &App) -> Option<usize> {
        let metrics = self.metrics;
        if metrics.line <= Pixels::ZERO || metrics.advance <= Pixels::ZERO {
            return None;
        }
        let scrolled = -self.scroll.0.borrow().base_handle.offset().y;
        let row = ((position.y - metrics.top + scrolled) / metrics.line).floor();
        let row = self.frame.rows.get(row.max(0.0) as usize)?;
        let (line, range) = self.visual_range(*row)?;
        Some(self.offset_in_range(line, range, position.x, cx))
    }

    /// 将窗口横坐标映射为视觉段内最近的 buffer 字节 offset。
    pub(crate) fn offset_in_range(
        &self,
        _line: usize,
        range: Range<usize>,
        x: Pixels,
        cx: &App,
    ) -> usize {
        let text = &self.buffer.text()[range.clone()];
        let shown = Shown::new(text, self.notes_in(_line, &range, cx), Vec::new());
        let column = ((x - self.metrics.left) / self.metrics.advance)
            .round()
            .max(0.0) as usize;
        let source_column = shown.back(text, column);
        let byte = text
            .char_indices()
            .nth(source_column)
            .map_or(text.len(), |(at, _)| at);
        range.start + byte
    }

    fn utf16_range(&self, range: &Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        to_utf16(text, range.start)..to_utf16(text, range.end)
    }

    fn byte_range(&self, range: &Range<usize>) -> Range<usize> {
        let text = self.buffer.text();
        from_utf16(text, range.start)..from_utf16(text, range.end)
    }
}

impl EntityInputHandler for CodeEditor {
    fn paste(&mut self, item: ClipboardItem, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.paste(&text, cx);
        }
    }

    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.byte_range(&range_utf16);
        actual_range.replace(self.utf16_range(&range));
        Some(self.buffer.text()[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let primary = self.primary();
        Some(UTF16Selection {
            range: self.utf16_range(&primary.range()),
            reversed: primary.head < primary.anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.utf16_range(range))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        self.end_composition();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .map(|range| self.byte_range(&range))
            .or(self.marked.clone());
        match range {
            Some(range) => {
                self.marked = None;
                self.apply(vec![(range, text.to_string())], true, cx);
            }
            None => self.type_text(text, cx),
        }
        self.end_composition();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        let range = range_utf16
            .map(|range| self.byte_range(&range))
            .or(self.marked.clone())
            .unwrap_or(self.primary().range());
        if self.composing.is_none() {
            self.composing = Some(self.snapshot());
        }
        self.apply(vec![(range.clone(), text.to_string())], true, cx);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        if self.marked.is_none() {
            self.end_composition();
        }
        let caret = match selected_utf16 {
            Some(inner) => range.start + from_utf16(text, inner.end),
            None => range.start + text.len(),
        };
        self.set_selections(vec![Selection::caret(caret)], cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.byte_range(&range_utf16);
        let (row, line, visual) = self.visual_row_at(range.start)?;
        let text = &self.buffer.text()[visual.clone()];
        let shown = Shown::new(text, self.notes_in(line, &visual, cx), Vec::new());
        let at = range.start.clamp(visual.start, visual.end) - visual.start;
        let column = shown.column(text, text[..at].chars().count());
        let metrics = self.metrics;
        let scrolled = -self.scroll.0.borrow().base_handle.offset().y;
        let origin = point(
            metrics.left + metrics.advance * column as f32,
            metrics.top + metrics.line * row as f32 - scrolled,
        );
        Some(Bounds::new(
            origin,
            gpui::size(metrics.advance, metrics.line),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let offset = self.offset_at(position, cx)?;
        Some(to_utf16(self.buffer.text(), offset))
    }
}
