//! 设置页根实体。
//!
//! Panel 只管理当前页的 UI 投影、有限缓存和订阅生命周期。所有读取与写入都经由
//! [`NativeSettingsService`] 完成，避免页面切换时重新读取文件或在 UI 内复制事实源。

use std::rc::Rc;
use std::sync::Arc;

use ely_gpui_component::layout::{Scrollbar, ScrollbarVisualStyle};
use ely_gpui_component::primitives::{FocusRing, Icon, IconName};
use ely_gpui_component::{
    theme::{ActiveTheme, IconSize},
    typography::Caption,
};
use gpui::{
    AnyElement, App, Axis, ClipboardItem, Context, Entity, FontWeight, IntoElement, MouseButton,
    ParentElement, Pixels, Render, Role, ScrollHandle, Styled, Task, Window, div, img, point,
    prelude::*, px,
};

use super::{
    SettingsCommandHandler, agents, appearance, automation,
    cache::BoundedSettingsCache,
    contracts::{
        NativeSettingsError, NativeSettingsService, NativeSettingsSubscription, SettingsCommand,
        SettingsEvent, SettingsNotice, SettingsPage, SettingsResult, SettingsSnapshot,
    },
    diagnostics, general, hooks, keyboard, providers, resources, usage, workflows,
};
use crate::native_ui::style::{
    CONTENT_MAX_WIDTH, DESKTOP_INSET, ShellColors, TITLEBAR_HEIGHT, UiTextSize, page_title_size,
    ui_text_size,
};

const SETTINGS_CACHE_CAPACITY: usize = 4;
/// 来源 SettingsPage 桌面标题为 `text-3xl`：30px 字号配 36px 行高；顶部占位仍独立为 48px。
const SETTINGS_TITLE_LINE_HEIGHT: f32 = 36.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsNavItem {
    Page(SettingsPage),
    Resource(resources::ResourceCategory),
}

impl SettingsNavItem {
    const fn title(self) -> &'static str {
        match self {
            Self::Page(page) => page.title(),
            Self::Resource(category) => category.title(),
        }
    }

    const fn key(self) -> &'static str {
        match self {
            Self::Page(SettingsPage::General) => "general",
            Self::Page(SettingsPage::Appearance) => "appearance",
            Self::Page(SettingsPage::Keyboard) => "keyboard",
            Self::Page(SettingsPage::Providers) => "providers",
            Self::Page(SettingsPage::Hooks) => "hooks",
            Self::Page(SettingsPage::Resources) => "resources",
            Self::Page(SettingsPage::Automations) => "automations",
            Self::Page(SettingsPage::Workflows) => "workflows",
            Self::Page(SettingsPage::Agents) => "agents",
            Self::Page(SettingsPage::Usage) => "usage",
            Self::Page(SettingsPage::Diagnostics) => "diagnostics",
            Self::Resource(category) => category.key(),
        }
    }
}

const SETTINGS_NAV_GROUPS: &[(&str, &[SettingsNavItem])] = &[
    (
        "基础设置",
        &[
            SettingsNavItem::Page(SettingsPage::General),
            SettingsNavItem::Page(SettingsPage::Appearance),
            SettingsNavItem::Page(SettingsPage::Providers),
            SettingsNavItem::Page(SettingsPage::Keyboard),
        ],
    ),
    (
        "Agent 能力",
        &[
            SettingsNavItem::Resource(resources::ResourceCategory::Memory),
            SettingsNavItem::Page(SettingsPage::Agents),
            SettingsNavItem::Resource(resources::ResourceCategory::Plugins),
            SettingsNavItem::Resource(resources::ResourceCategory::Mcp),
            SettingsNavItem::Resource(resources::ResourceCategory::Skills),
            SettingsNavItem::Resource(resources::ResourceCategory::AgentTemplates),
            SettingsNavItem::Page(SettingsPage::Automations),
            SettingsNavItem::Page(SettingsPage::Hooks),
            SettingsNavItem::Page(SettingsPage::Workflows),
        ],
    ),
    (
        "数据与统计",
        &[
            SettingsNavItem::Page(SettingsPage::Usage),
            SettingsNavItem::Page(SettingsPage::Diagnostics),
        ],
    ),
];

