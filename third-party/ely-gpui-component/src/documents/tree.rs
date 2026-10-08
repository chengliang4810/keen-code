use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};

use super::markdown::options;

/// How a stretch of inline text prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Strong,
    Emphasis,
    Strike,
    Code,
    Link,
    Math,
    Footnote,
}

/// Inline content: its text, the styled stretches of it, and the address of each link, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub styles: Vec<(Range<usize>, Style)>,
    pub links: Vec<(Range<usize>, String)>,
}

/// A block of a markdown document.
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Paragraph(Inline),
    Heading(u8, Inline),
    Quote(Vec<Node>),
    Code {
        language: Option<String>,
        text: String,
    },
    /// Items with their blocks, numbered from `start` when ordered, each with its checkbox when a task.
    List {
        start: Option<u64>,
        items: Vec<(Option<bool>, Vec<Node>)>,
    },
    Table {
        aligns: Vec<Alignment>,
        head: Vec<Inline>,
        rows: Vec<Vec<Inline>>,
    },
    Rule,
    Math(String),
    Image {
        alt: String,
        url: String,
    },
    Footnote {
        label: String,
        body: Vec<Node>,
    },
}

/// Parses into nodes as it goes: a stack of open containers and the inline text being built.
#[derive(Default)]
struct Builder {
    stack: Vec<(Frame, Vec<Node>)>,
    inline: Inline,
    open: Vec<(usize, Style)>,
    link: Vec<(usize, String)>,
    image: Option<(String, String)>,
    code: Option<(Option<String>, String)>,
    head: Vec<Inline>,
    row: Vec<Inline>,
    rows: Vec<Vec<Inline>>,
}

enum Frame {
    Quote,
    List(Option<u64>, Vec<(Option<bool>, Vec<Node>)>),
    Item(Option<bool>),
    Table(Vec<Alignment>),
    Footnote(String),
}

impl Builder {
    fn take_inline(&mut self) -> Inline {
        std::mem::take(&mut self.inline)
    }
}

/// `markdown` as blocks.
pub fn parse(markdown: &str) -> Vec<Node> {
    let mut built = Builder::default();
    let mut out: Vec<Node> = Vec::new();
    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::TableCell) => {}
            Event::End(TagEnd::Paragraph) => {
                let text = built.take_inline();
                if !text.text.is_empty() {
                    push(&mut built, &mut out, Node::Paragraph(text));
                }
            }
            Event::End(TagEnd::Heading(level)) => {
                let text = built.take_inline();
                push(&mut built, &mut out, Node::Heading(depth(level), text));
            }
            Event::Start(Tag::BlockQuote(_)) => built.stack.push((Frame::Quote, Vec::new())),
            Event::Start(Tag::List(start)) => {
                flush(&mut built, &mut out);
                built
                    .stack
                    .push((Frame::List(start, Vec::new()), Vec::new()));
            }
            Event::Start(Tag::Item) => built.stack.push((Frame::Item(None), Vec::new())),
            Event::TaskListMarker(done) => {
                if let Some((Frame::Item(task), _)) = built.stack.last_mut() {
                    *task = Some(done);
                }
            }
            Event::End(TagEnd::Item) => {
                flush(&mut built, &mut out);
                let (frame, nodes) = built.stack.pop().expect("an item was open");
                let Frame::Item(task) = frame else {
                    unreachable!("an item closes an item")
                };
                if let Some((Frame::List(_, items), _)) = built.stack.last_mut() {
                    items.push((task, nodes));
                }
            }
            Event::End(TagEnd::List(_)) => {
                let (frame, _) = built.stack.pop().expect("a list was open");
                let Frame::List(start, items) = frame else {
                    unreachable!("a list closes a list")
                };
                push(&mut built, &mut out, Node::List { start, items });
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                let (_, nodes) = built.stack.pop().expect("a quote was open");
                push(&mut built, &mut out, Node::Quote(nodes));
            }
            Event::Start(Tag::FootnoteDefinition(label)) => built
                .stack
                .push((Frame::Footnote(label.to_string()), Vec::new())),
            Event::End(TagEnd::FootnoteDefinition) => {
                let (frame, body) = built.stack.pop().expect("a footnote was open");
                let Frame::Footnote(label) = frame else {
                    unreachable!("a footnote closes a footnote")
                };
                push(&mut built, &mut out, Node::Footnote { label, body });
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match kind {
                    CodeBlockKind::Fenced(name) if !name.is_empty() => Some(name.to_string()),
                    _ => None,
                };
                built.code = Some((language, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                let (language, text) = built.code.take().expect("a code block was open");
                push(
                    &mut built,
                    &mut out,
                    Node::Code {
                        language,
                        text: text.trim_end_matches('\n').to_string(),
                    },
                );
            }
            Event::Start(Tag::Table(aligns)) => {
                built.stack.push((Frame::Table(aligns), Vec::new()))
            }
            Event::End(TagEnd::TableCell) => {
                let cell = built.take_inline();
                built.row.push(cell);
            }
            Event::End(TagEnd::TableHead) => built.head = std::mem::take(&mut built.row),
            Event::End(TagEnd::TableRow) => {
                let row = std::mem::take(&mut built.row);
                built.rows.push(row);
            }
            Event::End(TagEnd::Table) => {
                let (frame, _) = built.stack.pop().expect("a table was open");
                let Frame::Table(aligns) = frame else {
                    unreachable!("a table closes a table")
                };
                let (head, rows) = (
                    std::mem::take(&mut built.head),
                    std::mem::take(&mut built.rows),
                );
                push(&mut built, &mut out, Node::Table { aligns, head, rows });
            }
            Event::Rule => push(&mut built, &mut out, Node::Rule),
            Event::DisplayMath(math) => push(&mut built, &mut out, Node::Math(math.to_string())),
            Event::Start(Tag::Image { dest_url, .. }) => {
                built.image = Some((String::new(), dest_url.to_string()))
            }
            Event::End(TagEnd::Image) => {
                let (alt, url) = built.image.take().expect("an image was open");
                push(&mut built, &mut out, Node::Image { alt, url });
            }
            Event::Start(Tag::Strong) => built.open.push((built.inline.text.len(), Style::Strong)),
            Event::Start(Tag::Emphasis) => {
                built.open.push((built.inline.text.len(), Style::Emphasis))
            }
            Event::Start(Tag::Strikethrough) => {
                built.open.push((built.inline.text.len(), Style::Strike))
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                let (start, style) = built.open.pop().expect("a style closes after it opens");
                let end = built.inline.text.len();
                built.inline.styles.push((start..end, style));
            }
            Event::Start(Tag::Link { dest_url, .. }) => built
                .link
                .push((built.inline.text.len(), dest_url.to_string())),
            Event::End(TagEnd::Link) => {
                let (start, url) = built.link.pop().expect("a link closes after it opens");
                let range = start..built.inline.text.len();
                built.inline.styles.push((range.clone(), Style::Link));
                built.inline.links.push((range, url));
            }
            Event::Text(text) => match (&mut built.code, &mut built.image) {
                (Some((_, code)), _) => code.push_str(&text),
                (None, Some((alt, _))) => alt.push_str(&text),
                (None, None) => built.inline.text.push_str(&text),
            },
            Event::Code(code) => styled(&mut built.inline, &code, Style::Code),
            Event::InlineMath(math) => styled(&mut built.inline, &math, Style::Math),
            Event::FootnoteReference(label) => {
                styled(&mut built.inline, &format!("[{label}]"), Style::Footnote)
            }
            Event::SoftBreak => built.inline.text.push(' '),
            Event::HardBreak => built.inline.text.push('\n'),
            _ => {}
        }
    }
    flush(&mut built, &mut out);
    out
}

