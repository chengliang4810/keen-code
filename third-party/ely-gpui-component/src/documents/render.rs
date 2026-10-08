use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, Entity, FontStyle, FontWeight, HighlightStyle, InteractiveText,
    IntoElement, ParentElement, RenderOnce, SharedString, StrikethroughStyle, Styled, StyledText,
    UnderlineStyle, Window, div, prelude::*,
};
use pulldown_cmark::Alignment;

use super::tree::{Inline, Node, Style, parse};
use crate::{
    editor::code_colors,
    navigation::{Anchor, Sections},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, IconSize, Radius, TextSize},
    typography::Latex,
};

type OnLink = Rc<dyn Fn(&str, &mut Window, &mut App)>;
type DrawCode = Rc<dyn Fn(ElementId, Option<SharedString>, SharedString) -> AnyElement>;

/// Markdown as a document: headings, prose with its styles and links, lists and tasks, quotes, code colored as code, tables, rules, math and footnotes. A press on a link opens it, or hands it to the host.
#[derive(IntoElement)]
pub struct MarkdownRenderer {
    id: ElementId,
    source: SharedString,
    on_link: Option<OnLink>,
    size: TextSize,
    sections: Option<Entity<Sections>>,
    code: Option<DrawCode>,
}

impl MarkdownRenderer {
    pub fn new(id: impl Into<ElementId>, source: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            on_link: None,
            size: TextSize::Base,
            sections: None,
            code: None,
        }
    }

    /// The body's size; headings keep theirs.
    pub fn text_size(mut self, size: TextSize) -> Self {
        self.size = size;
        self
    }

    /// Marks each heading for a table of contents over `sections`; `outline` gives its entries.
    pub fn sections(mut self, sections: &Entity<Sections>) -> Self {
        self.sections = Some(sections.clone());
        self
    }

    /// Draws code blocks with the owner's element, given an id, the language and the code.
    pub fn code(
        mut self,
        draw: impl Fn(ElementId, Option<SharedString>, SharedString) -> AnyElement + 'static,
    ) -> Self {
        self.code = Some(Rc::new(draw));
        self
    }

    /// Takes link presses instead of the browser.
    pub fn on_link(mut self, handler: impl Fn(&str, &mut Window, &mut App) + 'static) -> Self {
        self.on_link = Some(Rc::new(handler));
        self
    }
}

/// Renders nodes with one set of links handlers and ids under one owner.
struct Draw<'a> {
    id: &'a ElementId,
    on_link: &'a Option<OnLink>,
    next: std::cell::Cell<usize>,
    sections: &'a Option<Entity<Sections>>,
    headings: std::cell::Cell<usize>,
    code: &'a Option<DrawCode>,
}