type SettingsBackHandler = Rc<dyn Fn(&mut App)>;
type SettingsNavigationHandler = Rc<dyn Fn(SettingsPage, &mut App)>;

/// 设置窗口持有的可克隆句柄。句柄只引用 GPUI Entity，不复制 domain 或页面快照。
#[derive(Clone)]
pub struct SettingsPanelHandle {
    entity: Entity<SettingsPanel>,
}

impl SettingsPanelHandle {
    /// 返回面板实体，供 NativeUi 的 panel factory 嵌入当前窗口。
    pub fn entity(&self) -> Entity<SettingsPanel> {
        self.entity.clone()
    }

    /// 返回可直接挂载到 GPUI 树的元素。
    pub fn element(&self) -> AnyElement {
        self.entity.clone().into_any_element()
    }

    /// 窗口关闭路径可重复调用；实际订阅关闭由实体保证幂等。
    pub fn close(&self, cx: &mut App) {
        self.entity.update(cx, |panel, cx| panel.close(cx));
    }

    /// 通过面板实体切换设置页，供 NativeUi 的外部入口复用同一份缓存和请求代次。
    pub fn select_page(&self, page: SettingsPage, cx: &mut App) {
        self.entity
            .update(cx, |panel, cx| panel.select_page(page, cx));
    }

    /// 延迟安装返回工作区回调，避免 SettingsPanel 与 NativeUi 在创建阶段形成闭环。
    pub fn with_back_handler(&self, handler: impl Fn(&mut App) + 'static, cx: &mut App) {
        let handler: SettingsBackHandler = Rc::new(handler);
        self.entity.update(cx, |panel, _| {
            panel.back_handler = Some(handler);
        });
    }

    /// 设置页内部点击只通知 NativeUi 记录用户历史；外部恢复调用 `select_page` 时不触发。
    pub fn with_navigation_handler(
        &self,
        handler: impl Fn(SettingsPage, &mut App) + 'static,
        cx: &mut App,
    ) {
        let handler: SettingsNavigationHandler = Rc::new(handler);
        self.entity.update(cx, |panel, _| {
            panel.navigation_handler = Some(handler);
        });
    }
}

/// 设置页的原生 GPUI 根实体。
pub struct SettingsPanel {
    service: Arc<dyn NativeSettingsService>,
    active_page: SettingsPage,
    resource_category: resources::ResourceCategory,
    current: Option<SettingsSnapshot>,
    cache: BoundedSettingsCache,
    subscription: Option<Arc<dyn NativeSettingsSubscription>>,
    subscription_task: Option<Task<()>>,
    request_generation: u64,
    loading: bool,
    closed: bool,
    error: Option<NativeSettingsError>,
    notice: Option<SettingsNotice>,
    back_handler: Option<SettingsBackHandler>,
    navigation_handler: Option<SettingsNavigationHandler>,
    /// 设置页共享同一滚动句柄；切页时归零，避免把上一页的偏移带入新页面。
    page_scroll: ScrollHandle,
}

impl SettingsPanel {
    /// 创建尚未激活的面板；订阅和首个页面读取在真正进入设置页时启动。
    pub fn create(service: Arc<dyn NativeSettingsService>, cx: &mut App) -> SettingsPanelHandle {
        let entity = cx.new(|_| Self {
            service,
            active_page: SettingsPage::General,
            resource_category: resources::ResourceCategory::Plugins,
            current: None,
            cache: BoundedSettingsCache::new(SETTINGS_CACHE_CAPACITY),
            subscription: None,
            subscription_task: None,
            request_generation: 0,
            loading: false,
            closed: false,
            error: None,
            notice: None,
            back_handler: None,
            navigation_handler: None,
            page_scroll: ScrollHandle::new(),
        });
        SettingsPanelHandle { entity }
    }

