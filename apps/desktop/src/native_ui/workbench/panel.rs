//! 原生工作台交互面板。
//!
//! 面板只持有 UI 状态和 GPUI Entity；文件、Git、PTY、工作树和开发服务的事实与
//! 副作用全部进入 `NativeWorkbench`。每次写操作成功后重新读取事实快照，避免用
//! 前端乐观状态伪造已提交结果。

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use alacritty_terminal::{
    index::{Column, Line, Point as CellPoint, Side},
    selection::SelectionType,
    term::TermMode,
};

use ely_gpui_component::{
    buttons::{Button, ButtonVariant, IconButton},
    editor::{CodeEditor, EditorEvent, FindOptions, FindWidget, LineNumbers, find_all},
    forms::{Checkbox, Input, InputEvent, TextInput},
    primitives::{Icon, IconName},
    theme::{ActiveTheme, ControlSize, IconSize, TextSize},
};
use futures::{
    StreamExt,
    channel::{mpsc::channel, oneshot},
};
use gpui::{
    AnyElement, App, BorderStyle, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, Focusable, Font, FontStyle, FontWeight, Hsla, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Point, Render, Rgba, Role, ScrollWheelEvent, Size,
    StrikethroughStyle, Styled, Subscription, Task, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, canvas, div, fill, outline, point, prelude::*, px, size,
};
use tokio::runtime::Handle;

use crate::native_ui::style::{
    ShellColors, UiTextSize, code_font, code_text_size, mono_font, ui_text_size,
};

use super::files::{ContentMatch, ContentSearchInput, FileSearchEntry, FileTreeInput};
use super::{
    DevServerEvent, DevServerInput, DevServerSnapshot, DiffDocument, DiffOptions, EditorInfo,
    FileEntry, FileKind, FileSearchInput, GitAction, GitFileStatus, GitStatus,
    MAX_TERMINAL_INPUT_BYTES, NativeWorkbench, PtyCell, PtyColor, PtyCursorShape, PtyEvent, PtyRgb,
    PtySelection, PtySnapshot, PtySubscription, ReviewComment, ReviewSide, StashEntry,
    WorktreeEntry, WorktreeRequest, terminal_key_bytes, terminal_paste_bytes,
};

// 根目录与多个展开目录共享这个预算，避免每个目录各取 500 项导致总快照失控。
const MAX_SNAPSHOT_FILES: usize = 2_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkbenchPane {
    #[default]
    Files,
    Search,
    Diff,
    Git,
    Worktrees,
    Terminal,
    DevServers,
}

impl WorkbenchPane {
    const ALL: [Self; 7] = [
        Self::Files,
        Self::Search,
        Self::Diff,
        Self::Git,
        Self::Worktrees,
        Self::Terminal,
        Self::DevServers,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Files => "文件",
            Self::Search => "查询",
            Self::Diff => "Diff",
            Self::Git => "Git",
            Self::Worktrees => "工作树",
            Self::Terminal => "终端",
            Self::DevServers => "进程",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Files => IconName::FolderOpen,
            Self::Search => IconName::Search,
            Self::Diff => IconName::FileText,
            Self::Git => IconName::GitBranch,
            Self::Worktrees => IconName::Folder,
            Self::Terminal => IconName::Terminal,
            Self::DevServers => IconName::Wrench,
        }
    }
}

fn code_editor_language(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "rs" => "Rust",
        "json" | "jsonc" => "JSON",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "c" | "h" => "C",
        "cc" | "cpp" | "cxx" | "hpp" => "C++",
        "css" | "scss" => "CSS",
        "html" | "htm" => "HTML",
        "sql" => "SQL",
        "md" | "markdown" => "Markdown",
        "sh" | "bash" | "zsh" | "fish" => "Shell",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        _ => "Plain text",
    }
}

fn code_editor_state_key(path: &Path, revision: u64) -> String {
    // 只有成功读取或写入磁盘事实后才递增 revision，输入期间保持 Entity 和撤销栈。
    format!("native-workbench-editor:{}:{revision}", path.display())
}

fn hsla_bits(color: Hsla) -> [u32; 4] {
    [
        color.h.to_bits(),
        color.s.to_bits(),
        color.l.to_bits(),
        color.a.to_bits(),
    ]
}

#[derive(Clone, Debug, Default)]
struct WorkbenchSnapshot {
    root: PathBuf,
    files: Vec<FileEntry>,
    diff: Option<DiffDocument>,
    diff_path: Option<String>,
    git: Option<GitStatus>,
    stashes: Vec<StashEntry>,
    worktrees: Vec<WorktreeEntry>,
    editors: Vec<EditorInfo>,
    comments: Vec<ReviewComment>,
    terminals: Vec<PtySnapshot>,
    dev_servers: Vec<DevServerSnapshot>,
    search_count: usize,
    search_results: Vec<FileSearchEntry>,
    search_truncated: bool,
    content_results: Vec<ContentMatch>,
    content_truncated: bool,
    files_truncated: bool,
}

#[derive(Clone, Copy, Debug)]
struct TerminalGeometry {
    origin: Point<Pixels>,
    cell: Size<Pixels>,
}

/// 终端鼠标报告的语义输入，统一按钮、释放、移动和修饰键状态。
#[derive(Clone, Copy, Debug)]
struct TerminalMouseReport {
    button: u8,
    release: bool,
    motion: bool,
    shift: bool,
    alt: bool,
    control: bool,
}

#[derive(Debug)]
struct EditorState {
    relative: PathBuf,
    path: PathBuf,
    /// 磁盘正文只作为 keyed CodeEditor 的初始化输入；重绘仅复制 Arc，不复制正文。
    text: Arc<str>,
    modified_unix_ms: Option<u64>,
    revision: u64,
}

#[derive(Clone, Debug)]
struct EditorFindCache {
    key: String,
    text_revision: u64,
    query: String,
    options: FindOptions,
    matches: Vec<Range<usize>>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorHighlightState {
    key: String,
    open: bool,
    current: Option<usize>,
    generation: u64,
    warning: [u32; 4],
}

struct EditorSubscriptions {
    key: String,
    _subscriptions: Vec<Subscription>,
}

/// 可嵌入 `NativeUi` 的工作台句柄。句柄只引用 GPUI Entity，不复制工作台服务。
#[derive(Clone)]
pub struct NativeWorkbenchPanelHandle {
    entity: Entity<NativeWorkbenchPanel>,
}

type PaneNavigationHandler = Rc<dyn Fn(WorkbenchPane, &str, &mut App)>;

impl NativeWorkbenchPanelHandle {
    pub fn entity(&self) -> Entity<NativeWorkbenchPanel> {
        self.entity.clone()
    }

    pub fn element(&self) -> AnyElement {
        self.entity.clone().into_any_element()
    }

    /// 程序化导航不触发用户回调；根目录切换被未保存编辑或在途操作阻挡时不改变 pane。
    pub fn navigate(&self, pane: WorkbenchPane, root: &str, cx: &mut App) -> Result<(), String> {
        self.entity.update(cx, |panel, cx| {
            if panel.closed {
                return Err("工作台已关闭".to_owned());
            }
            let root = if root.is_empty() {
                panel.root.clone()
            } else {
                PathBuf::from(root)
            };
            if root != panel.root {
                panel.set_root(root.clone(), cx);
                if panel.root != root {
                    return Err(panel
                        .error
                        .clone()
                        .unwrap_or_else(|| "暂时无法切换项目".to_owned()));
                }
            }
            panel.select_pane(pane, cx);
            Ok(())
        })
    }

    /// 仅用户点击标签后通知根历史；回调在面板 update 返回后执行，避免实体重入。
    pub fn with_navigation_handler(
        &self,
        handler: impl Fn(WorkbenchPane, &str, &mut App) + 'static,
        cx: &mut App,
    ) {
        self.entity.update(cx, |panel, _| {
            panel.navigation_handler = Some(Rc::new(handler))
        });
    }

    /// 根项目切换由宿主调用；无未保存编辑时清空展开目录和编辑器并重新读取事实状态，
    /// dirty 编辑器则保留当前项目并报告错误。
    pub fn set_root(&self, root: impl Into<PathBuf>, cx: &mut App) {
        let root = root.into();
        self.entity.update(cx, |panel, cx| panel.set_root(root, cx));
    }

    /// 关闭面板的 UI 任务，不会关闭宿主仍持有的 PTY 或开发服务。
    pub fn close(&self, cx: &mut App) {
        self.entity.update(cx, |panel, _| {
            panel.closed = true;
            panel.refresh_task.take();
            panel.blocking_task.take();
            panel.terminal_event_task.take();
            panel.terminal_subscription.take();
            panel.editor_subscriptions.take();
            panel.editor_find_cache.take();
            panel.editor_highlight_state.take();
            panel.terminal_focuses.clear();
            panel.terminal_geometry.clear();
            panel.terminal_selecting.clear();
            panel.active_terminal_id = None;
            panel.window_bounds_subscription.take();
            panel.loading = false;
        });
    }
}

/// 唯一的工作台 UI 实体。构造器使用真实服务，不提供静态快照或空操作回调。
pub struct NativeWorkbenchPanel {
    workbench: Arc<NativeWorkbench>,
    root: PathBuf,
    pane: WorkbenchPane,
    navigation_handler: Option<PaneNavigationHandler>,
    snapshot: WorkbenchSnapshot,
    expanded_dirs: HashSet<PathBuf>,
    editor: Option<EditorState>,
    editor_subscriptions: Option<EditorSubscriptions>,
    editor_find_cache: Option<EditorFindCache>,
    editor_highlight_state: Option<EditorHighlightState>,
    runtime: Option<Handle>,
    dev_events: Arc<Mutex<Vec<DevServerEvent>>>,
    loading: bool,
    closed: bool,
    error: Option<String>,
    status: Option<String>,
    refresh_task: Option<Task<()>>,
    blocking_task: Option<Task<()>>,
    /// 后台阻塞接收线程通过有界异步邮箱把 PTY 变化投影到 GPUI。
    terminal_event_task: Option<Task<()>>,
    /// 面板关闭时移除 PTY sender，唤醒没有输出事件的接收线程。
    terminal_subscription: Option<PtySubscription>,
    /// 每个终端网格拥有独立 GPUI 焦点，键盘事件直接进入 PTY。
    terminal_focuses: HashMap<String, gpui::FocusHandle>,
    /// 由 canvas 的真实布局写入，用于自动 resize 和鼠标报告坐标换算。
    terminal_geometry: HashMap<String, TerminalGeometry>,
    /// 本地文本选择拖拽中的终端 ID；启用鼠标报告时不建立本地选择。
    terminal_selecting: HashSet<String>,
    /// GPUI 的输入回调共用面板实体，由此 ID 将 IME 文本路由到当前终端。
    active_terminal_id: Option<String>,
    window_bounds_subscription: Option<Subscription>,
    terminal_dimensions: Option<(u16, u16)>,
    root_generation: u64,
    editor_revision: u64,
    editor_text_revision: u64,
    editor_find_generation: u64,
}

impl NativeWorkbenchPanel {
    /// 创建面板并立即读取当前项目的文件、Git、工作树、PTY 和开发服务状态。
    pub fn create(
        workbench: Arc<NativeWorkbench>,
        root: impl Into<PathBuf>,
        cx: &mut App,
    ) -> NativeWorkbenchPanelHandle {
        Self::new_with_runtime_option(workbench, root, Handle::try_current().ok(), cx)
    }

    /// 正式 Host 应传入自己的 Tokio Handle，确保开发服务进入 Host 的 reactor。
    pub fn new_with_runtime(
        workbench: Arc<NativeWorkbench>,
        root: impl Into<PathBuf>,
        runtime: Handle,
        cx: &mut App,
    ) -> NativeWorkbenchPanelHandle {
        Self::new_with_runtime_option(workbench, root, Some(runtime), cx)
    }

    fn new_with_runtime_option(
        workbench: Arc<NativeWorkbench>,
        root: impl Into<PathBuf>,
        runtime: Option<Handle>,
        cx: &mut App,
    ) -> NativeWorkbenchPanelHandle {
        let entity = cx.new(|_| Self {
            workbench,
            root: root.into(),
            pane: WorkbenchPane::Files,
            navigation_handler: None,
            snapshot: WorkbenchSnapshot::default(),
            expanded_dirs: HashSet::new(),
            editor: None,
            editor_subscriptions: None,
            editor_find_cache: None,
            editor_highlight_state: None,
            runtime,
            dev_events: Arc::new(Mutex::new(Vec::new())),
            loading: false,
            closed: false,
            error: None,
            status: None,
            refresh_task: None,
            blocking_task: None,
            terminal_event_task: None,
            terminal_subscription: None,
            terminal_focuses: HashMap::new(),
            terminal_geometry: HashMap::new(),
            terminal_selecting: HashSet::new(),
            active_terminal_id: None,
            window_bounds_subscription: None,
            terminal_dimensions: None,
            root_generation: 0,
            editor_revision: 0,
            editor_text_revision: 0,
            editor_find_generation: 0,
        });
        entity.update(cx, |panel, cx| {
            panel.refresh(cx);
            panel.start_terminal_events(cx);
        });
        NativeWorkbenchPanelHandle { entity }
    }