impl Draw<'_> {
    fn key(&self, what: &str) -> ElementId {
        let at = self.next.get();
        self.next.set(at + 1);
        (self.id.clone(), format!("{what}-{at}")).into()
    }

    fn inline(&self, inline: &Inline, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let colors = &theme.colors;
        let rule = theme.underline_thickness();
        let styles: Vec<_> = inline
            .styles
            .iter()
            .map(|(range, style)| {
                let highlight = match style {
                    Style::Strong => HighlightStyle {
                        font_weight: Some(FontWeight::SEMIBOLD),
                        ..HighlightStyle::default()
                    },
                    Style::Emphasis => HighlightStyle {
                        font_style: Some(FontStyle::Italic),
                        ..HighlightStyle::default()
                    },
                    Style::Strike => HighlightStyle {
                        strikethrough: Some(StrikethroughStyle {
                            thickness: rule,
                            color: Some(colors.fg_muted),
                        }),
                        ..HighlightStyle::default()
                    },
                    Style::Code | Style::Math => HighlightStyle {
                        background_color: Some(colors.sunken),
                        color: Some(if *style == Style::Math {
                            colors.syntax.function
                        } else {
                            colors.fg
                        }),
                        ..HighlightStyle::default()
                    },
                    Style::Link => HighlightStyle {
                        color: Some(colors.link),
                        underline: Some(UnderlineStyle {
                            thickness: rule,
                            color: Some(colors.link.opacity(0.4)),
                            wavy: false,
                        }),
                        ..HighlightStyle::default()
                    },
                    Style::Footnote => HighlightStyle {
                        color: Some(colors.fg_subtle),
                        ..HighlightStyle::default()
                    },
                };
                (range.clone(), highlight)
            })
            .collect();
        let styled =
            StyledText::new(inline.text.clone()).with_highlights(crate::editor::stack(styles));
        if inline.links.is_empty() {
            return styled.into_any_element();
        }
        let (ranges, urls): (Vec<_>, Vec<_>) = inline.links.iter().cloned().unzip();
        let on_link = self.on_link.clone();
        InteractiveText::new(self.key("links"), styled)
            .on_click(ranges, move |ix, window, cx| {
                let url = &urls[ix];
                log::info!("markdown: open {url}");
                match &on_link {
                    Some(on_link) => on_link(url, window, cx),
                    None => cx.open_url(url),
                }
            })
            .into_any_element()
    }

    fn nodes(&self, nodes: &[Node], cx: &App) -> Vec<AnyElement> {
        nodes.iter().map(|node| self.node(node, cx)).collect()
    }

    fn node(&self, node: &Node, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        match node {
            Node::Paragraph(inline) => div()
                .text_color(colors.fg)
                .child(self.inline(inline, cx))
                .into_any_element(),
            Node::Heading(level, inline) => {
                let size = match level {
                    1 => TextSize::Xxl,
                    2 => TextSize::Xl,
                    3 => TextSize::Lg,
                    _ => TextSize::Md,
                };
                let heading = div()
                    .pt_2()
                    .text_size(theme.text_size(size))
                    .font_weight(if *level <= 2 {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .text_color(colors.fg)
                    .child(self.inline(inline, cx));
                let ordinal = self.headings.get();
                self.headings.set(ordinal + 1);
                match self.sections {
                    Some(sections) => Anchor::new(sections, anchor(ordinal))
                        .child(heading)
                        .into_any_element(),
                    None => heading.into_any_element(),
                }
            }
            Node::Quote(inner) => div()
                .flex()
                .flex_col()
                .gap_2()
                .pl_4()
                .border_l_2()
                .border_color(colors.border_strong)
                .text_color(colors.fg_muted)
                .children(self.nodes(inner, cx))
                .into_any_element(),
            Node::Code { language, text } => {
                if let Some(draw) = self.code {
                    return draw(
                        self.key("code"),
                        language.clone().map(Into::into),
                        text.clone().into(),
                    );
                }
                let styles = code_colors(text, cx);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded(theme.radius(Radius::Md))
                    .bg(colors.sunken)
                    .font_family(theme.mono_family.clone())
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(colors.syntax.variable)
                    .children(language.clone().map(|name| {
                        div()
                            .text_size(theme.text_size(TextSize::Xs))
                            .text_color(colors.fg_subtle)
                            .child(name)
                    }))
                    .child(StyledText::new(text.clone()).with_highlights(styles))
                    .into_any_element()
            }
            Node::List { start, items } => div()
                .flex()
                .flex_col()
                .gap_1()
                .children(items.iter().enumerate().map(|(ix, (task, body))| {
                    let marker: AnyElement = match (task, start) {
                        (Some(done), _) => Icon::new(if *done {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        })
                        .size(IconSize::Sm)
                        .color(if *done {
                            colors.focus
                        } else {
                            colors.fg_subtle
                        })
                        .into_any_element(),
                        (None, Some(first)) => div()
                            .text_color(colors.fg_muted)
                            .child(format!("{}.", first + ix as u64))
                            .into_any_element(),
                        (None, None) => div()
                            .text_color(colors.fg_muted)
                            .child("•")
                            .into_any_element(),
                    };
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .min_w_5()
                                .flex()
                                .justify_end()
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .when(*task == Some(true), |body| {
                                    body.text_color(colors.fg_subtle)
                                })
                                .children(self.nodes(body, cx)),
                        )
                }))
                .into_any_element(),
            Node::Table { aligns, head, rows } => {
                let cell = |inline: &Inline, column: usize, header: bool, cx: &App| {
                    div()
                        .flex_1()
                        .flex()
                        .px_3()
                        .py_1p5()
                        .when(aligns.get(column) == Some(&Alignment::Right), |cell| {
                            cell.justify_end()
                        })
                        .when(aligns.get(column) == Some(&Alignment::Center), |cell| {
                            cell.justify_center()
                        })
                        .when(header, |cell| {
                            cell.font_weight(FontWeight::MEDIUM)
                                .text_color(colors.fg_muted)
                        })
                        .child(self.inline(inline, cx))
                };
                div()
                    .flex()
                    .flex_col()
                    .rounded(theme.radius(Radius::Md))
                    .border_1()
                    .border_color(colors.border)
                    .overflow_hidden()
                    .child(
                        div().flex().bg(colors.hover).children(
                            head.iter()
                                .enumerate()
                                .map(|(column, inline)| cell(inline, column, true, cx)),
                        ),
                    )
                    .children(rows.iter().map(|row| {
                        div()
                            .flex()
                            .border_t_1()
                            .border_color(colors.border)
                            .children(
                                row.iter()
                                    .enumerate()
                                    .map(|(column, inline)| cell(inline, column, false, cx)),
                            )
                    }))
                    .into_any_element()
            }
            Node::Rule => div()
                .my_2()
                .h_0()
                .border_t_1()
                .border_color(colors.border)
                .into_any_element(),
            Node::Math(tex) => div()
                .flex()
                .justify_center()
                .py_2()
                .child(match Latex::parse(tex) {
                    Ok(math) => math.into_any_element(),
                    Err(error) => div()
                        .text_color(colors.danger)
                        .child(error.to_string())
                        .into_any_element(),
                })
                .into_any_element(),
            Node::Image { alt, url } => div()
                .flex()
                .items_center()
                .gap_2()
                .p_3()
                .rounded(theme.radius(Radius::Md))
                .border_1()
                .border_color(colors.border)
                .text_color(colors.fg_muted)
                .child(
                    Icon::new(IconName::Image)
                        .size(IconSize::Sm)
                        .color(colors.fg_subtle),
                )
                .child(div().flex_1().child(alt.clone()))
                .child(div().text_color(colors.fg_subtle).child(url.clone()))
                .into_any_element(),
            Node::Footnote { label, body } => div()
                .flex()
                .gap_2()
                .text_size(theme.text_size(TextSize::Sm))
                .text_color(colors.fg_muted)
                .child(div().flex_none().child(format!("[{label}]")))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .children(self.nodes(body, cx)),
                )
                .into_any_element(),
        }
    }
}

