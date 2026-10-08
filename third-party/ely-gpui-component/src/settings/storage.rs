use std::{path::PathBuf, rc::Rc};

use gpui::{
    App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
};

use crate::{
    buttons::{Button, ButtonVariant, ConfirmButton, ConfirmMode, IconButton},
    data_display::UsageBar,
    forms::DropZone,
    primitives::IconName,
    theme::{ActiveTheme, TextSize},
    typography::{Ellipsis, format},
};

/// A store on disk: its key and name, its size in bytes, and whether it may be cleared.
#[derive(Clone, Debug, PartialEq)]
pub struct Store {
    pub key: SharedString,
    pub name: SharedString,
    pub bytes: u64,
    pub clearable: bool,
}

type OnKey = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

/// What an app keeps on disk: a bar of each store's share, then each store with its size and, where it may go, a Clear that asks twice.
#[derive(IntoElement)]
pub struct StorageSettings {
    id: ElementId,
    stores: Vec<Store>,
    on_clear: Option<OnKey>,
}

impl StorageSettings {
    pub fn new(id: impl Into<ElementId>, stores: impl IntoIterator<Item = Store>) -> Self {
        Self {
            id: id.into(),
            stores: stores.into_iter().collect(),
            on_clear: None,
        }
    }

    pub fn on_clear(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_clear = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for StorageSettings {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_clear = self
            .on_clear
            .unwrap_or_else(|| panic!("storage settings {:?} has no on_clear", self.id));
        let theme = cx.theme();
        let total: u64 = self.stores.iter().map(|store| store.bytes).sum();
        let bar = self.stores.iter().fold(
            UsageBar::new(total.max(1) as f64)
                .amounts(|bytes| format::file_size(bytes as u64, true)),
            |bar, store| bar.part(store.name.clone(), store.bytes as f64),
        );
        let rows = self.stores.iter().map(|store| {
            let (key, on_clear) = (store.key.clone(), on_clear.clone());
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .py_2()
                .border_b_1()
                .border_color(theme.colors.border)
                .child(
                    div()
                        .flex_1()
                        .min_w(theme.label_width())
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(theme.text_size(TextSize::Sm))
                                .child(Ellipsis::new(store.name.clone())),
                        )
                        .child(
                            div()
                                .text_size(theme.text_size(TextSize::Xs))
                                .text_color(theme.colors.fg_muted)
                                .child(format::file_size(store.bytes, true)),
                        ),
                )
                .children(store.clearable.then(|| {
                    ConfirmButton::new(
                        (self.id.clone(), format!("clear-{}", store.key)),
                        "Clear",
                        ConfirmMode::Twice,
                    )
                    .on_confirm(move |window, cx| {
                        log::info!("storage: clear {key}");
                        on_clear(&key, window, cx);
                    })
                }))
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(theme.text_size(TextSize::Sm))
                    .text_color(theme.colors.fg_muted)
                    .child(format!("{} in all", format::file_size(total, true))),
            )
            .child(bar)
            .child(div().flex().flex_col().children(rows))
    }
}

/// A setting changed from its default: its key and name, and both values as words.
#[derive(Clone, Debug, PartialEq)]
pub struct Changed {
    pub key: SharedString,
    pub name: SharedString,
    pub now: SharedString,
    pub default: SharedString,
}

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

/// The settings changed from their defaults, each with what it is and what it was, and a way back for each; Reset all asks twice. With none changed it says so.
#[derive(IntoElement)]
pub struct ResetToDefault {
    id: ElementId,
    changed: Vec<Changed>,
    on_reset: Option<OnKey>,
    on_reset_all: Option<Run>,
}

impl ResetToDefault {
    pub fn new(id: impl Into<ElementId>, changed: impl IntoIterator<Item = Changed>) -> Self {
        Self {
            id: id.into(),
            changed: changed.into_iter().collect(),
            on_reset: None,
            on_reset_all: None,
        }
    }

    pub fn on_reset(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_reset = Some(Rc::new(handler));
        self
    }

    pub fn on_reset_all(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_reset_all = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ResetToDefault {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let on_reset = self
            .on_reset
            .unwrap_or_else(|| panic!("reset to default {:?} has no on_reset", self.id));
        let on_reset_all = self
            .on_reset_all
            .unwrap_or_else(|| panic!("reset to default {:?} has no on_reset_all", self.id));
        let theme = cx.theme();
        if self.changed.is_empty() {
            return div()
                .text_size(theme.text_size(TextSize::Sm))
                .text_color(theme.colors.fg_muted)
                .child("Every setting is at its default.")
                .into_any_element();
        }
        let rows = self.changed.iter().map(|changed| {
            let (key, on_reset) = (changed.key.clone(), on_reset.clone());
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_2()
                .border_b_1()
                .border_color(theme.colors.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(theme.text_size(TextSize::Sm))
                                .child(Ellipsis::new(changed.name.clone())),
                        )
                        .child(
                            div()
                                .text_size(theme.text_size(TextSize::Xs))
                                .text_color(theme.colors.fg_muted)
                                .child(Ellipsis::new(format!(
                                    "{} · was {}",
                                    changed.now, changed.default
                                ))),
                        ),
                )
                .child(
                    IconButton::new(
                        (self.id.clone(), format!("reset-{}", changed.key)),
                        IconName::RotateCcw,
                    )
                    .tooltip("Back to the default")
                    .on_click(move |_, window, cx| {
                        log::info!("reset: {key}");
                        on_reset(&key, window, cx);
                    }),
                )
        });
        let on_reset_all = on_reset_all;
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().flex().flex_col().children(rows))
            .child(
                div().flex().child(
                    ConfirmButton::new(
                        (self.id, "all"),
                        format!("Reset all {}", self.changed.len()),
                        ConfirmMode::Twice,
                    )
                    .on_confirm(move |window, cx| {
                        log::info!("reset: all");
                        on_reset_all(window, cx);
                    }),
                ),
            )
            .into_any_element()
    }
}

type OnFile = Rc<dyn Fn(PathBuf, &mut Window, &mut App)>;

/// Settings out to a file and back: Export asks the owner to write them, and a JSON file dropped or picked asks it to read one.
#[derive(IntoElement)]
pub struct ImportExportSettings {
    id: ElementId,
    on_export: Option<Run>,
    on_import: Option<OnFile>,
}

impl ImportExportSettings {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            on_export: None,
            on_import: None,
        }
    }

    pub fn on_export(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_export = Some(Rc::new(handler));
        self
    }

    pub fn on_import(mut self, handler: impl Fn(PathBuf, &mut Window, &mut App) + 'static) -> Self {
        self.on_import = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ImportExportSettings {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let on_export = self
            .on_export
            .unwrap_or_else(|| panic!("import export settings {:?} has no on_export", self.id));
        let on_import = self
            .on_import
            .unwrap_or_else(|| panic!("import export settings {:?} has no on_import", self.id));
        let (on_export, on_import) = (on_export, on_import);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div().flex().child(
                    Button::new((self.id.clone(), "export"), "Export settings")
                        .variant(ButtonVariant::Secondary)
                        .on_click(move |_, window, cx| {
                            log::info!("settings: export");
                            on_export(window, cx);
                        }),
                ),
            )
            .child(DropZone::new((self.id, "import")).kinds(&["json"]).on_drop(
                move |paths, window, cx| {
                    let Some(path) = paths.first().cloned() else {
                        return;
                    };
                    log::info!("settings: import {}", path.display());
                    on_import(path, window, cx);
                },
            ))
    }
}
