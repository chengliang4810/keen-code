use std::rc::Rc;

use gpui::{
    AbsoluteLength, AnyElement, App, ClickEvent, Div, ElementId, Entity, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Point, RenderOnce, SharedString, Stateful,
    Styled, Window, div, prelude::*,
};
use smallvec::SmallVec;

use super::{
    menu::{Open, Spot, hang, measure_host},
    model::Menu,
};
use crate::{
    buttons::{Button, ButtonGroup, ButtonVariant, IconButton},
    forms::TextInput,
    primitives::IconName,
    theme::IconSize,
};

pub(crate) type Click = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

fn state(id: &ElementId, window: &mut Window, cx: &mut App) -> Entity<Open> {
    window.use_keyed_state((id.clone(), "menu"), cx, |_, _| Open::default())
}

/// Toggles the menu under the host; a keyboard press marks its first row.
fn toggle(state: &Entity<Open>, menu: &Menu, cx: &App) -> Click {
    let (state, menu, open) = (state.clone(), menu.clone(), state.read(cx).is_open());
    Rc::new(move |event, window, cx| {
        if open {
            Open::close(&state, window, cx);
        } else {
            Open::show(&state, &menu, Spot::Under, event.is_keyboard(), cx);
        }
    })
}

/// A trigger, the canvas that measures it, and its menu while open.
fn host(
    id: ElementId,
    trigger: impl IntoElement,
    menu: &Menu,
    state: &Entity<Open>,
    field: Option<&Entity<TextInput>>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    div()
        .id(id.clone())
        .relative()
        .flex_none()
        .child(trigger)
        .child(measure_host(state.clone(), true))
        .children(hang(&id, menu, state, field, window, cx))
}

/// A menu under a trigger of the caller's making, which runs `click` when pressed.
pub(crate) fn menu_under(
    id: ElementId,
    menu: &Menu,
    trigger: impl FnOnce(Click) -> AnyElement,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let state = state(&id, window, cx);
    let click = toggle(&state, menu, cx);
    host(id, trigger(click), menu, &state, None, window, cx)
}

/// A button that opens a menu under it.
#[derive(IntoElement)]
pub struct DropdownMenu {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    variant: ButtonVariant,
    /// 仅覆盖下拉触发按钮的文字字号，菜单行字号保持原有主题行为。
    trigger_text_size: Option<AbsoluteLength>,
    /// 仅覆盖下拉触发按钮的左右内边距，未设置时沿用按钮主题默认值。
    trigger_horizontal_padding: Option<Pixels>,
    /// 仅覆盖下拉触发按钮的右内边距，未设置时沿用左右对称值。
    trigger_right_padding: Option<Pixels>,
    /// 仅覆盖下拉触发按钮内容间距，菜单行不受影响。
    trigger_content_gap: Option<Pixels>,
    /// 覆盖下拉触发按钮外部高度；菜单行仍沿用原有主题密度。
    trigger_height: Option<Pixels>,
    /// 覆盖下拉触发按钮内图标尺寸；菜单行图标不受影响。
    trigger_icon_size: Option<IconSize>,
    menu: Menu,
}

impl DropdownMenu {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, menu: Menu) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            variant: ButtonVariant::Secondary,
            trigger_text_size: None,
            trigger_horizontal_padding: None,
            trigger_right_padding: None,
            trigger_content_gap: None,
            trigger_height: None,
            trigger_icon_size: None,
            menu,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// 覆盖触发按钮文字字号；不会改变下拉菜单内的菜单项字号。
    pub fn trigger_text_size(mut self, size: impl Into<AbsoluteLength>) -> Self {
        self.trigger_text_size = Some(size.into());
        self
    }

    /// 覆盖触发按钮左右内边距；未设置时保留主题密度对应的默认值。
    pub fn trigger_horizontal_padding(mut self, padding: Pixels) -> Self {
        self.trigger_horizontal_padding = Some(padding);
        self
    }

    /// 覆盖触发按钮右内边距；不会改变下拉菜单内的菜单项。
    pub fn trigger_right_padding(mut self, padding: Pixels) -> Self {
        self.trigger_right_padding = Some(padding);
        self
    }

    /// 覆盖触发按钮内容间距；不会改变下拉菜单内的菜单项。
    pub fn trigger_content_gap(mut self, gap: Pixels) -> Self {
        self.trigger_content_gap = Some(gap);
        self
    }

    /// 覆盖下拉触发按钮外部高度；省略时保留主题 density 行为。
    pub fn trigger_height(mut self, height: Pixels) -> Self {
        self.trigger_height = Some(height);
        self
    }

    /// 覆盖下拉触发按钮内图标尺寸；不会改变菜单项图标。
    pub fn trigger_icon_size(mut self, size: IconSize) -> Self {
        self.trigger_icon_size = Some(size);
        self
    }
}

impl RenderOnce for DropdownMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = state(&self.id, window, cx);
        let click = toggle(&state, &self.menu, cx);
        let button = Button::new((self.id.clone(), "button"), self.label)
            .variant(self.variant)
            .trailing_icon(IconName::ChevronDown)
            .when_some(self.icon, |button, icon| button.icon(icon))
            .when_some(self.trigger_text_size, |button, size| {
                button.text_size(size)
            })
            .when_some(self.trigger_horizontal_padding, |button, padding| {
                button.horizontal_padding(padding)
            })
            .when_some(self.trigger_right_padding, |button, padding| {
                button.right_padding(padding)
            })
            .when_some(self.trigger_content_gap, |button, gap| {
                button.content_gap(gap)
            })
            .when_some(self.trigger_height, |button, height| button.height(height))
            .when_some(self.trigger_icon_size, |button, size| {
                button.icon_size(size)
            })
            .on_click(move |event, window, cx| click(event, window, cx));
        host(self.id, button, &self.menu, &state, None, window, cx)
    }
}

