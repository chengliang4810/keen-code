//! GPUI 原生工作台根实体。
//!
//! 这里负责把侧栏、会话正文和输入区接到 `NativeHostApi`。已创建会话的组件发出
//! `NativeUiAction`，未创建会话的 Draft Composer 先把配置意图交给根实体；任何
//! 成功事实都必须来自宿主回执、Journal 事件或重新读取的 projection。设置和文件
//! 工作台通过工厂挂载，避免 UI 层直接持有领域服务。

use std::{
    path::Path,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ely_gpui_component::{
    buttons::{ButtonVariant, IconButton},
    forms::{InputEvent, TextInput},
    primitives::{FocusRing, Icon, IconName, Tooltip},
    theme::{ActiveTheme, ControlSize, IconSize},
};
use futures::{
    StreamExt,
    channel::{
        mpsc::{Receiver, Sender, channel},
        oneshot,
    },
};
use gpui::{
    AnyElement, App, Context, Entity, EntityInputHandler, Focusable, FollowMode, IntoElement,
    Keystroke, ListAlignment, ListState, MouseButton, ParentElement, Render, Role, Styled,
    Subscription, Task, Window, WindowControlArea, div, prelude::*, px, relative,
};

use crate::app_updates::{AppUpdateDownloadState, AppUpdateStatus};

use super::{
    chat::ChatView,
    composer::{
        ComposerView, DraftComposerAction, DraftComposerOptions, DraftComposerView,
        MAX_DRAFT_ATTACHMENT_PATH_BYTES, MAX_DRAFT_ATTACHMENT_PATHS,
    },
    model::{
        AttachmentFact, ConversationFact, DraftFact, MAX_SNAPSHOT_METADATA_BYTES, ModelSelection,
        NativeActionReceipt, NativeEventBatch, NativeHostApi, NativeUiAction, NativeUiError,
        NativeUiEvent, NativeUiState, NativeUiSubscription, PageCursor, PermissionMode, PlanMode,
        SessionFact, SessionStatus, UiMessage, WorkspacePage, trim_message_window,
    },
    navigation::{
        NativeNavigationTarget, NavigationDirection, NavigationHistory, NavigationTraversal,
        normalize_project_root,
    },
    settings::{NativeKeybindingAction, NativeKeybindingsState, SettingsPage},
    sidebar::Sidebar,
    style::{
        CONVERSATION_SCROLL_GUTTER, DESKTOP_INSET, SIDEBAR_WIDTH, ShellColors, TITLEBAR_HEIGHT,
        UiTextSize, conversation_content_width, ui_font, ui_text_size,
    },
    workbench::WorkbenchPane,
};

/// 根工作台当前显示的产品区域。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativeUiPage {
    #[default]
    Chat,
    Settings,
    Workbench,
}

const NATIVE_DRAFT_ID: &str = "native-window-draft";

type PanelFactory = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;
type WorkbenchRootChangeHandler = Rc<dyn Fn(&str, &mut App)>;
type NativeCloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;
type SettingsNavigationHandler = Rc<dyn Fn(SettingsPage, &mut App)>;
type WorkbenchNavigationHandler = Rc<dyn Fn(WorkbenchPane, &str, &mut App) -> Result<(), String>>;

const NATIVE_EVENT_MAILBOX_CAPACITY: usize = 512;
/// 普通事件和 Snapshot 正文共用的邮箱文本预算；超出预算的单批次直接记缺口
/// 并触发权威重同步。
const NATIVE_EVENT_MAILBOX_TEXT_BUDGET: usize = 8 * 1024 * 1024;
/// Snapshot 的会话元数据、草稿和输入队列单独计入有限预算，让正文达到 8 MiB
/// 显示上限时仍能完成一次权威快照，同时避免把普通事件预算放大。
const NATIVE_EVENT_MAILBOX_SNAPSHOT_METADATA_BUDGET: usize = MAX_SNAPSHOT_METADATA_BYTES;
/// 一次 receiver 唤醒最多消费的 ready 批次，避免高频流式事件反复调度 GPUI。
const NATIVE_EVENT_MAX_BATCHES_PER_WAKE: usize = 64;
const DRAFT_DEBOUNCE: Duration = Duration::from_millis(200);
const MAX_DRAFT_WAITERS: usize = 32;
static UI_OPERATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// 来源 icon-md 的固定 28px 命中面和 16px 图标，不随 Ely Compact 控件缩成 20px。
fn workspace_header_button(
    id: &'static str,
    icon: Icon,
    label: &'static str,
    on_activate: impl Fn(&mut App) + 'static,
    cx: &mut App,
) -> AnyElement {
    let colors = cx.theme().colors.clone();
    let handler = Rc::new(on_activate);
    let click = Rc::clone(&handler);
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .tab_index(0)
        .size(px(28.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .text_color(colors.fg)
        .cursor_pointer()
        .hover(|style| style.bg(colors.hover))
        .active(|style| style.bg(colors.active))
        .focus_ring(cx)
        .tooltip(Tooltip::text(label))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, _, cx| click(cx))
        .on_key_down(move |event, _, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
                handler(cx);
            }
        })
        .child(icon.size(IconSize::Md))
        .into_any_element()
}

/// 事件邮箱的正文与 Snapshot 元数据双重预算。每个入队元素持有一个 RAII
/// reservation，收取或丢弃元素时自动释放；因此取消订阅销毁 receiver 也不会把预算永久占住。
struct EventMailboxBudget {
    reserved: AtomicUsize,
    snapshot_metadata_reserved: AtomicUsize,
}

impl EventMailboxBudget {
    fn try_reserve_counter(counter: &AtomicUsize, limit: usize, bytes: usize) -> bool {
        if bytes > limit {
            return false;
        }
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > limit {
                return false;
            }
            match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn release_counter(counter: &AtomicUsize, bytes: usize) {
        let previous = counter.fetch_sub(bytes, Ordering::AcqRel);
        debug_assert!(previous >= bytes, "事件邮箱预算释放次数不应超过预留");
    }

    fn try_reserve(&self, text_bytes: usize, snapshot_metadata_bytes: usize) -> bool {
        if !Self::try_reserve_counter(&self.reserved, NATIVE_EVENT_MAILBOX_TEXT_BUDGET, text_bytes)
        {
            return false;
        }
        if Self::try_reserve_counter(
            &self.snapshot_metadata_reserved,
            NATIVE_EVENT_MAILBOX_SNAPSHOT_METADATA_BUDGET,
            snapshot_metadata_bytes,
        ) {
            return true;
        }
        Self::release_counter(&self.reserved, text_bytes);
        false
    }

    fn release(&self, text_bytes: usize, snapshot_metadata_bytes: usize) {
        Self::release_counter(&self.reserved, text_bytes);
        Self::release_counter(&self.snapshot_metadata_reserved, snapshot_metadata_bytes);
    }

    #[cfg(test)]
    fn reserved(&self) -> usize {
        self.reserved
            .load(Ordering::Acquire)
            .saturating_add(self.snapshot_metadata_reserved.load(Ordering::Acquire))
    }
}

struct EventMailboxReservation {
    budget: Arc<EventMailboxBudget>,
    text_bytes: usize,
    snapshot_metadata_bytes: usize,
}

impl Drop for EventMailboxReservation {
    fn drop(&mut self) {
        self.budget
            .release(self.text_bytes, self.snapshot_metadata_bytes);
    }
}

enum EventMailboxItem {
    Batch {
        batch: NativeEventBatch,
        _reservation: EventMailboxReservation,
    },
    /// 大批次或满载丢弃后插入的无正文唤醒标记；实际缺口范围存放在 EventOverflow。
    OverflowWake,
}

impl EventMailboxItem {
    fn into_batch(self) -> Option<NativeEventBatch> {
        match self {
            Self::Batch {
                batch,
                _reservation,
            } => Some(batch),
            Self::OverflowWake => None,
        }
    }
}

fn try_queue_event_batch(
    sender: &mut Sender<EventMailboxItem>,
    budget: &Arc<EventMailboxBudget>,
    overflow: &Arc<Mutex<EventOverflow>>,
    batch: NativeEventBatch,
) -> bool {
    let delivery_sequence = batch.delivery_sequence;
    let snapshot_metadata_bytes = batch.mailbox_snapshot_metadata_bytes();
    let text_bytes = batch
        .mailbox_text_bytes()
        .saturating_sub(snapshot_metadata_bytes);
    if budget.try_reserve(text_bytes, snapshot_metadata_bytes) {
        let item = EventMailboxItem::Batch {
            batch,
            _reservation: EventMailboxReservation {
                budget: Arc::clone(budget),
                text_bytes,
                snapshot_metadata_bytes,
            },
        };
        if sender.try_send(item).is_ok() {
            return true;
        }
    }

    if let Ok(mut overflow) = overflow.lock() {
        overflow.record(delivery_sequence);
    }
    // 若邮箱没有空间，已有元素被消费后会自然看到 overflow；否则用无正文标记
    // 唤醒消费者，保证“只有超大批次被丢弃”时也不会永久等待重同步。
    let _ = sender.try_send(EventMailboxItem::OverflowWake);
    false
}

/// 需要根据宿主回执切换到新会话的动作路由。
///
/// 新建会话可以直接使用动作中的项目根；分支会话必须从当前工作区投影解析项目根，
/// 不能把 `project_key` 当作路径传回宿主。
enum ReceiptRoute {
    Create {
        project_root: String,
        initial_draft: Option<PendingNewSessionDraft>,
    },
    InitialAttachment {
        generation: u64,
        session_id: String,
        path_index: usize,
        operation_id: String,
    },
    InitialSend {
        generation: u64,
        session_id: String,
        operation_id: String,
    },
    Branch {
        project_root: Option<String>,
    },
}

#[derive(Clone)]
struct PendingNewSessionDraft {
    generation: u64,
    project_root: String,
    text: String,
    attachment_paths: Vec<String>,
    model: Option<ModelSelection>,
    effort: Option<String>,
    permission: PermissionMode,
    plan: PlanMode,
}

#[derive(Clone)]
struct PendingInitialSend {
    generation: u64,
    session_id: String,
    project_root: String,
    text: String,
    attachment_paths: Vec<String>,
    next_attachment_index: usize,
    confirmed_attachments: Vec<AttachmentFact>,
    model: Option<ModelSelection>,
    effort: Option<String>,
    permission: PermissionMode,
    plan: PlanMode,
    confirming_attachment: bool,
}

/// 活动会话首个 Snapshot 尚未到达时的发送意图。输入仍由同一个 TextInput 持有，
/// Snapshot 到达后再绑定会话事实并发出唯一的 Send，避免快捷键被加载窗口丢弃。
#[derive(Clone)]
struct PendingSessionSubmit {
    session_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialSendStep {
    Attach,
    Send,
    AwaitConfirmation,
}

fn initial_send_step(pending: &PendingInitialSend) -> InitialSendStep {
    if pending.confirming_attachment {
        return InitialSendStep::AwaitConfirmation;
    }
    if pending.next_attachment_index < pending.attachment_paths.len() {
        return InitialSendStep::Attach;
    }
    if pending.confirmed_attachments.len() == pending.attachment_paths.len() {
        InitialSendStep::Send
    } else {
        InitialSendStep::AwaitConfirmation
    }
}

impl ReceiptRoute {
    fn from_action(ui: &NativeUi, action: &NativeUiAction) -> Option<Self> {
        match action {
            NativeUiAction::CreateSession {
                project_root,
                operation_id,
            } => Some(Self::Create {
                project_root: project_root.clone(),
                initial_draft: operation_id
                    .starts_with("ui:draft-send:")
                    .then(|| ui.pending_new_session_draft.clone())
                    .flatten(),
            }),
            NativeUiAction::AddAttachment {
                session_id,
                operation_id,
                ..
            } => ui
                .pending_initial_send
                .as_ref()
                .filter(|pending| {
                    pending.session_id == *session_id
                        && pending.next_attachment_index < pending.attachment_paths.len()
                        && operation_id.starts_with("ui:draft-attachment:")
                })
                .map(|pending| Self::InitialAttachment {
                    generation: pending.generation,
                    session_id: pending.session_id.clone(),
                    path_index: pending.next_attachment_index,
                    operation_id: operation_id.clone(),
                }),
            NativeUiAction::Send {
                session_id,
                operation_id,
                ..
            } => ui
                .pending_initial_send
                .as_ref()
                .filter(|pending| {
                    pending.session_id == *session_id
                        && pending.next_attachment_index == pending.attachment_paths.len()
                        && operation_id.starts_with("ui:draft-send-prompt:")
                })
                .map(|pending| Self::InitialSend {
                    generation: pending.generation,
                    session_id: pending.session_id.clone(),
                    operation_id: operation_id.clone(),
                }),
            NativeUiAction::Branch { session_id, .. } => Some(Self::Branch {
                project_root: ui.project_root_for_session(session_id),
            }),
            _ => None,
        }
    }
}

/// 订阅回调运行在宿主投递线程上，不能等待 UI 消费者；满载时只保留缺口范围。
/// 消费任务看到该范围后会丢弃旧邮箱内容并重新读取权威快照，避免 token 在内存中堆积。
#[derive(Default)]
struct EventOverflow {
    first_missing_delivery_sequence: Option<u64>,
    last_missing_delivery_sequence: Option<u64>,
}

#[derive(Clone)]
struct PendingDraft {
    session_id: String,
    draft: DraftFact,
    edit_generation: u64,
}

#[derive(Clone)]
struct SubmittedDraft {
    session_id: String,
    text: String,
    field_generation: u64,
}

fn draft_from_field_text(field_text: String, conversation: Option<&ConversationFact>) -> DraftFact {
    let (attachments, mention_query) = conversation
        .map(|conversation| {
            (
                conversation.draft.attachments.clone(),
                conversation.draft.mention_query.clone(),
            )
        })
        .unwrap_or_default();
    DraftFact {
        text: field_text,
        attachments,
        mention_query,
    }
}

enum DraftFlushStart {
    Send(PendingDraft),
    Stop,
}

impl EventOverflow {
    fn record(&mut self, delivery_sequence: u64) {
        self.first_missing_delivery_sequence = Some(
            self.first_missing_delivery_sequence
                .map_or(delivery_sequence, |first| first.min(delivery_sequence)),
        );
        self.last_missing_delivery_sequence = Some(
            self.last_missing_delivery_sequence
                .map_or(delivery_sequence, |last| last.max(delivery_sequence)),
        );
    }

