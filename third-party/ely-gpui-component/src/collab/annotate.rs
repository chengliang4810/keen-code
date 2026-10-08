use std::{ops::Range, rc::Rc};

use gpui::{
    App, ElementId, HighlightStyle, InteractiveText, IntoElement, RenderOnce, SharedString,
    StyledText, UnderlineStyle, Window,
};

use super::peers::Peer;
use crate::{forms::Pick, primitives::Tooltip, theme::ActiveTheme};

/// How strongly an author's color washes what they marked, and the one picked.
const WASH: f32 = 0.18;
const PICKED: f32 = 0.36;

/// A stretch of text someone marked: its byte range, who marked it, and the note they left; without a note it is a plain highlight.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub range: Range<usize>,
    pub author: Peer,
    pub note: Option<SharedString>,
}

/// Text with stretches people marked, each washed in its author's color and noted ones underlined: resting on a noted one shows the note and who left it; a press picks one.
#[derive(IntoElement)]
pub struct AnnotatedText {
    id: ElementId,
    text: SharedString,
    annotations: Rc<Vec<Annotation>>,
    picked: Option<usize>,
    on_pick: Option<Pick>,
}

impl AnnotatedText {
    /// `annotations` in order, none overlapping.
    pub fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        annotations: impl IntoIterator<Item = Annotation>,
    ) -> Self {
        let text = text.into();
        let annotations: Vec<Annotation> = annotations.into_iter().collect();
        for annotation in &annotations {
            let range = &annotation.range;
            assert!(
                range.start < range.end
                    && range.end <= text.len()
                    && text.is_char_boundary(range.start)
                    && text.is_char_boundary(range.end),
                "annotation {range:?} lies outside the text"
            );
        }
        assert!(
            annotations
                .windows(2)
                .all(|pair| pair[0].range.end <= pair[1].range.start),
            "annotations come in order without overlapping"
        );
        Self {
            id: id.into(),
            text,
            annotations: Rc::new(annotations),
            picked: None,
            on_pick: None,
        }
    }

    /// The annotation picked, by index, and what a press on one asks.
    pub fn picked(
        mut self,
        picked: Option<usize>,
        on_pick: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.picked = picked;
        self.on_pick = Some(Rc::new(on_pick));
        self
    }
}

impl RenderOnce for AnnotatedText {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let thickness = cx.theme().underline_thickness();
        let highlights: Vec<(Range<usize>, HighlightStyle)> = self
            .annotations
            .iter()
            .enumerate()
            .map(|(ix, annotation)| {
                let color = annotation.author.color(cx);
                let wash = if self.picked == Some(ix) {
                    PICKED
                } else {
                    WASH
                };
                let style = HighlightStyle {
                    background_color: Some(color.opacity(wash)),
                    underline: annotation.note.is_some().then_some(UnderlineStyle {
                        thickness,
                        color: Some(color),
                        wavy: false,
                    }),
                    ..HighlightStyle::default()
                };
                (annotation.range.clone(), style)
            })
            .collect();
        let ranges: Vec<Range<usize>> = self
            .annotations
            .iter()
            .map(|annotation| annotation.range.clone())
            .collect();
        let notes = self.annotations.clone();
        let text = InteractiveText::new(
            self.id,
            StyledText::new(self.text).with_highlights(highlights),
        )
        .tooltip(move |at, window, cx| {
            let annotation = notes
                .iter()
                .find(|annotation| annotation.range.contains(&at))?;
            let note = annotation.note.clone()?;
            Some(Tooltip::with_meta(note, annotation.author.name.clone())(
                window, cx,
            ))
        });
        match self.on_pick {
            Some(pick) => text.on_click(ranges, move |ix, window, cx| {
                log::info!("annotated text: picked {ix}");
                pick(ix, window, cx)
            }),
            None => text,
        }
    }
}