    fn start_subscription(&mut self, cx: &mut Context<Self>) {
        if self.subscription.is_some() || self.closed {
            return;
        }
        let subscription = match self.service.subscribe() {
            Ok(subscription) => subscription,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        let weak = cx.entity().downgrade();
        let event_subscription = Arc::clone(&subscription);
        self.subscription = Some(subscription);
        self.subscription_task = Some(cx.spawn(async move |_, cx| {
            loop {
                let Some(event) = event_subscription.next().await else {
                    break;
                };
                let should_continue = weak
                    .update(cx, |panel, cx| panel.apply_event(event, cx))
                    .ok()
                    .flatten()
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }
            }
        }));
    }

    fn select_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        // SettingsPanel 在 Chat 首屏也被窗口宿主持有；首次进入设置页才建立事件订阅，
        // 避免后台长期等待设置事件并提前保留 General 快照。
        self.start_subscription(cx);
        // 页面切换会使旧页的加载和命令结果失效；事件快照仍可作为外部确认事实进入缓存。
        self.request_generation = self.request_generation.wrapping_add(1);
        self.page_scroll
            .set_offset(point(Pixels::ZERO, Pixels::ZERO));
        self.active_page = page;
        // 外部按 SettingsPage 进入资源设置时从插件分类开始；分类导航自身不经过此重置。
        if page == SettingsPage::Resources {
            self.resource_category = resources::ResourceCategory::Plugins;
        }
        if let Some(snapshot) = self.cache.get(page) {
            self.current = Some(snapshot);
            self.loading = false;
            self.error = None;
            cx.notify();
            return;
        }
        self.current = None;
        self.load_page(page, cx);
        cx.notify();
    }

    fn select_resource_category(
        &mut self,
        category: resources::ResourceCategory,
        cx: &mut Context<Self>,
    ) {
        if self.closed {
            return;
        }
        if self.active_page != SettingsPage::Resources {
            self.select_page(SettingsPage::Resources, cx);
        }
        if self.resource_category != category {
            self.resource_category = category;
            self.page_scroll
                .set_offset(point(Pixels::ZERO, Pixels::ZERO));
            cx.notify();
        }
    }

    fn select_nav_item(
        &mut self,
        item: SettingsNavItem,
        cx: &mut Context<Self>,
    ) -> Option<SettingsPage> {
        if self.closed {
            return None;
        }
        let user_page = match item {
            SettingsNavItem::Page(page) if self.active_page != page => Some(page),
            SettingsNavItem::Resource(_) if self.active_page != SettingsPage::Resources => {
                Some(SettingsPage::Resources)
            }
            _ => None,
        };
        match item {
            SettingsNavItem::Page(page) => self.select_page(page, cx),
            SettingsNavItem::Resource(category) => self.select_resource_category(category, cx),
        }
        user_page
    }

    fn load_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.loading = true;
        self.error = None;
        let service = Arc::clone(&self.service);
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = service.load(page).await;
            let _ = weak.update(cx, |panel, cx| {
                panel.apply_load(page, generation, result, cx);
            });
        })
        .detach();
    }

    fn apply_load(
        &mut self,
        page: SettingsPage,
        generation: u64,
        result: SettingsResult<SettingsSnapshot>,
        cx: &mut Context<Self>,
    ) {
        if self.closed || generation != self.request_generation || page != self.active_page {
            return;
        }
        self.loading = false;
        match result {
            Ok(snapshot) => {
                self.error = None;
                self.notice = snapshot.notices.first().cloned();
                self.cache.insert(page, snapshot.clone());
                if let Some(general) = snapshot.general.as_ref() {
                    general::apply_general_settings(general, &mut *cx);
                }
                self.current = Some(snapshot);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn execute(&mut self, command: SettingsCommand, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.request_generation = self.request_generation.wrapping_add(1);
        let generation = self.request_generation;
        self.loading = true;
        self.error = None;
        let service = Arc::clone(&self.service);
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = service.execute(command).await;
            let _ = weak.update(cx, |panel, cx| panel.apply_command(generation, result, cx));
        })
        .detach();
    }

    fn apply_command(
        &mut self,
        generation: u64,
        result: SettingsResult<SettingsSnapshot>,
        cx: &mut Context<Self>,
    ) {
        if self.closed || generation != self.request_generation {
            return;
        }
        self.loading = false;
        match result {
            Ok(snapshot) => {
                let exported = snapshot
                    .diagnostics
                    .as_ref()
                    .and_then(|value| value.exported_json.clone());
                self.error = None;
                self.notice = if exported.is_some() {
                    Some(SettingsNotice {
                        level: super::contracts::NoticeLevel::Success,
                        code: "settings_diagnostics_copied".to_owned(),
                        message: "脱敏诊断 JSON 已复制到剪贴板".to_owned(),
                    })
                } else {
                    snapshot.notices.first().cloned()
                };
                if let Some(exported) = exported {
                    cx.write_to_clipboard(ClipboardItem::new_string(exported));
                }
                self.cache.insert(snapshot.page, snapshot.clone());
                if let Some(general) = snapshot.general.as_ref() {
                    general::apply_general_settings(general, &mut *cx);
                }
                if snapshot.page == self.active_page {
                    self.current = Some(snapshot);
                }
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn apply_event(&mut self, event: SettingsEvent, cx: &mut Context<Self>) -> Option<bool> {
        if self.closed {
            return Some(false);
        }
        match event {
            SettingsEvent::Invalidate(page) => {
                self.cache.invalidate(page);
                if page == self.active_page {
                    self.current = None;
                    self.load_page(page, cx);
                }
            }
            SettingsEvent::Snapshot(snapshot) => {
                if snapshot.page == self.active_page {
                    // 外部事件是 NativeHost 的确认事实；任何尚未返回的本地请求都不能覆盖它。
                    self.request_generation = self.request_generation.wrapping_add(1);
                    self.error = None;
                }
                self.notice = snapshot.notices.first().cloned();
                self.cache.insert(snapshot.page, (*snapshot).clone());
                if let Some(general) = snapshot.general.as_ref() {
                    general::apply_general_settings(general, &mut *cx);
                }
                if snapshot.page == self.active_page {
                    self.current = Some(*snapshot);
                    self.loading = false;
                }
            }
            SettingsEvent::Notice(notice) => {
                if notice.level == super::contracts::NoticeLevel::Error {
                    self.error = Some(NativeSettingsError::new(
                        notice.code.clone(),
                        notice.message.clone(),
                    ));
                }
                self.notice = Some(notice);
            }
        }
        cx.notify();
        Some(true)
    }

    fn close(&mut self, _cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.loading = false;
        self.subscription_task.take();
        if let Some(subscription) = self.subscription.take() {
            subscription.close();
        }
    }

    fn dispatch_handler(&self, cx: &Context<Self>) -> SettingsCommandHandler {
        let entity = cx.entity();
        Rc::new(move |command, _, app| {
            entity.update(app, |panel, cx| panel.execute(command, cx));
        })
    }

    fn navigation(&self, cx: &Context<Self>) -> AnyElement {
        let active = self.active_page;
        let active_resource_category = self.resource_category;
        let entity = cx.entity();
        let theme = cx.theme();
        let shell = ShellColors::from_theme(theme);
        let colors = theme.colors.clone();
        let text_size = ui_text_size(theme, UiTextSize::Base);
        let group_text_size = ui_text_size(theme, UiTextSize::Sm);
        let back_handler = self.back_handler.clone();
        let navigation_handler = self.navigation_handler.clone();
        div()
            .flex()
            .flex_col()
            .w(px(268.0))
            .flex_none()
            .bg(shell.sidebar)
            .child(
                div().flex_none().relative().h(px(48.0)).child(
                    img("native/brand/icon.png")
                        .absolute()
                        .left(px(17.0))
                        .top(px(19.0))
                        .size(px(20.0)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap_3()
                    .px_2()
                    .pt_3()
                    .pb_3()
                    .child(
                        div()
                            .id("settings-back-to-workspace")
                            .role(Role::Button)
                            .aria_label("返回工作区")
                            .tab_index(0)
                            .flex()
                            .items_center()
                            .h(px(32.0))
                            .m_1()
                            .w(px(244.0))
                            .gap_2()
                            .px_1p5()
                            .rounded(px(12.0))
                            .text_size(text_size)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(colors.fg_muted)
                            .hover(|style| style.bg(colors.hover).text_color(colors.fg))
                            .cursor_pointer()
                            .focus_ring(cx)
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.prevent_default()
                            })
                            .when_some(back_handler.clone(), |view, handler| {
                                view.on_click(move |_, _, cx| handler(cx))
                            })
                            .on_key_down(move |event, _, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    let Some(handler) = back_handler.as_ref() else {
                                        return;
                                    };
                                    cx.stop_propagation();
                                    handler(cx);
                                }
                            })
                            .child(Icon::new(IconName::ArrowLeft).size(IconSize::Md))
                            .child("返回工作区"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .id("settings-nav-scroll")
                    .overflow_y_scroll()
                    .px_2()
                    .pb_3()
                    .child(div().flex().flex_col().gap_4().children(
                        SETTINGS_NAV_GROUPS.iter().enumerate().map(
                            |(group_index, (title, items))| {
                                div()
                                    .id(format!("settings-nav-group-{group_index}"))
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .px_2p5()
                                            .pb_1()
                                            .text_size(group_text_size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(colors.fg_subtle)
                                            .child(*title),
                                    )
                                    .children(items.iter().copied().map(|item| {
                                        let click_entity = entity.clone();
                                        let key_entity = entity.clone();
                                        let click_navigation = navigation_handler.clone();
                                        let key_navigation = navigation_handler.clone();
                                        let selected = match item {
                                            SettingsNavItem::Page(page) => page == active,
                                            SettingsNavItem::Resource(category) => {
                                                active == SettingsPage::Resources
                                                    && category == active_resource_category
                                            }
                                        };
                                        let icon_color = colors.fg;
                                        div()
                                            .id(format!("settings-nav-item-{}", item.key()))
                                            .role(Role::Button)
                                            .aria_label(item.title())
                                            .tab_index(0)
                                            .flex()
                                            .items_center()
                                            .h(px(32.0))
                                            .w_full()
                                            .gap_2()
                                            .px_2p5()
                                            .rounded(px(12.0))
                                            .text_size(text_size)
                                            // 来源导航正文继承 400；只有分组标题使用 500。
                                            .font_weight(FontWeight::NORMAL)
                                            // 来源的图标和正文各自显式使用 foreground，
                                            // 未选中项也不能继承按钮外层的 subtle 色。
                                            .text_color(colors.fg)
                                            .when(selected, |row| row.bg(colors.active))
                                            .when(!selected, |row| {
                                                row.hover(|style| style.bg(colors.hover))
                                            })
                                            .cursor_pointer()
                                            .focus_ring(cx)
                                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                                window.prevent_default()
                                            })
                                            .on_click(move |_, _, cx| {
                                                if let Some(page) = click_entity
                                                    .update(cx, |panel, cx| {
                                                        panel.select_nav_item(item, cx)
                                                    })
                                                    && let Some(handler) = click_navigation.as_ref()
                                                {
                                                    handler(page, cx);
                                                }
                                            })
                                            .on_key_down(move |event, _, cx| {
                                                if matches!(
                                                    event.keystroke.key.as_str(),
                                                    "enter" | "space"
                                                ) {
                                                    cx.stop_propagation();
                                                    if let Some(page) = key_entity
                                                        .update(cx, |panel, cx| {
                                                            panel.select_nav_item(item, cx)
                                                        })
                                                        && let Some(handler) =
                                                            key_navigation.as_ref()
                                                    {
                                                        handler(page, cx);
                                                    }
                                                }
                                            })
                                            .child(
                                                settings_nav_item_icon(item)
                                                    .size(IconSize::Md)
                                                    .color(icon_color),
                                            )
                                            .child(item.title())
                                    }))
                            },
                        ),
                    )),
            )
            .into_any_element()
    }

    fn page_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = self.current.as_ref() else {
            return div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(ui_text_size(cx.theme(), UiTextSize::Sm))
                        .text_color(cx.theme().colors.fg_muted)
                        .child(if self.loading {
                            "正在加载设置..."
                        } else {
                            "当前页面没有可显示的确认快照。"
                        }),
                )
                .into_any_element();
        };
        let dispatch = self.dispatch_handler(cx);
        match self.active_page {
            SettingsPage::General => snapshot
                .general
                .as_ref()
                .map(|value| general::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("常规")),
            SettingsPage::Appearance => snapshot
                .general
                .as_ref()
                .map(|value| appearance::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("外观")),
            SettingsPage::Keyboard => snapshot
                .keyboard
                .as_ref()
                .map(|value| keyboard::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("键盘")),
            SettingsPage::Providers => snapshot
                .providers
                .as_ref()
                .map(|value| providers::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("模型与供应商")),
            SettingsPage::Hooks => snapshot
                .hooks
                .as_ref()
                .map(|value| hooks::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("Hooks")),
            SettingsPage::Resources => snapshot
                .resources
                .as_ref()
                .map(|value| {
                    resources::render(value, self.resource_category, dispatch, window, &mut *cx)
                })
                .unwrap_or_else(|| self.missing_page(self.resource_category.title())),
            SettingsPage::Automations => snapshot
                .automations
                .as_ref()
                .map(|value| automation::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("自动化")),
            SettingsPage::Workflows => snapshot
                .workflows
                .as_ref()
                .map(|value| workflows::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("已保存工作流")),
            SettingsPage::Agents => snapshot
                .agents
                .as_ref()
                .map(|value| agents::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("智能体与子智能体")),
            SettingsPage::Usage => snapshot
                .usage
                .as_ref()
                .map(|value| usage::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("用量")),
            SettingsPage::Diagnostics => snapshot
                .diagnostics
                .as_ref()
                .map(|value| diagnostics::render(value, dispatch, window, &mut *cx))
                .unwrap_or_else(|| self.missing_page("诊断")),
        }
    }

    fn active_title(&self) -> &'static str {
        if self.active_page == SettingsPage::Resources {
            self.resource_category.title()
        } else {
            self.active_page.title()
        }
    }

    fn missing_page(&self, title: &'static str) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Caption::new(format!(
                "{title}页面暂时没有可显示的设置数据。"
            )))
            .into_any_element()
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (shell, colors, loading_text_size, error_text_size) = {
            let theme = cx.theme();
            (
                ShellColors::from_theme(theme),
                theme.colors.clone(),
                ui_text_size(theme, UiTextSize::Xs),
                ui_text_size(theme, UiTextSize::Sm),
            )
        };
        let body = self.page_body(window, cx);
        let page_scroll = self.page_scroll.clone();
        let scrollbar_style = ScrollbarVisualStyle::windows_settings(&colors);
        let error = self.error.clone();
        let notice = self.notice.clone();
        let title_size = page_title_size(cx.theme());
        let title_line_height = px(SETTINGS_TITLE_LINE_HEIGHT * cx.theme().font_scale);
        let frame_radius = if cfg!(target_os = "windows") {
            5.0
        } else {
            12.0
        };
        div()
            .size_full()
            .flex()
            .bg(shell.sidebar)
            .text_color(colors.fg)
            .child(self.navigation(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .mt(px(DESKTOP_INSET))
                    .mb(px(DESKTOP_INSET))
                    .mr(px(DESKTOP_INSET))
                    .border_1()
                    .border_color(colors.border)
                    .bg(shell.content)
                    .rounded(px(frame_radius))
                    .overflow_hidden()
                    .child(div().h(px(TITLEBAR_HEIGHT)).flex_none())
                    // 表单可能位于长页底部；错误固定在滚动区外，避免保存失败后反馈藏在页首。
                    .when_some(error, |view, error| {
                        view.child(
                            div()
                                .id("settings-command-error")
                                .role(Role::Alert)
                                .aria_label(error.message.clone())
                                .flex_none()
                                .w_full()
                                .max_w(px(CONTENT_MAX_WIDTH))
                                .mx_auto()
                                .px_8()
                                .pb_3()
                                .text_size(error_text_size)
                                .text_color(colors.danger)
                                .child(error.message),
                        )
                    })
                    .when_some(
                        notice
                            .filter(|notice| notice.level != super::contracts::NoticeLevel::Error),
                        |view, notice| {
                            view.child(
                                div()
                                    .id("settings-command-notice")
                                    // 成功/信息反馈不能随表单滚动离开窗口；状态语义同时
                                    // 让 UIA 读取到完整文案，而不是只看到视觉文本。
                                    .role(Role::Status)
                                    .aria_label(notice.message.clone())
                                    .flex_none()
                                    .w_full()
                                    .max_w(px(CONTENT_MAX_WIDTH))
                                    .mx_auto()
                                    .px_8()
                                    .pb_3()
                                    .text_size(loading_text_size)
                                    .text_color(colors.fg_muted)
                                    .child(notice.message),
                            )
                        },
                    )
                    .child(
                        div()
                            .id("settings-page-scroll")
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("settings-page-scroll-body")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .track_scroll(&page_scroll)
                                    // 来源 main 使用 scrollbar-gutter: stable；桌面滚动槽约占 15px，
                                    // 这里保留相同的内容侧 inset，避免标题和设置卡片向右偏移。
                                    .pr(px(15.0))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_8()
                                            .w_full()
                                            .max_w(px(CONTENT_MAX_WIDTH))
                                            .mx_auto()
                                            .px_8()
                                            .pb(px(40.0))
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .text_size(title_size)
                                                    .line_height(title_line_height)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(colors.fg)
                                                    .child(self.active_title())
                                                    .when(self.loading, |view| {
                                                        view.child(
                                                            div()
                                                                .text_size(loading_text_size)
                                                                .text_color(colors.fg_muted)
                                                                .child("处理中"),
                                                        )
                                                    }),
                                            )
                                            .child(body),
                                    ),
                            )
                            .child(
                                Scrollbar::new(
                                    "settings-page-scrollbar",
                                    &page_scroll,
                                    Axis::Vertical,
                                )
                                .visual_style(scrollbar_style),
                            ),
                    ),
            )
    }
}