    fn take(&mut self) -> Option<(u64, u64)> {
        let first = self.first_missing_delivery_sequence.take()?;
        let last = self.last_missing_delivery_sequence.take().unwrap_or(first);
        Some((first, last))
    }
}

/// 单窗口原生 UI。
pub struct NativeUi {
    host: Arc<dyn NativeHostApi>,
    keybindings: NativeKeybindingsState,
    root_path: String,
    state: NativeUiState,
    /// 消息列表由 GPUI 保留测量和滚动状态；正文变化只让受影响的条目重新测量。
    message_list: ListState,
    message_list_ids: Vec<String>,
    field: Entity<TextInput>,
    /// 必须随根实体持有订阅，提前丢弃会停止输入草稿的保存回调。
    _field_subscription: Subscription,
    /// NativeHost 的应用级快捷键订阅；由窗口宿主安装并随根实体释放。
    native_control_shortcut: Option<Subscription>,
    /// 在 Ely TextInput keymap 之前接收 Composer 发送键，覆盖可热更新的自定义绑定。
    composer_submit_interceptor: Option<Subscription>,
    /// 保存应用历史键订阅；使用弱实体避免窗口关闭后保留整棵 UI。
    navigation_interceptor: Option<Subscription>,
    /// 自绘标题栏的关闭按钮必须复用 NativeHost 的退出审批链。
    native_close_handler: Option<NativeCloseHandler>,
    subscription: Option<Box<dyn NativeUiSubscription>>,
    event_sender: Option<Sender<EventMailboxItem>>,
    event_task: Option<Task<()>>,
    draft_task: Option<Task<()>>,
    pending_draft: Option<PendingDraft>,
    inflight_draft: Option<PendingDraft>,
    /// 记录最近一次已提交正文，只有提交后用户再次编辑时才保护本地输入。
    submitted_draft: Option<SubmittedDraft>,
    /// 首次发送的输入先停留在 UI，等待 CreateSession 的权威回执后再绑定到新会话。
    pending_new_session_draft: Option<PendingNewSessionDraft>,
    /// 未创建会话时的本地配置意图；附件仅保存路径，事实由 Host 创建会话后确认。
    draft_composer_options: DraftComposerOptions,
    draft_composer_project_root: String,
    draft_composer_generation: u64,
    /// 创建回执后的首次发送流水线；附件必须逐个得到 Host 事实后才能 Send。
    pending_initial_send: Option<PendingInitialSend>,
    pending_session_submit: Option<PendingSessionSubmit>,
    draft_waiters: Vec<oneshot::Sender<Result<(), NativeUiError>>>,
    draft_generation: u64,
    session_generation: u64,
    /// 同一会话的订阅与历史加载各有代次，阻止晚到回执覆盖新的观察边界。
    subscription_generation: u64,
    conversation_load_generation: u64,
    active_session_id: Option<String>,
    last_delivery_sequence: u64,
    field_generation: u64,
    suppress_field_events: bool,
    page: NativeUiPage,
    /// UI 历史只保存稳定的会话/草稿身份；文本和附件继续由当前 Draft 投影持有。
    navigation: NavigationHistory,
    navigation_generation: u64,
    navigation_traversal: Option<NavigationTraversal>,
    settings_page: SettingsPage,
    workbench_pane: WorkbenchPane,
    workbench_root_override: Option<String>,
    last_error: Option<NativeUiError>,
    settings_factory: Option<PanelFactory>,
    settings_navigation: Option<SettingsNavigationHandler>,
    workbench_navigation: Option<WorkbenchNavigationHandler>,
    workbench_factory: Option<PanelFactory>,
    workbench_root_changed: Option<WorkbenchRootChangeHandler>,
    last_workbench_root: Option<String>,
    update_status: Option<AppUpdateStatus>,
    native_badge: Option<u32>,
}

impl NativeUi {
    /// 创建窗口根实体，并立即读取首屏工作区和模型目录。
    pub fn new(
        host: Arc<dyn NativeHostApi>,
        root_path: impl Into<String>,
        keybindings: NativeKeybindingsState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = cx.new(|cx| {
            TextInput::new(window, cx)
                .label("任务输入")
                .multi_line(2, 8)
                .placeholder("输入任务，按 Enter 发送")
        });
        let field_subscription = cx.subscribe(&field, |ui, _, event: &InputEvent, cx| {
            match *event {
                InputEvent::Changed => {
                    // subscribe 已独占借用根实体，再次 Entity::update 会在真实窗口中 double lease。
                    ui.on_field_changed(cx);
                }
                // Composer 的发送快捷键由 NativeUi 级别的拦截器统一处理；若在这里
                // 响应固定的 Ely `secondary-enter` action，用户改绑后旧快捷键仍会发送。
                InputEvent::Submit => {}
                InputEvent::Focus | InputEvent::Blur => {}
            }
        });

        let root_path = root_path.into();
        let mut ui = Self {
            host,
            keybindings,
            root_path: root_path.clone(),
            state: NativeUiState::default(),
            message_list: {
                // 来源聊天列表以 Top 布局承载 Tail 跟随；短会话从内容顶部开始，流式内容
                // 超出视口后仍自动贴住末尾，用户手动上滚时由 GPUI 暂停跟随。
                let list = ListState::new(0, ListAlignment::Top, px(240.0));
                list.set_follow_mode(FollowMode::Tail);
                list
            },
            message_list_ids: Vec::new(),
            field,
            _field_subscription: field_subscription,
            native_control_shortcut: None,
            composer_submit_interceptor: None,
            navigation_interceptor: None,
            native_close_handler: None,
            subscription: None,
            event_sender: None,
            event_task: None,
            draft_task: None,
            pending_draft: None,
            inflight_draft: None,
            submitted_draft: None,
            pending_new_session_draft: None,
            draft_composer_options: DraftComposerOptions::default(),
            draft_composer_project_root: root_path.clone(),
            draft_composer_generation: 0,
            pending_initial_send: None,
            pending_session_submit: None,
            draft_waiters: Vec::new(),
            draft_generation: 0,
            session_generation: 0,
            subscription_generation: 0,
            conversation_load_generation: 0,
            active_session_id: None,
            last_delivery_sequence: 0,
            field_generation: 0,
            suppress_field_events: false,
            page: NativeUiPage::Chat,
            navigation: NavigationHistory::with_initial(NativeNavigationTarget::chat_draft(
                NATIVE_DRAFT_ID,
                &root_path,
            )),
            navigation_generation: 0,
            navigation_traversal: None,
            settings_page: SettingsPage::General,
            workbench_pane: WorkbenchPane::Files,
            workbench_root_override: None,
            last_error: None,
            settings_factory: None,
            settings_navigation: None,
            workbench_navigation: None,
            workbench_factory: None,
            workbench_root_changed: None,
            last_workbench_root: None,
            update_status: None,
            native_badge: None,
        };
        let weak = cx.entity().downgrade();
        let keybindings_for_interceptor = ui.keybindings.clone();
        ui.composer_submit_interceptor =
            Some(cx.intercept_keystrokes(move |event, window, app| {
                if !keybindings_for_interceptor
                    .matches(NativeKeybindingAction::ComposerSubmit, &event.keystroke)
                {
                    return;
                }
                let handled = weak
                    .update(app, |ui, cx| {
                        ui.intercept_composer_submit(&event.keystroke, window, cx)
                    })
                    .ok()
                    .unwrap_or(false);
                if handled {
                    app.stop_propagation();
                }
            }));
        let navigation_ui = cx.entity().downgrade();
        ui.navigation_interceptor = Some(cx.intercept_keystrokes(move |event, _, app| {
            let key = &event.keystroke;
            if !key.modifiers.alt
                || key.modifiers.control
                || key.modifiers.shift
                || key.modifiers.platform
            {
                return;
            }
            let direction = match key.key.as_str() {
                "left" => NavigationDirection::Back,
                "right" => NavigationDirection::Forward,
                _ => return,
            };
            let handled = navigation_ui
                .update(app, |ui, cx| {
                    let enabled = ui.navigation_traversal.is_none()
                        && match direction {
                            NavigationDirection::Back => ui.navigation.can_go_back(),
                            NavigationDirection::Forward => ui.navigation.can_go_forward(),
                        };
                    if enabled {
                        ui.start_history_traversal(direction, cx);
                    }
                    enabled
                })
                .unwrap_or(false);
            if handled {
                app.stop_propagation();
            }
        }));
        ui.load_workspace_page(None, false, cx);
        ui.load_model_catalog(cx);
        ui
    }

    /// 保存 NativeHost 的全局退出快捷键订阅，避免订阅在安装函数返回时被释放。
    pub(crate) fn install_native_control_shortcut(&mut self, subscription: Subscription) {
        self.native_control_shortcut = Some(subscription);
    }

    /// 由窗口宿主安装经过 NativeControls 的关闭回调；标题栏只转发，不直接销毁窗口。
    pub(crate) fn install_native_close_handler(
        &mut self,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) {
        self.native_close_handler = Some(Rc::new(handler));
    }

    /// 允许在创建根实体前提供关闭回调；窗口控制器尚未就绪时通常使用 install 版本。
    pub fn with_native_close_handler(
        mut self,
        handler: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.install_native_close_handler(handler);
        self
    }

    /// 允许宿主在创建实体时挂载真正的设置面板。
    pub fn with_settings_panel(
        mut self,
        factory: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.settings_factory = Some(Rc::new(factory));
        self
    }

    /// 设置页由独立 SettingsPanel 持有事实；根实体只负责切换页面并转发选中页。
    pub fn with_settings_navigation(
        mut self,
        handler: impl Fn(SettingsPage, &mut App) + 'static,
    ) -> Self {
        self.settings_navigation = Some(Rc::new(handler));
        self
    }

    /// 所有工作台入口和历史恢复共用同一实体；切换失败时保持当前页面与 cursor。
    pub fn with_workbench_navigation(
        mut self,
        handler: impl Fn(WorkbenchPane, &str, &mut App) -> Result<(), String> + 'static,
    ) -> Self {
        self.workbench_navigation = Some(Rc::new(handler));
        self
    }

    /// 允许宿主在创建实体时挂载真正的文件/终端工作台面板。
    pub fn with_workbench_panel(
        mut self,
        factory: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.workbench_factory = Some(Rc::new(factory));
        self
    }

    /// 工作台面板由宿主持有根目录；仅在活动项目根真正变化时通知宿主切换资源。
    pub fn with_workbench_root_changed(
        mut self,
        handler: impl Fn(&str, &mut App) + 'static,
    ) -> Self {
        self.workbench_root_changed = Some(Rc::new(handler));
        self
    }

    /// 托盘/原生菜单的新建会话入口，仍复用 CreateSession 的宿主授权和回执路由。
    pub fn native_new_chat(&mut self, cx: &mut Context<Self>) {
        self.invalidate_draft_composer(cx);
        let project_root = self
            .state
            .workspace_root
            .clone()
            .unwrap_or_else(|| self.root_path.clone());
        self.dispatch_action(
            NativeUiAction::CreateSession {
                project_root,
                operation_id: "ui:native-new-chat".to_owned(),
            },
            cx,
        );
    }

    /// 原生菜单的搜索入口；切换到工作台后由宿主选择真实 Search pane。
    pub fn native_open_search(&mut self, cx: &mut Context<Self>) {
        self.open_search(cx);
    }

    /// SettingsPanel 返回按钮使用同一页面状态入口，避免宿主直接依赖私有 `set_page`。
    pub fn native_back_to_chat(&mut self, cx: &mut Context<Self>) {
        self.set_page(NativeUiPage::Chat, cx);
    }

    /// SettingsPanel 的用户点击回调只更新历史；页面事实已由面板先行切换，避免再次
    /// 调用外部 select_page 形成递归或把历史恢复误记成用户导航。
    pub fn native_settings_page_selected(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.page != NativeUiPage::Settings {
            return;
        }
        self.record_user_navigation(NativeNavigationTarget::settings(page));
        self.settings_page = page;
        self.last_error = None;
        cx.notify();
    }

    /// 面板已确认用户切换标签后只记录身份，不再次调用面板造成递归。
    pub fn native_workbench_pane_selected(
        &mut self,
        pane: WorkbenchPane,
        project_root: &str,
        cx: &mut Context<Self>,
    ) {
        if self.page != NativeUiPage::Workbench {
            return;
        }
        self.record_user_navigation(NativeNavigationTarget::workbench(pane, project_root));
        self.workbench_pane = pane;
        self.workbench_root_override = Some(project_root.to_owned());
        self.last_workbench_root = Some(project_root.to_owned());
        cx.notify();
    }

    /// 托盘/原生菜单的会话入口，先从当前工作区投影解析项目根，再走 OpenSession。
    pub fn native_open_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let Some(project_root) = self.project_root_for_session(session_id) else {
            self.set_error(NativeUiError::new(
                "session-project-missing",
                "未找到会话所属项目，无法打开会话",
                true,
            ));
            cx.notify();
            return;
        };
        self.invalidate_draft_composer(cx);
        self.record_user_navigation(NativeNavigationTarget::chat_session(
            session_id,
            &project_root,
        ));
        self.open_session_after_draft(session_id.to_owned(), project_root, cx);
    }

    /// 将 NativeHost 的更新事实投影到工作台，避免后台更新事件只写日志而用户不可见。
    pub fn handle_native_update_status(&mut self, status: AppUpdateStatus, cx: &mut Context<Self>) {
        self.update_status = Some(status);
        cx.notify();
    }

    /// 保存已由平台应用的角标数量，并在工作台顶部显示同一事实。
    pub fn handle_native_badge(&mut self, count: Option<u32>, cx: &mut Context<Self>) {
        self.native_badge = count.filter(|count| *count > 0);
        cx.notify();
    }

    /// 供 NativeHost 构造窗口后按需重新读取工作区。
    pub fn load_workspace(&mut self, cx: &mut Context<Self>) {
        self.load_workspace_page(None, false, cx);
    }

    /// 打开指定 Session；成功读取快照后才建立事件订阅。
    pub fn open_session(
        &mut self,
        session_id: String,
        project_root: String,
        cx: &mut Context<Self>,
    ) {
        self.open_session_internal(session_id, project_root, None, cx);
    }

