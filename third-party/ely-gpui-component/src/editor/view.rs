use std::ops::Range;

use gpui::{
    AnyElement, Bounds, Context, ElementInputHandler, Hsla, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Render, SharedString,
    Styled, StyledText, TextRun, Window, canvas, div, fill, font, point, size, uniform_list,
};

use super::{
    decor::ReadOnlyBanner,
    keys::{CONTEXT, listen},
    layout::{Row, around, displayed_range, hidden, rows_with_wrap},
    state::{CodeEditor, EditorEvent},
    syntax::{self, Bracket, brackets, folds},
};
use crate::theme::ActiveTheme;

/// What one render worked out, for the rows the list builds after it.
#[derive(Default)]
pub(crate) struct Frame {
    pub rows: Vec<Row>,
    /// 每个逻辑行的视觉段；首段对应 `Row::Line`，其余段对应续行。
    pub wraps: Vec<Vec<Range<usize>>>,
    wrap_key: Option<WrapKey>,
    pub folds: Vec<(usize, usize)>,
    pub brackets: Vec<Bracket>,
    pub advance: Pixels,
    pub line: Pixels,
    pub matched: Option<(usize, usize)>,
    pub occurrences: Vec<Range<usize>>,
    /// The minimap's box, from its last paint.
    pub minimap: Bounds<Pixels>,
}

impl Frame {
    /// 正文或视觉布局参数变化后，下一帧必须重新生成行和软换行范围。
    pub(crate) fn invalidate(&mut self) {
        self.rows.clear();
        self.wraps.clear();
        self.wrap_key = None;
    }
}

/// 字体换行只依赖正文版本和实际绘制参数；折叠、标记等行结构变化不应触发全文 shaping。
#[derive(Clone, PartialEq)]
struct WrapKey {
    revision: u64,
    width: Pixels,
    font_family: SharedString,
    pixels: Pixels,
    advance: Pixels,
    gutter_columns: usize,
    enabled: bool,
    /// 多行 ghost 会截断所属逻辑行，截断位置变化时必须重新计算视觉段。
    ghost_cut: Option<usize>,
}

/// Lines of code a row's height holds, as leading.
const LEADING: f32 = 1.6;
/// Blocks the sticky header shows at most.
const STICKY: usize = 3;
/// Columns the minimap spans, in code characters.
const MINIMAP: f32 = 12.0;

impl CodeEditor {
    /// The text the primary selection or its word gives, for highlighting where else it appears.
    fn needle(&self) -> Option<String> {
        let primary = self.primary();
        let range = if primary.is_empty() {
            let word = self.buffer.word_at(primary.head);
            let text = &self.buffer.text()[word.clone()];
            text.chars()
                .all(|ch| ch.is_alphanumeric() || ch == '_')
                .then_some(word)?
        } else {
            primary.range()
        };
        (!range.is_empty()).then(|| self.buffer.text()[range].to_string())
    }

    /// The first line the list shows, after scrolling.
    fn top_line(&self) -> Option<usize> {
        let scrolled = -self.scroll.0.borrow().base_handle.offset().y;
        let first = (scrolled / self.frame.line).floor().max(0.0) as usize;
        self.frame.rows[first.min(self.frame.rows.len().saturating_sub(1))..]
            .iter()
            .find_map(|row| match row {
                Row::Line(line) | Row::Wrapped(line, ..) => Some(*line),
                _ => None,
            })
    }

    /// 将代码 Row 映射到逻辑行和实际绘制的字节范围。
    pub(crate) fn visual_range(&self, row: Row) -> Option<(usize, Range<usize>)> {
        match row {
            Row::Line(line) if line < self.buffer.lines() => {
                let range = if self.options.soft_wrap {
                    self.frame
                        .wraps
                        .get(line)
                        .and_then(|ranges| ranges.first().cloned())
                        .unwrap_or_else(|| displayed_range(&self.buffer, &self.marks, line))
                } else {
                    displayed_range(&self.buffer, &self.marks, line)
                };
                (range.start <= range.end
                    && range.end <= self.buffer.len()
                    && self.buffer.text().is_char_boundary(range.start)
                    && self.buffer.text().is_char_boundary(range.end))
                .then_some((line, range))
            }
            Row::Wrapped(line, start, end)
                if line < self.buffer.lines()
                    && start <= end
                    && end <= self.buffer.len()
                    && self.buffer.text().is_char_boundary(start)
                    && self.buffer.text().is_char_boundary(end) =>
            {
                Some((line, start..end))
            }
            _ => None,
        }
    }