    fn start_terminal_events(&mut self, cx: &mut Context<Self>) {
        if self.terminal_event_task.is_some() {
            return;
        }
        let Ok((event_receiver, subscription)) = self.workbench.terminals.subscribe() else {
            self.error = Some("终端事件订阅数量超过上限".to_owned());
            return;
        };
        self.terminal_subscription = Some(subscription);
        // 事件只负责唤醒面板读取权威快照；高频输出时可以丢弃通知，不能让
        // GPUI 暂停期间的无界队列复制完整 PTY 流并持续增长。
        let (mut sender, mut receiver) = channel::<PtyEvent>(64);
        thread::spawn(move || {
            while let Ok(event) = event_receiver.recv() {
                match sender.try_send(event) {
                    Ok(()) => {}
                    Err(error) if error.is_full() => {}
                    Err(error) if error.is_disconnected() => break,
                    Err(_) => break,
                }
            }
        });
        let weak = cx.entity().downgrade();
        self.terminal_event_task = Some(cx.spawn(async move |_, cx| {
            while let Some(event) = receiver.next().await {
                if weak
                    .update(cx, |panel, cx| {
                        if !panel.closed {
                            panel.apply_terminal_event(event, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn apply_terminal_event(&mut self, event: PtyEvent, cx: &mut Context<Self>) {
        let id = match &event {
            PtyEvent::Output { id, .. }
            | PtyEvent::Exited { id, .. }
            | PtyEvent::Cleared { id, .. }
            | PtyEvent::Scrolled { id, .. } => id,
        };
        let Ok(Some(snapshot)) = self.workbench.terminals.snapshot(id) else {
            return;
        };
        if !snapshot.cwd.starts_with(&self.root) {
            return;
        }
        if let Some(current) = self
            .snapshot
            .terminals
            .iter_mut()
            .find(|item| item.id == id.as_str())
        {
            *current = snapshot;
        } else {
            self.snapshot.terminals.push(snapshot);
            self.snapshot
                .terminals
                .sort_by(|left, right| left.id.cmp(&right.id));
        }
        cx.notify();
    }

    fn clear_terminal(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.workbench.terminals.clear_history(id) {
            Ok(_) => {
                self.error = None;
                self.status = Some("终端屏幕已清空".to_owned());
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn close_terminal(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.workbench.terminals.close(id) {
            Ok(()) => {
                self.snapshot.terminals.retain(|terminal| terminal.id != id);
                self.terminal_focuses.remove(id);
                self.terminal_geometry.remove(id);
                self.terminal_selecting.remove(id);
                if self.active_terminal_id.as_deref() == Some(id) {
                    self.active_terminal_id = None;
                }
                self.error = None;
                self.status = Some("终端已关闭".to_owned());
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn write_terminal(&mut self, id: &str, bytes: &[u8], cx: &mut Context<Self>) {
        match self.workbench.terminals.write(id, bytes) {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn input_terminal(&mut self, id: &str, bytes: &[u8], cx: &mut Context<Self>) {
        self.terminal_selecting.remove(id);
        match self.workbench.terminals.input(id, bytes) {
            Ok(()) => {
                self.error = None;
                self.refresh_terminal_snapshot(id);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn refresh_terminal_snapshot(&mut self, id: &str) {
        let Ok(Some(snapshot)) = self.workbench.terminals.snapshot(id) else {
            return;
        };
        if let Some(current) = self
            .snapshot
            .terminals
            .iter_mut()
            .find(|terminal| terminal.id == id)
        {
            *current = snapshot;
        }
    }

    fn copy_terminal(&mut self, id: &str, cx: &mut Context<Self>) {
        match self.workbench.terminals.selection_to_string(id) {
            Ok(Some(text)) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn paste_terminal(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if text.len() > MAX_TERMINAL_INPUT_BYTES {
            self.error = Some("终端粘贴内容超过 1 MiB 限制".to_owned());
            cx.notify();
            return;
        }
        let bracketed = self
            .snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == id)
            .is_some_and(|terminal| terminal.mode & TermMode::BRACKETED_PASTE.bits() != 0);
        self.input_terminal(id, &terminal_paste_bytes(&text, bracketed), cx);
    }

    fn activate_terminal(&mut self, id: &str) {
        self.active_terminal_id = Some(id.to_owned());
    }

    fn terminal_mouse_reporting(&self, id: &str) -> bool {
        self.snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == id)
            .is_some_and(|terminal| {
                TermMode::from_bits_retain(terminal.mode).intersects(
                    TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION,
                )
            })
    }

    fn terminal_cell_at(&self, id: &str, position: Point<Pixels>) -> Option<(CellPoint, Side)> {
        let geometry = self.terminal_geometry.get(id).copied()?;
        let snapshot = self
            .snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == id)?;
        let cell_width = f32::from(geometry.cell.width);
        let cell_height = f32::from(geometry.cell.height);
        if cell_width <= 0.0 || cell_height <= 0.0 || snapshot.cols == 0 || snapshot.rows == 0 {
            return None;
        }
        let x = (f32::from(position.x - geometry.origin.x) / cell_width).max(0.0);
        let y = (f32::from(position.y - geometry.origin.y) / cell_height).max(0.0);
        let column = (x.floor() as usize).min(usize::from(snapshot.cols).saturating_sub(1));
        let row = (y.floor() as usize).min(usize::from(snapshot.rows).saturating_sub(1));
        let display_offset = i32::try_from(snapshot.display_offset).ok()?;
        let row = i32::try_from(row).ok()?.saturating_sub(display_offset);
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        Some((CellPoint::new(Line(row), Column(column)), side))
    }

    fn start_terminal_selection(
        &mut self,
        id: &str,
        kind: SelectionType,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some((point, side)) = self.terminal_cell_at(id, position) else {
            return;
        };
        match self
            .workbench
            .terminals
            .start_selection(id, kind, point, side)
        {
            Ok(()) => {
                self.activate_terminal(id);
                self.terminal_selecting.insert(id.to_owned());
                self.refresh_terminal_snapshot(id);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn update_terminal_selection(
        &mut self,
        id: &str,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some((point, side)) = self.terminal_cell_at(id, position) else {
            return;
        };
        match self.workbench.terminals.update_selection(id, point, side) {
            Ok(()) => self.refresh_terminal_snapshot(id),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn update_terminal_layout(
        &mut self,
        id: &str,
        cols: u16,
        rows: u16,
        origin: Point<Pixels>,
        cell: Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.terminal_geometry
            .insert(id.to_owned(), TerminalGeometry { origin, cell });
        let current = self
            .snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == id)
            .map(|terminal| (terminal.cols, terminal.rows));
        if current == Some((cols, rows)) {
            return;
        }
        if let Err(error) = self.workbench.terminals.resize(id, cols, rows) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        if let Ok(Some(snapshot)) = self.workbench.terminals.snapshot(id)
            && let Some(current) = self
                .snapshot
                .terminals
                .iter_mut()
                .find(|terminal| terminal.id == id)
        {
            *current = snapshot;
        }
        cx.notify();
    }

    fn scroll_terminal(&mut self, id: &str, lines: i32, cx: &mut Context<Self>) {
        if lines == 0 {
            return;
        }
        let lines = lines.clamp(-256, 256);
        let mode = self
            .snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == id)
            .map(|terminal| terminal.mode)
            .unwrap_or_default();
        if mode & TermMode::ALT_SCREEN.bits() != 0 && mode & TermMode::ALTERNATE_SCROLL.bits() != 0
        {
            let app_cursor = mode & TermMode::APP_CURSOR.bits() != 0;
            let (up, down) = if app_cursor {
                (b"\x1bOA".as_slice(), b"\x1bOB".as_slice())
            } else {
                (b"\x1b[A".as_slice(), b"\x1b[B".as_slice())
            };
            let bytes = if lines > 0 { up } else { down };
            let count = lines.unsigned_abs().min(256) as usize;
            self.input_terminal(id, &bytes.repeat(count), cx);
        } else if let Err(error) = self.workbench.terminals.scroll(id, lines) {
            self.error = Some(error);
            cx.notify();
        }
    }

    fn send_mouse_event(
        &mut self,
        id: &str,
        position: Point<Pixels>,
        report: TerminalMouseReport,
        cx: &mut Context<Self>,
    ) {
        let mode = self
            .snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == id)
            .map(|terminal| terminal.mode)
            .unwrap_or_default();
        let Some(geometry) = self.terminal_geometry.get(id).copied() else {
            return;
        };
        let x = (f32::from(position.x - geometry.origin.x) / f32::from(geometry.cell.width))
            .floor()
            .max(0.0) as usize;
        let y = (f32::from(position.y - geometry.origin.y) / f32::from(geometry.cell.height))
            .floor()
            .max(0.0) as usize;
        let Some(bytes) = terminal_mouse_bytes(mode, x, y, report) else {
            return;
        };
        self.write_terminal(id, &bytes, cx);
    }

    fn ensure_terminal_resize_observer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.window_bounds_subscription.is_none() {
            self.window_bounds_subscription =
                Some(cx.observe_window_bounds(window, |panel, window, cx| {
                    panel.resize_terminals(window, cx)
                }));
        }
        self.resize_terminals(window, cx);
    }

    fn resize_terminals(&mut self, window: &Window, cx: &mut Context<Self>) {
        let bounds = window.bounds();
        let width = f32::from(bounds.size.width).max(320.0);
        let height = f32::from(bounds.size.height).max(240.0);
        let cols = (width / 8.0).floor().clamp(40.0, 240.0) as u16;
        let rows = ((height / 16.0).floor() - 8.0).clamp(8.0, 80.0) as u16;
        let dimensions = (cols, rows);
        if self.terminal_dimensions == Some(dimensions) {
            return;
        }
        self.terminal_dimensions = Some(dimensions);
        let ids = self
            .snapshot
            .terminals
            .iter()
            .map(|terminal| terminal.id.clone())
            .collect::<Vec<_>>();
        let mut resize_error = None;
        for id in ids {
            if let Err(error) = self.workbench.terminals.resize(&id, cols, rows) {
                resize_error = Some(error);
            }
        }
        if let Some(error) = resize_error {
            self.error = Some(error);
        }
        if !self.snapshot.terminals.is_empty() {
            if let Ok(snapshots) = self.workbench.terminals.snapshots() {
                self.snapshot.terminals = snapshots
                    .into_iter()
                    .filter(|terminal| terminal.cwd.starts_with(&self.root))
                    .collect();
            }
            cx.notify();
        }
    }

    /// 所有文件、Git 和工作树操作都在 Tokio blocking 池（测试无 Tokio 时为独立线程）执行；
    /// 回到 GPUI 前再校验工作区 generation，避免旧项目结果覆盖新项目。generation 检查
    /// 必须放在清理当前任务字段之前，否则旧回调可能把新根目录的任务标记为已结束。
    fn spawn_blocking<T, Work, Apply>(
        &mut self,
        generation: u64,
        work: Work,
        apply: Apply,
        cx: &mut Context<Self>,
    ) where
        T: Send + 'static,
        Work: FnOnce() -> T + Send + 'static,
        Apply: FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    {
        // Git、文件和工作树副作用必须串行；丢弃 GPUI Task 不能取消已经启动的线程。
        if self.loading || self.refresh_task.is_some() || self.blocking_task.is_some() {
            self.error = Some("工作台仍在处理上一项操作，请稍后重试".to_owned());
            cx.notify();
            return;
        }
        self.loading = true;
        // 写操作开始时丢弃尚未回来的旧快照；否则旧回调可能清理新任务字段。
        self.refresh_task.take();
        self.blocking_task.take();
        let (sender, receiver) = oneshot::channel();
        if let Some(runtime) = self.runtime.clone() {
            runtime.spawn_blocking(move || {
                let _ = sender.send(work());
            });
        } else {
            thread::spawn(move || {
                let _ = sender.send(work());
            });
        }
        let weak = cx.entity().downgrade();
        self.blocking_task = Some(cx.spawn(async move |_, cx| {
            let value = match receiver.await {
                Ok(value) => value,
                Err(_) => {
                    let _ = weak.update(cx, |panel, cx| {
                        if panel.closed || panel.root_generation != generation {
                            return;
                        }
                        panel.blocking_task = None;
                        panel.loading = false;
                        panel.error = Some("操作未完成，请重试".to_owned());
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = weak.update(cx, |panel, cx| {
                if panel.closed || panel.root_generation != generation {
                    return;
                }
                panel.blocking_task = None;
                panel.loading = false;
                apply(panel, value, cx);
            });
        }));
    }

    fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.editor_is_dirty() {
            self.error = Some("当前文件有未保存修改，请先保存后再切换项目".to_owned());
            cx.notify();
            return;
        }
        if self.blocking_task.is_some() {
            self.error = Some("当前操作尚未完成，请稍后再切换项目".to_owned());
            cx.notify();
            return;
        }
        // 切换根目录时取消旧快照读取，避免旧项目结果覆盖新项目状态。
        self.refresh_task.take();
        self.blocking_task.take();
        self.root_generation = self.root_generation.saturating_add(1);
        self.loading = false;
        self.root = root;
        self.expanded_dirs.clear();
        self.editor = None;
        self.editor_subscriptions = None;
        self.editor_find_cache = None;
        self.editor_highlight_state = None;
        self.editor_text_revision = 0;
        self.editor_find_generation = 0;
        self.snapshot = WorkbenchSnapshot::default();
        self.terminal_selecting.clear();
        self.active_terminal_id = None;
        self.status = None;
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.closed || self.loading {
            return;
        }
        self.loading = true;
        self.error = None;
        let workbench = Arc::clone(&self.workbench);
        let root = self.root.clone();
        let expanded_dirs = self.expanded_dirs.clone();
        let diff_path = self.snapshot.diff_path.clone();
        let generation = self.root_generation;
        let weak = cx.entity().downgrade();
        let events = Arc::clone(&self.dev_events);
        let (sender, receiver) = oneshot::channel();
        let load_workbench = Arc::clone(&workbench);
        let load_root = root.clone();
        let load_expanded_dirs = expanded_dirs.clone();
        let load_diff_path = diff_path.clone();
        let load = move || {
            load_snapshot(
                &load_workbench,
                &load_root,
                &load_expanded_dirs,
                load_diff_path.as_deref(),
            )
        };
        if let Some(runtime) = self.runtime.clone() {
            runtime.spawn_blocking(move || {
                let _ = sender.send(load());
            });
        } else {
            thread::spawn(move || {
                let _ = sender.send(load());
            });
        }
        self.refresh_task = Some(cx.spawn(async move |_, cx| {
            let result = match receiver.await {
                Ok(result) => result,
                Err(_) => {
                    let _ = weak.update(cx, |panel, cx| {
                        if panel.closed || panel.root_generation != generation {
                            return;
                        }
                        panel.refresh_task = None;
                        panel.loading = false;
                        panel.error = Some("工作台状态读取失败，请刷新重试".to_owned());
                        cx.notify();
                    });
                    return;
                }
            };
            let event_status = events.lock().ok().and_then(|mut events| {
                events.pop().map(|event| match event {
                    DevServerEvent::Upserted { server } => {
                        format!("开发服务已启动：{} (pid {})", server.project_id, server.pid)
                    }
                    DevServerEvent::Removed { project_id, reason } => {
                        format!("开发服务已结束：{project_id} ({reason})")
                    }
                })
            });
            let _ = weak.update(cx, |panel, cx| {
                if panel.closed || panel.root_generation != generation {
                    return;
                }
                panel.loading = false;
                panel.refresh_task = None;
                if let Some(status) = event_status {
                    panel.status = Some(status);
                }
                match result {
                    Ok(snapshot) => {
                        panel.root = snapshot.root.clone();
                        panel.snapshot = snapshot;
                        // 初始快照可能在首次 render 的尺寸探测之前完成；让下一帧把
                        // 已加载的 PTY 调整到当前窗口尺寸，而不是停留在默认 100x30。
                        panel.terminal_dimensions = None;
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn finish(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        match result {
            Ok(status) => {
                self.error = None;
                self.status = (!status.is_empty()).then_some(status);
                self.refresh(cx);
            }
            Err(error) => {
                self.loading = false;
                self.status = None;
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn editor_is_dirty(&self) -> bool {
        self.editor.is_some() && self.editor_text_revision != 0
    }

    fn select_pane(&mut self, pane: WorkbenchPane, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.editor_subscriptions.take();
        self.pane = pane;
        if matches!(pane, WorkbenchPane::Terminal)
            && let Err(error) = self.workbench.sync_terminal_settings()
        {
            self.error = Some(error);
        }
        self.refresh(cx);
        cx.notify();
    }

    fn toggle_directory(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.expanded_dirs.insert(path.clone()) {
            self.expanded_dirs.remove(&path);
        }
        self.refresh(cx);
    }

    fn open_file(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.editor_is_dirty() {
            self.error = Some("当前文件有未保存修改，请先保存后再打开其他文件".to_owned());
            cx.notify();
            return;
        }
        let Some(relative) = path.strip_prefix(&self.root).ok().map(PathBuf::from) else {
            self.error = Some("文件不属于当前项目根".to_owned());
            cx.notify();
            return;
        };
        let root = self.root.clone();
        let workbench = Arc::clone(&self.workbench);
        let generation = self.root_generation;
        let apply_relative = relative.clone();
        let expected_editor_revision = self.editor.as_ref().map(|editor| editor.revision);
        let expected_text_revision = self.editor_text_revision;
        self.spawn_blocking(
            generation,
            move || workbench.files.read_text(&root, &relative),
            move |panel, result, cx| {
                match result {
                    Ok(document) => {
                        if panel.editor.as_ref().map(|editor| editor.revision)
                            != expected_editor_revision
                            || panel.editor_text_revision != expected_text_revision
                        {
                            panel.error = Some(
                                "编辑器在切换期间产生了未保存修改，已保留当前编辑内容".to_owned(),
                            );
                            cx.notify();
                            return;
                        }
                        panel.editor_revision = panel.editor_revision.saturating_add(1);
                        panel.editor_text_revision = 0;
                        panel.editor_find_cache = None;
                        panel.editor_highlight_state = None;
                        panel.editor = Some(EditorState {
                            relative: apply_relative,
                            path: document.path,
                            text: Arc::from(document.text),
                            modified_unix_ms: document.modified_unix_ms,
                            revision: panel.editor_revision,
                        });
                        panel.error = None;
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            },
            cx,
        );
    }

    fn save_file(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else {
            self.error = Some("尚未打开 UTF-8 文本文件".to_owned());
            cx.notify();
            return;
        };
        let root = self.root.clone();
        let workbench = Arc::clone(&self.workbench);
        let relative = editor.relative.clone();
        let work_relative = relative.clone();
        let modified_unix_ms = editor.modified_unix_ms;
        let expected_editor_revision = editor.revision;
        let expected_text_revision = self.editor_text_revision;
        let expected_relative = relative.clone();
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || {
                workbench
                    .files
                    .write_text(&root, &work_relative, &text, modified_unix_ms)
            },
            move |panel, result, cx| {
                match result {
                    Ok(document) => {
                        let same_editor = panel.editor.as_ref().is_some_and(|editor| {
                            editor.revision == expected_editor_revision
                                && editor.relative == expected_relative
                        });
                        if !same_editor {
                            panel.error =
                                Some("文件已保存，但编辑器已切换；当前编辑内容未被覆盖".to_owned());
                            panel.refresh(cx);
                            cx.notify();
                            return;
                        }
                        if panel.editor_text_revision == expected_text_revision {
                            panel.editor_revision = panel.editor_revision.saturating_add(1);
                            panel.editor_text_revision = 0;
                            panel.editor_find_cache = None;
                            panel.editor_highlight_state = None;
                            panel.editor = Some(EditorState {
                                relative,
                                path: document.path,
                                text: Arc::from(document.text),
                                modified_unix_ms: document.modified_unix_ms,
                                revision: panel.editor_revision,
                            });
                        } else if let Some(editor) = panel.editor.as_mut() {
                            // 保存期间继续输入时，只更新磁盘版本时间戳；保留当前
                            // CodeEditor Entity 和未保存正文，避免新输入被重置。
                            editor.modified_unix_ms = document.modified_unix_ms;
                        }
                        panel.error = None;
                        panel.status = Some(if panel.editor_text_revision == 0 {
                            "文件已保存".to_owned()
                        } else {
                            "已保存当前版本，仍有新的未保存修改".to_owned()
                        });
                        panel.refresh(cx);
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            },
            cx,
        );
    }

    fn open_external_editor(&mut self, editor_id: String, path: PathBuf, cx: &mut Context<Self>) {
        let Some(editor) = self
            .snapshot
            .editors
            .iter()
            .find(|editor| editor.id == editor_id)
        else {
            self.error = Some("外部编辑器未被发现，未启动".to_owned());
            cx.notify();
            return;
        };
        let editor_name = editor.name.clone();
        let workbench = Arc::clone(&self.workbench);
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || {
                workbench
                    .editors
                    .open(&editor_id, &path)
                    .map(|result| format!("已在 {} 中打开 {}", editor_name, result.path.display()))
            },
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn search(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            self.snapshot.search_count = 0;
            self.snapshot.search_results.clear();
            self.snapshot.search_truncated = false;
            self.status = None;
            self.error = None;
            cx.notify();
            return;
        }
        let workbench = Arc::clone(&self.workbench);
        let input = FileSearchInput {
            root: self.root.clone(),
            query,
            limit: Some(100),
            include_files: Some(true),
        };
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || workbench.files.search_files(input),
            move |panel, result, cx| {
                match result {
                    Ok(result) => {
                        panel.snapshot.search_count = result.entries.len();
                        panel.snapshot.search_truncated = result.truncated;
                        panel.snapshot.search_results = result.entries;
                        panel.error = None;
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            },
            cx,
        );
    }

    fn search_content(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            self.snapshot.content_results.clear();
            self.snapshot.content_truncated = false;
            self.error = None;
            cx.notify();
            return;
        }
        let workbench = Arc::clone(&self.workbench);
        let input = ContentSearchInput {
            root: self.root.clone(),
            query,
            limit: Some(100),
        };
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || workbench.files.search_content(input),
            move |panel, result, cx| {
                match result {
                    Ok(result) => {
                        panel.snapshot.content_results = result.matches;
                        panel.snapshot.content_truncated = result.truncated;
                        panel.error = None;
                    }
                    Err(error) => panel.error = Some(error),
                }
                cx.notify();
            },
            cx,
        );
    }

    fn read_diff(&mut self, path: String, cx: &mut Context<Self>) {
        let path = path.trim().to_owned();
        let path = (!path.is_empty()).then_some(path);
        let workbench = Arc::clone(&self.workbench);
        let root = self.root.clone();
        let options = DiffOptions {
            staged: false,
            path: path.clone(),
            context_lines: Some(3),
        };
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || workbench.diff.read(&root, options),
            move |panel, result, cx| {
                match result {
                    Ok(diff) => {
                        panel.snapshot.diff = Some(diff);
                        panel.snapshot.diff_path = path;
                        panel.error = None;
                        panel.status = Some("Diff 已加载".to_owned());
                    }
                    Err(error) => {
                        panel.status = None;
                        panel.error = Some(error);
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    fn save_review_comments(&mut self, comments: Vec<ReviewComment>, cx: &mut Context<Self>) {
        let workbench = Arc::clone(&self.workbench);
        let root = self.root.clone();
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || {
                workbench
                    .diff
                    .save_comments(&root, comments)
                    .map(|_| "review 标注已保存".to_owned())
            },
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn delete_review_comment(&mut self, id: String, cx: &mut Context<Self>) {
        if !self
            .snapshot
            .comments
            .iter()
            .any(|comment| comment.id == id)
        {
            self.error = Some("review 标注已不存在".to_owned());
            cx.notify();
            return;
        }
        let comments = self
            .snapshot
            .comments
            .iter()
            .filter(|comment| comment.id != id)
            .cloned()
            .collect();
        self.save_review_comments(comments, cx);
    }

    fn git_action(&mut self, action: GitAction, cx: &mut Context<Self>) {
        let workbench = Arc::clone(&self.workbench);
        let root = self.root.clone();
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || workbench.git.action(&root, action),
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn create_worktree(
        &mut self,
        reference: String,
        target: String,
        branch: String,
        copy_changes_from: String,
        cx: &mut Context<Self>,
    ) {
        let reference = reference.trim().to_owned();
        let target = target.trim().to_owned();
        let branch = branch.trim().to_owned();
        let copy_changes_from = copy_changes_from.trim().to_owned();
        if reference.is_empty() {
            self.error = Some("工作树引用不能为空".to_owned());
            cx.notify();
            return;
        }
        let workbench = Arc::clone(&self.workbench);
        let generation = self.root_generation;
        let request = WorktreeRequest {
            root: self.root.clone(),
            reference,
            path: (!target.is_empty()).then_some(PathBuf::from(target)),
            new_branch: (!branch.is_empty()).then_some(branch),
            copy_changes_from: (!copy_changes_from.is_empty())
                .then_some(PathBuf::from(copy_changes_from)),
            checkout_branch: false,
        };
        self.spawn_blocking(
            generation,
            move || {
                workbench
                    .worktrees
                    .create(request)
                    .map(|result| format!("工作树已创建：{}", result.path.display()))
            },
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn remove_worktree(&mut self, target: String, force: bool, cx: &mut Context<Self>) {
        let target = target.trim().to_owned();
        if target.is_empty() {
            self.error = Some("移除目标路径不能为空".to_owned());
            cx.notify();
            return;
        }
        let workbench = Arc::clone(&self.workbench);
        let root = self.root.clone();
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || {
                workbench
                    .worktrees
                    .remove(&root, Path::new(&target), force)
                    .map(|_| "工作树已移除".to_owned())
            },
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn archive_worktree(&mut self, target: String, session_id: String, cx: &mut Context<Self>) {
        let target = target.trim().to_owned();
        let session_id = session_id.trim().to_owned();
        if target.is_empty() || session_id.is_empty() {
            self.error = Some("归档目标和会话标识都不能为空".to_owned());
            cx.notify();
            return;
        }
        let workbench = Arc::clone(&self.workbench);
        let root = self.root.clone();
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || {
                workbench
                    .worktrees
                    .archive(&root, Path::new(&target), &session_id)
                    .map(|receipt| format!("工作树已归档，回执：{}", receipt.display()))
            },
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn handoff_worktree(
        &mut self,
        source: String,
        target: String,
        branch: String,
        cx: &mut Context<Self>,
    ) {
        let source = source.trim().to_owned();
        let target = target.trim().to_owned();
        let branch = branch.trim().to_owned();
        if source.is_empty() || target.is_empty() {
            self.error = Some("交接源和目标路径都不能为空".to_owned());
            cx.notify();
            return;
        }
        let workbench = Arc::clone(&self.workbench);
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || {
                workbench
                    .worktrees
                    .handoff(
                        Path::new(&source),
                        Path::new(&target),
                        (!branch.is_empty()).then_some(branch.as_str()),
                    )
                    .map(|path| format!("工作树已交接：{}", path.display()))
            },
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn start_dev_server(
        &mut self,
        project_id: String,
        cwd: String,
        command: String,
        cx: &mut Context<Self>,
    ) {
        let project_id = project_id.trim().to_owned();
        let cwd = cwd.trim().to_owned();
        let command = command.trim().to_owned();
        if project_id.is_empty() || cwd.is_empty() || command.is_empty() {
            self.error = Some("项目标识、工作目录和命令都不能为空".to_owned());
            cx.notify();
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            self.error = Some("当前无法启动开发服务，请稍后重试".to_owned());
            cx.notify();
            return;
        };
        let events = Arc::clone(&self.dev_events);
        let sink = Arc::new(move |event: DevServerEvent| {
            if let Ok(mut pending) = events.lock() {
                pending.push(event);
                if pending.len() > 32 {
                    pending.remove(0);
                }
            }
        });
        let input = DevServerInput {
            project_id: project_id.clone(),
            project_root: self.root.clone(),
            cwd: PathBuf::from(cwd),
            command,
            env: None,
        };
        let manager = Arc::clone(&self.workbench.dev_servers);
        let generation = self.root_generation;
        self.spawn_blocking(
            generation,
            move || {
                manager.start(&runtime, input, sink).map(|server| {
                    format!("开发服务已启动：{} (pid {})", server.project_id, server.pid)
                })
            },
            move |panel, result, cx| panel.finish(result, cx),
            cx,
        );
    }

    fn stop_dev_server(&mut self, project_id: String, cx: &mut Context<Self>) {
        if self.loading || self.refresh_task.is_some() || self.blocking_task.is_some() {
            self.error = Some("工作台仍在处理上一项操作，请稍后重试".to_owned());
            cx.notify();
            return;
        }
        let Some(runtime) = self.runtime.clone() else {
            self.error = Some("当前无法停止开发服务，请稍后重试".to_owned());
            cx.notify();
            return;
        };
        let manager = Arc::clone(&self.workbench.dev_servers);
        let weak = cx.entity().downgrade();
        let generation = self.root_generation;
        self.loading = true;
        cx.spawn(async move |_, cx| {
            let stop_task = runtime.spawn(async move { manager.stop(&project_id).await });
            let result = match stop_task.await {
                Ok(result) => result.map(|stopped| {
                    if stopped {
                        "开发服务已停止"
                    } else {
                        "开发服务不存在"
                    }
                    .to_owned()
                }),
                Err(error) => Err(format!("停止开发服务失败：{error}")),
            };
            let _ = weak.update(cx, |panel, cx| {
                // 停止请求仍需完成，但关闭后的实体不再接收后台结果或触发刷新。
                if panel.closed || panel.root_generation != generation {
                    return;
                }
                panel.loading = false;
                panel.finish(result, cx);
            });
        })
        .detach();
    }

    fn tabs(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let entity = cx.entity();
        let active = self.pane;
        let navigation_handler = self.navigation_handler.clone();
        WorkbenchPane::ALL
            .into_iter()
            .map(move |pane| {
                let entity = entity.clone();
                let navigation_handler = navigation_handler.clone();
                IconButton::new(
                    format!("native-workbench-live-pane:{}", pane.label()),
                    pane.icon(),
                )
                .variant(if pane == active {
                    ButtonVariant::Subtle
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Sm)
                .tooltip(pane.label())
                .on_click(move |_, _, cx| {
                    let selected_root = entity.update(cx, |panel, cx| {
                        if panel.closed {
                            return None;
                        }
                        panel.select_pane(pane, cx);
                        Some(panel.root.to_string_lossy().into_owned())
                    });
                    if let Some(root) = selected_root
                        && let Some(handler) = navigation_handler.as_ref()
                    {
                        handler(pane, &root, cx);
                    }
                })
                .into_any_element()
            })
            .collect()
    }

    fn root_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let value = self.root.display().to_string();
        let field = window.use_keyed_state(
            format!("native-workbench-root:{}", self.root.display()),
            cx,
            move |window, cx| {
                let mut field = TextInput::new(window, cx).placeholder("项目绝对路径");
                field.set_text(value, cx);
                field
            },
        );
        let entity = cx.entity();
        let apply_field = field.clone();
        let navigation_handler = self.navigation_handler.clone();
        div()
            .flex()
            .gap_1()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&field).size(ControlSize::Sm)),
            )
            .child(
                Button::new("native-workbench-root-apply", "切换项目")
                    .icon(IconName::FolderOpen)
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        let root = PathBuf::from(apply_field.read(cx).text());
                        let selected = entity.update(cx, |panel, cx| {
                            panel.set_root(root.clone(), cx);
                            // 被未保存编辑或在途操作阻挡时，不把请求路径写入根历史。
                            (panel.root == root)
                                .then(|| (panel.pane, panel.root.to_string_lossy().into_owned()))
                        });
                        if let Some((pane, root)) = selected
                            && let Some(handler) = navigation_handler.as_ref()
                        {
                            handler(pane, &root, cx);
                        }
                    }),
            )
            .into_any_element()
    }

    fn files_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        if self.editor.is_none() {
            self.editor_subscriptions.take();
            self.editor_find_cache.take();
            self.editor_highlight_state.take();
        }
        let mut body = div().flex().flex_col().gap_1().flex_1().min_h_0();
        if self.snapshot.files_truncated {
            let message = "文件列表达到总量上限，展开更多目录以继续查看";
            // 截断是实际文件快照状态；同步暴露给屏幕阅读器，避免有界列表隐藏这条提示。
            body = body.child(
                div()
                    .id("native-workbench-files-truncated")
                    .role(Role::Status)
                    .aria_label(message)
                    .aria_value(message)
                    .text_color(cx.theme().colors.warning)
                    .child(message),
            );
        }
        for entry in self.snapshot.files.clone() {
            let relative = entry
                .path
                .strip_prefix(&self.root)
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| entry.name.clone());
            let path = entry.path.clone();
            let is_dir = entry.kind == FileKind::Directory;
            let expanded = self.expanded_dirs.contains(&path);
            let label = if is_dir {
                format!("{} {}", if expanded { "收起" } else { "展开" }, relative)
            } else {
                format!("打开 {}", relative)
            };
            let entity = entity.clone();
            body = body.child(
                // 列表行保持左对齐；不能让纵向 flex 的 stretch 把按钮文字放到页面中央。
                div().flex().w_full().child(
                    Button::new(format!("native-workbench-file:{}", relative), label)
                        .icon(if is_dir {
                            if expanded {
                                IconName::FolderOpen
                            } else {
                                IconName::Folder
                            }
                        } else {
                            IconName::File
                        })
                        .variant(ButtonVariant::Ghost)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |panel, cx| {
                                if is_dir {
                                    panel.toggle_directory(path.clone(), cx);
                                } else {
                                    panel.open_file(path.clone(), cx);
                                }
                            });
                        }),
                ),
            );
        }
        if let Some(path) = self.editor.as_ref().map(|editor| editor.path.clone())
            && !self.snapshot.editors.is_empty()
        {
            let mut external = div()
                .flex()
                .items_center()
                .gap_1()
                .text_color(cx.theme().colors.fg_muted)
                .child("外部编辑器");
            for editor in self.snapshot.editors.clone() {
                let editor_id = editor.id.clone();
                let entity = entity.clone();
                let path = path.clone();
                external = external.child(
                    Button::new(
                        format!("native-workbench-editor:{}", editor.id),
                        format!("打开 {}", editor.name),
                    )
                    .icon(IconName::ExternalLink)
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        entity.update(cx, |panel, cx| {
                            panel.open_external_editor(editor_id.clone(), path.clone(), cx)
                        });
                    }),
                );
            }
            body = body.child(external);
        }
        if let Some((editor_path, editor_relative, initial, revision)) =
            self.editor.as_ref().map(|editor| {
                (
                    editor.path.clone(),
                    editor.relative.clone(),
                    Arc::clone(&editor.text),
                    editor.revision,
                )
            })
        {
            let key = code_editor_state_key(&editor_path, revision);
            let language = code_editor_language(&editor_path);
            let show_line_numbers = cx.theme().code.show_line_numbers;
            let wrap_long_lines = cx.theme().code.wrap_long_lines;
            let editor_settings = (show_line_numbers, wrap_long_lines);
            let code_editor = window.use_keyed_state(key.clone(), cx, move |window, cx| {
                CodeEditor::new(initial.as_ref(), window, cx)
                    .language(language)
                    .line_numbers(if show_line_numbers {
                        LineNumbers::Absolute
                    } else {
                        LineNumbers::Hidden
                    })
                    .soft_wrap(wrap_long_lines)
                    .sticky_scroll()
            });
            let applied_editor_settings =
                window.use_keyed_state(format!("{key}:code-render-settings"), cx, |_, _| {
                    editor_settings
                });
            if *applied_editor_settings.read(cx) != editor_settings {
                code_editor.update(cx, |editor, cx| {
                    editor.set_line_numbers(
                        if show_line_numbers {
                            LineNumbers::Absolute
                        } else {
                            LineNumbers::Hidden
                        },
                        cx,
                    );
                    editor.set_soft_wrap(wrap_long_lines, cx);
                });
                applied_editor_settings.update(cx, |settings, _| *settings = editor_settings);
            }

            let find_field = window.use_keyed_state(format!("{key}:find"), cx, |window, cx| {
                TextInput::new(window, cx).placeholder("查找")
            });
            let replacement_field =
                window.use_keyed_state(format!("{key}:replace"), cx, |window, cx| {
                    TextInput::new(window, cx).placeholder("替换为")
                });
            let find_options = window.use_keyed_state(format!("{key}:find-options"), cx, |_, _| {
                FindOptions::default()
            });
            let find_current =
                window.use_keyed_state(format!("{key}:find-current"), cx, |_, _| None::<usize>);
            let find_open = window.use_keyed_state(format!("{key}:find-open"), cx, |_, _| false);
            let replace_open =
                window.use_keyed_state(format!("{key}:replace-open"), cx, |_, _| false);

            if self
                .editor_subscriptions
                .as_ref()
                .is_none_or(|subscriptions| subscriptions.key != key)
            {
                let subscriptions = vec![
                    cx.subscribe(&code_editor, |panel, _, event: &EditorEvent, cx| {
                        if *event == EditorEvent::Changed {
                            panel.editor_text_revision =
                                panel.editor_text_revision.saturating_add(1);
                            panel.editor_find_cache = None;
                            panel.editor_highlight_state = None;
                            cx.notify();
                        }
                    }),
                    cx.subscribe(&find_field, |_, _, event: &InputEvent, cx| {
                        if *event == InputEvent::Changed {
                            cx.notify();
                        }
                    }),
                    cx.observe(&find_options, |_, _, cx| cx.notify()),
                    cx.observe(&find_current, |_, _, cx| cx.notify()),
                    cx.observe(&find_open, |_, _, cx| cx.notify()),
                    cx.observe(&replace_open, |_, _, cx| cx.notify()),
                ];
                self.editor_subscriptions = Some(EditorSubscriptions {
                    key: key.clone(),
                    _subscriptions: subscriptions,
                });
            }

            let query = find_field.read(cx).text().to_owned();
            let options = *find_options.read(cx);
            let find_is_open = *find_open.read(cx);
            let cache_matches = self.editor_find_cache.as_ref().is_some_and(|cache| {
                cache.key == key
                    && cache.text_revision == self.editor_text_revision
                    && cache.query == query
                    && cache.options == options
            });
            if find_is_open && !cache_matches {
                let result = {
                    let text = code_editor.read(cx).text();
                    find_all(text, &query, options)
                };
                let (matches, error) = match result {
                    Ok(matches) => (matches, None),
                    Err(error) => (Vec::new(), Some(error)),
                };
                self.editor_find_generation = self.editor_find_generation.saturating_add(1);
                self.editor_find_cache = Some(EditorFindCache {
                    key: key.clone(),
                    text_revision: self.editor_text_revision,
                    query: query.clone(),
                    options,
                    matches,
                    error,
                });
            }
            let empty_find_matches: &[Range<usize>] = &[];
            let (matches, find_error): (&[Range<usize>], Option<String>) = if find_is_open {
                self.editor_find_cache
                    .as_ref()
                    .filter(|cache| {
                        cache.key == key
                            && cache.text_revision == self.editor_text_revision
                            && cache.query == query
                            && cache.options == options
                    })
                    .map(|cache| (cache.matches.as_slice(), cache.error.clone()))
                    .unwrap_or((empty_find_matches, None))
            } else {
                (empty_find_matches, None)
            };
            let current = find_current
                .read(cx)
                .and_then(|index| matches.get(index).map(|_| index));
            let colors = cx.theme().colors.clone();
            let highlight_state = EditorHighlightState {
                key: key.clone(),
                open: find_is_open,
                current,
                generation: self.editor_find_generation,
                warning: hsla_bits(colors.warning),
            };
            if self.editor_highlight_state.as_ref() != Some(&highlight_state) {
                code_editor.update(cx, |editor, cx| {
                    editor.set_backgrounds(
                        if find_is_open {
                            matches
                                .iter()
                                .enumerate()
                                .map(|(index, range)| {
                                    let color = if Some(index) == current {
                                        colors.warning.opacity(0.45)
                                    } else {
                                        colors.warning.opacity(0.2)
                                    };
                                    (range.clone(), color)
                                })
                                .collect()
                        } else {
                            Vec::new()
                        },
                        cx,
                    );
                });
                self.editor_highlight_state = Some(highlight_state);
            }

            let find_widget = if find_is_open {
                let mut widget = FindWidget::new(
                    format!("{key}:find-widget"),
                    &find_field,
                    current,
                    matches.len(),
                )
                .options(options)
                .on_options({
                    let find_options = find_options.clone();
                    move |next, _, cx| {
                        find_options.update(cx, |options, cx| {
                            *options = next;
                            cx.notify();
                        });
                    }
                })
                .on_step({
                    let code_editor = code_editor.clone();
                    let find_field = find_field.clone();
                    let find_options = find_options.clone();
                    let find_current = find_current.clone();
                    move |forward, _, cx| {
                        let result = {
                            let text = code_editor.read(cx).text();
                            let query = find_field.read(cx).text();
                            find_all(text, query, *find_options.read(cx))
                        };
                        let Ok(matches) = result else {
                            return;
                        };
                        if matches.is_empty() {
                            return;
                        }
                        let current = *find_current.read(cx);
                        let index = match current {
                            Some(index) if forward => (index + 1) % matches.len(),
                            Some(index) => (index + matches.len() - 1) % matches.len(),
                            None if forward => 0,
                            None => matches.len() - 1,
                        };
                        find_current.update(cx, |current, cx| {
                            *current = Some(index);
                            cx.notify();
                        });
                        code_editor.update(cx, |editor, cx| {
                            editor.select([matches[index].clone()], cx);
                        });
                    }
                })
                .on_toggle_replace({
                    let replace_open = replace_open.clone();
                    move |open, _, cx| {
                        replace_open.update(cx, |current, cx| {
                            *current = open;
                            cx.notify();
                        });
                    }
                })
                .on_replace({
                    let code_editor = code_editor.clone();
                    let find_field = find_field.clone();
                    let replacement_field = replacement_field.clone();
                    let find_options = find_options.clone();
                    let find_current = find_current.clone();
                    move |all, _, cx| {
                        let result = {
                            let text = code_editor.read(cx).text();
                            let query = find_field.read(cx).text();
                            find_all(text, query, *find_options.read(cx))
                        };
                        let Ok(matches) = result else {
                            return;
                        };
                        let replacement = replacement_field.read(cx).text().to_owned();
                        let edits = if all {
                            matches
                                .into_iter()
                                .map(|range| (range, replacement.clone()))
                                .collect()
                        } else {
                            let Some(index) = (*find_current.read(cx))
                                .filter(|index| *index < matches.len())
                                .or_else(|| (!matches.is_empty()).then_some(0))
                            else {
                                return;
                            };
                            vec![(matches[index].clone(), replacement)]
                        };
                        code_editor.update(cx, |editor, cx| editor.edit(edits, cx));
                        find_current.update(cx, |current, cx| {
                            *current = None;
                            cx.notify();
                        });
                    }
                })
                .on_close({
                    let find_open = find_open.clone();
                    move |_, cx| {
                        find_open.update(cx, |open, cx| {
                            *open = false;
                            cx.notify();
                        });
                    }
                });
                if *replace_open.read(cx) {
                    widget = widget.replace(&replacement_field);
                }
                if let Some(error) = find_error {
                    widget = widget.error(error);
                }
                Some(widget)
            } else {
                None
            };

            let open_find = find_open.clone();
            let focus_find = find_field.clone();
            let find_button = IconButton::new(format!("{key}:find-button"), IconName::Search)
                .size(ControlSize::Sm)
                .tooltip("查找")
                .on_click(move |_, window, cx| {
                    open_find.update(cx, |open, cx| {
                        *open = true;
                        cx.notify();
                    });
                    let focus = focus_find.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                });

            let save_entity = entity.clone();
            let save_editor = code_editor.clone();
            let file_path = editor_relative.display().to_string();
            let editor_view = div()
                .id("native-workbench-file-editor")
                .role(Role::MultilineTextInput)
                .aria_label("文件编辑器")
                .relative()
                .flex_1()
                .min_h_0()
                .on_key_down({
                    let find_open = find_open.clone();
                    let find_field = find_field.clone();
                    move |event, window, cx| {
                        let modifiers = event.keystroke.modifiers;
                        if (modifiers.control || modifiers.platform)
                            && !modifiers.alt
                            && event.keystroke.key == "f"
                        {
                            find_open.update(cx, |open, cx| {
                                *open = true;
                                cx.notify();
                            });
                            let focus = find_field.read(cx).focus_handle(cx);
                            window.focus(&focus, cx);
                            cx.stop_propagation();
                        }
                    }
                })
                .child(code_editor)
                .when_some(find_widget, |view, widget| {
                    view.child(div().absolute().top_2().right_3().child(widget))
                });
            body = body
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(cx.theme().colors.fg_muted)
                        .child(div().flex_1().min_w_0().child(format!("编辑：{file_path}")))
                        .child(find_button),
                )
                .child(editor_view)
                .child(
                    Button::new("native-workbench-file-save", "保存 UTF-8 文件")
                        .icon(IconName::Save)
                        .variant(ButtonVariant::Primary)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let text = save_editor.read(cx).text().to_owned();
                            save_entity.update(cx, |panel, cx| panel.save_file(text, cx));
                        }),
                );
        }
        body.into_any_element()
    }

    fn search_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let file_field =
            window.use_keyed_state("native-workbench-search-files", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("文件名或路径关键词")
            });
        let content_field =
            window.use_keyed_state("native-workbench-search-content", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("文件内容关键词")
            });
        let entity = cx.entity();
        let file_submit = file_field.clone();
        let content_submit = content_field.clone();
        let file_entity = entity.clone();
        let content_entity = entity.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&file_field).size(ControlSize::Sm)),
                    )
                    .child(
                        Button::new("native-workbench-search-files-submit", "查询文件")
                            .icon(IconName::Search)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let query = file_submit.read(cx).text().to_owned();
                                file_entity.update(cx, |panel, cx| panel.search(query, cx));
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&content_field).size(ControlSize::Sm)),
                    )
                    .child(
                        Button::new("native-workbench-search-content-submit", "查询内容")
                            .icon(IconName::Search)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let query = content_submit.read(cx).text().to_owned();
                                content_entity
                                    .update(cx, |panel, cx| panel.search_content(query, cx));
                            }),
                    ),
            )
            .child(div().text_color(cx.theme().colors.fg_muted).child(format!(
                "文件匹配 {} 项{}；内容匹配 {} 项{}",
                self.snapshot.search_count,
                if self.snapshot.search_truncated {
                    "（已截断）"
                } else {
                    ""
                },
                self.snapshot.content_results.len(),
                if self.snapshot.content_truncated {
                    "（已截断）"
                } else {
                    ""
                }
            )))
            .child(
                div()
                    .id("native-workbench-search-results")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(self.snapshot.search_results.iter().map(|entry| {
                        let entity = entity.clone();
                        let path = entry.path.clone();
                        let is_dir = entry.kind == FileKind::Directory;
                        let relative = path
                            .strip_prefix(&self.root)
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|_| path.display().to_string());
                        let label = if is_dir {
                            format!("展开 {relative}")
                        } else {
                            format!("打开 {relative}")
                        };
                        div().flex().w_full().child(
                            Button::new(
                                format!("native-workbench-search-file:{}", relative),
                                label,
                            )
                            .icon(if is_dir {
                                IconName::Folder
                            } else {
                                IconName::File
                            })
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |panel, cx| {
                                    if is_dir {
                                        panel.toggle_directory(path.clone(), cx);
                                    } else {
                                        panel.open_file(path.clone(), cx);
                                    }
                                });
                            }),
                        )
                    }))
                    .children(self.snapshot.content_results.iter().map(|item| {
                        let entity = entity.clone();
                        let item_path = item.path.clone();
                        let path = item
                            .path
                            .strip_prefix(&self.root)
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|_| item.path.display().to_string());
                        let label = format!("打开 {path}:{} {}", item.line_number, item.line_text);
                        div().flex().w_full().child(
                            Button::new(
                                format!(
                                    "native-workbench-search-content:{}:{}",
                                    path, item.line_number
                                ),
                                label,
                            )
                            .icon(IconName::FileText)
                            .variant(ButtonVariant::Ghost)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |panel, cx| {
                                    panel.open_file(item_path.clone(), cx);
                                });
                            }),
                        )
                    })),
            )
            .into_any_element()
    }

    fn diff_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let value = self.snapshot.diff_path.clone().unwrap_or_default();
        let comment_path_value = value.clone();
        let field = window.use_keyed_state("native-workbench-diff-path", cx, move |window, cx| {
            let mut field =
                TextInput::new(window, cx).placeholder("相对文件路径，留空显示全部 Diff");
            field.set_text(value, cx);
            field
        });
        let entity = cx.entity();
        let submit = field.clone();
        let patch = self
            .snapshot
            .diff
            .as_ref()
            .map(|diff| diff.patch.clone())
            .unwrap_or_else(|| "尚未加载 Diff".to_owned());
        let code_settings = cx.theme().code.clone();
        let diff_entity = entity.clone();
        let comment_path =
            window.use_keyed_state("native-workbench-review-new-path", cx, move |window, cx| {
                let mut field = TextInput::new(window, cx).placeholder("评论文件相对路径");
                field.set_text(comment_path_value, cx);
                field
            });
        let comment_line =
            window.use_keyed_state("native-workbench-review-new-line", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("行号")
            });
        let comment_body =
            window.use_keyed_state("native-workbench-review-new-body", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("评论内容")
            });
        let comment_old_side =
            window.use_keyed_state("native-workbench-review-new-old-side", cx, |_, _| false);
        let add_entity = entity.clone();
        let add_path = comment_path.clone();
        let add_line = comment_line.clone();
        let add_body = comment_body.clone();
        let add_old_side = comment_old_side.clone();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&field).size(ControlSize::Sm)),
                    )
                    .child(
                        Button::new("native-workbench-diff-load", "加载 Diff")
                            .icon(IconName::FileText)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let path = submit.read(cx).text().to_owned();
                                diff_entity.update(cx, |panel, cx| panel.read_diff(path, cx));
                            }),
                    ),
            )
            .child(
                div()
                    .id("native-workbench-diff-output")
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .overflow_y_scroll()
                    .font(code_font(cx.theme()))
                    .text_size(code_text_size(cx.theme()))
                    .text_color(cx.theme().colors.fg)
                    .when(code_settings.wrap_long_lines, |diff| {
                        diff.whitespace_normal()
                    })
                    .when(!code_settings.wrap_long_lines, |diff| {
                        diff.overflow_x_scroll().whitespace_nowrap()
                    })
                    .child(patch),
            )
            .child(
                div()
                    .text_color(cx.theme().colors.fg_muted)
                    .child("新增 review 标注"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(Input::new(&comment_path).size(ControlSize::Sm))
                    .child(Input::new(&comment_line).size(ControlSize::Sm))
                    .child(Input::new(&comment_body).size(ControlSize::Sm))
                    .child(
                        Checkbox::new(
                            "native-workbench-review-new-old-side",
                            *comment_old_side.read(cx),
                        )
                        .label("旧侧（未勾选为新侧）")
                        .on_change({
                            let comment_old_side = comment_old_side.clone();
                            move |checked, _, cx| {
                                comment_old_side.update(cx, |value, cx| {
                                    *value = checked;
                                    cx.notify();
                                });
                            }
                        }),
                    )
                    .child(
                        Button::new("native-workbench-review-add", "添加评论")
                            .icon(IconName::MessageSquareDiff)
                            .variant(ButtonVariant::Primary)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let comment = parse_review_comment(
                                    next_review_comment_id(),
                                    add_path.read(cx).text().to_owned(),
                                    add_line.read(cx).text().to_owned(),
                                    add_body.read(cx).text().to_owned(),
                                    *add_old_side.read(cx),
                                );
                                add_entity.update(cx, |panel, cx| match comment {
                                    Ok(comment) => {
                                        let mut comments = panel.snapshot.comments.clone();
                                        comments.push(comment);
                                        panel.save_review_comments(comments, cx);
                                    }
                                    Err(error) => {
                                        panel.error = Some(error);
                                        cx.notify();
                                    }
                                });
                            }),
                    ),
            );
        for comment in self.snapshot.comments.clone() {
            let comment_id = comment.id.clone();
            let path_field =
                window.use_keyed_state(format!("native-workbench-review-path:{comment_id}"), cx, {
                    let value = comment.path.clone();
                    move |window, cx| {
                        let mut field = TextInput::new(window, cx).placeholder("评论文件相对路径");
                        field.set_text(value, cx);
                        field
                    }
                });
            let line_field =
                window.use_keyed_state(format!("native-workbench-review-line:{comment_id}"), cx, {
                    let value = comment.line.to_string();
                    move |window, cx| {
                        let mut field = TextInput::new(window, cx).placeholder("行号");
                        field.set_text(value, cx);
                        field
                    }
                });
            let body_field =
                window.use_keyed_state(format!("native-workbench-review-body:{comment_id}"), cx, {
                    let value = comment.body.clone();
                    move |window, cx| {
                        let mut field = TextInput::new(window, cx).placeholder("评论内容");
                        field.set_text(value, cx);
                        field
                    }
                });
            let old_side = window.use_keyed_state(
                format!("native-workbench-review-side:{comment_id}"),
                cx,
                move |_, _| matches!(comment.side, ReviewSide::Old),
            );
            let save_entity = entity.clone();
            let delete_entity = entity.clone();
            let save_id = comment_id.clone();
            let delete_id = comment_id.clone();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().colors.border)
                    .pt_1()
                    .child(Input::new(&path_field).size(ControlSize::Sm))
                    .child(Input::new(&line_field).size(ControlSize::Sm))
                    .child(Input::new(&body_field).size(ControlSize::Sm))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                Checkbox::new(
                                    format!("native-workbench-review-side:{comment_id}"),
                                    *old_side.read(cx),
                                )
                                .label("旧侧（未勾选为新侧）")
                                .on_change({
                                    let old_side = old_side.clone();
                                    move |checked, _, cx| {
                                        old_side.update(cx, |value, cx| {
                                            *value = checked;
                                            cx.notify();
                                        });
                                    }
                                }),
                            )
                            .child(
                                Button::new(
                                    format!("native-workbench-review-save:{comment_id}"),
                                    "保存评论",
                                )
                                .icon(IconName::Save)
                                .size(ControlSize::Sm)
                                .on_click({
                                    let path_field = path_field.clone();
                                    let line_field = line_field.clone();
                                    let body_field = body_field.clone();
                                    let old_side = old_side.clone();
                                    move |_, _, cx| {
                                        let comment = parse_review_comment(
                                            save_id.clone(),
                                            path_field.read(cx).text().to_owned(),
                                            line_field.read(cx).text().to_owned(),
                                            body_field.read(cx).text().to_owned(),
                                            *old_side.read(cx),
                                        );
                                        save_entity.update(cx, |panel, cx| match comment {
                                            Ok(comment) => {
                                                let mut comments = panel.snapshot.comments.clone();
                                                if let Some(current) = comments
                                                    .iter_mut()
                                                    .find(|current| current.id == comment.id)
                                                {
                                                    *current = comment;
                                                    panel.save_review_comments(comments, cx);
                                                } else {
                                                    panel.error =
                                                        Some("review 标注已不存在".to_owned());
                                                    cx.notify();
                                                }
                                            }
                                            Err(error) => {
                                                panel.error = Some(error);
                                                cx.notify();
                                            }
                                        });
                                    }
                                }),
                            )
                            .child(
                                Button::new(
                                    format!("native-workbench-review-delete:{comment_id}"),
                                    "删除",
                                )
                                .icon(IconName::Trash2)
                                .variant(ButtonVariant::Danger)
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    delete_entity.update(cx, |panel, cx| {
                                        panel.delete_review_comment(delete_id.clone(), cx)
                                    });
                                }),
                            ),
                    ),
            );
        }
        body.into_any_element()
    }

    fn git_file_row(&self, file: GitFileStatus, entity: Entity<Self>) -> AnyElement {
        let staged = file.index_status != " " && file.index_status != "?";
        let path = file.path.clone();
        let action = if staged {
            GitAction::Unstage {
                paths: vec![path.clone()],
            }
        } else {
            GitAction::Stage {
                paths: vec![path.clone()],
            }
        };
        let label = if staged { "撤销暂存" } else { "暂存" };
        Button::new(
            format!("native-workbench-git-file:{}", path),
            format!("{} {}", label, file.path),
        )
        .icon(IconName::SquareCheck)
        .variant(if staged {
            ButtonVariant::Subtle
        } else {
            ButtonVariant::Ghost
        })
        .size(ControlSize::Sm)
        .on_click(move |_, _, cx| {
            let action = action.clone();
            entity.update(cx, |panel, cx| panel.git_action(action, cx));
        })
        .into_any_element()
    }

    fn git_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let mut body = div().flex().flex_col().gap_1().flex_1().min_h_0();
        if let Some(status) = self.snapshot.git.clone() {
            body = body.child(div().child(format!(
                "分支：{} · 暂存 {} · 未暂存 {}",
                status.branch.as_deref().unwrap_or("无分支"),
                status
                    .files
                    .iter()
                    .filter(|file| file.index_status != " ")
                    .count(),
                status
                    .files
                    .iter()
                    .filter(|file| file.worktree_status != " ")
                    .count()
            )));
            body = body.children(
                status
                    .files
                    .into_iter()
                    .map(|file| self.git_file_row(file, entity.clone())),
            );
        } else {
            body = body.child("当前目录不是 Git 工作树");
        }
        let is_repo = self
            .snapshot
            .git
            .as_ref()
            .is_some_and(|status| status.is_repo);
        if !is_repo {
            let init_entity = entity.clone();
            body = body.child(
                Button::new("native-workbench-git-init", "初始化 Git")
                    .icon(IconName::GitBranch)
                    .variant(ButtonVariant::Primary)
                    .size(ControlSize::Sm)
                    .on_click(move |_, _, cx| {
                        init_entity.update(cx, |panel, cx| panel.git_action(GitAction::Init, cx));
                    }),
            );
        } else {
            let branch = window.use_keyed_state("native-workbench-git-branch", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("新分支名")
            });
            let publish = window.use_keyed_state("native-workbench-git-publish", cx, |_, _| false);
            let branch_entity = entity.clone();
            let branch_submit = branch.clone();
            let publish_submit = publish.clone();
            body = body.child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&branch).size(ControlSize::Sm)),
                    )
                    .child(
                        Checkbox::new("native-workbench-git-publish", *publish.read(cx))
                            .label("发布并设置上游")
                            .on_change({
                                let publish = publish.clone();
                                move |checked, _, cx| {
                                    publish.update(cx, |value, cx| {
                                        *value = checked;
                                        cx.notify();
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("native-workbench-git-create-branch", "创建分支")
                            .icon(IconName::GitBranch)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let name = branch_submit.read(cx).text().trim().to_owned();
                                let publish = *publish_submit.read(cx);
                                branch_entity.update(cx, |panel, cx| {
                                    panel.git_action(GitAction::CreateBranch { name, publish }, cx)
                                });
                            }),
                    ),
            );
            let checkout =
                window.use_keyed_state("native-workbench-git-checkout", cx, |window, cx| {
                    TextInput::new(window, cx).placeholder("切换目标分支")
                });
            let checkout_entity = entity.clone();
            let checkout_submit = checkout.clone();
            body = body.child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&checkout).size(ControlSize::Sm)),
                    )
                    .child(
                        Button::new("native-workbench-git-stash-checkout", "保存修改并切换")
                            .icon(IconName::GitBranch)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let branch = checkout_submit.read(cx).text().trim().to_owned();
                                checkout_entity.update(cx, |panel, cx| {
                                    panel.git_action(GitAction::StashAndCheckout { branch }, cx)
                                });
                            }),
                    ),
            );
            let pull_entity = entity.clone();
            let push_entity = entity.clone();
            body = body.child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("native-workbench-git-pull", "快进拉取")
                            .icon(IconName::Download)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                pull_entity.update(cx, |panel, cx| {
                                    panel.git_action(GitAction::PullFastForward, cx)
                                });
                            }),
                    )
                    .child(
                        Button::new("native-workbench-git-push", "推送")
                            .icon(IconName::Upload)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                push_entity
                                    .update(cx, |panel, cx| panel.git_action(GitAction::Push, cx));
                            }),
                    ),
            );
        }
        let commit = window.use_keyed_state("native-workbench-git-commit", cx, |window, cx| {
            TextInput::new(window, cx).placeholder("提交信息（必填）")
        });
        let commit_entity = entity.clone();
        let commit_field = commit.clone();
        body = body.child(
            div()
                .flex()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&commit).size(ControlSize::Sm)),
                )
                .child(
                    Button::new("native-workbench-git-commit-submit", "提交")
                        .icon(IconName::GitCommitHorizontal)
                        .variant(ButtonVariant::Primary)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let message = commit_field.read(cx).text().trim().to_owned();
                            let action = GitAction::Commit {
                                message,
                                paths: None,
                            };
                            commit_entity.update(cx, |panel, cx| panel.git_action(action, cx));
                        }),
                ),
        );
        let stash = window.use_keyed_state("native-workbench-git-stash", cx, |window, cx| {
            TextInput::new(window, cx).placeholder("stash 说明（必填）")
        });
        let stash_entity = entity.clone();
        let stash_field = stash.clone();
        body = body.child(
            div()
                .flex()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&stash).size(ControlSize::Sm)),
                )
                .child(
                    Button::new("native-workbench-git-stash-submit", "保存 Stash")
                        .icon(IconName::Archive)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            let message = stash_field.read(cx).text().trim().to_owned();
                            let action = GitAction::Stash { message };
                            stash_entity.update(cx, |panel, cx| panel.git_action(action, cx));
                        }),
                ),
        );
        body = body.child(
            div()
                .text_color(cx.theme().colors.fg_muted)
                .child("已有 Stash"),
        );
        for stash in self.snapshot.stashes.clone() {
            let hash = stash.hash.clone();
            let drop_entity = entity.clone();
            body = body.child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(format!("{} {}", stash.reference, stash.message)),
                    )
                    .child(
                        Button::new(format!("native-workbench-git-stash-drop:{hash}"), "删除")
                            .icon(IconName::Trash2)
                            .variant(ButtonVariant::Danger)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                let action = GitAction::DropStash { hash: hash.clone() };
                                drop_entity.update(cx, |panel, cx| panel.git_action(action, cx));
                            }),
                    ),
            );
        }
        body.into_any_element()
    }

    fn worktrees_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let mut body = div().flex().flex_col().gap_1().flex_1().min_h_0();
        for worktree in self.snapshot.worktrees.clone() {
            body = body.child(
                div()
                    .flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(format!(
                        "{} {}",
                        worktree.branch.as_deref().unwrap_or("detached"),
                        worktree.path.display()
                    )))
                    .when(!worktree.is_main, |row| {
                        let target = worktree.path.display().to_string();
                        let remove_entity = entity.clone();
                        row.child(
                            Button::new(
                                format!("native-workbench-worktree-remove:{target}"),
                                "移除",
                            )
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                remove_entity.update(cx, |panel, cx| {
                                    panel.remove_worktree(target.clone(), false, cx)
                                });
                            }),
                        )
                    }),
            );
        }
        let reference =
            window.use_keyed_state("native-workbench-worktree-reference", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("引用，例如 HEAD 或分支名")
            });
        let target =
            window.use_keyed_state("native-workbench-worktree-target", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("目标绝对路径（可选）")
            });
        let branch =
            window.use_keyed_state("native-workbench-worktree-branch", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("新分支名（可选）")
            });
        let copy_changes_from = window.use_keyed_state(
            "native-workbench-worktree-copy-changes",
            cx,
            |window, cx| TextInput::new(window, cx).placeholder("复制修改来源绝对路径（可选）"),
        );
        let create_entity = entity.clone();
        let reference_submit = reference.clone();
        let target_submit = target.clone();
        let branch_submit = branch.clone();
        let copy_changes_submit = copy_changes_from.clone();
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Input::new(&reference).size(ControlSize::Sm))
                .child(Input::new(&target).size(ControlSize::Sm))
                .child(Input::new(&branch).size(ControlSize::Sm))
                .child(Input::new(&copy_changes_from).size(ControlSize::Sm))
                .child(
                    Button::new("native-workbench-worktree-create", "创建工作树")
                        .icon(IconName::FolderPlus)
                        .variant(ButtonVariant::Primary)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            create_entity.update(cx, |panel, cx| {
                                panel.create_worktree(
                                    reference_submit.read(cx).text().to_owned(),
                                    target_submit.read(cx).text().to_owned(),
                                    branch_submit.read(cx).text().to_owned(),
                                    copy_changes_submit.read(cx).text().to_owned(),
                                    cx,
                                )
                            });
                        }),
                ),
        );
        let archive_target = window.use_keyed_state(
            "native-workbench-worktree-archive-target",
            cx,
            |window, cx| TextInput::new(window, cx).placeholder("归档目标绝对路径"),
        );
        let archive_session = window.use_keyed_state(
            "native-workbench-worktree-archive-session",
            cx,
            |window, cx| TextInput::new(window, cx).placeholder("归档会话标识"),
        );
        let archive_entity = entity.clone();
        let archive_target_submit = archive_target.clone();
        let archive_session_submit = archive_session.clone();
        body = body.child(
            div()
                .flex()
                .gap_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&archive_target).size(ControlSize::Sm)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&archive_session).size(ControlSize::Sm)),
                )
                .child(
                    Button::new("native-workbench-worktree-archive", "归档")
                        .icon(IconName::Archive)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            archive_entity.update(cx, |panel, cx| {
                                panel.archive_worktree(
                                    archive_target_submit.read(cx).text().to_owned(),
                                    archive_session_submit.read(cx).text().to_owned(),
                                    cx,
                                )
                            });
                        }),
                ),
        );
        let handoff_source = window.use_keyed_state(
            "native-workbench-worktree-handoff-source",
            cx,
            |window, cx| TextInput::new(window, cx).placeholder("交接源绝对路径"),
        );
        let handoff_target = window.use_keyed_state(
            "native-workbench-worktree-handoff-target",
            cx,
            |window, cx| TextInput::new(window, cx).placeholder("交接目标绝对路径"),
        );
        let handoff_branch = window.use_keyed_state(
            "native-workbench-worktree-handoff-branch",
            cx,
            |window, cx| TextInput::new(window, cx).placeholder("目标分支（可选）"),
        );
        let handoff_entity = entity.clone();
        let source_submit = handoff_source.clone();
        let target_submit = handoff_target.clone();
        let branch_submit = handoff_branch.clone();
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Input::new(&handoff_source).size(ControlSize::Sm))
                .child(Input::new(&handoff_target).size(ControlSize::Sm))
                .child(Input::new(&handoff_branch).size(ControlSize::Sm))
                .child(
                    Button::new("native-workbench-worktree-handoff", "交接")
                        .icon(IconName::GitMerge)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            handoff_entity.update(cx, |panel, cx| {
                                panel.handoff_worktree(
                                    source_submit.read(cx).text().to_owned(),
                                    target_submit.read(cx).text().to_owned(),
                                    branch_submit.read(cx).text().to_owned(),
                                    cx,
                                )
                            });
                        }),
                ),
        );
        body.into_any_element()
    }

    fn terminal_body(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let shell = ShellColors::from_theme(cx.theme());
        let mut body = div().flex().flex_col().gap_1().flex_1().min_h_0();
        let new_entity = entity.clone();
        body = body.child(
            Button::new("native-workbench-terminal-create", "新建终端")
                .icon(IconName::Terminal)
                .variant(ButtonVariant::Primary)
                .size(ControlSize::Sm)
                .on_click(move |_, _, cx| {
                    new_entity.update(cx, |panel, cx| {
                        let (cols, rows) = panel.terminal_dimensions.unwrap_or((100, 30));
                        let result = panel.workbench.sync_terminal_settings().and_then(|()| {
                            panel.workbench.terminals.create(
                                next_terminal_id(),
                                &panel.root,
                                Some(cols),
                                Some(rows),
                                super::PtyShell::Auto,
                                None,
                            )
                        });
                        panel.finish(result.map(|_| "终端已创建".to_owned()), cx);
                    });
                }),
        );
        let terminal_font_family = self.workbench.terminal_settings().font_family;
        for terminal in self.snapshot.terminals.clone() {
            let terminal_id = terminal.id.clone();
            let focus = self
                .terminal_focuses
                .entry(terminal_id.clone())
                .or_insert_with(|| cx.focus_handle().tab_stop(true))
                .clone();
            let clear_entity = entity.clone();
            let clear_id = terminal_id.clone();
            let close_entity = entity.clone();
            let close_id = terminal_id.clone();
            let grid = terminal_grid(
                entity.clone(),
                terminal.clone(),
                focus,
                terminal_font_family.clone(),
            );
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .border_1()
                    .border_color(cx.theme().colors.border)
                    .p_1()
                    .bg(shell.panel)
                    .child(
                        div()
                            .text_size(ui_text_size(cx.theme(), UiTextSize::Caption))
                            .text_color(cx.theme().colors.fg_muted)
                            .child(format!(
                                "{} · {}",
                                terminal_id,
                                if terminal.exited {
                                    format!("已退出 ({})", terminal.exit_code.unwrap_or_default())
                                } else {
                                    format!("运行中 pid={}", terminal.pid.unwrap_or_default())
                                }
                            )),
                    )
                    .child(div().flex_1().min_h_0().bg(shell.canvas).child(grid))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                Button::new(
                                    format!("native-workbench-terminal-clear:{clear_id}"),
                                    "清空",
                                )
                                .icon(IconName::Eraser)
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    clear_entity.update(cx, |panel, cx| {
                                        panel.clear_terminal(&clear_id, cx);
                                    });
                                }),
                            )
                            .child(
                                Button::new(
                                    format!("native-workbench-terminal-close:{close_id}"),
                                    "关闭",
                                )
                                .icon(IconName::X)
                                .variant(ButtonVariant::Danger)
                                .size(ControlSize::Sm)
                                .on_click(move |_, _, cx| {
                                    close_entity.update(cx, |panel, cx| {
                                        panel.close_terminal(&close_id, cx);
                                    });
                                }),
                            ),
                    ),
            );
        }
        body.into_any_element()
    }

    fn dev_servers_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let project_id =
            window.use_keyed_state("native-workbench-dev-project", cx, |window, cx| {
                TextInput::new(window, cx).placeholder("项目标识（必填）")
            });
        let cwd = window.use_keyed_state("native-workbench-dev-cwd", cx, |window, cx| {
            TextInput::new(window, cx).placeholder("脚本工作目录绝对路径（必填）")
        });
        let command = window.use_keyed_state("native-workbench-dev-command", cx, |window, cx| {
            TextInput::new(window, cx).placeholder("启动命令（必填）")
        });
        let start_entity = entity.clone();
        let project_submit = project_id.clone();
        let cwd_submit = cwd.clone();
        let command_submit = command.clone();
        let mut body = div().flex().flex_col().gap_1().child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(Input::new(&project_id).size(ControlSize::Sm))
                .child(Input::new(&cwd).size(ControlSize::Sm))
                .child(Input::new(&command).size(ControlSize::Sm))
                .child(
                    Button::new("native-workbench-dev-start", "启动开发服务")
                        .icon(IconName::Play)
                        .variant(ButtonVariant::Primary)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            start_entity.update(cx, |panel, cx| {
                                panel.start_dev_server(
                                    project_submit.read(cx).text().to_owned(),
                                    cwd_submit.read(cx).text().to_owned(),
                                    command_submit.read(cx).text().to_owned(),
                                    cx,
                                )
                            });
                        }),
                ),
        );
        for server in self.snapshot.dev_servers.clone() {
            let stop_entity = entity.clone();
            let project = server.project_id.clone();
            body = body.child(
                div()
                    .flex()
                    .gap_1()
                    .child(div().flex_1().min_w_0().child(format!(
                        "{} pid={} {}",
                        server.project_id, server.pid, server.command
                    )))
                    .child(
                        Button::new(
                            format!("native-workbench-dev-stop:{}", server.project_id),
                            "停止",
                        )
                        .icon(IconName::CircleStop)
                        .variant(ButtonVariant::Danger)
                        .size(ControlSize::Sm)
                        .on_click(move |_, _, cx| {
                            stop_entity
                                .update(cx, |panel, cx| panel.stop_dev_server(project.clone(), cx));
                        }),
                    ),
            );
        }
        body.into_any_element()
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let shell = ShellColors::from_theme(theme);
        let heading_text_size = ui_text_size(theme, UiTextSize::Base);
        let body_text_size = ui_text_size(theme, UiTextSize::Sm);
        let caption_text_size = ui_text_size(theme, UiTextSize::Caption);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .flex_1()
            .min_h_0()
            .text_size(body_text_size)
            .text_color(colors.fg)
            .bg(shell.content)
            .child(self.root_bar(window, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(heading_text_size)
                    .child(Icon::new(self.pane.icon()).size(IconSize::Sm))
                    .child(self.pane.label())
                    .child({
                        let entity = cx.entity();
                        Button::new("native-workbench-refresh", "刷新")
                            .icon(IconName::RefreshCw)
                            .size(ControlSize::Sm)
                            .on_click(move |_, _, cx| {
                                entity.update(cx, |panel, cx| panel.refresh(cx));
                            })
                    }),
            )
            .when(self.loading, |view| {
                view.child(
                    div()
                        .text_size(caption_text_size)
                        .text_color(colors.fg_muted)
                        .child("正在读取工作台状态"),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .text_size(caption_text_size)
                        .text_color(colors.danger)
                        .child(error),
                )
            })
            .when_some(self.status.clone(), |view, status| {
                view.child(
                    div()
                        .id("native-workbench-status")
                        // 异步操作反馈同时暴露稳定节点和 Status 语义，便于 UIA 观察实际结果。
                        .role(Role::Status)
                        .aria_label(status.clone())
                        .aria_value(status.clone())
                        .text_size(caption_text_size)
                        .text_color(colors.fg_muted)
                        .child(status),
                )
            });
        body = body.child(match self.pane {
            WorkbenchPane::Files => self.files_body(window, cx),
            WorkbenchPane::Search => self.search_body(window, cx),
            WorkbenchPane::Diff => self.diff_body(window, cx),
            WorkbenchPane::Git => self.git_body(window, cx),
            WorkbenchPane::Worktrees => self.worktrees_body(window, cx),
            WorkbenchPane::Terminal => self.terminal_body(window, cx),
            WorkbenchPane::DevServers => self.dev_servers_body(window, cx),
        });
        body.into_any_element()
    }
}