    fn open_session_internal(
        &mut self,
        session_id: String,
        project_root: String,
        traversal: Option<NavigationTraversal>,
        cx: &mut Context<Self>,
    ) {
        if let Some(traversal) = traversal {
            self.open_session_for_history(session_id, project_root, traversal, cx);
            return;
        }
        let generation = self.prepare_session_activation(&session_id, &project_root, cx);
        let host = Arc::clone(&self.host);
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let opened = host
                .dispatch(NativeUiAction::OpenSession {
                    session_id: session_id.clone(),
                    project_root: project_root.clone(),
                })
                .await;
            if let Err(error) = opened {
                let _ = weak.update(cx, |ui, cx| {
                    if ui.session_generation != generation {
                        return;
                    }
                    // 打开失败同时失效所有晚到的订阅回执。
                    ui.cancel_subscription();
                    ui.active_session_id = None;
                    ui.clear_conversation_projection();
                    ui.set_error(error);
                    cx.notify();
                });
                return;
            }
            let _ = weak.update(cx, |ui, cx| {
                if ui.session_generation != generation {
                    return;
                }
                ui.restart_subscription(session_id, cx);
            });
        })
        .detach();
    }

    fn prepare_session_activation(
        &mut self,
        session_id: &str,
        project_root: &str,
        cx: &mut Context<Self>,
    ) -> u64 {
        let previous_session_id = self.active_session_id.clone();
        let preserve_initial_send = self
            .pending_initial_send
            .as_ref()
            .is_some_and(|pending| pending.session_id == session_id);
        if !preserve_initial_send {
            self.invalidate_draft_composer(cx);
            if previous_session_id.as_deref() != Some(session_id) {
                // 快照到达前清掉上一会话的输入；随后用户在空投影中输入的正文由
                // `on_field_changed` 保留，避免把旧会话文本误当成当前会话草稿。
                self.suppress_field_events = true;
                self.field.update(cx, |field, cx| field.set_text("", cx));
                self.suppress_field_events = false;
            }
        }
        // 打开跨项目会话时同步切换当前新建会话根目录；否则侧栏顶部按钮会在
        // 异步快照返回前继续把新会话发到上一个项目。
        self.root_path = project_root.to_owned();
        self.state.workspace_root = Some(project_root.to_owned());
        self.workbench_root_override = None;
        self.cancel_subscription();
        self.session_generation = self.session_generation.wrapping_add(1);
        let generation = self.session_generation;
        self.active_session_id = Some(session_id.to_owned());
        // 历史恢复在宿主打开成功后进入此处，页面与目标会话一起切换。
        self.page = NativeUiPage::Chat;
        self.last_delivery_sequence = 0;
        self.clear_conversation_projection();
        self.last_error = None;
        cx.notify();
        generation
    }

    fn open_session_for_history(
        &mut self,
        session_id: String,
        project_root: String,
        traversal: NavigationTraversal,
        cx: &mut Context<Self>,
    ) {
        if !self
            .navigation_traversal
            .as_ref()
            .is_some_and(|current| current.token == traversal.token)
        {
            return;
        }
        let host = Arc::clone(&self.host);
        let weak = cx.entity().downgrade();
        let token = traversal.token;
        cx.spawn(async move |_, cx| {
            let opened = host
                .dispatch(NativeUiAction::OpenSession {
                    session_id: session_id.clone(),
                    project_root: project_root.clone(),
                })
                .await;
            if let Err(error) = opened {
                let _ = weak.update(cx, |ui, cx| {
                    if !ui
                        .navigation_traversal
                        .as_ref()
                        .is_some_and(|current| current.token == token)
                    {
                        return;
                    }
                    // 恢复失败时旧会话投影仍然有效；只移除失效目标并沿原方向继续。
                    ui.retry_navigation_after_failure(traversal.clone(), error, cx);
                    cx.notify();
                });
                return;
            }
            let _ = weak.update(cx, |ui, cx| {
                if !ui
                    .navigation_traversal
                    .as_ref()
                    .is_some_and(|current| current.token == token)
                {
                    return;
                }
                ui.prepare_session_activation(&session_id, &project_root, cx);
                ui.restart_subscription(session_id, cx);
                ui.navigation_traversal = None;
            });
        })
        .detach();
    }

    fn restart_subscription(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.active_session_id.as_deref() != Some(session_id.as_str()) {
            return;
        }
        // 新订阅的首批 Snapshot 与后续增量共用一个序列边界。一次性的
        // load_conversation 无法排除旧订阅在读取期间产生的重复 token。
        self.cancel_subscription();
        self.conversation_load_generation = self.conversation_load_generation.wrapping_add(1);
        let generation = self.session_generation;
        let subscription_generation = self.subscription_generation;
        let (sender, overflow, budget) = self.start_event_loop(session_id.clone(), cx);
        let host = Arc::clone(&self.host);
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let sink_sender = Arc::new(Mutex::new(sender.clone()));
            let sink_overflow = Arc::clone(&overflow);
            let sink_budget = Arc::clone(&budget);
            let subscription = host
                .subscribe_session(
                    session_id.clone(),
                    Arc::new(move |batch| {
                        if let Ok(mut sender) = sink_sender.lock() {
                            let _ = try_queue_event_batch(
                                &mut sender,
                                &sink_budget,
                                &sink_overflow,
                                batch,
                            );
                        } else if let Ok(mut overflow) = sink_overflow.lock() {
                            overflow.record(batch.delivery_sequence);
                        }
                    }),
                )
                .await;
            let _ = weak.update(cx, |ui, cx| {
                if ui.session_generation != generation
                    || ui.subscription_generation != subscription_generation
                    || ui.active_session_id.as_deref() != Some(session_id.as_str())
                {
                    if let Ok(mut subscription) = subscription {
                        subscription.cancel();
                    }
                    return;
                }
                match subscription {
                    Ok(subscription) => ui.subscription = Some(subscription),
                    Err(error) => {
                        // 订阅建立失败同样需要终止已启动的消费任务；否则邮箱会继续
                        // 等待事件，即使 UI 已经无法收到任何权威更新。
                        ui.cancel_subscription();
                        ui.active_session_id = None;
                        ui.clear_conversation_projection();
                        ui.set_error(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 显式取消当前 Session 订阅，供窗口关闭路径调用。
    pub fn shutdown(&mut self) {
        self.cancel_subscription();
    }

    /// 清理当前消息投影时同时清空 GPUI 的列表长度；否则切换到空会话时旧索引
    /// 仍会被 ListState 请求，表现为正文区域空白但占用上一会话的滚动空间。
    fn clear_conversation_projection(&mut self) {
        self.state.conversation = None;
        self.message_list_ids.clear();
        self.message_list.reset(0);
        self.submitted_draft = None;
        self.pending_session_submit = None;
    }

    /// Host 目录把当前激活的默认模型标在目录项上；这里只做渲染投影，不写回
    /// Session 事实，避免把用户尚未发送的默认值伪装成 Journal 回执。
    fn default_model_projection(&self) -> Option<ModelSelection> {
        self.state.model_catalog.iter().find_map(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.is_default)
                .map(|model| ModelSelection {
                    provider_id: provider.provider_id.clone(),
                    model: model.model.clone(),
                })
        })
    }

    fn invalidate_draft_composer(&mut self, cx: &mut Context<Self>) {
        self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
        self.pending_new_session_draft = None;
        self.pending_initial_send = None;
        self.pending_session_submit = None;
        self.draft_composer_options = DraftComposerOptions {
            model: self.default_model_projection(),
            ..DraftComposerOptions::default()
        };
        if self.state.conversation.is_none() && !self.field.read(cx).text().is_empty() {
            self.suppress_field_events = true;
            self.field.update(cx, |field, cx| field.set_text("", cx));
            self.suppress_field_events = false;
        }
    }

    fn reset_draft_composer_for_project(&mut self, project_root: &str, cx: &mut Context<Self>) {
        if self.draft_composer_project_root == project_root {
            return;
        }
        self.draft_composer_project_root = project_root.to_owned();
        self.invalidate_draft_composer(cx);
    }

    /// 在 GPUI keymap 解析前拦截发送键；否则 Ely TextInput 的固定 `secondary-enter`
    /// action 会先消费事件，Composer 自己的 capture listener 无法看到 ctrl-enter。
    fn intercept_composer_submit(
        &mut self,
        stroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // 拦截器属于窗口根实体；隐藏 Composer 时不能沿用残留焦点提交会话。
        if self.page != NativeUiPage::Chat {
            return false;
        }
        if !self.field.read(cx).focus_handle(cx).is_focused(window) {
            return false;
        }
        let composing = self.field.update(cx, |field, cx| {
            field.marked_text_range(window, cx).is_some()
        });
        if composing
            || !self
                .keybindings
                .matches(NativeKeybindingAction::ComposerSubmit, stroke)
        {
            return false;
        }
        self.submit_composer(cx);
        true
    }

    /// 根键盘拦截器统一处理提交，确保草稿态与活动会话态都经过 NativeHost 的
    /// 真实 Send/CreateSession 链路；TextInput 的 Submit action 不再重复发送。
    fn submit_composer(&mut self, cx: &mut Context<Self>) {
        if self.pending_new_session_draft.is_some() || self.pending_initial_send.is_some() {
            self.handle_draft_composer_action(DraftComposerAction::Submit, cx);
        } else if self.state.conversation.is_some() {
            self.dispatch_active_composer_send(cx);
        } else {
            self.handle_draft_composer_action(DraftComposerAction::Submit, cx);
        }
    }

    fn dispatch_active_composer_send(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(conversation) = self.state.conversation.as_ref() else {
            if let Some(session_id) = self.active_session_id.clone()
                && !self.field.read(cx).text().trim().is_empty()
            {
                self.pending_session_submit = Some(PendingSessionSubmit { session_id });
            }
            return false;
        };
        if matches!(
            conversation.session.status,
            SessionStatus::Running | SessionStatus::Queued | SessionStatus::Waiting
        ) {
            return true;
        }
        let text = self.field.read(cx).text().trim().to_owned();
        if text.is_empty() {
            return true;
        }
        let session_id = conversation.session.session_id.clone();
        self.dispatch_action(
            NativeUiAction::Send {
                session_id: session_id.clone(),
                text,
                attachments: conversation.draft.attachments.clone(),
                model: conversation.session.model.clone(),
                effort: conversation.session.effort.clone(),
                permission: conversation.session.permission,
                plan: conversation.session.plan,
                draft_edit_generation: 0,
                operation_id: format!("ui:send:{session_id}"),
            },
            cx,
        );
        true
    }

    fn handle_draft_composer_action(
        &mut self,
        action: DraftComposerAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            DraftComposerAction::Submit => {
                if self.pending_new_session_draft.is_some() {
                    return;
                }
                if self.pending_initial_send.is_some() {
                    if !self.initial_send_in_flight() {
                        let text = self.field.read(cx).text().trim().to_owned();
                        if !text.is_empty()
                            && let Some(pending) = self.pending_initial_send.as_mut()
                        {
                            pending.text = text;
                        }
                    }
                    self.dispatch_pending_initial_send(cx);
                    return;
                }
                let text = self.field.read(cx).text().trim().to_owned();
                if self.active_session_id.is_some() && self.state.conversation.is_none() {
                    // 创建回执已切换观察边界，但首个 Snapshot 尚未到达；先记录发送意图，
                    // 由 Snapshot 到达后的活动会话路径继续发送，避免再次创建空会话。
                    if !text.is_empty()
                        && let Some(session_id) = self.active_session_id.clone()
                    {
                        self.pending_session_submit = Some(PendingSessionSubmit { session_id });
                    }
                    return;
                }
                if self.root_path.trim().is_empty() || text.is_empty() {
                    return;
                }
                let options = self.draft_composer_options.clone();
                self.pending_new_session_draft = Some(PendingNewSessionDraft {
                    generation: self.draft_composer_generation,
                    project_root: self.root_path.clone(),
                    text,
                    attachment_paths: options.attachment_paths,
                    model: options.model,
                    effort: options.effort,
                    permission: options.permission,
                    plan: options.plan,
                });
                self.dispatch_action(
                    NativeUiAction::CreateSession {
                        project_root: self.root_path.clone(),
                        operation_id: "ui:draft-send".to_owned(),
                    },
                    cx,
                );
            }
            DraftComposerAction::SelectModel(selection) => {
                if self.pending_new_session_draft.is_some() || self.pending_initial_send.is_some() {
                    return;
                }
                self.draft_composer_options.model = Some(selection);
                // 模型切换后旧思考强度可能不再属于新模型，让 Host 使用新模型默认值。
                self.draft_composer_options.effort = None;
                self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
                cx.notify();
            }
            DraftComposerAction::SelectEffort(effort) => {
                if self.pending_new_session_draft.is_some() || self.pending_initial_send.is_some() {
                    return;
                }
                self.draft_composer_options.effort = Some(effort);
                self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
                cx.notify();
            }
            DraftComposerAction::SelectPermission(permission) => {
                if self.pending_new_session_draft.is_some() || self.pending_initial_send.is_some() {
                    return;
                }
                self.draft_composer_options.permission = permission;
                self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
                cx.notify();
            }
            DraftComposerAction::SelectPlan(plan) => {
                if self.pending_new_session_draft.is_some() || self.pending_initial_send.is_some() {
                    return;
                }
                self.draft_composer_options.plan = plan;
                self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
                cx.notify();
            }
            DraftComposerAction::AddAttachmentPaths(paths) => {
                if self.pending_new_session_draft.is_some() || self.pending_initial_send.is_some() {
                    return;
                }
                let requested = paths.len();
                let accepted = append_draft_attachment_paths(
                    &mut self.draft_composer_options.attachment_paths,
                    paths,
                );
                if accepted < requested {
                    self.set_error(NativeUiError::new(
                        "draft-attachment-limit",
                        "附件数量或路径长度超过限制，已保留可接受的文件",
                        true,
                    ));
                }
                self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
                cx.notify();
            }
            DraftComposerAction::RemoveAttachmentPath(path) => {
                if self.pending_new_session_draft.is_some() || self.pending_initial_send.is_some() {
                    return;
                }
                self.draft_composer_options
                    .attachment_paths
                    .retain(|current| current != &path);
                self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
                cx.notify();
            }
        }
    }

    fn initial_send_in_flight(&self) -> bool {
        let Some(pending) = self.pending_initial_send.as_ref() else {
            return false;
        };
        pending.confirming_attachment
            || self
                .state
                .pending_operations
                .values()
                .any(|action| match action {
                    NativeUiAction::AddAttachment { session_id, .. }
                    | NativeUiAction::Send { session_id, .. } => session_id == &pending.session_id,
                    _ => false,
                })
    }

    fn dispatch_pending_initial_send(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_initial_send.as_ref().cloned() else {
            return;
        };
        if pending.project_root != self.root_path {
            return;
        }
        if self.initial_send_in_flight() {
            return;
        }
        match initial_send_step(&pending) {
            InitialSendStep::AwaitConfirmation => {
                self.set_error(NativeUiError::new(
                    "draft-attachment-unconfirmed",
                    "附件尚未得到宿主确认，暂时无法发送",
                    true,
                ));
            }
            InitialSendStep::Attach => {
                let session_id = pending.session_id.clone();
                let path = pending.attachment_paths[pending.next_attachment_index].clone();
                self.dispatch_action(
                    NativeUiAction::AddAttachment {
                        session_id,
                        path,
                        operation_id: "ui:draft-attachment".to_owned(),
                    },
                    cx,
                );
            }
            InitialSendStep::Send => {
                let session_id = pending.session_id.clone();
                self.dispatch_action(
                    NativeUiAction::Send {
                        session_id,
                        text: pending.text.clone(),
                        attachments: pending.confirmed_attachments.clone(),
                        model: pending.model.clone(),
                        effort: pending.effort.clone(),
                        permission: pending.permission,
                        plan: pending.plan,
                        draft_edit_generation: self.field_generation,
                        operation_id: "ui:draft-send-prompt".to_owned(),
                    },
                    cx,
                );
            }
        }
    }

    fn confirm_initial_attachment(
        &mut self,
        generation: u64,
        session_id: String,
        path_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_initial_send.as_mut() else {
            return;
        };
        if pending.generation != generation
            || pending.session_id != session_id
            || pending.next_attachment_index != path_index
            || pending.confirming_attachment
        {
            return;
        }
        pending.confirming_attachment = true;
        let expected_path = pending.attachment_paths[path_index].clone();
        let host = Arc::clone(&self.host);
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = host.load_conversation(session_id.clone(), None).await;
            let _ =
                weak.update(cx, |ui, cx| {
                    let valid = ui.pending_initial_send.as_ref().is_some_and(|pending| {
                        pending.generation == generation
                            && pending.session_id == session_id
                            && pending.next_attachment_index == path_index
                            && pending.confirming_attachment
                    });
                    if !valid {
                        return;
                    }
                    match result {
                        Ok(conversation) => {
                            let Some(attachment) =
                                conversation
                                    .draft
                                    .attachments
                                    .into_iter()
                                    .find(|attachment| {
                                        same_attachment_path(&attachment.path, &expected_path)
                                    })
                            else {
                                if let Some(pending) = ui.pending_initial_send.as_mut() {
                                    pending.confirming_attachment = false;
                                }
                                ui.set_error(NativeUiError::new(
                                    "draft-attachment-unconfirmed",
                                    "宿主已接受附件操作，但尚未返回确认事实，请重试",
                                    true,
                                ));
                                cx.notify();
                                return;
                            };
                            {
                                let Some(pending) = ui.pending_initial_send.as_mut() else {
                                    return;
                                };
                                pending.confirming_attachment = false;
                                if !pending.confirmed_attachments.iter().any(|current| {
                                    current.attachment_id == attachment.attachment_id
                                }) {
                                    pending.confirmed_attachments.push(attachment);
                                }
                                pending.next_attachment_index += 1;
                            }
                            ui.dispatch_pending_initial_send(cx);
                        }
                        Err(error) => {
                            if let Some(pending) = ui.pending_initial_send.as_mut() {
                                pending.confirming_attachment = false;
                            }
                            ui.set_error(error);
                        }
                    }
                    cx.notify();
                });
        })
        .detach();
    }

    fn conversation_for_render(&self) -> Option<ConversationFact> {
        let mut conversation = self.state.conversation.clone()?;
        if conversation.session.model.is_none() {
            conversation.session.model = self.default_model_projection();
        }
        Some(conversation)
    }

    fn load_workspace_page(
        &mut self,
        cursor: Option<PageCursor>,
        include_archived: bool,
        cx: &mut Context<Self>,
    ) {
        let root_path = self.root_path.clone();
        let host = Arc::clone(&self.host);
        let append = cursor.is_some();
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = host
                .load_workspace(root_path.clone(), cursor, include_archived)
                .await;
            let _ = weak.update(cx, |ui, cx| {
                match result {
                    Ok(page) => ui.apply_workspace(page, append, root_path, cx),
                    Err(error) => ui.set_error(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_model_catalog(&mut self, cx: &mut Context<Self>) {
        let host = Arc::clone(&self.host);
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = host.model_catalog().await;
            let _ = weak.update(cx, |ui, cx| {
                match result {
                    Ok(catalog) => {
                        ui.state.model_catalog = Arc::new(catalog);
                        if ui.draft_composer_options.model.is_none() {
                            ui.draft_composer_options.model = ui.default_model_projection();
                        }
                    }
                    Err(error) => ui.set_error(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_conversation(
        &mut self,
        session_id: String,
        cursor: Option<PageCursor>,
        cx: &mut Context<Self>,
    ) {
        if cursor.is_none() {
            self.restart_subscription(session_id, cx);
            return;
        }
        let host = Arc::clone(&self.host);
        let generation = self.session_generation;
        self.conversation_load_generation = self.conversation_load_generation.wrapping_add(1);
        let load_generation = self.conversation_load_generation;
        let prepend_history = cursor.is_some();
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = host.load_conversation(session_id.clone(), cursor).await;
            let _ = weak.update(cx, |ui, cx| {
                if ui.session_generation != generation
                    || ui.conversation_load_generation != load_generation
                    || ui.active_session_id.as_deref() != Some(session_id.as_str())
                {
                    return;
                }
                match result {
                    Ok(snapshot) => ui.apply_snapshot_mode(snapshot, prepend_history, cx),
                    Err(error) => ui.set_error(error),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn handle_action(&mut self, action: NativeUiAction, cx: &mut Context<Self>) {
        match action {
            NativeUiAction::OpenProject { project_root } => {
                self.record_user_navigation(NativeNavigationTarget::chat_draft(
                    NATIVE_DRAFT_ID,
                    &project_root,
                ));
                self.handle_action(
                    NativeUiAction::LoadWorkspace {
                        root_path: project_root,
                        cursor: None,
                        include_archived: false,
                    },
                    cx,
                );
            }
            NativeUiAction::LoadWorkspace {
                root_path,
                cursor,
                include_archived,
            } => {
                if cursor.is_none() {
                    if !root_path.is_empty() {
                        self.reset_draft_composer_for_project(&root_path, cx);
                    }
                    self.root_path = root_path;
                }
                self.load_workspace_page(cursor, include_archived, cx);
            }
            NativeUiAction::OpenSession {
                session_id,
                project_root,
            } => {
                self.record_user_navigation(NativeNavigationTarget::chat_session(
                    session_id.clone(),
                    &project_root,
                ));
                self.invalidate_draft_composer(cx);
                self.open_session_after_draft(session_id, project_root, cx)
            }
            NativeUiAction::CreateSession {
                project_root,
                operation_id,
            } => {
                let is_draft_send = operation_id == "ui:draft-send";
                if is_draft_send {
                    if self.pending_new_session_draft.is_some() {
                        return;
                    }
                    let text = self.field.read(cx).text().trim().to_owned();
                    if text.is_empty() {
                        return;
                    }
                    let options = self.draft_composer_options.clone();
                    self.pending_new_session_draft = Some(PendingNewSessionDraft {
                        generation: self.draft_composer_generation,
                        project_root: project_root.clone(),
                        text,
                        attachment_paths: options.attachment_paths,
                        model: options.model,
                        effort: options.effort,
                        permission: options.permission,
                        plan: options.plan,
                    });
                } else {
                    self.pending_new_session_draft = None;
                }
                self.dispatch_action(
                    NativeUiAction::CreateSession {
                        project_root,
                        operation_id,
                    },
                    cx,
                );
            }
            NativeUiAction::LoadHistory { session_id, cursor } => {
                self.load_conversation(session_id, Some(cursor), cx)
            }
            NativeUiAction::LoadLatest { session_id } => {
                self.load_conversation(session_id, None, cx)
            }
            action => self.dispatch_action(action, cx),
        }
    }

    fn dispatch_action(&mut self, action: NativeUiAction, cx: &mut Context<Self>) {
        let mut action = admit_operation(action);
        // Send/SetDraft 在 UI 线程接纳时取得编辑代次；等待草稿屏障期间不能
        // 被后续重入重新标成更晚代次，否则发送边界会误清理新编辑。
        self.capture_send_field_draft(&action, cx);
        self.assign_draft_edit_generation(&mut action);
        self.dispatch_action_admitted(action, cx);
    }

    fn draft_barrier_needed(&self) -> bool {
        self.pending_draft.is_some() || self.inflight_draft.is_some()
    }

    fn enqueue_draft_waiter(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<Option<oneshot::Receiver<Result<(), NativeUiError>>>, NativeUiError> {
        if !self.draft_barrier_needed() {
            return Ok(None);
        }
        if self.draft_waiters.len() >= MAX_DRAFT_WAITERS {
            return Err(NativeUiError::new(
                "draft-barrier-full",
                "草稿仍在保存，等待请求过多，请稍后重试",
                true,
            ));
        }
        let (sender, receiver) = oneshot::channel();
        self.draft_waiters.push(sender);
        if self.draft_task.is_none() {
            self.start_draft_flush(cx);
        }
        Ok(Some(receiver))
    }

    fn resolve_draft_waiters(&mut self, result: Result<(), NativeUiError>) {
        for waiter in self.draft_waiters.drain(..) {
            let _ = waiter.send(result.clone());
        }
    }

    fn draft_waiter_cancelled_error() -> NativeUiError {
        NativeUiError::new("draft-barrier-cancelled", "草稿保存等待已取消", true)
    }

    fn defer_send_until_draft(&mut self, action: NativeUiAction, cx: &mut Context<Self>) -> bool {
        if !matches!(&action, NativeUiAction::Send { .. }) || !self.draft_barrier_needed() {
            return false;
        }
        let receiver = match self.enqueue_draft_waiter(cx) {
            Ok(Some(receiver)) => receiver,
            Ok(None) => return false,
            Err(error) => {
                self.set_error(error);
                return true;
            }
        };
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| match receiver.await {
            Ok(Ok(())) => {
                let _ = weak.update(cx, |ui, cx| ui.dispatch_action_admitted(action, cx));
            }
            Ok(Err(error)) => {
                let _ = weak.update(cx, |ui, _| ui.set_error(error));
            }
            Err(_) => {
                let _ = weak.update(cx, |ui, _| {
                    ui.set_error(Self::draft_waiter_cancelled_error())
                });
            }
        })
        .detach();
        true
    }

    fn capture_send_field_draft(&mut self, action: &NativeUiAction, cx: &mut Context<Self>) {
        let NativeUiAction::Send { session_id, .. } = action else {
            return;
        };
        let Some(conversation) = self
            .state
            .conversation
            .as_ref()
            .filter(|conversation| conversation.session.session_id == *session_id)
        else {
            return;
        };
        let field_text = self.field.read(cx).text().to_owned();
        if field_text == conversation.draft.text {
            return;
        }

        // TextInput 的 Changed 事件可能仍在 GPUI 队列中；Send admission 不能只看
        // pending/inflight，否则会在 Host 清空草稿后才补发旧正文。这里直接把输入控件
        // 的当前文本登记为本地事实，并让 Send 等待同一条草稿屏障。
        self.field_generation = self.field_generation.wrapping_add(1);
        let draft = DraftFact {
            text: field_text,
            attachments: conversation.draft.attachments.clone(),
            mention_query: conversation.draft.mention_query.clone(),
        };
        if let Some(conversation) = self.state.conversation.as_mut() {
            conversation.draft = draft.clone();
        }
        self.pending_draft = Some(PendingDraft {
            session_id: session_id.clone(),
            draft,
            edit_generation: self.field_generation,
        });
        if self.draft_task.is_none() {
            self.start_draft_flush(cx);
        }
    }

    fn assign_draft_edit_generation(&mut self, action: &mut NativeUiAction) {
        match action {
            NativeUiAction::SetDraft {
                edit_generation, ..
            } if *edit_generation == 0 => {
                self.field_generation = self.field_generation.wrapping_add(1);
                *edit_generation = self.field_generation;
            }
            NativeUiAction::Send {
                draft_edit_generation,
                ..
            } => {
                *draft_edit_generation = self.field_generation;
            }
            _ => {}
        }
    }

    fn dispatch_action_admitted(&mut self, action: NativeUiAction, cx: &mut Context<Self>) {
        // Send 必须先把最近一次本地编辑的草稿写入 Host；否则延迟的 SetDraft
        // 可能在 Send 清空草稿后到达，重新把已发送正文恢复到输入框。
        if self.defer_send_until_draft(action.clone(), cx) {
            return;
        }
        let submitted_draft = match &action {
            NativeUiAction::Send {
                session_id, text, ..
            } => Some((session_id.clone(), text.clone())),
            _ => None,
        };
        if let Some((session_id, text)) = submitted_draft.as_ref() {
            self.submitted_draft = Some(SubmittedDraft {
                session_id: session_id.clone(),
                text: text.clone(),
                field_generation: self.field_generation,
            });
        }
        let operation_id = operation_id(&action).map(str::to_owned);
        let forgotten_root = match &action {
            NativeUiAction::ForgetProject { project_root, .. } => Some(project_root.clone()),
            _ => None,
        };
        let deleted_session_id = match &action {
            NativeUiAction::DeleteSession { session_id, .. } => Some(session_id.clone()),
            _ => None,
        };
        if let Some(operation_id) = operation_id.as_ref() {
            self.state
                .pending_operations
                .insert(operation_id.clone(), action.clone());
        }
        let reload_workspace = affects_workspace(&action);
        let reload_session_id = reload_conversation_session(&action).map(str::to_owned);
        let receipt_route = ReceiptRoute::from_action(self, &action);
        let host = Arc::clone(&self.host);
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = host.dispatch(action).await;
            let _ =
                weak.update(cx, |ui, cx| {
                    if let Some(operation_id) = operation_id.as_ref() {
                        ui.state.pending_operations.remove(operation_id);
                    }
                    match result {
                        Ok(receipt) => {
                            ui.last_error = None;
                            if let Some(forgotten_root) = forgotten_root.as_deref() {
                                ui.navigation.remove_project(forgotten_root);
                                // 回执到达时用户可能已切换项目，只释放仍属于被移除项目的
                                // 窗口投影；后台 Agent 的执行与生命周期由 Host 保持。
                                let active_belongs =
                                    ui.active_session_belongs_to_project(forgotten_root);
                                if active_belongs {
                                    ui.cancel_subscription();
                                    ui.session_generation = ui.session_generation.wrapping_add(1);
                                    ui.active_session_id = None;
                                    ui.clear_conversation_projection();
                                    ui.pending_draft = None;
                                    ui.inflight_draft = None;
                                    ui.draft_generation = ui.draft_generation.wrapping_add(1);
                                    ui.resolve_draft_waiters(Err(NativeUiError::new(
                                        "project-forgotten",
                                        "项目已移除，未确认的草稿操作已取消",
                                        false,
                                    )));
                                    ui.draft_task = None;
                                    ui.message_list_ids.clear();
                                    ui.message_list.reset(0);
                                    ui.suppress_field_events = true;
                                    ui.field.update(cx, |field, cx| field.set_text("", cx));
                                    ui.suppress_field_events = false;
                                }
                                if normalize_project_root(&ui.root_path)
                                    == normalize_project_root(forgotten_root)
                                {
                                    ui.draft_composer_project_root.clear();
                                    ui.invalidate_draft_composer(cx);
                                    ui.root_path.clear();
                                    ui.state.workspace_root = None;
                                }
                            }
                            if let Some(session_id) = deleted_session_id.as_deref() {
                                ui.navigation.remove_sessions(session_id);
                            }
                            if reload_workspace {
                                ui.load_workspace_page(None, false, cx);
                            }
                            if let Some(session_id) = reload_session_id.as_deref() {
                                ui.reload_conversation_if_active(session_id, cx);
                            }
                            ui.route_receipt(receipt_route.as_ref(), &receipt, cx);
                        }
                        Err(error) => {
                            if let Some((session_id, text)) = submitted_draft.as_ref()
                                && ui.submitted_draft.as_ref().is_some_and(|submitted| {
                                    submitted.session_id == session_id.as_str()
                                        && submitted.text == text.as_str()
                                })
                            {
                                ui.submitted_draft = None;
                            }
                            if receipt_route
                                .as_ref()
                                .is_none_or(|route| ui.current_draft_receipt(route))
                            {
                                if let Some(ReceiptRoute::Create {
                                    initial_draft: Some(initial_draft),
                                    ..
                                }) = receipt_route.as_ref()
                                    && ui.pending_new_session_draft.as_ref().is_some_and(
                                        |pending| pending.generation == initial_draft.generation,
                                    )
                                {
                                    ui.pending_new_session_draft = None;
                                }
                                ui.set_error(error);
                            }
                        }
                    }
                    cx.notify();
                });
        })
        .detach();
    }

    fn on_field_changed(&mut self, cx: &mut Context<Self>) {
        if self.suppress_field_events {
            return;
        }
        let Some(session_id) = self.active_session_id.clone() else {
            return;
        };
        let conversation = self
            .state
            .conversation
            .as_ref()
            .filter(|conversation| conversation.session.session_id == session_id);
        let field_text = self.field.read(cx).text().to_owned();
        if self.submitted_draft.as_ref().is_some_and(|submitted| {
            submitted.session_id == session_id
                && submitted.field_generation == self.field_generation
                && submitted.text == field_text
        }) {
            // Send admission 已记录当前代次；GPUI 可能随后投递同一文本的迟到
            // Changed。消费这一次重复通知，避免它在 Host 清空之后重新写回旧正文。
            return;
        }
        self.field_generation = self.field_generation.wrapping_add(1);
        let draft = draft_from_field_text(field_text, conversation);
        // 文本输入是用户当前已确认的本地事实；Host 的 DraftChanged 只负责确认持久化，
        // 不应在异步保存期间把 UI 投影回滚到旧文本。
        if let Some(conversation) = self.state.conversation.as_mut()
            && conversation.session.session_id == session_id
        {
            conversation.draft = draft.clone();
        }
        self.pending_draft = Some(PendingDraft {
            session_id,
            draft,
            edit_generation: self.field_generation,
        });
        if self.draft_task.is_none() {
            self.start_draft_flush(cx);
        }
    }

    fn start_draft_flush(&mut self, cx: &mut Context<Self>) {
        if self.draft_task.is_some() {
            return;
        }
        self.draft_generation = self.draft_generation.wrapping_add(1);
        let draft_generation = self.draft_generation;
        let weak = cx.entity().downgrade();
        self.draft_task = Some(cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(DRAFT_DEBOUNCE).await;
                let start = weak
                    .update(cx, |ui, _| {
                        if ui.draft_generation != draft_generation {
                            return DraftFlushStart::Stop;
                        }
                        let Some(pending) = ui.pending_draft.take() else {
                            if ui.inflight_draft.is_none() {
                                ui.resolve_draft_waiters(Ok(()));
                                // 与检查 pending_draft 使用同一个 UI update，避免
                                // 任务结束窗口中新输入看见旧的 Some(Task) 后丢失唤醒。
                                ui.draft_task = None;
                            }
                            return DraftFlushStart::Stop;
                        };
                        ui.inflight_draft = Some(pending.clone());
                        DraftFlushStart::Send(pending)
                    })
                    .ok();
                let Some(DraftFlushStart::Send(pending)) = start else {
                    break;
                };
                let host = match weak.update(cx, |ui, _| Arc::clone(&ui.host)) {
                    Ok(host) => host,
                    Err(_) => break,
                };
                let result = host
                    .dispatch(NativeUiAction::SetDraft {
                        session_id: pending.session_id.clone(),
                        draft: pending.draft.clone(),
                        edit_generation: pending.edit_generation,
                    })
                    .await;
                let continue_flush = weak
                    .update(cx, |ui, _| {
                        if ui.draft_generation != draft_generation {
                            return false;
                        }
                        ui.inflight_draft = None;
                        match result {
                            Ok(_) => {
                                if ui.pending_draft.is_some() {
                                    true
                                } else {
                                    ui.resolve_draft_waiters(Ok(()));
                                    ui.draft_task = None;
                                    false
                                }
                            }
                            Err(error) => {
                                // 保留未确认文本，下一次用户编辑或显式等待时仍能
                                // 看到并重试同一份本地事实；所有 Send/Open waiter
                                // 都必须收到错误，不能在失败后继续发出动作。
                                if ui.pending_draft.is_none() {
                                    ui.pending_draft = Some(pending.clone());
                                }
                                ui.set_error(error.clone());
                                ui.resolve_draft_waiters(Err(error));
                                ui.draft_task = None;
                                false
                            }
                        }
                    })
                    .unwrap_or(false);
                if !continue_flush {
                    break;
                }
            }
        }));
    }

    fn open_session_after_draft(
        &mut self,
        session_id: String,
        project_root: String,
        cx: &mut Context<Self>,
    ) {
        self.open_session_after_draft_with_navigation(session_id, project_root, None, cx);
    }

    fn open_session_after_draft_with_navigation(
        &mut self,
        session_id: String,
        project_root: String,
        traversal: Option<NavigationTraversal>,
        cx: &mut Context<Self>,
    ) {
        if !self.draft_barrier_needed() {
            self.open_session_internal(session_id, project_root, traversal, cx);
            return;
        }
        let receiver = match self.enqueue_draft_waiter(cx) {
            Ok(Some(receiver)) => receiver,
            Ok(None) => {
                self.open_session_internal(session_id, project_root, traversal, cx);
                return;
            }
            Err(error) => {
                self.fail_draft_navigation(traversal, error, cx);
                return;
            }
        };
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| match receiver.await {
            Ok(Ok(())) => {
                let _ = weak.update(cx, |ui, cx| {
                    ui.open_session_internal(session_id, project_root, traversal, cx)
                });
            }
            Ok(Err(error)) => {
                let _ = weak.update(cx, |ui, cx| ui.fail_draft_navigation(traversal, error, cx));
            }
            Err(_) => {
                let _ = weak.update(cx, |ui, cx| {
                    ui.fail_draft_navigation(traversal, Self::draft_waiter_cancelled_error(), cx)
                });
            }
        })
        .detach();
    }

    fn fail_draft_navigation(
        &mut self,
        traversal: Option<NavigationTraversal>,
        error: NativeUiError,
        cx: &mut Context<Self>,
    ) {
        if let Some(traversal) = traversal {
            if !self
                .navigation_traversal
                .as_ref()
                .is_some_and(|current| current.token == traversal.token)
            {
                return;
            }
            // 保存失败不代表历史目标无效；退回原页面并释放 guard，允许保存后重试。
            self.navigation.return_from_traversal(traversal.direction);
            self.navigation_traversal = None;
        }
        self.set_error(error);
        cx.notify();
    }

    fn apply_workspace(
        &mut self,
        page: WorkspacePage,
        append: bool,
        root_path: String,
        cx: &mut Context<Self>,
    ) {
        // 项目切换允许请求并发返回；旧请求不能覆盖用户刚选中的项目和分页游标。
        if root_path != self.root_path {
            return;
        }
        // 空根用于初始加载或移除当前项目；选中宿主返回的首个可用项目，
        // 后续新建会话必须使用真实路径而不是空字符串。
        let selected_root = if root_path.is_empty() {
            page.projects
                .iter()
                .find(|project| !project.archived && !project.root_path.is_empty())
                .map(|project| project.root_path.clone())
                .unwrap_or_default()
        } else {
            root_path
        };
        self.reset_draft_composer_for_project(&selected_root, cx);
        self.root_path = selected_root.clone();
        self.state.workspace_root = (!selected_root.is_empty()).then_some(selected_root);
        if !append {
            self.state.workspace = Some(page);
            return;
        }
        let Some(current) = self.state.workspace.as_mut() else {
            self.state.workspace = Some(page);
            return;
        };
        for project in page.projects {
            if let Some(existing) = current
                .projects
                .iter_mut()
                .find(|existing| existing.project_key == project.project_key)
            {
                *existing = project;
            } else {
                current.projects.push(project);
            }
        }
        for session in page.sessions {
            if let Some(existing) = current
                .sessions
                .iter_mut()
                .find(|existing| existing.session_id == session.session_id)
            {
                *existing = session;
            } else {
                current.sessions.push(session);
            }
        }
        for group in page.groups {
            if let Some(existing) = current.groups.iter_mut().find(|existing| {
                existing.project_key == group.project_key && existing.group_id == group.group_id
            }) {
                *existing = group;
            } else {
                current.groups.push(group);
            }
        }
        current.next = page.next;
    }

    fn apply_snapshot(&mut self, snapshot: ConversationFact, cx: &mut Context<Self>) {
        self.apply_snapshot_mode(snapshot, false, cx);
    }

    fn active_tail_messages(conversation: &ConversationFact) -> Vec<Arc<UiMessage>> {
        let active_turn_id = conversation.active_turn_id.as_deref();
        conversation
            .messages
            .iter()
            .filter(|message| {
                message.turn_id.as_deref() == active_turn_id && active_turn_id.is_some()
                    || message.blocks.iter().any(|block| match block {
                        super::model::MessageBlock::Markdown { streaming, .. }
                        | super::model::MessageBlock::Reasoning { streaming, .. } => *streaming,
                        super::model::MessageBlock::Tool(tool) => matches!(
                            tool.status,
                            super::model::ToolStatus::Requested
                                | super::model::ToolStatus::Running
                                | super::model::ToolStatus::SideEffectUnknown
                        ),
                        super::model::MessageBlock::Approval(approval) => {
                            approval.can_approve || approval.can_deny
                        }
                        super::model::MessageBlock::Question(question) => !question.answered,
                        _ => false,
                    })
            })
            .cloned()
            .collect()
    }

    fn sync_message_list(
        &mut self,
        messages: &[Arc<UiMessage>],
        prepend_history: bool,
        force_reset: bool,
        dirty_message_ids: &[String],
    ) {
        let message_ids = messages
            .iter()
            .map(|message| message.message_id.clone())
            .collect::<Vec<_>>();
        self.sync_message_list_ids(message_ids, prepend_history, force_reset, dirty_message_ids);
    }

    fn sync_message_list_ids(
        &mut self,
        message_ids: Vec<String>,
        prepend_history: bool,
        force_reset: bool,
        dirty_message_ids: &[String],
    ) {
        let previous_ids = &self.message_list_ids;
        if force_reset {
            self.message_list.reset(message_ids.len());
        } else if prepend_history
            && message_ids.len() > previous_ids.len()
            && message_ids.ends_with(previous_ids)
        {
            self.message_list
                .splice(0..0, message_ids.len() - previous_ids.len());
        } else if message_ids.len() > previous_ids.len() && message_ids.starts_with(previous_ids) {
            self.message_list.splice(
                previous_ids.len()..previous_ids.len(),
                message_ids.len() - previous_ids.len(),
            );
        } else if message_ids.as_slice() != previous_ids.as_slice() {
            self.message_list.reset(message_ids.len());
        } else {
            for message_id in dirty_message_ids {
                if let Some(index) = message_ids.iter().position(|current| current == message_id) {
                    self.message_list.remeasure_items(index..index + 1);
                }
            }
        }
        self.message_list_ids = message_ids;
    }

    fn local_draft_for_session(&self, session_id: &str) -> Option<DraftFact> {
        self.pending_draft
            .as_ref()
            .filter(|pending| pending.session_id == session_id)
            .map(|pending| pending.draft.clone())
            .or_else(|| {
                self.inflight_draft
                    .as_ref()
                    .filter(|pending| pending.session_id == session_id)
                    .map(|pending| pending.draft.clone())
            })
    }

    fn local_field_draft_for_session(
        &self,
        session_id: &str,
        cx: &mut Context<Self>,
    ) -> Option<DraftFact> {
        if self.active_session_id.as_deref() != Some(session_id) {
            return None;
        }
        let conversation = self
            .state
            .conversation
            .as_ref()
            .filter(|conversation| conversation.session.session_id == session_id);
        let field_text = self.field.read(cx).text().to_owned();
        if self.submitted_draft.as_ref().is_some_and(|submitted| {
            submitted.session_id == session_id
                && submitted.field_generation == self.field_generation
                && submitted.text == field_text
        }) {
            // Send 后等待 Host 清空草稿时，原发送文本仍在输入控件中；此时不能把它
            // 当作用户的新编辑重新覆盖权威的空草稿。
            return None;
        }
        if conversation.is_some_and(|conversation| field_text == conversation.draft.text)
            || (conversation.is_none() && field_text.is_empty())
        {
            return None;
        }
        // TextInput 的 Changed 事件和宿主快照都在 GPUI 事件循环中排队；在
        // Changed 回调尚未登记 pending_draft 的窗口内，仍以输入控件当前值为本地
        // 未确认事实，避免随后到达的旧 Snapshot/DraftChanged 清空用户已输入内容。
        Some(draft_from_field_text(field_text, conversation))
    }

    fn apply_snapshot_mode(
        &mut self,
        mut snapshot: ConversationFact,
        prepend_history: bool,
        cx: &mut Context<Self>,
    ) {
        let snapshot_session_id = snapshot.session.session_id.clone();
        let local_field_draft = self.local_field_draft_for_session(&snapshot_session_id, cx);
        if prepend_history
            && let Some(current) = self.state.conversation.as_ref()
            && current.session.session_id == snapshot.session.session_id
        {
            // 历史页使用独立显示窗口，避免把当前 200 条最新消息和旧页合并后
            // 又从头裁剪，导致用户点击“更早消息”却仍然看不到旧内容。只携带
            // 当前运行尾块、待审批和未回答问题，确保用户不会在翻页时丢掉交互入口。
            let mut merged = snapshot.messages.as_ref().clone();
            for message in Self::active_tail_messages(current) {
                if let Some(index) = merged
                    .iter()
                    .position(|existing| existing.message_id == message.message_id)
                {
                    merged[index] = message;
                } else {
                    merged.push(message);
                }
            }
            snapshot.messages = Arc::new(merged);
            // 历史页只替换正文窗口；读取期间到达的运行状态和输入事实不能回退。
            snapshot.session = current.session.clone();
            snapshot.draft = current.draft.clone();
            snapshot.input_queue = current.input_queue.clone();
            snapshot.active_turn_id = current.active_turn_id.clone();
            snapshot.transcript_revision = current.transcript_revision;
        }
        if let Some(local) =
            local_field_draft.or_else(|| self.local_draft_for_session(&snapshot.session.session_id))
        {
            // Host 是附件事实的唯一来源；首个快照竞态时也保留 snapshot.draft.attachments，
            // 本地只保护尚未确认的文本和提及状态。
            snapshot.draft.text = local.text;
            snapshot.draft.mention_query = local.mention_query;
        }
        trim_message_window(Arc::make_mut(&mut snapshot.messages));
        self.last_error = None;
        self.active_session_id = Some(snapshot.session.session_id.clone());
        self.suppress_field_events = true;
        let draft_text = snapshot.draft.text.clone();
        self.sync_message_list(&snapshot.messages, prepend_history, !prepend_history, &[]);
        if self.field.read(cx).text() != draft_text {
            self.field
                .update(cx, |field, cx| field.set_text(draft_text, cx));
        }
        self.suppress_field_events = false;
        self.merge_session(snapshot.session.clone());
        // 正文已经由 Arc 共享；直接移动快照，避免再复制整份队列、草稿与会话元数据。
        self.state.conversation = Some(snapshot);
        if self
            .pending_session_submit
            .as_ref()
            .is_some_and(|pending| pending.session_id == snapshot_session_id)
        {
            self.pending_session_submit = None;
            self.dispatch_active_composer_send(cx);
        }
    }

    fn reload_conversation_if_active(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.active_session_id.as_deref() == Some(session_id) {
            self.load_conversation(session_id.to_owned(), None, cx);
        }
    }

    fn project_root_for_session(&self, session_id: &str) -> Option<String> {
        let workspace = self.state.workspace.as_ref()?;
        let session = workspace
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)?;
        workspace
            .projects
            .iter()
            .find(|project| project.project_key == session.project_key)
            .map(|project| project.root_path.clone())
    }

    fn normalized_project_path(path: &str) -> String {
        let normalized = crate::path_utils::path_text_to_frontend(path);
        if normalized.len() == 3
            && normalized.as_bytes().get(1) == Some(&b':')
            && normalized.ends_with('/')
        {
            return normalized;
        }
        normalized.trim_end_matches('/').to_owned()
    }

    fn active_session_belongs_to_project(&self, project_root: &str) -> bool {
        let Some(session_id) = self.active_session_id.as_deref() else {
            return false;
        };
        let target = Self::normalized_project_path(project_root);
        if self
            .state
            .conversation
            .as_ref()
            .filter(|conversation| conversation.session.session_id == session_id)
            .is_some_and(|conversation| {
                Self::normalized_project_path(&conversation.session.project_key) == target
            })
        {
            return true;
        }
        self.project_root_for_session(session_id)
            .is_some_and(|root| Self::normalized_project_path(&root) == target)
    }

    fn active_workbench_root(&self) -> String {
        self.workbench_root_override
            .clone()
            .or_else(|| {
                self.active_session_id
                    .as_deref()
                    .and_then(|session_id| self.project_root_for_session(session_id))
            })
            .or_else(|| self.state.workspace_root.clone())
            .unwrap_or_else(|| self.root_path.clone())
    }

    fn route_receipt(
        &mut self,
        route: Option<&ReceiptRoute>,
        receipt: &NativeActionReceipt,
        cx: &mut Context<Self>,
    ) {
        let Some(route) = route else {
            return;
        };
        match route {
            ReceiptRoute::Create {
                project_root,
                initial_draft: Some(initial_draft),
            } if initial_draft.generation != self.draft_composer_generation
                || initial_draft.project_root != *project_root
                || self.root_path != *project_root =>
            {
                // 项目切换后到达的旧创建回执只能留下 Host 中的会话，不能把旧输入
                // 路由到当前 Draft Composer。
                return;
            }
            ReceiptRoute::InitialAttachment {
                generation,
                session_id,
                path_index,
                operation_id,
            } => {
                if receipt.session_id.as_deref() != Some(session_id.as_str())
                    || receipt.operation_id != *operation_id
                {
                    return;
                }
                self.confirm_initial_attachment(*generation, session_id.clone(), *path_index, cx);
                return;
            }
            ReceiptRoute::InitialSend {
                generation,
                session_id,
                operation_id,
            } => {
                if receipt.session_id.as_deref() != Some(session_id.as_str())
                    || receipt.operation_id != *operation_id
                {
                    return;
                }
                self.finish_initial_send(*generation, session_id, cx);
                return;
            }
            _ => {}
        }
        let Some(session_id) = receipt.session_id.clone() else {
            if matches!(
                route,
                ReceiptRoute::Create {
                    initial_draft: Some(_),
                    ..
                }
            ) {
                self.pending_new_session_draft = None;
            }
            self.set_error(NativeUiError::new(
                "missing-session-id",
                "宿主回执未提供会话 ID，无法打开结果",
                true,
            ));
            return;
        };
        let project_root = match route {
            ReceiptRoute::Create { project_root, .. } => Some(project_root.clone()),
            ReceiptRoute::Branch { project_root } => project_root.clone(),
            ReceiptRoute::InitialAttachment { .. } | ReceiptRoute::InitialSend { .. } => None,
        };
        let Some(project_root) = project_root else {
            self.set_error(NativeUiError::new(
                "project-root-missing",
                "未找到分支所属项目根目录，已保留当前会话",
                true,
            ));
            return;
        };
        match route {
            ReceiptRoute::Create {
                initial_draft: Some(initial_draft),
                ..
            } => {
                self.pending_new_session_draft = None;
                let initial_draft = initial_draft.clone();
                self.pending_initial_send = Some(PendingInitialSend {
                    generation: initial_draft.generation,
                    session_id: session_id.clone(),
                    project_root: initial_draft.project_root,
                    text: initial_draft.text,
                    attachment_paths: initial_draft.attachment_paths,
                    next_attachment_index: 0,
                    confirmed_attachments: Vec::new(),
                    model: initial_draft.model,
                    effort: initial_draft.effort,
                    permission: initial_draft.permission,
                    plan: initial_draft.plan,
                    confirming_attachment: false,
                });
                self.record_user_navigation(NativeNavigationTarget::chat_session(
                    session_id.clone(),
                    &project_root,
                ));
                self.open_session(session_id.clone(), project_root, cx);
                self.dispatch_pending_initial_send(cx);
            }
            ReceiptRoute::Create { .. } | ReceiptRoute::Branch { .. } => {
                self.record_user_navigation(NativeNavigationTarget::chat_session(
                    session_id.clone(),
                    &project_root,
                ));
                self.open_session_after_draft(session_id, project_root, cx);
            }
            ReceiptRoute::InitialAttachment { .. } | ReceiptRoute::InitialSend { .. } => {
                unreachable!("首次发送子动作已在上方处理")
            }
        }
    }

    fn finish_initial_send(&mut self, generation: u64, session_id: &str, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_initial_send.take() else {
            return;
        };
        if pending.generation != generation || pending.session_id != session_id {
            self.pending_initial_send = Some(pending);
            return;
        }
        if self.draft_composer_generation == generation {
            self.draft_composer_options.attachment_paths.clear();
            self.draft_composer_generation = self.draft_composer_generation.wrapping_add(1);
            if self.field.read(cx).text().trim() == pending.text.trim() {
                self.suppress_field_events = true;
                self.field.update(cx, |field, cx| field.set_text("", cx));
                self.suppress_field_events = false;
            }
        }
        cx.notify();
    }

    fn merge_session(&mut self, session: SessionFact) {
        let Some(workspace) = self.state.workspace.as_mut() else {
            return;
        };
        if let Some(existing) = workspace
            .sessions
            .iter_mut()
            .find(|existing| existing.session_id == session.session_id)
        {
            *existing = session;
        }
    }

    fn start_event_loop(
        &mut self,
        session_id: String,
        cx: &mut Context<Self>,
    ) -> (
        Sender<EventMailboxItem>,
        Arc<Mutex<EventOverflow>>,
        Arc<EventMailboxBudget>,
    ) {
        let (sender, receiver) = channel(NATIVE_EVENT_MAILBOX_CAPACITY);
        let overflow = Arc::new(Mutex::new(EventOverflow::default()));
        let budget = Arc::new(EventMailboxBudget {
            reserved: AtomicUsize::new(0),
            snapshot_metadata_reserved: AtomicUsize::new(0),
        });
        self.event_sender = Some(sender.clone());
        let weak = cx.entity().downgrade();
        let process_overflow = Arc::clone(&overflow);
        let subscription_generation = self.subscription_generation;
        self.event_task = Some(cx.spawn(async move |_, cx| {
            process_events(
                weak,
                session_id,
                subscription_generation,
                receiver,
                process_overflow,
                cx,
            )
            .await;
        }));
        (sender, overflow, budget)
    }

    fn apply_event_batch(&mut self, batch: NativeEventBatch, cx: &mut Context<Self>) -> bool {
        if self.active_session_id.as_deref() != Some(batch.session_id.as_str()) {
            return false;
        }
        if batch
            .events
            .iter()
            .any(|event| matches!(event, NativeUiEvent::ResyncRequired { .. }))
        {
            return true;
        }
        let has_snapshot = batch
            .events
            .iter()
            .any(|event| matches!(event, NativeUiEvent::Snapshot(_)));
        if !has_snapshot
            && self.last_delivery_sequence != 0
            && batch.delivery_sequence != self.last_delivery_sequence.saturating_add(1)
        {
            return true;
        }
        self.last_delivery_sequence = batch.delivery_sequence;
        let mut dirty_message_ids = Vec::new();
        let batch_session_id = batch.session_id.clone();
        for event in batch.events {
            match event {
                NativeUiEvent::Snapshot(snapshot) => {
                    self.conversation_load_generation =
                        self.conversation_load_generation.wrapping_add(1);
                    self.apply_snapshot(*snapshot, cx);
                }
                NativeUiEvent::Closed => self.cancel_subscription(),
                NativeUiEvent::DraftChanged(mut draft) => {
                    // Changed 回调可能晚于新键盘输入；实时控件值必须先于旧 pending 文本。
                    if let Some(local) = self
                        .local_field_draft_for_session(&batch_session_id, cx)
                        .or_else(|| self.local_draft_for_session(&batch_session_id))
                    {
                        draft.text = local.text.clone();
                        draft.mention_query = local.mention_query;
                    }
                    self.suppress_field_events = true;
                    if self.field.read(cx).text() != draft.text {
                        self.field
                            .update(cx, |field, cx| field.set_text(draft.text.clone(), cx));
                    }
                    self.suppress_field_events = false;
                    self.state.apply_event(NativeUiEvent::DraftChanged(draft));
                }
                NativeUiEvent::SessionChanged(session) => {
                    self.merge_session(session.clone());
                    self.state
                        .apply_event(NativeUiEvent::SessionChanged(session));
                }
                event => {
                    if let Some(message_id) = self.state.apply_event(event) {
                        dirty_message_ids.push(message_id);
                    }
                    if let Some(message) = self.state.take_queue_load_rejection() {
                        self.set_error(NativeUiError::new("queue-load-blocked", message, true));
                    }
                }
            }
        }
        if let Some(conversation) = self.state.conversation.as_mut()
            && batch.journal_sequence > conversation.session.last_sequence
        {
            conversation.session.last_sequence = batch.journal_sequence;
        }
        // 流式批次通常只改变正文；ID 顺序未变时不重复分配最多 200 个字符串。
        let message_ids = self.state.conversation.as_ref().and_then(|conversation| {
            let same_ids = conversation.messages.len() == self.message_list_ids.len()
                && conversation
                    .messages
                    .iter()
                    .zip(&self.message_list_ids)
                    .all(|(message, id)| &message.message_id == id);
            (!same_ids || has_snapshot).then(|| {
                conversation
                    .messages
                    .iter()
                    .map(|message| message.message_id.clone())
                    .collect::<Vec<_>>()
            })
        });
        if let Some(message_ids) = message_ids {
            self.sync_message_list_ids(message_ids, false, has_snapshot, &dirty_message_ids);
        } else {
            for message_id in dirty_message_ids {
                if let Some(index) = self
                    .message_list_ids
                    .iter()
                    .position(|id| id == &message_id)
                {
                    self.message_list.remeasure_items(index..index + 1);
                }
            }
        }
        false
    }

    fn cancel_subscription(&mut self) {
        self.subscription_generation = self.subscription_generation.wrapping_add(1);
        if let Some(mut subscription) = self.subscription.take() {
            subscription.cancel();
        }
        self.event_sender = None;
        self.event_task = None;
        self.last_delivery_sequence = 0;
    }

    fn set_error(&mut self, error: NativeUiError) {
        self.last_error = Some(error);
    }

    fn current_draft_receipt(&self, route: &ReceiptRoute) -> bool {
        match route {
            ReceiptRoute::Create {
                project_root,
                initial_draft: Some(initial_draft),
            } => {
                self.root_path == project_root.as_str()
                    && self.draft_composer_generation == initial_draft.generation
                    && self
                        .pending_new_session_draft
                        .as_ref()
                        .is_some_and(|pending| {
                            pending.generation == initial_draft.generation
                                && pending.project_root == project_root.as_str()
                        })
            }
            ReceiptRoute::InitialAttachment {
                generation,
                session_id,
                operation_id,
                ..
            }
            | ReceiptRoute::InitialSend {
                generation,
                session_id,
                operation_id,
            } => self.pending_initial_send.as_ref().is_some_and(|pending| {
                pending.generation == *generation
                    && pending.session_id == *session_id
                    && pending.project_root == self.root_path
                    && !self
                        .state
                        .pending_operations
                        .iter()
                        .any(|(current_id, action)| {
                            current_id != operation_id
                                && reload_conversation_session(action).is_some_and(
                                    |current_session| current_session == session_id.as_str(),
                                )
                        })
            }),
            _ => true,
        }
    }

    fn action_handler(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(NativeUiAction, &mut Window, &mut App) + 'static {
        let this = cx.entity();
        move |action, _, app| {
            this.update(app, |ui, cx| ui.handle_action(action, cx));
        }
    }

    fn draft_action_handler(
        &self,
        cx: &Context<Self>,
    ) -> impl Fn(DraftComposerAction, &mut Window, &mut App) + 'static {
        let this = cx.entity();
        move |action, _, app| {
            this.update(app, |ui, cx| ui.handle_draft_composer_action(action, cx));
        }
    }

    fn current_navigation_target(&self) -> NativeNavigationTarget {
        match self.page {
            NativeUiPage::Chat => self
                .active_session_id
                .as_deref()
                .map(|session_id| {
                    NativeNavigationTarget::chat_session(
                        session_id,
                        self.project_root_for_session(session_id)
                            .as_deref()
                            .unwrap_or(&self.root_path),
                    )
                })
                .unwrap_or_else(|| {
                    NativeNavigationTarget::chat_draft(NATIVE_DRAFT_ID, &self.root_path)
                }),
            NativeUiPage::Settings => NativeNavigationTarget::settings(self.settings_page),
            NativeUiPage::Workbench => NativeNavigationTarget::workbench(
                self.workbench_pane,
                &self.active_workbench_root(),
            ),
        }
    }

    fn record_user_navigation(&mut self, target: NativeNavigationTarget) {
        // 新用户动作会使未完成的异步历史恢复失效；旧回执只能被 session generation
        // 和这里的 traversal token 双重拦截，不能覆盖新选择的页面。
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        self.navigation_traversal = None;
        let current = self.current_navigation_target();
        self.navigation.record_user(current);
        self.navigation.record_user(target);
    }

    fn apply_page(&mut self, page: NativeUiPage, cx: &mut Context<Self>) {
        self.page = page;
        if page == NativeUiPage::Chat {
            self.workbench_root_override = None;
        }
        self.last_error = None;
        cx.notify();
    }

    fn set_page(&mut self, page: NativeUiPage, cx: &mut Context<Self>) {
        self.record_user_navigation(match page {
            NativeUiPage::Chat => {
                if let Some(session_id) = self.active_session_id.as_deref() {
                    NativeNavigationTarget::chat_session(
                        session_id,
                        self.project_root_for_session(session_id)
                            .as_deref()
                            .unwrap_or(&self.root_path),
                    )
                } else {
                    NativeNavigationTarget::chat_draft(NATIVE_DRAFT_ID, &self.root_path)
                }
            }
            NativeUiPage::Settings => NativeNavigationTarget::settings(self.settings_page),
            NativeUiPage::Workbench => NativeNavigationTarget::workbench(
                self.workbench_pane,
                &self.active_workbench_root(),
            ),
        });
        self.apply_page(page, cx);
    }

    fn open_settings(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.record_user_navigation(NativeNavigationTarget::settings(page));
        self.apply_settings_page(page, cx);
    }

    fn apply_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.page = NativeUiPage::Settings;
        self.settings_page = page;
        self.last_error = None;
        if let Some(handler) = self.settings_navigation.as_ref() {
            handler(page, cx);
        }
        cx.notify();
    }

    fn open_search(&mut self, cx: &mut Context<Self>) {
        self.open_workbench(WorkbenchPane::Search, cx);
    }

    fn open_workbench(&mut self, pane: WorkbenchPane, cx: &mut Context<Self>) {
        let root = self.active_workbench_root();
        if let Err(error) = self.prepare_workbench_navigation(pane, &root, cx) {
            self.set_error(error);
            cx.notify();
            return;
        }
        self.record_user_navigation(NativeNavigationTarget::workbench(pane, &root));
        self.apply_workbench(pane, root, cx);
    }

    fn prepare_workbench_navigation(
        &mut self,
        pane: WorkbenchPane,
        root: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), NativeUiError> {
        if let Some(handler) = self.workbench_navigation.as_ref() {
            handler(pane, root, cx).map_err(|message| {
                NativeUiError::new("workbench-navigation-blocked", message, true)
            })?;
            self.last_workbench_root = Some(root.to_owned());
        }
        Ok(())
    }

    fn apply_workbench(
        &mut self,
        pane: WorkbenchPane,
        project_root: String,
        cx: &mut Context<Self>,
    ) {
        self.page = NativeUiPage::Workbench;
        self.workbench_pane = pane;
        self.workbench_root_override = (!project_root.is_empty()).then_some(project_root);
        self.last_error = None;
        cx.notify();
    }

    fn go_back(&mut self, cx: &mut Context<Self>) {
        self.start_history_traversal(NavigationDirection::Back, cx);
    }

    fn go_forward(&mut self, cx: &mut Context<Self>) {
        self.start_history_traversal(NavigationDirection::Forward, cx);
    }

    fn start_history_traversal(&mut self, direction: NavigationDirection, cx: &mut Context<Self>) {
        if self.navigation_traversal.is_some() {
            return;
        }
        let target = match direction {
            NavigationDirection::Back => self.navigation.move_back(),
            NavigationDirection::Forward => self.navigation.move_forward(),
        };
        let Some(target) = target else {
            return;
        };
        self.navigation_generation = self.navigation_generation.wrapping_add(1);
        let traversal = NavigationTraversal {
            token: self.navigation_generation,
            direction,
            target,
        };
        self.navigation_traversal = Some(traversal.clone());
        self.activate_navigation_target(traversal, cx);
        cx.notify();
    }

    fn activate_navigation_target(
        &mut self,
        traversal: NavigationTraversal,
        cx: &mut Context<Self>,
    ) {
        match traversal.target.clone() {
            NativeNavigationTarget::ChatSession {
                session_id,
                project_root,
            } => self.open_session_after_draft_with_navigation(
                session_id,
                project_root,
                Some(traversal),
                cx,
            ),
            NativeNavigationTarget::ChatDraft { project_root, .. } => {
                self.activate_draft_navigation(project_root, cx);
                self.navigation_traversal = None;
            }
            NativeNavigationTarget::Settings { page } => {
                self.apply_settings_page(page, cx);
                self.navigation_traversal = None;
            }
            NativeNavigationTarget::Workbench { pane, project_root } => {
                if let Err(error) = self.prepare_workbench_navigation(pane, &project_root, cx) {
                    self.retry_navigation_after_failure(traversal, error, cx);
                    cx.notify();
                    return;
                }
                self.apply_workbench(pane, project_root, cx);
                self.navigation_traversal = None;
            }
        }
    }

    fn activate_draft_navigation(&mut self, project_root: String, cx: &mut Context<Self>) {
        let previous_root = self.root_path.clone();
        if self.active_session_id.is_none()
            && normalize_project_root(&previous_root) == normalize_project_root(&project_root)
        {
            // 设置/工作台未替换当前窗口草稿时直接复用输入实体，不清空尚未发送的正文。
            self.apply_page(NativeUiPage::Chat, cx);
            return;
        }
        self.cancel_subscription();
        self.session_generation = self.session_generation.wrapping_add(1);
        self.active_session_id = None;
        self.clear_conversation_projection();
        self.invalidate_draft_composer(cx);
        self.root_path = project_root.clone();
        self.state.workspace_root = (!project_root.is_empty()).then_some(project_root.clone());
        self.reset_draft_composer_for_project(&project_root, cx);
        self.suppress_field_events = true;
        self.field.update(cx, |field, cx| field.set_text("", cx));
        self.suppress_field_events = false;
        self.apply_page(NativeUiPage::Chat, cx);
        if normalize_project_root(&previous_root) != normalize_project_root(&project_root) {
            self.load_workspace_page(None, false, cx);
        }
    }

    fn retry_navigation_after_failure(
        &mut self,
        traversal: NavigationTraversal,
        error: NativeUiError,
        cx: &mut Context<Self>,
    ) {
        if !self
            .navigation_traversal
            .as_ref()
            .is_some_and(|current| current.token == traversal.token)
        {
            return;
        }
        if error.retryable {
            // 暂时读取失败时保留历史条目，并把 cursor 和真实 UI 一起退回 anchor。
            self.navigation.return_from_traversal(traversal.direction);
            self.navigation_traversal = None;
            self.set_error(error);
            return;
        }
        self.navigation
            .remove_failed_target(&traversal.target, traversal.direction);
        let next = match traversal.direction {
            NavigationDirection::Back => self.navigation.move_back(),
            NavigationDirection::Forward => self.navigation.move_forward(),
        };
        let Some(next) = next else {
            self.navigation_traversal = None;
            self.set_error(error);
            return;
        };
        let next_traversal = NavigationTraversal {
            token: traversal.token,
            direction: traversal.direction,
            target: next,
        };
        self.navigation_traversal = Some(next_traversal.clone());
        self.activate_navigation_target(next_traversal, cx);
    }

    fn titlebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let metadata_size = ui_text_size(theme, UiTextSize::Xs);
        let this = cx.entity();
        let pending = self.state.pending_operations.len();
        let page = self.page;
        let update_status = self.update_status.as_ref().and_then(update_status_label);
        let window_controls = window.window_controls();
        let can_minimize = window_controls.minimize && window.is_minimizable();
        let can_maximize = window_controls.maximize && window.is_resizable();
        let maximized = window.is_maximized();

        let mut controls = div()
            .id("native-window-controls")
            .flex()
            .items_center()
            .gap_0p5()
            .h_full();
        if can_minimize {
            controls = controls.child(
                div()
                    .id("native-window-minimize")
                    .w(px(28.0))
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(colors.fg)
                    .window_control_area(WindowControlArea::Min)
                    .hover(|style| style.bg(colors.hover))
                    .active(|style| style.bg(colors.active))
                    .on_click(|_, window, _| window.minimize_window())
                    .child(Icon::new(IconName::Minus).size(IconSize::Md)),
            );
        }
        if can_maximize {
            controls = controls.child(
                div()
                    .id("native-window-maximize")
                    .w(px(28.0))
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(colors.fg)
                    .window_control_area(WindowControlArea::Max)
                    .hover(|style| style.bg(colors.hover))
                    .active(|style| style.bg(colors.active))
                    .on_click(|_, window, _| window.zoom_window())
                    .child(
                        // 来源窗口图标使用独立的 14/24 路径轮廓，
                        // Lucide Square/Copy 的默认轮廓更大，不能只靠相同 size-4 对齐。
                        Icon::from_path(if maximized {
                            "native/icons/window-restore.svg"
                        } else {
                            "native/icons/window-maximize.svg"
                        })
                        .size(IconSize::Md),
                    ),
            );
        }
        let close_handler = self.native_close_handler.clone();
        controls = controls.child(
            div()
                .id("native-window-close")
                .w(px(28.0))
                .h(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .text_color(colors.fg)
                .window_control_area(WindowControlArea::Close)
                .hover(|style| style.bg(colors.danger).text_color(colors.on_accent))
                .active(|style| style.bg(colors.danger.opacity(0.8)))
                .on_click(move |_, window, cx| {
                    if let Some(handler) = close_handler.as_ref() {
                        handler(window, cx);
                    }
                })
                .child(Icon::new(IconName::X).size(IconSize::Md)),
        );

        let workbench_page = this;
        let workbench_target = if page == NativeUiPage::Workbench {
            NativeUiPage::Chat
        } else {
            NativeUiPage::Workbench
        };
        let workbench_icon = if page == NativeUiPage::Workbench {
            IconName::MessageSquare
        } else {
            IconName::FileText
        };
        let workbench_tooltip = if page == NativeUiPage::Workbench {
            "返回会话"
        } else {
            "工作台"
        };
        let drag_region = div()
            .id("native-titlebar-drag-region")
            .flex_1()
            .min_w_0()
            .h_full()
            .window_control_area(WindowControlArea::Drag)
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .text_color(colors.fg)
            .child(div().flex_1().min_w_0())
            .when(pending > 0, |bar| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(metadata_size)
                        .text_color(colors.fg_subtle)
                        .child(format!("处理中 {pending}")),
                )
            })
            .when_some(self.native_badge, |bar, count| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(metadata_size)
                        .text_color(colors.fg_subtle)
                        .child(format!("通知 {count}")),
                )
            })
            .when_some(update_status, |bar, (label, danger)| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(metadata_size)
                        .text_color(if danger {
                            colors.danger
                        } else {
                            colors.fg_subtle
                        })
                        .child(label),
                )
            })
            .when_some(self.last_error.as_ref(), |bar, error| {
                bar.child(
                    div()
                        .flex_none()
                        .text_size(metadata_size)
                        .text_color(colors.danger)
                        .child(error.message.clone()),
                )
            });
        let navigation = div().flex().items_center().gap_0p5().child(
            IconButton::new("native-workbench-page", workbench_icon)
                .variant(if page == NativeUiPage::Workbench {
                    ButtonVariant::Subtle
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Sm)
                .tooltip(workbench_tooltip)
                .on_click(move |_, _, cx| {
                    workbench_page.update(cx, |ui, cx| {
                        if workbench_target == NativeUiPage::Workbench {
                            ui.open_workbench(WorkbenchPane::Files, cx);
                        } else {
                            ui.set_page(workbench_target, cx);
                        }
                    });
                }),
        );
        let terminal_entity = cx.entity();
        let workbench_entity = cx.entity();
        let help_entity = cx.entity();
        let settings_help_entity = cx.entity();
        let workspace_actions = div()
            .flex()
            .items_center()
            .gap_0p5()
            .child(workspace_header_button(
                "native-workspace-help",
                Icon::new(IconName::CircleHelp),
                "帮助与诊断",
                move |app| {
                    help_entity.update(app, |ui, cx| {
                        ui.open_settings(SettingsPage::Diagnostics, cx)
                    });
                },
                cx,
            ))
            .child(workspace_header_button(
                "native-workspace-terminal",
                Icon::from_path("native/icons/square-terminal.svg"),
                "终端",
                move |app| {
                    terminal_entity.update(app, |ui, cx| {
                        ui.open_workbench(WorkbenchPane::Terminal, cx);
                    });
                },
                cx,
            ))
            .child(workspace_header_button(
                "native-workspace-panel",
                Icon::from_path("native/icons/panel-right-open.svg"),
                "文件与工作台",
                move |app| {
                    workbench_entity
                        .update(app, |ui, cx| ui.open_workbench(WorkbenchPane::Files, cx));
                },
                cx,
            ));

        // Chat 页左侧 SIDEBAR_WIDTH（当前 264px）包含后退/前进按钮；绝对标题栏从内容区
        // 起点开始，避免其 Drag control area 覆盖 Sidebar 的真实按钮命中范围。Sidebar
        // 顶部自己的 Drag control area 仍覆盖左侧空白，因此窗口拖动区域保持连续。
        let titlebar_left = if page == NativeUiPage::Chat {
            SIDEBAR_WIDTH
        } else {
            0.0
        };
        div()
            .id("native-titlebar")
            .absolute()
            .top(px(DESKTOP_INSET + 1.0))
            .left(px(titlebar_left))
            .right(px(DESKTOP_INSET + 8.0))
            .flex()
            .items_center()
            .flex_none()
            .h(px(TITLEBAR_HEIGHT))
            .child(drag_region)
            .when(page == NativeUiPage::Chat, |bar| {
                bar.child(workspace_actions)
            })
            .when(page == NativeUiPage::Settings, |bar| {
                // 设置壳层也保留固定来源的帮助图标，入口指向本地诊断页。
                bar.child(workspace_header_button(
                    "native-settings-help",
                    Icon::new(IconName::CircleHelp),
                    "帮助与诊断",
                    move |app| {
                        settings_help_entity.update(app, |ui, cx| {
                            ui.open_settings(SettingsPage::Diagnostics, cx)
                        });
                    },
                    cx,
                ))
            })
            // 聊天和设置使用侧栏入口；独立工作台保留返回，避免形成不可退出页面。
            .when(page == NativeUiPage::Workbench, |bar| bar.child(navigation))
            .child(controls)
            .into_any_element()
    }

    fn empty_panel(&self, title: &'static str, icon: IconName, cx: &mut App) -> AnyElement {
        let theme = cx.theme();
        let shell = ShellColors::from_theme(theme);
        let colors = theme.colors.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(shell.content)
            .text_color(colors.fg_muted)
            .child(Icon::new(icon).size(IconSize::Lg))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_size(ui_text_size(theme, UiTextSize::Lg))
                    .child(title),
            )
            .into_any_element()
    }
}