fn styled(inline: &mut Inline, text: &str, style: Style) {
    let start = inline.text.len();
    inline.text.push_str(text);
    inline.styles.push((start..inline.text.len(), style));
}

fn depth(level: HeadingLevel) -> u8 {
    level as u8
}

/// A finished node goes into the open container, or the document.
fn push(built: &mut Builder, out: &mut Vec<Node>, node: Node) {
    match built.stack.last_mut() {
        Some((_, nodes)) => nodes.push(node),
        None => out.push(node),
    }
}

/// Text left from a tight list item becomes its paragraph.
fn flush(built: &mut Builder, out: &mut Vec<Node>) {
    if !built.inline.text.is_empty() {
        let text = built.take_inline();
        push(built, out, Node::Paragraph(text));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_nest_as_written() {
        let nodes = parse(
            "# Lift\n\nBlend **toward** [white](https://ely.dev).\n\n- [x] clamp\n- [ ] gamma\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n---\n",
        );
        assert!(matches!(&nodes[0], Node::Heading(1, text) if text.text == "Lift"));
        let Node::Paragraph(line) = &nodes[1] else {
            panic!("a paragraph, not {:?}", nodes[1])
        };
        assert_eq!(line.text, "Blend toward white.");
        assert_eq!(line.styles, [(6..12, Style::Strong), (13..18, Style::Link)]);
        assert_eq!(line.links, [(13..18, "https://ely.dev".to_string())]);
        let Node::List { start: None, items } = &nodes[2] else {
            panic!("a list, not {:?}", nodes[2])
        };
        assert_eq!(
            items.iter().map(|(task, _)| *task).collect::<Vec<_>>(),
            [Some(true), Some(false)]
        );
        assert!(matches!(&items[0].1[0], Node::Paragraph(text) if text.text == "clamp"));
        assert!(matches!(&nodes[3], Node::Quote(inner) if inner.len() == 1));
        assert_eq!(
            nodes[4],
            Node::Code {
                language: Some("rust".into()),
                text: "fn main() {}".into()
            }
        );
        assert_eq!(nodes[5], Node::Rule);
    }

    #[test]
    fn display_math_stands_alone() {
        assert_eq!(parse("$$c = 1$$"), [Node::Math("c = 1".into())]);
    }

    #[test]
    fn tables_keep_their_cells_and_alignment() {
        let nodes = parse("| Name | Lift |\n|:--|--:|\n| accent | 0.5 |\n");
        let Node::Table { aligns, head, rows } = &nodes[0] else {
            panic!("a table, not {:?}", nodes[0])
        };
        assert_eq!(aligns, &[Alignment::Left, Alignment::Right]);
        assert_eq!(
            head.iter()
                .map(|cell| cell.text.as_str())
                .collect::<Vec<_>>(),
            ["Name", "Lift"]
        );
        assert_eq!(rows[0][1].text, "0.5");
    }
}