    /// 查找 offset 所在的视觉代码行；位于换行边界时归入后一个续段。
    pub(crate) fn visual_row_at(&self, offset: usize) -> Option<(usize, usize, Range<usize>)> {
        let line = self.buffer.line_of(offset);
        let mut last = None;
        for (row, item) in self.frame.rows.iter().copied().enumerate() {
            let Some((item_line, range)) = self.visual_range(item) else {
                continue;
            };
            if item_line != line {
                continue;
            }
            if range.contains(&offset) || range.start == range.end && range.start == offset {
                return Some((row, item_line, range));
            }
            if range.end == offset {
                last = Some((row, item_line, range));
            }
        }
        last
    }

    /// 返回多行 ghost 对逻辑行产生的截断位置，单行 ghost 不影响代码换行。
    fn ghost_wrap_cut(&self) -> Option<usize> {
        let ghost = self.marks.ghost.as_ref()?;
        let line = self.buffer.line_of(ghost.offset);
        let range = self.buffer.line_range(line);
        (ghost.text.contains('\n') && range.contains(&ghost.offset)).then_some(ghost.offset)
    }

    /// 使用 GPUI 实际字体计算边界，避免按 Unicode 字符数估算宽度。
    fn wrapped_ranges(
        &self,
        width: Pixels,
        pixels: Pixels,
        mono_family: SharedString,
        advance: Pixels,
        color: Hsla,
        window: &Window,
    ) -> Vec<Vec<Range<usize>>> {
        if !self.options.soft_wrap || width <= Pixels::ZERO {
            return vec![Vec::new(); self.buffer.lines()];
        }
        let font = font(mono_family);
        let gutter = advance * (self.gutter_columns() + 1) as f32;
        let wrap_width = (width - gutter).max(advance);
        (0..self.buffer.lines())
            .map(|line| {
                let range = displayed_range(&self.buffer, &self.marks, line);
                if range.is_empty() {
                    return vec![range];
                }
                let text: SharedString = self.buffer.text()[range.clone()].to_owned().into();
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let Ok(lines) = window.text_system().shape_text(
                    text.clone(),
                    pixels,
                    &[run],
                    Some(wrap_width),
                    None,
                ) else {
                    return vec![range];
                };
                let Some(shaped) = lines.first() else {
                    return vec![range];
                };
                let mut starts = vec![0];
                for boundary in shaped.wrap_boundaries() {
                    let Some(run) = shaped.runs().get(boundary.run_ix) else {
                        continue;
                    };
                    let Some(glyph) = run.glyphs.get(boundary.glyph_ix) else {
                        continue;
                    };
                    if glyph.index > *starts.last().unwrap_or(&0) && glyph.index < text.len() {
                        starts.push(glyph.index);
                    }
                }
                starts.push(text.len());
                starts
                    .windows(2)
                    .map(|pair| range.start + pair[0]..range.start + pair[1])
                    .collect()
            })
            .collect()
    }