async fn process_events(
    weak: gpui::WeakEntity<NativeUi>,
    session_id: String,
    subscription_generation: u64,
    mut receiver: Receiver<EventMailboxItem>,
    overflow: Arc<Mutex<EventOverflow>>,
    cx: &mut gpui::AsyncApp,
) {
    while let Some(item) = receiver.next().await {
        let (batches, mailbox_overflow) = drain_ready_event_items(&mut receiver, item);
        let overflow_range = overflow
            .lock()
            .ok()
            .and_then(|mut overflow| overflow.take());
        let keep_receiving = weak
            .update(cx, |ui, cx| {
                if ui.subscription_generation != subscription_generation
                    || ui.active_session_id.as_deref() != Some(session_id.as_str())
                {
                    return false;
                }
                if mailbox_overflow || overflow_range.is_some() {
                    // 释放整个旧邮箱，从新订阅的权威首批开始；Agent 执行继续运行。
                    ui.restart_subscription(session_id.clone(), cx);
                    return false;
                }
                for batch in batches {
                    if ui.apply_event_batch(batch, cx) {
                        // delivery_sequence 发现缺口时不能继续应用后续增量。
                        ui.restart_subscription(session_id.clone(), cx);
                        return false;
                    }
                }
                cx.notify();
                true
            })
            .unwrap_or(false);
        if !keep_receiving {
            return;
        }
    }
}