impl Render for NativeWorkbenchPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_terminal_resize_observer(window, cx);
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let shell = ShellColors::from_theme(theme);
        let tabs = self.tabs(cx);
        div()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .border_l_1()
            .border_color(colors.border)
            .bg(shell.panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .py_1()
                    .border_b_1()
                    .border_color(colors.border)
                    .bg(shell.header)
                    .children(tabs),
            )
            .child(self.body(window, cx))
    }
}

impl EntityInputHandler for NativeWorkbenchPanel {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_terminal_id.clone() {
            self.input_terminal(&id, text.as_bytes(), cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        _new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        // 预编辑串只由系统输入法绘制，提交时再由 replace_text_in_range 写入 PTY。
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let id = self.active_terminal_id.as_deref()?;
        let geometry = self.terminal_geometry.get(id).copied()?;
        let cursor = self
            .snapshot
            .terminals
            .iter()
            .find(|terminal| terminal.id == id)
            .and_then(|terminal| terminal.cursor)?;
        let origin = geometry.origin
            + point(
                geometry.cell.width * f32::from(cursor.column),
                geometry.cell.height * f32::from(cursor.row),
            );
        Some(Bounds::new(origin, geometry.cell))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

fn terminal_mouse_bytes(
    mode: u32,
    column: usize,
    row: usize,
    report: TerminalMouseReport,
) -> Option<Vec<u8>> {
    let modes = TermMode::from_bits_retain(mode);
    if report.motion {
        if !modes.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG) {
            return None;
        }
    } else if !modes.intersects(TermMode::MOUSE_REPORT_CLICK | TermMode::MOUSE_DRAG) {
        return None;
    }
    let mut code = if report.release { 3 } else { report.button };
    if report.shift {
        code |= 4;
    }
    if report.alt {
        code |= 8;
    }
    if report.control {
        code |= 16;
    }
    if report.motion {
        code |= 32;
    }
    let column = column.saturating_add(1);
    let row = row.saturating_add(1);
    if modes.contains(TermMode::SGR_MOUSE) {
        let suffix = if report.release { 'm' } else { 'M' };
        Some(format!("\x1b[<{code};{column};{row}{suffix}").into_bytes())
    } else {
        let column = u8::try_from(column.saturating_add(32)).ok()?;
        let row = u8::try_from(row.saturating_add(32)).ok()?;
        Some(vec![0x1b, b'[', b'M', code.saturating_add(32), column, row])
    }
}

fn terminal_grid(
    entity: Entity<NativeWorkbenchPanel>,
    snapshot: PtySnapshot,
    focus: gpui::FocusHandle,
    font_family: String,
) -> AnyElement {
    let terminal_id = snapshot.id.clone();
    let layout_entity = entity.clone();
    let layout_id = terminal_id.clone();
    let paint_snapshot = snapshot.clone();
    let paint_focus = focus.clone();
    let paint_entity = entity.clone();
    let paint_id = terminal_id.clone();
    let layout_font_family = font_family.clone();
    let paint_font_family = font_family;
    let grid = canvas(
        move |bounds, window, cx| {
            let (_, _, cell) = terminal_cell_metrics(window, cx, &layout_font_family);
            let cols = (f32::from(bounds.size.width) / f32::from(cell.width))
                .floor()
                .clamp(2.0, 240.0) as u16;
            let rows = (f32::from(bounds.size.height) / f32::from(cell.height))
                .floor()
                .clamp(1.0, 80.0) as u16;
            layout_entity.update(cx, |panel, cx| {
                panel.update_terminal_layout(&layout_id, cols, rows, bounds.origin, cell, cx);
            });
        },
        move |bounds, _, window, cx| {
            if paint_focus.is_focused(window) {
                paint_entity.update(cx, |panel, _| panel.activate_terminal(&paint_id));
                window.handle_input(
                    &paint_focus,
                    ElementInputHandler::new(bounds, paint_entity.clone()),
                    cx,
                );
            }
            paint_terminal_grid(
                bounds,
                &paint_snapshot,
                &paint_focus,
                &paint_font_family,
                window,
                cx,
            );
        },
    )
    .size_full();

    let key_entity = entity.clone();
    let key_id = terminal_id.clone();
    let app_cursor = snapshot.mode & TermMode::APP_CURSOR.bits() != 0;
    let mouse_down_entity = entity.clone();
    let mouse_down_id = terminal_id.clone();
    let mouse_focus = focus.clone();
    let mouse_move_entity = entity.clone();
    let mouse_move_id = terminal_id.clone();
    let mouse_up_entity = entity.clone();
    let mouse_up_id = terminal_id.clone();
    let scroll_entity = entity.clone();
    let scroll_id = terminal_id.clone();
    div()
        .id(format!("native-workbench-terminal-grid:{terminal_id}"))
        .role(Role::TextInput)
        .aria_label("终端输入")
        .relative()
        .size_full()
        .track_focus(&focus)
        .on_key_down(move |event, _, cx| {
            let modifiers = event.keystroke.modifiers;
            if !modifiers.platform
                && modifiers.control
                && modifiers.shift
                && !modifiers.alt
                && matches!(event.keystroke.key.as_str(), "c" | "v")
            {
                cx.stop_propagation();
                key_entity.update(cx, |panel, cx| {
                    panel.activate_terminal(&key_id);
                    match event.keystroke.key.as_str() {
                        "c" => panel.copy_terminal(&key_id, cx),
                        "v" => panel.paste_terminal(&key_id, cx),
                        _ => unreachable!(),
                    }
                });
                return;
            }
            let Some(bytes) = terminal_key_bytes(&event.keystroke, app_cursor) else {
                return;
            };
            cx.stop_propagation();
            key_entity.update(cx, |panel, cx| {
                panel.activate_terminal(&key_id);
                panel.input_terminal(&key_id, &bytes, cx);
            });
        })
        .on_mouse_down(
            MouseButton::Left,
            move |event: &MouseDownEvent, window, cx| {
                mouse_focus.focus(window, cx);
                mouse_down_entity.update(cx, |panel, cx| {
                    panel.activate_terminal(&mouse_down_id);
                    if panel.terminal_mouse_reporting(&mouse_down_id) {
                        panel.send_mouse_event(
                            &mouse_down_id,
                            event.position,
                            TerminalMouseReport {
                                button: 0,
                                release: false,
                                motion: false,
                                shift: event.modifiers.shift,
                                alt: event.modifiers.alt,
                                control: event.modifiers.control,
                            },
                            cx,
                        );
                    } else {
                        let kind = match event.click_count {
                            2 => SelectionType::Semantic,
                            count if count >= 3 => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };
                        panel.start_terminal_selection(&mouse_down_id, kind, event.position, cx);
                    }
                });
            },
        )
        .on_mouse_move(move |event, _, cx| {
            if !event.dragging() {
                return;
            }
            mouse_move_entity.update(cx, |panel, cx| {
                if panel.terminal_selecting.contains(&mouse_move_id) {
                    panel.update_terminal_selection(&mouse_move_id, event.position, cx);
                } else {
                    panel.send_mouse_event(
                        &mouse_move_id,
                        event.position,
                        TerminalMouseReport {
                            button: 0,
                            release: false,
                            motion: true,
                            shift: event.modifiers.shift,
                            alt: event.modifiers.alt,
                            control: event.modifiers.control,
                        },
                        cx,
                    );
                }
            });
        })
        .on_mouse_up(MouseButton::Left, move |event, _, cx| {
            mouse_up_entity.update(cx, |panel, cx| {
                if panel.terminal_selecting.remove(&mouse_up_id) {
                    panel.update_terminal_selection(&mouse_up_id, event.position, cx);
                } else {
                    panel.send_mouse_event(
                        &mouse_up_id,
                        event.position,
                        TerminalMouseReport {
                            button: 0,
                            release: true,
                            motion: false,
                            shift: event.modifiers.shift,
                            alt: event.modifiers.alt,
                            control: event.modifiers.control,
                        },
                        cx,
                    );
                }
            });
        })
        .on_scroll_wheel(move |event: &ScrollWheelEvent, _, cx| {
            let lines = (f32::from(event.delta.pixel_delta(px(16.)).y) / 16.0).round() as i32;
            if lines == 0 {
                return;
            }
            cx.stop_propagation();
            scroll_entity.update(cx, |panel, cx| {
                panel.scroll_terminal(&scroll_id, lines, cx);
            });
        })
        .child(grid)
        .into_any_element()
}

fn terminal_cell_metrics(
    window: &Window,
    cx: &App,
    font_family: &str,
) -> (Font, Pixels, Size<Pixels>) {
    let text = cx.theme().text_size(TextSize::Sm);
    let pixels = text.to_pixels(window.rem_size());
    let mono = mono_font(font_family.to_owned());
    let advance = window
        .text_system()
        .advance(window.text_system().resolve_font(&mono), pixels, 'm')
        .expect("终端字体必须支持 m 字符")
        .width;
    (mono, pixels, size(advance, (pixels * 1.35).round()))
}

#[derive(Clone, Copy, PartialEq)]
struct TerminalCellStyle {
    foreground: Hsla,
    background: Option<Hsla>,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

fn paint_terminal_grid(
    bounds: Bounds<Pixels>,
    snapshot: &PtySnapshot,
    focus: &gpui::FocusHandle,
    font_family: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let (mono, pixels, cell_size) = terminal_cell_metrics(window, cx, font_family);
    let colors = cx.theme().colors.clone();
    let focused = focus.is_focused(window);
    for row in 0..usize::from(snapshot.rows) {
        let mut text = String::new();
        let mut runs = Vec::new();
        let mut last_style = None;
        let mut run_start = 0;
        for column in 0..usize::from(snapshot.cols) {
            let Some(cell) = snapshot.cells.get(
                row.saturating_mul(usize::from(snapshot.cols))
                    .saturating_add(column),
            ) else {
                break;
            };
            let flags = alacritty_terminal::term::cell::Flags::from_bits_retain(cell.flags);
            let style = terminal_cell_style(cell, flags, &snapshot.palette, &colors);
            if last_style != Some(style) {
                if let Some(previous) = last_style {
                    runs.push(terminal_text_run(run_start, text.len(), previous, &mono));
                }
                run_start = text.len();
                last_style = Some(style);
            }
            if flags.intersects(
                alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER
                    | alacritty_terminal::term::cell::Flags::LEADING_WIDE_CHAR_SPACER,
            ) {
                text.push(' ');
            } else {
                text.push(cell.character);
                text.extend(cell.combining.iter().copied());
            }
        }
        if let Some(last) = last_style {
            runs.push(terminal_text_run(run_start, text.len(), last, &mono));
        }
        let origin = bounds.origin + point(Pixels::ZERO, cell_size.height * row as f32);
        let shaped =
            window
                .text_system()
                .shape_line(text.into(), pixels, &runs, Some(cell_size.width));
        shaped
            .paint_background(origin, cell_size.height, TextAlign::Left, None, window, cx)
            .expect("终端行背景必须可绘制");
        if let Some(selection) = snapshot.selection {
            let mut selected_start = None;
            for column in 0..usize::from(snapshot.cols) {
                let index = row
                    .saturating_mul(usize::from(snapshot.cols))
                    .saturating_add(column);
                let selected = snapshot
                    .cells
                    .get(index)
                    .map(|cell| {
                        let flags =
                            alacritty_terminal::term::cell::Flags::from_bits_retain(cell.flags);
                        terminal_selection_contains_cell(&selection, row, column, flags)
                    })
                    .unwrap_or(false);
                match (selected, selected_start) {
                    (true, None) => selected_start = Some(column),
                    (false, Some(start)) => {
                        paint_terminal_selection(
                            bounds,
                            origin,
                            cell_size,
                            start,
                            column,
                            colors.selection,
                            window,
                        );
                        selected_start = None;
                    }
                    _ => {}
                }
            }
            if let Some(start) = selected_start {
                paint_terminal_selection(
                    bounds,
                    origin,
                    cell_size,
                    start,
                    usize::from(snapshot.cols),
                    colors.selection,
                    window,
                );
            }
        }
        shaped
            .paint(origin, cell_size.height, TextAlign::Left, None, window, cx)
            .expect("终端行文字必须可绘制");
    }
    if let Some(cursor) = snapshot.cursor {
        let origin = bounds.origin
            + point(
                cell_size.width * f32::from(cursor.column),
                cell_size.height * f32::from(cursor.row),
            );
        let caret = colors.focus;
        let caret_width = cx.theme().caret_width().to_pixels(window.rem_size());
        let quad = match (cursor.shape, focused) {
            (PtyCursorShape::HollowBlock, _) | (_, false) => {
                outline(Bounds::new(origin, cell_size), caret, BorderStyle::Solid)
            }
            (PtyCursorShape::Beam, true) => fill(
                Bounds::new(origin, size(caret_width, cell_size.height)),
                caret,
            ),
            (PtyCursorShape::Underline, true) => fill(
                Bounds::new(
                    origin + point(Pixels::ZERO, cell_size.height - caret_width),
                    size(cell_size.width, caret_width),
                ),
                caret,
            ),
            (PtyCursorShape::Block, true) => {
                fill(Bounds::new(origin, cell_size), caret.opacity(0.45))
            }
            (PtyCursorShape::Hidden, _) => return,
        };
        window.paint_quad(quad);
    }
}

fn terminal_selection_contains_cell(
    selection: &PtySelection,
    row: usize,
    column: usize,
    flags: alacritty_terminal::term::cell::Flags,
) -> bool {
    let contains = |row: usize, column: usize| {
        let row = i32::try_from(row).unwrap_or(i32::MAX);
        let column = i32::try_from(column).unwrap_or(i32::MAX);
        if row < selection.start_row || row > selection.end_row {
            return false;
        }
        if selection.is_block {
            return column >= selection.start_column && column <= selection.end_column;
        }
        if selection.start_row == selection.end_row {
            return column >= selection.start_column && column <= selection.end_column;
        }
        if row == selection.start_row {
            return column >= selection.start_column;
        }
        if row == selection.end_row {
            return column <= selection.end_column;
        }
        true
    };

    contains(row, column)
        || (flags.contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR)
            && contains(row, column.saturating_add(1)))
}

fn paint_terminal_selection(
    _bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    cell_size: Size<Pixels>,
    start: usize,
    end: usize,
    color: Hsla,
    window: &mut Window,
) {
    let selection_origin = origin + point(cell_size.width * start as f32, Pixels::ZERO);
    let extent = size(
        cell_size.width * end.saturating_sub(start) as f32,
        cell_size.height,
    );
    window.paint_quad(fill(Bounds::new(selection_origin, extent), color));
}

fn terminal_text_run(start: usize, end: usize, style: TerminalCellStyle, mono: &Font) -> TextRun {
    TextRun {
        len: end.saturating_sub(start),
        font: Font {
            weight: if style.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if style.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            ..mono.clone()
        },
        color: style.foreground,
        background_color: style.background,
        underline: style.underline.then_some(UnderlineStyle {
            thickness: px(1.),
            color: Some(style.foreground),
            wavy: false,
        }),
        strikethrough: style.strikethrough.then_some(StrikethroughStyle {
            thickness: px(1.),
            color: Some(style.foreground),
        }),
    }
}

fn terminal_cell_style(
    cell: &PtyCell,
    flags: alacritty_terminal::term::cell::Flags,
    palette: &[Option<PtyRgb>],
    colors: &ely_gpui_component::theme::Palette,
) -> TerminalCellStyle {
    let mut foreground = resolve_terminal_color(cell.foreground, palette, colors);
    let mut background = resolve_terminal_color(cell.background, palette, colors);
    if flags.contains(alacritty_terminal::term::cell::Flags::INVERSE) {
        std::mem::swap(&mut foreground, &mut background);
    }
    if flags.contains(alacritty_terminal::term::cell::Flags::DIM) {
        foreground = foreground.opacity(0.7);
    }
    if flags.contains(alacritty_terminal::term::cell::Flags::HIDDEN) {
        foreground = background;
    }
    TerminalCellStyle {
        foreground,
        background: (background != colors.surface).then_some(background),
        bold: flags.contains(alacritty_terminal::term::cell::Flags::BOLD),
        italic: flags.contains(alacritty_terminal::term::cell::Flags::ITALIC),
        underline: flags.intersects(alacritty_terminal::term::cell::Flags::ALL_UNDERLINES),
        strikethrough: flags.contains(alacritty_terminal::term::cell::Flags::STRIKEOUT),
    }
}

fn resolve_terminal_color(
    color: PtyColor,
    palette: &[Option<PtyRgb>],
    colors: &ely_gpui_component::theme::Palette,
) -> Hsla {
    let palette_color = |index: usize| {
        palette
            .get(index)
            .and_then(|color| color.map(terminal_rgb_to_hsla))
    };
    match color {
        PtyColor::Rgb { red, green, blue } => terminal_rgb_to_hsla(PtyRgb { red, green, blue }),
        PtyColor::Indexed { value } => palette_color(usize::from(value))
            .unwrap_or_else(|| terminal_indexed_color(usize::from(value), colors)),
        PtyColor::Named { value } => {
            palette_color(usize::from(value)).unwrap_or_else(|| match value {
                0..=15 => colors.ansi[usize::from(value)],
                256 | 267 => colors.fg,
                257 | 268 => colors.surface,
                258 => colors.fg,
                259..=266 => colors.ansi[usize::from(value - 259)].opacity(0.7),
                _ => colors.fg,
            })
        }
    }
}

fn terminal_indexed_color(index: usize, colors: &ely_gpui_component::theme::Palette) -> Hsla {
    match index {
        0..=15 => colors.ansi[index],
        16..=231 => {
            let step = |level: usize| if level == 0 { 0 } else { 55 + 40 * level };
            let cube = index - 16;
            terminal_rgb_to_hsla(PtyRgb {
                red: step(cube / 36) as u8,
                green: step(cube / 6 % 6) as u8,
                blue: step(cube % 6) as u8,
            })
        }
        232..=255 => {
            let gray = 8 + 10 * (index.saturating_sub(232).min(23));
            terminal_rgb_to_hsla(PtyRgb {
                red: gray as u8,
                green: gray as u8,
                blue: gray as u8,
            })
        }
        256 | 267 => colors.fg,
        257 | 268 => colors.surface,
        258 => colors.focus,
        259..=266 => colors.ansi[index - 259].opacity(0.7),
        _ => colors.fg,
    }
}

fn terminal_rgb_to_hsla(rgb: PtyRgb) -> Hsla {
    Rgba {
        r: f32::from(rgb.red) / 255.0,
        g: f32::from(rgb.green) / 255.0,
        b: f32::from(rgb.blue) / 255.0,
        a: 1.0,
    }
    .into()
}

impl Drop for NativeWorkbenchPanel {
    fn drop(&mut self) {
        self.closed = true;
        self.refresh_task.take();
        self.blocking_task.take();
        self.terminal_event_task.take();
        self.terminal_subscription.take();
        self.editor_subscriptions.take();
        self.editor_find_cache.take();
        self.editor_highlight_state.take();
        self.terminal_focuses.clear();
        self.terminal_geometry.clear();
        self.terminal_selecting.clear();
        self.active_terminal_id = None;
        self.window_bounds_subscription.take();
    }
}

fn load_snapshot(
    workbench: &NativeWorkbench,
    root: &Path,
    expanded_dirs: &HashSet<PathBuf>,
    diff_path: Option<&str>,
) -> Result<WorkbenchSnapshot, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("项目根不可访问：{error}"))?;
    if !root.is_dir() {
        return Err("项目根不是目录".to_owned());
    }
    let mut directories = vec![root.clone()];
    let mut expanded = expanded_dirs
        .iter()
        .filter(|path| *path != &root && path.starts_with(&root) && path.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    expanded.sort();
    directories.extend(expanded);
    let mut files = Vec::new();
    let mut files_truncated = false;
    for directory in directories {
        let remaining = MAX_SNAPSHOT_FILES.saturating_sub(files.len());
        if remaining == 0 {
            files_truncated = true;
            break;
        }
        let relative = if directory != root {
            directory.strip_prefix(&root).ok().map(PathBuf::from)
        } else {
            None
        };
        let tree = workbench.files.list_tree(FileTreeInput {
            root: root.clone(),
            relative,
            include_hidden: Some(false),
            limit: Some(remaining.min(500)),
        })?;
        files_truncated |= tree.truncated;
        files.extend(tree.entries);
    }
    if files.len() > MAX_SNAPSHOT_FILES {
        files.truncate(MAX_SNAPSHOT_FILES);
        files_truncated = true;
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let git = workbench.git.status(&root)?;
    let comments = workbench.diff.load_comments(&root)?;
    let editors = workbench.editors.discover();
    let (diff, worktrees, stashes) = if git.is_repo {
        let diff = workbench
            .diff
            .read(
                &root,
                DiffOptions {
                    staged: false,
                    path: diff_path.map(str::to_owned),
                    context_lines: Some(3),
                },
            )
            .map_err(|error| format!("读取 Git 差异失败：{error}"))?;
        let worktrees = workbench
            .worktrees
            .list(&root)
            .map_err(|error| format!("读取 Git 工作树失败：{error}"))?;
        let stashes = workbench
            .git
            .stash_list(&root)
            .map_err(|error| format!("读取 Git Stash 失败：{error}"))?;
        (Some(diff), worktrees, stashes)
    } else {
        (None, Vec::new(), Vec::new())
    };
    Ok(WorkbenchSnapshot {
        root: root.clone(),
        files,
        diff,
        diff_path: diff_path.map(str::to_owned),
        git: Some(git),
        stashes,
        worktrees,
        editors,
        comments,
        terminals: workbench
            .terminals
            .snapshots()?
            .into_iter()
            .filter(|terminal| terminal.cwd.starts_with(&root))
            .collect(),
        // 开发服务列表是宿主级共享状态，面板只展示当前项目根下的服务。
        dev_servers: workbench
            .dev_servers
            .list()
            .into_iter()
            .filter(|server| server.cwd.starts_with(&root))
            .collect(),
        search_count: 0,
        search_results: Vec::new(),
        search_truncated: false,
        content_results: Vec::new(),
        content_truncated: false,
        files_truncated,
    })
}

fn next_terminal_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    format!(
        "native-terminal-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn next_review_comment_id() -> String {
    // ID 只由进程号和单调计数构成，避免本地评论文件把控制字符带入 UI 标识。
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    format!(
        "native-review-{}-{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn parse_review_comment(
    id: String,
    path: String,
    line: String,
    body: String,
    old_side: bool,
) -> Result<ReviewComment, String> {
    let path = path.trim().to_owned();
    let line = line
        .trim()
        .parse::<u32>()
        .map_err(|_| "review 行号必须是正整数".to_owned())?;
    if path.is_empty() || line == 0 {
        return Err("review 路径和行号不能为空".to_owned());
    }
    let body = body.trim().to_owned();
    if body.is_empty() {
        return Err("review 内容不能为空".to_owned());
    }
    Ok(ReviewComment {
        id,
        path,
        line,
        side: if old_side {
            ReviewSide::Old
        } else {
            ReviewSide::New
        },
        body,
    })
}