    fn sticky(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let colors = cx.theme().colors.clone();
        let top = self.top_line()?;
        let headers = around(&self.frame.folds, top, STICKY);
        if headers.is_empty() {
            return None;
        }
        let rows = headers.into_iter().map(|line| {
            let text = self.buffer.line(line).to_string();
            let styles = syntax::colors(&text, cx);
            let jump = cx.entity();
            div()
                .id(("sticky", line))
                .h(self.frame.line)
                .flex()
                .items_center()
                .cursor_pointer()
                .hover(|style| style.bg(colors.hover))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    window.prevent_default();
                    jump.update(cx, |editor, cx| {
                        let at = editor.buffer.line_range(line).start + editor.buffer.indent(line);
                        let cursor = at..at;
                        editor.select([cursor], cx);
                        editor.reveal(false, cx);
                    });
                })
                .child(
                    div()
                        .flex_none()
                        .w(self.frame.advance * (self.gutter_columns() + 1) as f32),
                )
                .child(
                    div()
                        .line_height(self.frame.line)
                        .whitespace_nowrap()
                        .text_color(colors.syntax.variable)
                        .child(StyledText::new(text).with_highlights(styles)),
                )
        });
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bg(colors.surface)
                .border_b_1()
                .border_color(colors.border)
                .children(rows)
                .into_any_element(),
        )
    }

    fn minimap_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors.clone();
        let (advance, line) = (self.frame.advance, self.frame.line);
        let tokens: Vec<Vec<(usize, usize, gpui::Hsla)>> = self
            .frame
            .rows
            .iter()
            .map(|row| {
                let Some((line, range)) = self.visual_range(*row) else {
                    return Vec::new();
                };
                let text = &self.buffer.text()[range.clone()];
                let full = displayed_range(&self.buffer, &self.marks, line);
                syntax::colors(&self.buffer.text()[full.clone()], cx)
                    .into_iter()
                    .filter_map(|(span, style)| {
                        let span = full.start + span.start..full.start + span.end;
                        let start = span.start.max(range.start);
                        let end = span.end.min(range.end);
                        (start < end).then(|| {
                            let local_start = start - range.start;
                            let local_end = end - range.start;
                            let columns = text[..local_start].chars().count();
                            let len = text[local_start..local_end].chars().count();
                            (columns, len, style.color)
                        })
                    })
                    .filter_map(|(start, len, color)| color.map(|color| (start, len, color)))
                    .collect()
            })
            .collect();
        let scroll = self.scroll.clone();
        let entity = cx.entity();
        let (drag, press) = (cx.entity(), cx.entity());
        let shade = colors.hover;
        div()
            .id("minimap")
            .relative()
            .flex_none()
            .w(advance * MINIMAP)
            .h_full()
            .border_l_1()
            .border_color(colors.border)
            .cursor_pointer()
            .child(
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |editor, _| editor.frame.minimap = bounds);
                    },
                    move |bounds, _, window, _| {
                        let rows = tokens.len().max(1) as f32;
                        let tall = (line * 0.15).min(bounds.size.height / rows);
                        let wide = advance * 0.12;
                        let state = scroll.0.borrow();
                        let viewport = state.base_handle.bounds().size.height;
                        let scrolled = -state.base_handle.offset().y;
                        let top = bounds.origin.y + scrolled / line * tall;
                        let extent = size(bounds.size.width, viewport / line * tall);
                        window.paint_quad(fill(
                            Bounds::new(point(bounds.origin.x, top), extent),
                            shade,
                        ));
                        for (ix, spans) in tokens.iter().enumerate() {
                            for (start, len, color) in spans {
                                let origin = point(
                                    bounds.origin.x + wide * (*start + 2) as f32,
                                    bounds.origin.y + tall * ix as f32,
                                );
                                let extent = size(wide * *len as f32, tall * 0.7);
                                window.paint_quad(fill(
                                    Bounds::new(origin, extent),
                                    color.opacity(0.7),
                                ));
                            }
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, _, cx| {
                press.update(cx, |editor, cx| editor.jump(event.position.y, cx));
            })
            .on_mouse_move(move |event: &MouseMoveEvent, _, cx| {
                if event.dragging() {
                    drag.update(cx, |editor, cx| editor.jump(event.position.y, cx));
                }
            })
            .into_any_element()
    }

    /// Scrolls so the row under a minimap point sits in the middle of the view.
    fn jump(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let bounds = self.frame.minimap;
        let rows = self.frame.rows.len().max(1) as f32;
        let tall = (self.frame.line * 0.15).min(bounds.size.height / rows);
        let row = ((y - bounds.origin.y) / tall).max(0.0);
        let state = self.scroll.0.borrow();
        let viewport = state.base_handle.bounds().size.height;
        let top = (self.frame.line * row - viewport / 2.0).max(Pixels::ZERO);
        state.base_handle.set_offset(point(Pixels::ZERO, -top));
        drop(state);
        cx.notify();
    }
}