fn drain_ready_event_items(
    receiver: &mut Receiver<EventMailboxItem>,
    first: EventMailboxItem,
) -> (Vec<NativeEventBatch>, bool) {
    let mut batches = Vec::with_capacity(NATIVE_EVENT_MAX_BATCHES_PER_WAKE);
    let mut mailbox_overflow = false;
    if let Some(batch) = first.into_batch() {
        batches.push(batch);
    } else {
        mailbox_overflow = true;
    }
    // 同一次 ready 唤醒内批量收取，保持 delivery_sequence 顺序，最后只触发一次
    // GPUI notify；有界上限避免持续 streaming 把一次调度占满。
    while batches.len() < NATIVE_EVENT_MAX_BATCHES_PER_WAKE {
        match receiver.try_recv() {
            Ok(item) => {
                if let Some(batch) = item.into_batch() {
                    batches.push(batch);
                } else {
                    mailbox_overflow = true;
                }
            }
            Err(_) => break,
        }
    }
    (batches, mailbox_overflow)
}

fn update_status_label(status: &AppUpdateStatus) -> Option<(String, bool)> {
    if let Some(error) = status
        .download_error
        .as_deref()
        .filter(|error| !error.trim().is_empty())
    {
        return Some((format!("更新失败：{error}"), true));
    }

    let label = match status.download_state {
        AppUpdateDownloadState::Downloading => match status.total_bytes {
            Some(total) => format!("正在下载更新 {}/{}", status.downloaded_bytes, total),
            None => format!("正在下载更新 {}", status.downloaded_bytes),
        },
        AppUpdateDownloadState::Verifying => "正在校验更新".to_owned(),
        AppUpdateDownloadState::Ready => "更新已就绪".to_owned(),
        AppUpdateDownloadState::Installing => "正在安装更新".to_owned(),
        AppUpdateDownloadState::Failed => "更新失败".to_owned(),
        AppUpdateDownloadState::Idle if status.available => status
            .latest_release
            .as_deref()
            .or(status.latest_version.as_deref())
            .map(|version| format!("有新版本 {version}"))
            .unwrap_or_else(|| "有新版本".to_owned()),
        AppUpdateDownloadState::Idle if status.checked => "已检查更新".to_owned(),
        AppUpdateDownloadState::Idle => return None,
    };
    Some((label, false))
}

