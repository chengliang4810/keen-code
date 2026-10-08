use std::ops::Range;

use gpui::{App, FontWeight};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;

use crate::{forms::Highlight, theme::ActiveTheme};

/// The markdown this crate reads: GitHub's tables, strikethrough and task lists, footnotes, and math between dollars.
pub(crate) fn options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH
}

/// How a stretch of markdown prints while it is edited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Strong,
    Emphasis,
    Strike,
    Code,
    Link,
    Heading(u8),
    Quote,
    /// The characters that write the marks: asterisks, backticks, brackets, hashes.
    Syntax,
}

/// The run of `ch` that starts `text`, in bytes.
fn leading(text: &str, ch: char) -> usize {
    text.chars()
        .take_while(|found| *found == ch)
        .map(char::len_utf8)
        .sum()
}

/// How many bytes at each end of `span` write `mark`: its asterisks, tildes or backticks.
pub(crate) fn edge(span: &str, mark: Mark) -> usize {
    let first = span.chars().next().expect("a styled span has its marker");
    let run = leading(span, first).min(span.len() / 2);
    match mark {
        Mark::Emphasis => run.min(first.len_utf8()),
        _ => run,
    }
}

/// The marks over `text`'s bytes: styles, code, links, headings and quotes, and the syntax that writes each.
pub fn marks(text: &str) -> Vec<(Range<usize>, Mark)> {
    let mut out = Vec::new();
    let mut links: Vec<(Range<usize>, Option<Range<usize>>)> = Vec::new();
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        let span = &text[range.clone()];
        match event {
            Event::Start(Tag::Strong | Tag::Emphasis | Tag::Strikethrough) => {
                let mark = match event {
                    Event::Start(Tag::Strong) => Mark::Strong,
                    Event::Start(Tag::Emphasis) => Mark::Emphasis,
                    _ => Mark::Strike,
                };
                let edge = edge(span, mark);
                out.push((range.clone(), mark));
                out.push((range.start..range.start + edge, Mark::Syntax));
                out.push((range.end - edge..range.end, Mark::Syntax));
            }
            Event::Code(_) => {
                let ticks = edge(span, Mark::Code);
                out.push((range.clone(), Mark::Code));
                out.push((range.start..range.start + ticks, Mark::Syntax));
                out.push((range.end - ticks..range.end, Mark::Syntax));
            }
            Event::Start(Tag::Link { .. }) => links.push((range, None)),
            Event::Text(_) if !links.is_empty() => {
                let (_, inner) = links.last_mut().expect("inside a link");
                *inner = Some(match inner.take() {
                    Some(inner) => inner.start..range.end,
                    None => range,
                });
            }
            Event::End(TagEnd::Link) => {
                let (whole, inner) = links.pop().expect("a link ends after it starts");
                out.push((whole.clone(), Mark::Link));
                if let Some(inner) = inner {
                    out.push((whole.start..inner.start, Mark::Syntax));
                    out.push((inner.end..whole.end, Mark::Syntax));
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let hashes = leading(span, '#');
                out.push((range.clone(), Mark::Heading(level as u8)));
                if hashes > 0 {
                    let spaced = hashes + leading(&span[hashes..], ' ');
                    out.push((range.start..range.start + spaced, Mark::Syntax));
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                out.push((range.clone(), Mark::Quote));
                let mut at = range.start;
                for line in span.split_inclusive('\n') {
                    let marker = line.len() - line.trim_start_matches(['>', ' ']).len();
                    out.push((at..at + marker, Mark::Syntax));
                    at += line.len();
                }
            }
            _ => {}
        }
    }
    out.retain(|(range, _)| !range.is_empty());
    out
}

/// Markdown styled as it is typed: strong and headings heavier, emphasis slanted, strikes struck, code washed, links in their tone; the syntax that writes them recedes.
pub fn markdown_highlights(text: &str, cx: &App) -> Vec<(Range<usize>, Highlight)> {
    let colors = &cx.theme().colors;
    let plain = Highlight::new(colors.fg);
    marks(text)
        .into_iter()
        .map(|(range, mark)| {
            let range = match mark {
                Mark::Strike => {
                    let span = &text[range.clone()];
                    let edge = span.len() - span.trim_start_matches('~').len();
                    range.start + edge..range.end - edge
                }
                _ => range,
            };
            let highlight = match mark {
                Mark::Strong | Mark::Heading(_) => Highlight {
                    weight: Some(FontWeight::SEMIBOLD),
                    ..plain
                },
                Mark::Emphasis => Highlight {
                    italic: true,
                    ..plain
                },
                Mark::Strike => Highlight {
                    strike: true,
                    ..Highlight::new(colors.fg_muted)
                },
                Mark::Code => Highlight {
                    background: Some(colors.sunken),
                    ..plain
                },
                Mark::Link => Highlight::new(colors.link),
                Mark::Quote => Highlight::new(colors.fg_muted),
                Mark::Syntax => Highlight::new(colors.fg_subtle),
            };
            (range, highlight)
        })
        .collect()
}

/// A link written `[text](address)` around byte `at`: its range, its text and its address.
pub fn link_at(text: &str, at: usize) -> Option<(Range<usize>, String, String)> {
    Parser::new_ext(text, options())
        .into_offset_iter()
        .find_map(|(event, range)| match event {
            Event::Start(Tag::Link { dest_url, .. })
                if range.start <= at && at <= range.end && text[range.clone()].starts_with('[') =>
            {
                let written = &text[range.clone()];
                let label = &written[1..written.rfind("](")?];
                Some((range, label.to_string(), dest_url.to_string()))
            }
            _ => None,
        })
}

/// The document's top-level blocks, as ranges of its source.
pub fn block_ranges(text: &str) -> Vec<Range<usize>> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        match event {
            Event::Start(_) => {
                if depth == 0 {
                    out.push(range);
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            Event::Rule if depth == 0 => out.push(range),
            _ => {}
        }
    }
    out.into_iter()
        .map(|range| {
            let kept = text[range.clone()].trim_end_matches(['\n', '\r']).len();
            range.start..range.start + kept
        })
        .collect()
}

/// Words in `text` as a reader counts them; each ideograph counts as one.
pub fn words(text: &str) -> usize {
    text.unicode_words().count()
}

/// Words an adult reads in a minute, on average.
const PACE: usize = 238;

/// Minutes to read `words`, at least one when there are any.
pub fn reading_minutes(words: usize) -> usize {
    words.div_ceil(PACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(&str, Mark)> {
        marks(text)
            .into_iter()
            .map(|(range, mark)| (&text[range], mark))
            .collect()
    }

    #[test]
    fn styles_carry_their_markers_as_syntax() {
        use Mark::*;
        assert_eq!(
            found("a **bold** and *it* ~~no~~"),
            [
                ("**bold**", Strong),
                ("**", Syntax),
                ("**", Syntax),
                ("*it*", Emphasis),
                ("*", Syntax),
                ("*", Syntax),
                ("~~no~~", Strike),
                ("~~", Syntax),
                ("~~", Syntax),
            ]
        );
    }

    #[test]
    fn code_links_and_headings_mark_their_syntax() {
        use Mark::*;
        assert_eq!(found("`x`"), [("`x`", Code), ("`", Syntax), ("`", Syntax)]);
        assert_eq!(
            found("see [the docs](https://ely.dev)"),
            [
                ("[the docs](https://ely.dev)", Link),
                ("[", Syntax),
                ("](https://ely.dev)", Syntax)
            ]
        );
        assert_eq!(found("## Lift"), [("## Lift", Heading(2)), ("## ", Syntax)]);
    }

    #[test]
    fn a_link_gives_its_range_text_and_address() {
        let text = "see [the docs](https://ely.dev) or <https://x.dev>";
        assert_eq!(
            link_at(text, 6),
            Some((4..31, "the docs".to_string(), "https://ely.dev".to_string()))
        );
        assert_eq!(link_at(text, 1), None);
        assert_eq!(link_at(text, 40), None, "an autolink has no text to edit");
    }

    #[test]
    fn blocks_are_the_top_level_stretches() {
        let text = "# Lift\n\nBlend **it**.\n\n- a\n- b\n\n---\n";
        let blocks: Vec<&str> = block_ranges(text)
            .into_iter()
            .map(|range| &text[range])
            .collect();
        assert_eq!(blocks, ["# Lift", "Blend **it**.", "- a\n- b", "---"]);
    }

    #[test]
    fn reading_takes_a_minute_per_pace() {
        assert_eq!(words("Blend toward white, by lift."), 5);
        assert_eq!(words("你好世界"), 4);
        assert_eq!(
            (reading_minutes(0), reading_minutes(1), reading_minutes(239)),
            (0, 1, 2)
        );
    }
}
