use gpui::{
    App, ClickEvent, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*,
    transparent_black,
};

use crate::{
    primitives::{FocusRing, Icon, IconName},
    theme::{ActiveTheme, IconSize, Radius},
};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Inline link. Give it an `href` or an `on_click`.
#[derive(IntoElement)]
pub struct Link {
    id: ElementId,
    label: SharedString,
    href: Option<SharedString>,
    on_click: Option<ClickHandler>,
    external: bool,
}

impl Link {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            href: None,
            on_click: None,
            external: false,
        }
    }

    /// Opens in the system browser.
    pub fn href(mut self, url: impl Into<SharedString>) -> Self {
        self.href = Some(url.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Link {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = theme.colors.link;
        let action: ClickHandler = match (self.on_click, self.href) {
            (Some(handler), _) => handler,
            (None, Some(url)) => Box::new(move |_, _, cx| cx.open_url(&url)),
            (None, None) => panic!("link {:?} has neither href nor on_click", self.id),
        };
        div()
            .id(self.id)
            .flex()
            .items_center()
            .gap_0p5()
            .px_0p5()
            .rounded(theme.radius(Radius::Sm))
            .border_1()
            .border_color(transparent_black())
            .text_color(color)
            .cursor_pointer()
            .tab_index(0)
            .focus_ring(cx)
            .hover(|style| style.underline())
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(action)
            .child(self.label)
            .when(self.external, |link| {
                link.child(
                    Icon::new(IconName::ArrowUpRight)
                        .size(IconSize::Xs)
                        .color(color),
                )
            })
    }
}

/// Link that leaves the app, marked with an arrow.
#[derive(IntoElement)]
pub struct ExternalLink(Link);

impl ExternalLink {
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        url: impl Into<SharedString>,
    ) -> Self {
        let mut link = Link::new(id, label).href(url);
        link.external = true;
        Self(link)
    }
}

impl RenderOnce for ExternalLink {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.0
    }
}