impl Render for CodeEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sticky = if self.options.sticky {
            self.sticky(cx)
        } else {
            None
        };
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let pixels = gpui::px(theme.code.font_size_px);
        let mono = window
            .text_system()
            .resolve_font(&font(theme.code.font_family.clone()));
        let advance = window
            .text_system()
            .advance(mono, pixels, 'm')
            .expect("the code font has an m")
            .width;
        let line = (pixels * LEADING).round();
        let found = folds(&self.buffer);
        let hide = hidden(self.buffer.lines(), &found, &self.folded);
        let found_brackets = brackets(&self.buffer);
        let matched = syntax::matched(&found_brackets, self.primary().head);
        let occurrences = match self.needle() {
            Some(needle) if self.focus.is_focused(window) => {
                syntax::occurrences(&self.buffer, &needle, 0..self.buffer.lines())
            }
            _ => Vec::new(),
        };
        let minimap = self.frame.minimap;
        let wrap_key = WrapKey {
            revision: self.content_revision,
            width: self.metrics.width,
            font_family: theme.code.font_family.clone(),
            pixels,
            advance,
            gutter_columns: self.gutter_columns(),
            enabled: self.options.soft_wrap,
            ghost_cut: self.ghost_wrap_cut(),
        };
        if self.frame.wrap_key.as_ref() != Some(&wrap_key) {
            let wraps = self.wrapped_ranges(
                self.metrics.width,
                pixels,
                wrap_key.font_family.clone(),
                advance,
                colors.fg,
                window,
            );
            self.frame.wraps = wraps;
            self.frame.wrap_key = Some(wrap_key);
        }
        let rows = rows_with_wrap(&self.buffer, &hide, &self.marks, Some(&self.frame.wraps));
        self.frame.rows = rows;
        self.frame.folds = found;
        self.frame.brackets = found_brackets;
        self.frame.advance = advance;
        self.frame.line = line;
        self.frame.matched = matched;
        self.frame.occurrences = occurrences;
        self.frame.minimap = minimap;
        self.metrics.advance = advance;
        self.metrics.line = line;
        let count = self.frame.rows.len();
        let list = uniform_list(
            "editor-rows",
            count,
            cx.processor(|editor, range: Range<usize>, window, cx| {
                range.map(|ix| editor.row(ix, window, cx)).collect()
            }),
        )
        .track_scroll(&self.scroll)
        .size_full();
        let (top, focus, input) = (cx.entity(), self.focus.clone(), cx.entity());
        let unlock = cx.entity();
        let banner = self.options.read_only.then(|| {
            ReadOnlyBanner::new(
                ("read-only", cx.entity_id().as_u64()),
                "This file is read-only.",
            )
            .action("Make editable", move |_, cx| {
                unlock.update(cx, |editor, cx| {
                    editor.set_read_only(false, cx);
                    cx.emit(EditorEvent::Unlock);
                })
            })
        });
        let body = div()
            .relative()
            .flex_1()
            .h_full()
            .child(list)
            .children(sticky)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        top.update(cx, |editor, editor_cx| {
                            let changed = editor.metrics.top != bounds.origin.y
                                || editor.metrics.width != bounds.size.width;
                            editor.metrics.top = bounds.origin.y;
                            editor.metrics.width = bounds.size.width;
                            if changed {
                                editor_cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        let root = div()
            .id(("code-editor", cx.entity_id().as_u64()))
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(colors.surface)
            .font_family(theme.code.font_family.clone())
            .text_size(pixels)
            .text_color(colors.fg)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|editor, _, _, _| editor.dragging = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|editor, _, _, _| editor.dragging = false),
            )
            .children(banner)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(body)
                    .children(self.options.minimap.then(|| self.minimap_view(cx))),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, input.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        listen(root, cx)
    }
}
