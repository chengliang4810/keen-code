use std::rc::Rc;

use gpui::{
    App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
};

use crate::buttons::{Button, ButtonVariant};

type OnKey = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// A button for each sign-in provider, stacked full width: "Continue with GitHub". While one is on its way it spins and the others rest.
#[derive(IntoElement)]
pub struct OAuthButtons {
    id: ElementId,
    providers: Vec<(SharedString, SharedString)>,
    busy: Option<SharedString>,
    on_pick: Option<OnKey>,
}

impl OAuthButtons {
    /// `providers` are each a key and the name shown.
    pub fn new(
        id: impl Into<ElementId>,
        providers: impl IntoIterator<Item = (impl Into<SharedString>, impl Into<SharedString>)>,
    ) -> Self {
        let id = id.into();
        let providers: Vec<_> = providers
            .into_iter()
            .map(|(key, name)| (key.into(), name.into()))
            .collect();
        assert!(!providers.is_empty(), "oauth buttons {id:?}: no provider");
        Self {
            id,
            providers,
            busy: None,
            on_pick: None,
        }
    }

    /// The provider on its way.
    pub fn busy(mut self, key: impl Into<SharedString>) -> Self {
        let key = key.into();
        assert!(
            self.providers.iter().any(|(each, _)| *each == key),
            "oauth buttons: no provider {key}"
        );
        self.busy = Some(key);
        self
    }

    pub fn on_pick(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pick = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for OAuthButtons {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let id = self.id;
        let on_pick = self
            .on_pick
            .unwrap_or_else(|| panic!("oauth buttons {id:?} have no on_pick"));
        let busy = self.busy;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(self.providers.into_iter().map(|(key, name)| {
                let mine = busy.as_ref() == Some(&key);
                let pick = on_pick.clone();
                Button::new(
                    (id.clone(), format!("provider-{key}")),
                    format!("Continue with {name}"),
                )
                .variant(ButtonVariant::Outline)
                .full_width()
                .loading(mine)
                .disabled(busy.is_some() && !mine)
                .on_click(move |_, window, cx| {
                    log::info!("oauth buttons: {key}");
                    pick(&key, window, cx);
                })
            }))
    }
}
