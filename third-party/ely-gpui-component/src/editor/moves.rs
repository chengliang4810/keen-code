use std::ops::Range;

use gpui::{Context, ScrollStrategy};

use super::{
    cursor::{Motion, Selection, merged, moved},
    layout::{hidden, rows},
    state::CodeEditor,
    syntax::folds,
};

impl CodeEditor {
    /// 收集可垂直移动的视觉代码行，跳过 lens、diff、ghost 和 inline chat 行。
    fn visual_code_rows(&self) -> Vec<(usize, usize, Range<usize>)> {
        self.frame
            .rows
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(row, item)| {
                self.visual_range(item)
                    .map(|(line, range)| (row, line, range))
            })
            .collect()
    }

    /// 在视觉段之间移动光标，并保留段内目标列。
    fn moved_visual(&self, selection: Selection, motion: Motion, extend: bool) -> Selection {
        let rows = self.visual_code_rows();
        let Some((row, _, current_range)) = self.visual_row_at(selection.head) else {
            return moved(&self.buffer, selection, motion, extend);
        };
        let Some(current) = rows.iter().position(|(at, _, _)| *at == row) else {
            return moved(&self.buffer, selection, motion, extend);
        };
        let delta = match motion {
            Motion::Up => -1,
            Motion::Down => 1,
            Motion::Page(lines) => lines,
            _ => return moved(&self.buffer, selection, motion, extend),
        };
        let target =
            (current as isize + delta).clamp(0, rows.len().saturating_sub(1) as isize) as usize;
        let (_, _, target_range) = &rows[target];
        let current_column = selection.goal.unwrap_or_else(|| {
            self.buffer.text()
                [current_range.start..selection.head.clamp(current_range.start, current_range.end)]
                .chars()
                .count()
        });
        let target_text = &self.buffer.text()[target_range.clone()];
        let target_offset = target_text
            .char_indices()
            .nth(current_column)
            .map_or(target_range.end, |(at, _)| target_range.start + at);
        Selection {
            anchor: if extend {
                selection.anchor
            } else {
                target_offset
            },
            head: target_offset,
            goal: Some(current_column),
        }
    }

    /// Moves every cursor. Up and down step over folded lines.
    pub(crate) fn motion(&mut self, motion: Motion, extend: bool, cx: &mut Context<Self>) {
        let hide = hidden(self.buffer.lines(), &folds(&self.buffer), &self.folded);
        let vertical = matches!(motion, Motion::Up | Motion::Down | Motion::Page(_));
        let moved: Vec<Selection> = self
            .selections
            .iter()
            .map(|selection| {
                if self.options.soft_wrap && vertical {
                    return self.moved_visual(*selection, motion, extend);
                }
                let mut next = moved(&self.buffer, *selection, motion, extend);
                while vertical && hide[self.buffer.line_of(next.head)] {
                    let again = moved(&self.buffer, next, motion, extend);
                    if again.head == next.head {
                        break;
                    }
                    next = again;
                }
                next
            })
            .collect();
        let up = matches!(
            motion,
            Motion::Up | Motion::Start | Motion::LineStart | Motion::WordLeft | Motion::Left
        ) || matches!(motion, Motion::Page(lines) if lines < 0);
        self.set_selections(moved, cx);
        self.reveal(!up, cx);
    }

    pub(crate) fn select_all(&mut self, cx: &mut Context<Self>) {
        let all = 0..self.buffer.len();
        self.select([all], cx);
    }

    /// A new cursor a line above or below the primary one, at its column.
    pub(crate) fn add_cursor(&mut self, below: bool, cx: &mut Context<Self>) {
        let primary = self.primary();
        let motion = if below { Motion::Down } else { Motion::Up };
        let next = if self.options.soft_wrap {
            self.moved_visual(Selection::caret(primary.head), motion, false)
        } else {
            moved(&self.buffer, Selection::caret(primary.head), motion, false)
        };
        if next.head == primary.head {
            return;
        }
        let mut all = self.selections.clone();
        all.push(Selection { goal: None, ..next });
        self.set_selections(all, cx);
        self.reveal(below, cx);
    }

    /// Selects the word at the primary cursor, then each next place its text appears.
    pub(crate) fn select_next(&mut self, cx: &mut Context<Self>) {
        let primary = self.primary();
        if primary.is_empty() {
            let word = self.buffer.word_at(primary.head);
            let mut all = self.selections.clone();
            all.pop();
            all.push(Selection {
                anchor: word.start,
                head: word.end,
                goal: None,
            });
            return self.set_selections(all, cx);
        }
        let needle = self.buffer.text()[primary.range()].to_string();
        let text = self.buffer.text();
        let found = text[primary.range().end..]
            .find(&needle)
            .map(|at| primary.range().end + at)
            .or_else(|| text.find(&needle));
        let Some(start) = found else {
            return;
        };
        let range = start..start + needle.len();
        if self
            .selections
            .iter()
            .any(|selection| selection.range() == range)
        {
            return;
        }
        let mut all = self.selections.clone();
        all.push(Selection {
            anchor: range.start,
            head: range.end,
            goal: None,
        });
        self.set_selections(all, cx);
        self.reveal(true, cx);
    }

    /// Escape: one cursor again, where the primary one was, and no ghost text.
    pub(crate) fn single_cursor(&mut self, cx: &mut Context<Self>) {
        let primary = self.primary();
        self.marks.ghost = None;
        self.set_selections(vec![Selection::caret(primary.head)], cx);
    }

    /// Folds the block that opens on a line, or opens it again; cursors inside move to its header.
    pub(crate) fn toggle_fold(&mut self, line: usize, cx: &mut Context<Self>) {
        if !self.folded.remove(&line) {
            let Some((_, end)) = folds(&self.buffer)
                .into_iter()
                .find(|(header, _)| *header == line)
            else {
                return;
            };
            let header_end = self.buffer.line_range(line).end;
            let kept: Vec<Selection> = self
                .selections
                .iter()
                .map(|selection| {
                    if (line + 1..=end).contains(&self.buffer.line_of(selection.head)) {
                        Selection::caret(header_end)
                    } else {
                        *selection
                    }
                })
                .collect();
            self.selections = merged(kept);
            self.folded.insert(line);
        }
        log::info!("code editor: folded {:?}", self.folded);
        cx.notify();
    }

    /// Opens every fold that hides a cursor.
    pub(crate) fn unfold_cursors(&mut self) {
        if self.folded.is_empty() {
            return;
        }
        let folds = folds(&self.buffer);
        let lines: Vec<usize> = self
            .selections
            .iter()
            .map(|selection| self.buffer.line_of(selection.head))
            .collect();
        self.folded.retain(|header| {
            let end = folds
                .iter()
                .find(|(start, _)| start == header)
                .map(|(_, end)| *end);
            end.is_some_and(|end| !lines.iter().any(|line| (header + 1..=end).contains(line)))
        });
    }

    /// The row the primary cursor draws on.
    pub(crate) fn primary_row(&self) -> usize {
        if let Some((row, _, _)) = self.visual_row_at(self.primary().head) {
            return row;
        }
        let line = self.buffer.line_of(self.primary().head);
        let hide = hidden(self.buffer.lines(), &folds(&self.buffer), &self.folded);
        rows(&self.buffer, &hide, &self.marks)
            .iter()
            .position(|row| matches!(row, super::layout::Row::Line(at) if *at == line))
            .expect("the primary cursor sits on a shown line")
    }

    /// Scrolls the primary cursor into view: from below when moving down, from above when moving up.
    pub(crate) fn reveal(&mut self, down: bool, cx: &mut Context<Self>) {
        let strategy = if down {
            ScrollStrategy::Bottom
        } else {
            ScrollStrategy::Top
        };
        self.scroll.scroll_to_item(self.primary_row(), strategy);
        cx.notify();
    }
}