/// An icon button that opens a menu of what did not fit.
#[derive(IntoElement)]
pub struct OverflowMenu {
    id: ElementId,
    menu: Menu,
    icon: IconName,
    tooltip: SharedString,
}

impl OverflowMenu {
    pub fn new(id: impl Into<ElementId>, menu: Menu) -> Self {
        Self {
            id: id.into(),
            menu,
            icon: IconName::Ellipsis,
            tooltip: "More".into(),
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = tooltip.into();
        self
    }
}

impl RenderOnce for OverflowMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = state(&self.id, window, cx);
        let click = toggle(&state, &self.menu, cx);
        let button = IconButton::new((self.id.clone(), "button"), self.icon)
            .variant(ButtonVariant::Ghost)
            .tooltip(self.tooltip)
            .on_click(move |event, window, cx| click(event, window, cx));
        host(self.id, button, &self.menu, &state, None, window, cx)
    }
}

/// A button's main action, and related ones in a menu under its arrow.
#[derive(IntoElement)]
pub struct SplitButton {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    menu: Menu,
    on_click: Option<Click>,
}

impl SplitButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, menu: Menu) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::Secondary,
            menu,
            on_click: None,
        }
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    /// The main action.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SplitButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = state(&self.id, window, cx);
        let click = toggle(&state, &self.menu, cx);
        let (shut, run) = (state.clone(), self.on_click);
        let main = Button::new((self.id.clone(), "main"), self.label)
            .variant(self.variant)
            .on_click(move |event, window, cx| {
                if shut.read(cx).is_open() {
                    Open::close(&shut, window, cx);
                }
                if let Some(run) = &run {
                    run(event, window, cx);
                }
            });
        let more = Button::new((self.id.clone(), "more"), "")
            .variant(self.variant)
            .icon(IconName::ChevronDown)
            .on_click(move |event, window, cx| click(event, window, cx));
        host(
            self.id,
            ButtonGroup::new().button(main).button(more),
            &self.menu,
            &state,
            None,
            window,
            cx,
        )
    }
}

/// Its children, and a menu that opens at the pointer on a right click.
#[derive(IntoElement)]
pub struct ContextMenu {
    id: ElementId,
    menu: Menu,
    request: Option<Option<(u64, Point<Pixels>)>>,
    children: SmallVec<[AnyElement; 2]>,
}

impl ContextMenu {
    pub fn new(id: impl Into<ElementId>, menu: Menu) -> Self {
        Self {
            id: id.into(),
            menu,
            request: None,
            children: SmallVec::new(),
        }
    }

    /// Opens on the owner's numbered request, not a right click.
    pub fn manual(mut self, request: Option<(u64, Point<Pixels>)>) -> Self {
        self.request = Some(request);
        self
    }
}

impl ParentElement for ContextMenu {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ContextMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = state(&self.id, window, cx);
        if let Some(Some((number, at))) = self.request {
            let asked = window.use_keyed_state((self.id.clone(), "asked"), cx, |_, _| None);
            if *asked.read(cx) != Some(number) {
                asked.update(cx, |asked, _| *asked = Some(number));
                log::info!("context menu {:?}: asked at {at:?}", self.id);
                Open::show(&state, &self.menu, Spot::At(at), false, cx);
            }
        }
        let (open, shut, menu) = (state.clone(), state.clone(), self.menu.clone());
        div()
            .id(self.id.clone())
            .relative()
            .when(self.request.is_none(), |host| {
                host.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    window.prevent_default();
                    log::info!("context menu: at {:?}", event.position);
                    Open::show(&open, &menu, Spot::At(event.position), false, cx);
                })
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                if shut.read(cx).is_open() {
                    window.prevent_default();
                    Open::close(&shut, window, cx);
                }
            })
            .children(self.children)
            .child(measure_host(state.clone(), false))
            .children(hang(&self.id, &self.menu, &state, None, window, cx))
    }
}

/// A button that opens a menu headed by a filter field. Typing narrows the rows, best first, and marks the letters that matched.
#[derive(IntoElement)]
pub struct SearchableMenu {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    placeholder: SharedString,
    menu: Menu,
}

impl SearchableMenu {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, menu: Menu) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            placeholder: "Filter".into(),
            menu,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn placeholder(mut self, text: impl Into<SharedString>) -> Self {
        self.placeholder = text.into();
        self
    }
}

impl RenderOnce for SearchableMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = state(&self.id, window, cx);
        let field = state.read(cx).is_open().then(|| {
            let placeholder = self.placeholder.clone();
            window.use_keyed_state((self.id.clone(), "filter"), cx, move |window, cx| {
                TextInput::new(window, cx).placeholder(placeholder)
            })
        });
        let query = field
            .as_ref()
            .map(|field| field.read(cx).text().trim().to_string());
        let shown = match &query {
            Some(query) => self.menu.filtered(query),
            None => self.menu.clone(),
        };
        let last = window.use_keyed_state((self.id.clone(), "query"), cx, |_, _| None::<String>);
        if *last.read(cx) != query {
            last.update(cx, |last, _| *last = query.clone());
            if query.is_some() {
                log::info!(
                    "searchable menu {:?}: {} rows fit",
                    self.id,
                    shown.entries.len()
                );
                Open::restart(&state, &shown, cx);
            }
        }
        let click = toggle(&state, &shown, cx);
        let button = Button::new((self.id.clone(), "button"), self.label)
            .trailing_icon(IconName::ChevronDown)
            .when_some(self.icon, |button, icon| button.icon(icon))
            .on_click(move |event, window, cx| click(event, window, cx));
        host(self.id, button, &shown, &state, field.as_ref(), window, cx)
    }
}