/// The anchor of the document's `ordinal`th heading.
fn anchor(ordinal: usize) -> SharedString {
    format!("heading-{ordinal}").into()
}

/// The headings of `markdown` in order, as a table of contents takes them: anchor, text and depth from 0.
pub fn outline(markdown: &str) -> Vec<(SharedString, SharedString, usize)> {
    fn walk(nodes: &[Node], out: &mut Vec<(SharedString, SharedString, usize)>) {
        for node in nodes {
            match node {
                Node::Heading(level, inline) => {
                    let ordinal = out.len();
                    out.push((
                        anchor(ordinal),
                        inline.text.clone().into(),
                        *level as usize - 1,
                    ));
                }
                Node::Quote(inner) | Node::Footnote { body: inner, .. } => walk(inner, out),
                Node::List { items, .. } => {
                    for (_, body) in items {
                        walk(body, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&parse(markdown), &mut out);
    out
}

impl RenderOnce for MarkdownRenderer {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let nodes = parse(&self.source);
        let draw = Draw {
            id: &self.id,
            on_link: &self.on_link,
            next: std::cell::Cell::new(0),
            sections: &self.sections,
            headings: std::cell::Cell::new(0),
            code: &self.code,
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .text_size(theme.text_size(self.size))
            .line_height(theme.text_size(self.size) * 1.6)
            .children(draw.nodes(&nodes, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::outline;

    #[test]
    fn the_outline_numbers_headings_in_order_with_their_depth() {
        let found =
            outline("# Lift\n\ntext\n\n## In light\n\n> ### Quoted\n\n- #### Listed\n\n## In dark");
        let shape: Vec<(&str, &str, usize)> = found
            .iter()
            .map(|(anchor, text, depth)| (anchor.as_ref(), text.as_ref(), *depth))
            .collect();
        assert_eq!(
            shape,
            [
                ("heading-0", "Lift", 0),
                ("heading-1", "In light", 1),
                ("heading-2", "Quoted", 2),
                ("heading-3", "Listed", 3),
                ("heading-4", "In dark", 1),
            ]
        );
    }
}