impl Render for NativeUi {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (shell, colors, font_family) = {
            let theme = cx.theme();
            (
                ShellColors::from_theme(theme),
                theme.colors.clone(),
                ui_font(theme.font_family.clone()),
            )
        };
        let page = self.page;
        let titlebar = self.titlebar(window, cx);
        let body = match page {
            NativeUiPage::Chat => {
                let action = self.action_handler(cx);
                let workbench = {
                    let this = cx.entity();
                    move |_: &mut Window, app: &mut App| {
                        this.update(app, |ui, cx| ui.open_workbench(WorkbenchPane::Files, cx));
                    }
                };
                let search = {
                    let this = cx.entity();
                    move |_: &mut Window, app: &mut App| {
                        this.update(app, |ui, cx| ui.open_search(cx));
                    }
                };
                let settings = {
                    let this = cx.entity();
                    move |page: SettingsPage, _: &mut Window, app: &mut App| {
                        this.update(app, |ui, cx| ui.open_settings(page, cx));
                    }
                };
                let composer_settings = {
                    let this = cx.entity();
                    move |page: SettingsPage, _: &mut Window, app: &mut App| {
                        this.update(app, |ui, cx| ui.open_settings(page, cx));
                    }
                };
                let navigation_back = {
                    let this = cx.entity();
                    move |_: &mut Window, app: &mut App| {
                        this.update(app, |ui, cx| ui.go_back(cx));
                    }
                };
                let navigation_forward = {
                    let this = cx.entity();
                    move |_: &mut Window, app: &mut App| {
                        this.update(app, |ui, cx| ui.go_forward(cx));
                    }
                };
                let sidebar = Sidebar::new(
                    Some(self.root_path.clone()),
                    self.state.workspace.clone(),
                    self.active_session_id.clone(),
                    self.keybindings.clone(),
                    action,
                )
                .with_workbench_handler(workbench)
                .with_search_handler(search)
                .with_settings_handler(settings)
                .with_navigation_handlers(
                    self.navigation.can_go_back() && self.navigation_traversal.is_none(),
                    self.navigation.can_go_forward() && self.navigation_traversal.is_none(),
                    navigation_back,
                    navigation_forward,
                );
                // 首次附件确认/发送失败时仍保留 Draft Composer，用户必须能重试同一
                // 条路径意图；切换到已确认会话后才交给普通 Composer 投影。
                let conversation = if self.pending_new_session_draft.is_some()
                    || self.pending_initial_send.is_some()
                {
                    None
                } else {
                    self.conversation_for_render()
                };
                let conversation_width = conversation_content_width(
                    f32::from(window.viewport_size().width)
                        - SIDEBAR_WIDTH
                        - DESKTOP_INSET * 2.0
                        - 2.0,
                );
                let action = self.action_handler(cx);
                let draft_action = self.draft_action_handler(cx);
                let model_catalog = Arc::clone(&self.state.model_catalog);
                let composer = match conversation.as_ref() {
                    Some(conversation) => ComposerView::new(
                        conversation.session.session_id.clone(),
                        conversation,
                        &self.field,
                        Arc::clone(&model_catalog),
                        self.keybindings.clone(),
                        action,
                    )
                    .into_any_element(),
                    None => DraftComposerView::new(
                        self.root_path.clone(),
                        model_catalog,
                        self.draft_composer_options.clone(),
                        &self.field,
                        self.keybindings.clone(),
                        action,
                        draft_action,
                        composer_settings,
                    )
                    .with_projects(
                        self.state
                            .workspace
                            .as_ref()
                            .map(|workspace| workspace.projects.clone())
                            .unwrap_or_default(),
                    )
                    .into_any_element(),
                };
                let conversation_is_empty = conversation.as_ref().is_none_or(|conversation| {
                    conversation.messages.is_empty() && conversation.input_queue.is_empty()
                });
                let (empty_composer, bottom_composer) = if conversation_is_empty {
                    (Some(composer), None)
                } else {
                    (None, Some(composer))
                };
                let chat = ChatView::new(
                    conversation,
                    conversation_width,
                    empty_composer,
                    self.action_handler(cx),
                    self.message_list.clone(),
                )
                .with_draft_field(&self.field);
                div()
                    .size_full()
                    .flex()
                    .child(sidebar)
                    .child(
                        div()
                            .flex_none()
                            .w(px(DESKTOP_INSET))
                            .h_full()
                            .bg(shell.canvas),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .p(px(DESKTOP_INSET))
                            .pl(px(0.0))
                            .bg(shell.canvas)
                            .child(
                                div()
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .overflow_hidden()
                                    .rounded(px(5.0))
                                    .border_1()
                                    .border_color(colors.border)
                                    .bg(shell.content)
                                    .pr(px(CONVERSATION_SCROLL_GUTTER))
                                    // 来源 WorkspaceHeader 占据 48px；浮动窗控不能代替布局高度。
                                    .child(div().h(px(TITLEBAR_HEIGHT)).flex_none())
                                    .child(chat)
                                    .when_some(bottom_composer, |body, composer| {
                                        body.child(
                                            div()
                                                .flex_none()
                                                .bg(shell.content)
                                                .px_4()
                                                .pb_4()
                                                .child(
                                                    div()
                                                        .w_full()
                                                        .max_w(conversation_width)
                                                        .mx_auto()
                                                        .child(composer),
                                                ),
                                        )
                                    }),
                            ),
                    )
                    .into_any_element()
            }
            NativeUiPage::Settings => {
                let panel = self
                    .settings_factory
                    .as_ref()
                    .map(|factory| factory(window, cx))
                    .unwrap_or_else(|| self.empty_panel("设置", IconName::Settings, cx));
                div()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .bg(shell.canvas)
                    .child(div().size_full().bg(shell.content).child(panel))
                    .into_any_element()
            }
            NativeUiPage::Workbench => {
                let root = self.active_workbench_root();
                if self.last_workbench_root.as_deref() != Some(root.as_str()) {
                    self.last_workbench_root = Some(root.clone());
                    if let Some(handler) = self.workbench_root_changed.as_ref() {
                        handler(&root, cx);
                    }
                }
                let panel = self
                    .workbench_factory
                    .as_ref()
                    .map(|factory| factory(window, cx))
                    .unwrap_or_else(|| self.empty_panel("工作台", IconName::FileText, cx));
                div()
                    .flex()
                    .flex_1()
                    // NativeUi 根节点是覆盖层定位容器，不参与 flex 分配；显式继承
                    // client 高度，否则工作台面板会退化为内容高度，终端 grid 为零高。
                    .h_full()
                    .min_w_0()
                    .min_h_0()
                    .p(px(DESKTOP_INSET))
                    // 标题栏是绝对定位并覆盖在 body 之上；工作台 tabs 必须从标题栏下方开始，
                    // 否则标题栏拖拽区会吞掉 tabs 的真实点击事件。
                    .pt(px(TITLEBAR_HEIGHT + DESKTOP_INSET + 1.0))
                    .bg(shell.canvas)
                    .child(div().size_full().bg(shell.content).child(panel))
                    .into_any_element()
            }
        };
        div()
            .size_full()
            .relative()
            .bg(shell.canvas)
            .text_color(colors.fg)
            .font(font_family)
            // ZCode Tailwind 的默认行高是 1.5；GPUI 的 phi 默认值会让未显式设置
            // 行高的标签和导航产生累积偏移。代码/终端的局部行高仍由各自组件设置。
            .line_height(relative(1.5))
            .child(body)
            .child(titlebar)
    }
}