fn settings_page_icon(page: SettingsPage) -> IconName {
    match page {
        SettingsPage::General => IconName::SlidersHorizontal,
        SettingsPage::Appearance => IconName::Palette,
        SettingsPage::Keyboard => IconName::Keyboard,
        SettingsPage::Providers => IconName::Package,
        SettingsPage::Hooks => IconName::Link,
        SettingsPage::Resources => IconName::Puzzle,
        SettingsPage::Automations => IconName::AlarmClock,
        SettingsPage::Workflows => IconName::Workflow,
        SettingsPage::Agents => IconName::Bot,
        SettingsPage::Usage => IconName::ChartBar,
        SettingsPage::Diagnostics => IconName::Bug,
    }
}

fn settings_nav_item_icon(item: SettingsNavItem) -> Icon {
    match item {
        SettingsNavItem::Page(SettingsPage::General) => {
            Icon::from_path("native/icons/settings-2.svg")
        }
        SettingsNavItem::Page(SettingsPage::Hooks) => Icon::from_path("native/icons/anchor.svg"),
        SettingsNavItem::Page(page) => Icon::new(settings_page_icon(page)),
        SettingsNavItem::Resource(resources::ResourceCategory::Plugins) => {
            Icon::from_path("native/icons/blocks.svg")
        }
        SettingsNavItem::Resource(resources::ResourceCategory::Mcp) => {
            Icon::from_path("native/icons/cable.svg")
        }
        SettingsNavItem::Resource(resources::ResourceCategory::Skills) => {
            Icon::new(IconName::WandSparkles)
        }
        SettingsNavItem::Resource(resources::ResourceCategory::Memory) => {
            Icon::from_path("native/icons/brain.svg")
        }
        SettingsNavItem::Resource(resources::ResourceCategory::AgentTemplates) => {
            Icon::new(IconName::FileCog)
        }
    }
}

impl Drop for SettingsPanel {
    fn drop(&mut self) {
        self.closed = true;
        self.subscription_task.take();
        if let Some(subscription) = self.subscription.take() {
            subscription.close();
        }
    }
}