impl Drop for NativeUi {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn append_draft_attachment_paths(
    paths: &mut Vec<String>,
    incoming: Vec<std::path::PathBuf>,
) -> usize {
    let mut accepted = 0;
    for path in incoming {
        if paths.len() >= MAX_DRAFT_ATTACHMENT_PATHS {
            break;
        }
        let value = path.to_string_lossy().into_owned();
        if value.is_empty()
            || value.len() > MAX_DRAFT_ATTACHMENT_PATH_BYTES
            || paths
                .iter()
                .any(|current| same_attachment_path(current, &value))
        {
            continue;
        }
        paths.push(value);
        accepted += 1;
    }
    accepted
}

fn same_attachment_path(left: &str, right: &str) -> bool {
    left == right
        || Path::new(left)
            .canonicalize()
            .ok()
            .zip(Path::new(right).canonicalize().ok())
            .is_some_and(|(left, right)| left == right)
}

fn operation_id(action: &NativeUiAction) -> Option<&str> {
    match action {
        NativeUiAction::CreateSession { operation_id, .. }
        | NativeUiAction::CreateProject { operation_id, .. }
        | NativeUiAction::RenameProject { operation_id, .. }
        | NativeUiAction::ForgetProject { operation_id, .. }
        | NativeUiAction::ReorderProjects { operation_id, .. }
        | NativeUiAction::RenameSession { operation_id, .. }
        | NativeUiAction::DeleteSession { operation_id, .. }
        | NativeUiAction::SetSessionArchived { operation_id, .. }
        | NativeUiAction::SetSessionPinned { operation_id, .. }
        | NativeUiAction::GroupSessions { operation_id, .. }
        | NativeUiAction::Send { operation_id, .. }
        | NativeUiAction::SetModel { operation_id, .. }
        | NativeUiAction::SetEffort { operation_id, .. }
        | NativeUiAction::SetVision { operation_id, .. }
        | NativeUiAction::SetPermission { operation_id, .. }
        | NativeUiAction::SetPlan { operation_id, .. }
        | NativeUiAction::SetFollowupMode { operation_id, .. }
        | NativeUiAction::Stop { operation_id, .. }
        | NativeUiAction::ColdRestore { operation_id, .. }
        | NativeUiAction::Rewind { operation_id, .. }
        | NativeUiAction::Branch { operation_id, .. }
        | NativeUiAction::Feedback { operation_id, .. }
        | NativeUiAction::ApproveTool { operation_id, .. }
        | NativeUiAction::AnswerQuestion { operation_id, .. }
        | NativeUiAction::AddAttachment { operation_id, .. }
        | NativeUiAction::EditQueuedInput { operation_id, .. }
        | NativeUiAction::ReorderQueuedInput { operation_id, .. }
        | NativeUiAction::SendQueuedInput { operation_id, .. }
        | NativeUiAction::DeleteQueuedInput { operation_id, .. } => Some(operation_id),
        NativeUiAction::OpenProject { .. }
        | NativeUiAction::LoadWorkspace { .. }
        | NativeUiAction::OpenSession { .. }
        | NativeUiAction::SetDraft { .. }
        | NativeUiAction::LoadHistory { .. }
        | NativeUiAction::LoadLatest { .. }
        | NativeUiAction::RemoveAttachment { .. }
        | NativeUiAction::LoadQueuedInput { .. }
        | NativeUiAction::SelectPromptHistory { .. } => None,
    }
}

/// 在 UI 第一次接纳 mutation 时替换组件传入的固定意图 ID。
///
/// 组件仍可用旧 ID 表示动作类别，但 Journal 的幂等键必须代表一次具体提交；
/// 时间戳与进程内递增序列共同保证本窗口 admission 的有界唯一性，不把路径或用户
/// 文本拼进 operation ID。Send 经草稿 flush 递归进入时不会再次调用本函数。
fn admit_operation(mut action: NativeUiAction) -> NativeUiAction {
    let Some(previous) = operation_id(&action) else {
        return action;
    };
    let namespace = previous
        .strip_prefix("ui:")
        .and_then(|value| value.split(':').next())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        .unwrap_or("mutation");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = UI_OPERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let admitted = format!("ui:{namespace}:{timestamp:x}-{sequence:x}");
    set_operation_id(&mut action, admitted);
    action
}

fn set_operation_id(action: &mut NativeUiAction, value: String) {
    match action {
        NativeUiAction::CreateSession { operation_id, .. }
        | NativeUiAction::CreateProject { operation_id, .. }
        | NativeUiAction::RenameProject { operation_id, .. }
        | NativeUiAction::ForgetProject { operation_id, .. }
        | NativeUiAction::ReorderProjects { operation_id, .. }
        | NativeUiAction::RenameSession { operation_id, .. }
        | NativeUiAction::DeleteSession { operation_id, .. }
        | NativeUiAction::SetSessionArchived { operation_id, .. }
        | NativeUiAction::SetSessionPinned { operation_id, .. }
        | NativeUiAction::GroupSessions { operation_id, .. }
        | NativeUiAction::Send { operation_id, .. }
        | NativeUiAction::SetModel { operation_id, .. }
        | NativeUiAction::SetEffort { operation_id, .. }
        | NativeUiAction::SetVision { operation_id, .. }
        | NativeUiAction::SetPermission { operation_id, .. }
        | NativeUiAction::SetPlan { operation_id, .. }
        | NativeUiAction::SetFollowupMode { operation_id, .. }
        | NativeUiAction::Stop { operation_id, .. }
        | NativeUiAction::ColdRestore { operation_id, .. }
        | NativeUiAction::Rewind { operation_id, .. }
        | NativeUiAction::Branch { operation_id, .. }
        | NativeUiAction::Feedback { operation_id, .. }
        | NativeUiAction::ApproveTool { operation_id, .. }
        | NativeUiAction::AnswerQuestion { operation_id, .. }
        | NativeUiAction::AddAttachment { operation_id, .. }
        | NativeUiAction::EditQueuedInput { operation_id, .. }
        | NativeUiAction::ReorderQueuedInput { operation_id, .. }
        | NativeUiAction::SendQueuedInput { operation_id, .. }
        | NativeUiAction::DeleteQueuedInput { operation_id, .. } => *operation_id = value,
        NativeUiAction::OpenProject { .. }
        | NativeUiAction::LoadWorkspace { .. }
        | NativeUiAction::OpenSession { .. }
        | NativeUiAction::SetDraft { .. }
        | NativeUiAction::LoadHistory { .. }
        | NativeUiAction::LoadLatest { .. }
        | NativeUiAction::RemoveAttachment { .. }
        | NativeUiAction::LoadQueuedInput { .. }
        | NativeUiAction::SelectPromptHistory { .. } => {}
    }
}

fn affects_workspace(action: &NativeUiAction) -> bool {
    matches!(
        action,
        NativeUiAction::CreateSession { .. }
            | NativeUiAction::CreateProject { .. }
            | NativeUiAction::RenameProject { .. }
            | NativeUiAction::ForgetProject { .. }
            | NativeUiAction::ReorderProjects { .. }
            | NativeUiAction::RenameSession { .. }
            | NativeUiAction::DeleteSession { .. }
            | NativeUiAction::SetSessionArchived { .. }
            | NativeUiAction::SetSessionPinned { .. }
            | NativeUiAction::GroupSessions { .. }
            | NativeUiAction::Branch { .. }
    )
}

fn reload_conversation_session(action: &NativeUiAction) -> Option<&str> {
    match action {
        NativeUiAction::Send { session_id, .. }
        | NativeUiAction::ColdRestore { session_id, .. }
        | NativeUiAction::Rewind { session_id, .. }
        | NativeUiAction::Feedback { session_id, .. }
        | NativeUiAction::ApproveTool { session_id, .. }
        | NativeUiAction::AnswerQuestion { session_id, .. }
        | NativeUiAction::AddAttachment { session_id, .. }
        | NativeUiAction::RemoveAttachment { session_id, .. }
        | NativeUiAction::SelectPromptHistory { session_id, .. } => Some(session_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_ui::model::{
        FollowupMode, MAX_DISPLAY_BYTES, MessageBlock, MessageRole, PermissionMode, PlanMode,
        PromptHistoryEntry, PromptHistoryFact, QueuedInputFact, QueuedInputState, SessionStatus,
        message_display_bytes,
    };

    #[test]
    fn workspace_mutations_require_reload() {
        let action = NativeUiAction::SetSessionPinned {
            session_id: "s".to_owned(),
            pinned: true,
            operation_id: "op".to_owned(),
        };
        assert!(affects_workspace(&action));
        assert_eq!(operation_id(&action), Some("op"));
    }

    #[test]
    fn draft_does_not_create_pending_operation() {
        let action = NativeUiAction::SetDraft {
            session_id: "s".to_owned(),
            draft: DraftFact::default(),
            edit_generation: 1,
        };
        assert!(!affects_workspace(&action));
        assert_eq!(operation_id(&action), None);
    }

    #[test]
    fn draft_from_field_text_preserves_input_before_snapshot_arrives() {
        let text = "请只回复 KEENCODE_NATIVE_KEYBINDING_LIVE_OK，不要调用工具。".to_owned();
        let draft = draft_from_field_text(text.clone(), None);

        assert_eq!(draft.text, text);
        assert!(draft.attachments.is_empty());
        assert!(draft.mention_query.is_none());
    }

    #[test]
    fn admitted_mutations_receive_unique_bounded_ids() {
        let make = || NativeUiAction::CreateSession {
            project_root: "D:/project".to_owned(),
            operation_id: "ui:new-chat".to_owned(),
        };
        let first = admit_operation(make());
        let second = admit_operation(make());
        let first_id = operation_id(&first).expect("首次 mutation ID");
        let second_id = operation_id(&second).expect("第二次 mutation ID");

        assert_ne!(first_id, second_id);
        assert!(first_id.starts_with("ui:new-chat:"));
        assert!(second_id.starts_with("ui:new-chat:"));
        assert!(first_id.len() < 256);
        assert!(second_id.len() < 256);
    }

    #[test]
    fn admission_leaves_read_actions_without_operation_ids() {
        let action = admit_operation(NativeUiAction::LoadLatest {
            session_id: "s".to_owned(),
        });
        assert_eq!(operation_id(&action), None);
    }

    #[test]
    fn draft_attachment_paths_are_bounded_and_deduplicated() {
        let long_path = std::path::PathBuf::from("x".repeat(MAX_DRAFT_ATTACHMENT_PATH_BYTES + 1));
        let accepted = append_draft_attachment_paths(
            &mut vec!["D:/already.txt".to_owned()],
            vec![
                std::path::PathBuf::from("D:/already.txt"),
                std::path::PathBuf::from("D:/new.txt"),
                long_path,
            ],
        );
        assert_eq!(accepted, 1);
    }

    #[test]
    fn initial_send_waits_for_host_attachment_facts() {
        let mut pending = PendingInitialSend {
            generation: 1,
            session_id: "session".to_owned(),
            project_root: "D:/project".to_owned(),
            text: "发送".to_owned(),
            attachment_paths: vec!["D:/file.txt".to_owned()],
            next_attachment_index: 0,
            confirmed_attachments: Vec::new(),
            model: None,
            effort: None,
            permission: PermissionMode::Build,
            plan: PlanMode::Off,
            confirming_attachment: false,
        };
        assert_eq!(initial_send_step(&pending), InitialSendStep::Attach);
        pending.confirming_attachment = true;
        assert_eq!(
            initial_send_step(&pending),
            InitialSendStep::AwaitConfirmation
        );
        pending.confirming_attachment = false;
        pending.attachment_paths.clear();
        assert_eq!(initial_send_step(&pending), InitialSendStep::Send);
    }

    fn test_batch(delivery_sequence: u64, text: &str) -> NativeEventBatch {
        NativeEventBatch {
            session_id: "session-mailbox".to_owned(),
            delivery_sequence,
            journal_sequence: delivery_sequence,
            events: vec![NativeUiEvent::MarkdownDelta {
                message_id: "message".to_owned(),
                block_id: "block".to_owned(),
                append: text.to_owned(),
                completed: false,
            }],
        }
    }

    #[test]
    fn mailbox_drain_preserves_order_and_caps_one_ready_burst() {
        let budget = Arc::new(EventMailboxBudget {
            reserved: AtomicUsize::new(0),
            snapshot_metadata_reserved: AtomicUsize::new(0),
        });
        let overflow = Arc::new(Mutex::new(EventOverflow::default()));
        let (mut sender, mut receiver) = channel(NATIVE_EVENT_MAILBOX_CAPACITY);
        for sequence in 1..=(NATIVE_EVENT_MAX_BATCHES_PER_WAKE as u64 + 5) {
            assert!(try_queue_event_batch(
                &mut sender,
                &budget,
                &overflow,
                test_batch(sequence, "x"),
            ));
        }

        let first = receiver.try_recv().expect("邮箱首批应可读取");
        let (batches, saw_overflow) = drain_ready_event_items(&mut receiver, first);
        assert!(!saw_overflow);
        assert_eq!(batches.len(), NATIVE_EVENT_MAX_BATCHES_PER_WAKE);
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.delivery_sequence)
                .collect::<Vec<_>>(),
            (1..=NATIVE_EVENT_MAX_BATCHES_PER_WAKE as u64).collect::<Vec<_>>()
        );

        // 达到本次唤醒上限后，剩余 ready 批次仍按原顺序留在邮箱中。
        let remaining = std::iter::from_fn(|| receiver.try_recv().ok())
            .filter_map(EventMailboxItem::into_batch)
            .map(|batch| batch.delivery_sequence)
            .collect::<Vec<_>>();
        assert_eq!(
            remaining,
            (NATIVE_EVENT_MAX_BATCHES_PER_WAKE as u64 + 1
                ..=NATIVE_EVENT_MAX_BATCHES_PER_WAKE as u64 + 5)
                .collect::<Vec<_>>()
        );
        drop(sender);
        assert_eq!(budget.reserved(), 0, "批次收取后预算必须全部释放");
    }

    #[test]
    fn mailbox_accepts_full_message_window_with_snapshot_metadata() {
        let budget = Arc::new(EventMailboxBudget {
            reserved: AtomicUsize::new(0),
            snapshot_metadata_reserved: AtomicUsize::new(0),
        });
        let overflow = Arc::new(Mutex::new(EventOverflow::default()));
        let (mut sender, mut receiver) = channel(NATIVE_EVENT_MAILBOX_CAPACITY);
        let message_id = "snapshot-message".to_owned();
        let block_id = "snapshot-block".to_owned();
        let source_len = MAX_DISPLAY_BYTES
            .saturating_sub(message_id.len())
            .saturating_sub(block_id.len());
        let message = UiMessage {
            message_id,
            turn_id: None,
            role: MessageRole::Assistant,
            blocks: vec![MessageBlock::Markdown {
                block_id,
                source: "x".repeat(source_len),
                streaming: false,
            }],
            feedback: None,
            created_at_unix_ms: None,
        };
        assert_eq!(message_display_bytes(&message), MAX_DISPLAY_BYTES);
        let snapshot = ConversationFact {
            session: SessionFact {
                session_id: "session-mailbox".to_owned(),
                project_key: "project".to_owned(),
                title: "Snapshot".to_owned(),
                status: SessionStatus::Idle,
                pinned: false,
                archived: false,
                unread: false,
                model: None,
                vision_enabled: false,
                effort: None,
                permission: PermissionMode::Build,
                plan: PlanMode::Off,
                followup_mode: FollowupMode::Queue,
                usage: None,
                prompt_history: PromptHistoryFact::default(),
                updated_at_unix_ms: 0,
                last_sequence: 1,
            },
            messages: Arc::new(vec![Arc::new(message)]),
            history: None,
            input_queue: vec![QueuedInputFact {
                queue_item_id: "queue-1".to_owned(),
                text: "queued text".to_owned(),
                text_complete: true,
                text_fingerprint: 1,
                state: QueuedInputState::Waiting,
            }],
            active_turn_id: None,
            draft: DraftFact {
                text: "draft text".to_owned(),
                attachments: Vec::new(),
                mention_query: None,
            },
            transcript_revision: 1,
        };
        let batch = NativeEventBatch {
            session_id: "session-mailbox".to_owned(),
            delivery_sequence: 1,
            journal_sequence: 1,
            events: vec![NativeUiEvent::Snapshot(Box::new(snapshot))],
        };
        let metadata_bytes = batch.mailbox_snapshot_metadata_bytes();
        assert!(metadata_bytes > 0);
        assert_eq!(
            batch.mailbox_text_bytes().saturating_sub(metadata_bytes),
            MAX_DISPLAY_BYTES,
            "Snapshot 正文应独立占满 8 MiB，元数据另计预算"
        );
        assert!(
            batch.mailbox_text_bytes() > NATIVE_EVENT_MAILBOX_TEXT_BUDGET,
            "测试必须覆盖消息正文加快照元数据超过单一 8 MiB 预算的场景"
        );
        assert!(try_queue_event_batch(
            &mut sender,
            &budget,
            &overflow,
            batch,
        ));
        let queued = receiver.try_recv().expect("完整 Snapshot 应可入队");
        assert!(queued.into_batch().is_some());
        assert_eq!(budget.reserved(), 0, "取出 Snapshot 后双重预算都应释放");
    }

    #[test]
    fn mailbox_accepts_bounded_large_queue_and_history_snapshot() {
        let budget = Arc::new(EventMailboxBudget {
            reserved: AtomicUsize::new(0),
            snapshot_metadata_reserved: AtomicUsize::new(0),
        });
        let overflow = Arc::new(Mutex::new(EventOverflow::default()));
        let (mut sender, mut receiver) = channel(NATIVE_EVENT_MAILBOX_CAPACITY);
        let snapshot = ConversationFact {
            session: SessionFact {
                session_id: "session-large-metadata".to_owned(),
                project_key: "project".to_owned(),
                title: "Snapshot".to_owned(),
                status: SessionStatus::Idle,
                pinned: false,
                archived: false,
                unread: false,
                model: None,
                vision_enabled: false,
                effort: None,
                permission: PermissionMode::Build,
                plan: PlanMode::Off,
                followup_mode: FollowupMode::Queue,
                usage: None,
                prompt_history: PromptHistoryFact {
                    entries: (0..50)
                        .map(|index| PromptHistoryEntry {
                            entry_id: format!("history-{index}"),
                            text: "h".repeat(2_000),
                            created_at_unix_ms: index,
                        })
                        .collect(),
                    selected: None,
                    omitted: true,
                },
                updated_at_unix_ms: 0,
                last_sequence: 1,
            },
            messages: Arc::new(Vec::new()),
            history: None,
            input_queue: (0..256)
                .map(|index| QueuedInputFact {
                    queue_item_id: format!("queue-{index}"),
                    text: "q".repeat(512),
                    text_complete: false,
                    text_fingerprint: index as u64,
                    state: QueuedInputState::Waiting,
                })
                .collect(),
            active_turn_id: None,
            draft: DraftFact {
                text: "d".repeat(1024 * 1024),
                attachments: Vec::new(),
                mention_query: None,
            },
            transcript_revision: 1,
        };
        let batch = NativeEventBatch {
            session_id: "session-large-metadata".to_owned(),
            delivery_sequence: 1,
            journal_sequence: 1,
            events: vec![NativeUiEvent::Snapshot(Box::new(snapshot))],
        };
        assert!(
            batch.mailbox_snapshot_metadata_bytes()
                <= NATIVE_EVENT_MAILBOX_SNAPSHOT_METADATA_BUDGET,
            "合法的大队列、长历史和最大草稿应落在 Snapshot 元数据预算内"
        );
        assert!(try_queue_event_batch(
            &mut sender,
            &budget,
            &overflow,
            batch,
        ));
        assert!(
            receiver
                .try_recv()
                .expect("大 Snapshot 应可入队")
                .into_batch()
                .is_some()
        );
        assert_eq!(budget.reserved(), 0);
    }

    #[test]
    fn oversized_batch_records_gap_and_wakes_resync_in_order() {
        let budget = Arc::new(EventMailboxBudget {
            reserved: AtomicUsize::new(0),
            snapshot_metadata_reserved: AtomicUsize::new(0),
        });
        let overflow = Arc::new(Mutex::new(EventOverflow::default()));
        let (mut sender, mut receiver) = channel(NATIVE_EVENT_MAILBOX_CAPACITY);
        assert!(try_queue_event_batch(
            &mut sender,
            &budget,
            &overflow,
            test_batch(1, "first"),
        ));
        let oversized = test_batch(2, &"x".repeat(NATIVE_EVENT_MAILBOX_TEXT_BUDGET + 1));
        assert!(!try_queue_event_batch(
            &mut sender,
            &budget,
            &overflow,
            oversized,
        ));
        assert!(try_queue_event_batch(
            &mut sender,
            &budget,
            &overflow,
            test_batch(3, "third"),
        ));

        let first = receiver.try_recv().expect("邮箱首批应可读取");
        let (batches, saw_overflow) = drain_ready_event_items(&mut receiver, first);
        assert!(saw_overflow, "超大批次必须通过唤醒标记触发重同步");
        assert_eq!(
            batches
                .iter()
                .map(|batch| batch.delivery_sequence)
                .collect::<Vec<_>>(),
            vec![1, 3],
            "缺口前后批次仍应按邮箱顺序读取"
        );
        assert_eq!(overflow.lock().expect("溢出锁应可用").take(), Some((2, 2)));
        drop(sender);
        assert_eq!(budget.reserved(), 0, "收取和取消都必须释放预算");
    }

    #[test]
    fn mailbox_cancel_drops_queued_reservations() {
        let budget = Arc::new(EventMailboxBudget {
            reserved: AtomicUsize::new(0),
            snapshot_metadata_reserved: AtomicUsize::new(0),
        });
        let overflow = Arc::new(Mutex::new(EventOverflow::default()));
        let (mut sender, receiver) = channel(NATIVE_EVENT_MAILBOX_CAPACITY);
        for sequence in 1..=3 {
            assert!(try_queue_event_batch(
                &mut sender,
                &budget,
                &overflow,
                test_batch(sequence, "queued"),
            ));
        }
        assert!(budget.reserved() > 0);
        drop(receiver);
        drop(sender);
        assert_eq!(budget.reserved(), 0, "取消订阅丢弃邮箱后预算必须归零");
    }
}
