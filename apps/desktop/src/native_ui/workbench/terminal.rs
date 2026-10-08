//! 不依赖 WebView 生命周期的原生 PTY 服务。

use crate::native_paths::NativePaths;
use alacritty_terminal::{
    event::VoidListener,
    grid::{Dimensions, Scroll},
    index::{Point as CellPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, Term, color::COUNT, test::TermSize},
    vte::ansi::{ClearMode, Color, CursorShape, Handler, NamedColor, Processor, Rgb},
};
use gpui::Keystroke;
use parking_lot::Mutex;
use parking_lot::RwLock;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{Read, Write},
    mem::size_of,
    path::{Path, PathBuf},
    sync::{Arc, Weak, atomic::AtomicU64, mpsc},
    thread,
};

#[cfg(test)]
use alacritty_terminal::term::cell::Flags;

const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 30;
const MAX_COLS: u16 = 240;
const MAX_ROWS: u16 = 80;
const MAX_SESSIONS: usize = 32;
// 每个面板占用一个订阅；上限防止窗口重载或插件重复挂载长期占满广播列表。
const MAX_SUBSCRIBERS: usize = 16;
const OUTPUT_QUEUE_CAPACITY: usize = 64;
const MAX_SCROLLBACK_LINES: usize = 1000;
const MAX_SCROLLBACK_BYTES: usize = 1024 * 1024;
/// 键盘、IME 与粘贴的单次写入上限，避免大剪贴板内容在 PTY 边界无限复制。
pub const MAX_TERMINAL_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PtyShell {
    Auto,
    PowerShell,
    PowerShell7,
    GitBash,
    Cmd,
    Custom { path: PathBuf },
}

/// 工作台终端使用的 typed 设置投影；来源是 `AppSettings`，不在 PTY 层解析设置文件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeTerminalSettings {
    /// 设置页选择的 Shell 枚举；实际可执行文件由 PTY 创建时按能力解析。
    pub shell: crate::app_settings::TerminalShell,
    /// GPUI 终端网格使用的单一真实字体族名称。
    pub font_family: String,
    /// false 时必须把对应 Shell 的 no-profile 参数传给子进程。
    pub inherit_system_profile: bool,
}

impl Default for NativeTerminalSettings {
    fn default() -> Self {
        Self {
            shell: crate::app_settings::TerminalShell::Auto,
            font_family: crate::app_settings::DEFAULT_TERMINAL_FONT_FAMILY.to_owned(),
            inherit_system_profile: true,
        }
    }
}

impl From<&crate::app_settings::AppSettings> for NativeTerminalSettings {
    fn from(settings: &crate::app_settings::AppSettings) -> Self {
        Self {
            shell: settings.terminal_shell,
            font_family: settings.terminal_font_family.clone(),
            inherit_system_profile: settings.terminal_inherit_system_profile,
        }
    }
}

impl NativeTerminalSettings {
    fn configured_shell(&self) -> PtyShell {
        match self.shell {
            crate::app_settings::TerminalShell::Auto => PtyShell::Auto,
            crate::app_settings::TerminalShell::PowerShell => PtyShell::PowerShell,
            crate::app_settings::TerminalShell::PowerShell7 => PtyShell::PowerShell7,
            crate::app_settings::TerminalShell::GitBash => PtyShell::GitBash,
            crate::app_settings::TerminalShell::Cmd => PtyShell::Cmd,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum PtyEvent {
    /// 输出只通过偏移通知；面板使用同一事件触发固定屏幕快照刷新。
    Output {
        id: String,
        byte_offset: u64,
    },
    Exited {
        id: String,
        exit_code: u32,
    },
    Scrolled {
        id: String,
        byte_offset: u64,
    },
    Cleared {
        id: String,
        byte_offset: u64,
        generation: u64,
    },
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PtyColor {
    Named { value: u16 },
    Indexed { value: u8 },
    Rgb { red: u8, green: u8, blue: u8 },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyCell {
    pub character: char,
    pub combining: Vec<char>,
    pub foreground: PtyColor,
    pub background: PtyColor,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PtyCursorShape {
    Block,
    HollowBlock,
    Underline,
    Beam,
    Hidden,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyCursor {
    pub row: u16,
    pub column: u16,
    pub shape: PtyCursorShape,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyRgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtySelection {
    /// 选择范围已经按当前 viewport 的 display offset 转换，允许负数表示屏幕上方的历史行。
    pub start_row: i32,
    pub start_column: i32,
    pub end_row: i32,
    pub end_column: i32,
    pub is_block: bool,
}

/// PTY 事件订阅的生命周期 guard；丢弃后会移除宿主 sender，使接收线程退出。
pub struct PtySubscription {
    id: u64,
    subscribers: WeakPtySubscribers,
}

type PtySubscriber = (u64, mpsc::SyncSender<PtyEvent>);
type PtySubscribers = Vec<PtySubscriber>;
type WeakPtySubscribers = Weak<Mutex<PtySubscribers>>;
type SharedPtySubscribers = Arc<Mutex<PtySubscribers>>;

impl Drop for PtySubscription {
    fn drop(&mut self) {
        if let Some(subscribers) = self.subscribers.upgrade() {
            subscribers.lock().retain(|(id, _)| *id != self.id);
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PtySnapshot {
    pub id: String,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    /// Alacritty 当前 viewport 的 cell 与属性；UI 按 cell 绘制，不能退化为纯文本。
    pub cells: Vec<PtyCell>,
    pub palette: Vec<Option<PtyRgb>>,
    pub cursor: Option<PtyCursor>,
    pub selection: Option<PtySelection>,
    pub mode: u32,
    pub display_offset: usize,
    pub byte_offset: u64,
    pub exited: bool,
    pub exit_code: Option<u32>,
    pub generation: u64,
    pub cols: u16,
    pub rows: u16,
}

struct TerminalState {
    parser: Processor,
    term: Term<VoidListener>,
    byte_offset: u64,
    exited: bool,
    exit_code: Option<u32>,
    generation: u64,
}

impl TerminalState {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            parser: Processor::new(),
            term: Term::new(
                Config {
                    // 回滚保留在有限 cell 数量内，避免长期会话无限增长。
                    scrolling_history: bounded_scrollback_lines(cols),
                    ..Config::default()
                },
                &TermSize::new(usize::from(cols), usize::from(rows)),
                VoidListener,
            ),
            byte_offset: 0,
            exited: false,
            exit_code: None,
            generation: 0,
        }
    }

    fn process(&mut self, bytes: &[u8]) {
        self.byte_offset = self.byte_offset.saturating_add(bytes.len() as u64);
        self.parser.advance(&mut self.term, bytes);
    }

    fn clear(&mut self) {
        // 清空按钮要同时丢弃当前 viewport 和回滚；先清屏再清历史，避免清屏过程重新写入空行。
        self.term.clear_screen(ClearMode::All);
        self.term.clear_screen(ClearMode::Saved);
        self.generation = self.generation.saturating_add(1);
    }
}

struct Session {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    /// watcher 独占 child 并阻塞在 wait；关闭路径使用独立 killer 终止它。
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    killer: Arc<Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
    pid: Option<u32>,
    cwd: PathBuf,
    terminal: Arc<Mutex<TerminalState>>,
    output_thread: Option<thread::JoinHandle<()>>,
    watcher_thread: Option<thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct NativePtyManager {
    paths: Arc<NativePaths>,
    settings: Arc<RwLock<NativeTerminalSettings>>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    subscribers: SharedPtySubscribers,
    next_subscriber_id: Arc<AtomicU64>,
}

impl NativePtyManager {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self {
            paths,
            settings: Arc::new(RwLock::new(NativeTerminalSettings::default())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(Vec::new())),
            next_subscriber_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn paths(&self) -> &Arc<NativePaths> {
        &self.paths
    }

    /// 从 NativeHost 的 typed 应用设置刷新后续 PTY 的配置；不直接读取设置文件。
    pub fn sync_settings(&self) -> Result<(), String> {
        let settings = crate::app_settings::get(&self.paths)
            .map_err(|error| format!("读取终端设置失败：{error}"))?;
        self.set_settings(NativeTerminalSettings::from(&settings));
        Ok(())
    }

    pub fn set_settings(&self, settings: NativeTerminalSettings) {
        *self.settings.write() = settings;
    }

    pub fn settings(&self) -> NativeTerminalSettings {
        self.settings.read().clone()
    }

    pub fn subscribe(&self) -> Result<(mpsc::Receiver<PtyEvent>, PtySubscription), String> {
        let mut subscribers = self.subscribers.lock();
        if subscribers.len() >= MAX_SUBSCRIBERS {
            return Err("终端事件订阅数量超过上限".to_owned());
        }
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_QUEUE_CAPACITY);
        let id = self
            .next_subscriber_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        subscribers.push((id, sender));
        let subscription = PtySubscription {
            id,
            subscribers: Arc::downgrade(&self.subscribers),
        };
        Ok((receiver, subscription))
    }

    pub fn shells(&self) -> Vec<PtyShell> {
        let mut shells = vec![PtyShell::Auto];
        #[cfg(windows)]
        {
            if executable_on_path("pwsh.exe").is_some() {
                shells.push(PtyShell::PowerShell7);
            }
            if executable_on_path("powershell.exe").is_some() {
                shells.push(PtyShell::PowerShell);
            }
            if git_bash_path().is_some() {
                shells.push(PtyShell::GitBash);
            }
            shells.push(PtyShell::Cmd);
        }
        #[cfg(not(windows))]
        {
            shells.push(PtyShell::Custom {
                path: std::env::var_os("SHELL")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/bin/sh")),
            });
        }
        shells
    }

    pub fn create(
        &self,
        id: impl Into<String>,
        cwd: &Path,
        cols: Option<u16>,
        rows: Option<u16>,
        shell: PtyShell,
        env: Option<&std::collections::HashMap<String, String>>,
    ) -> Result<(), String> {
        let id = id.into();
        if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err("终端标识无效".to_owned());
        }
        validate_environment(env)?;
        let cwd = cwd
            .canonicalize()
            .map_err(|error| format!("终端目录不可访问：{error}"))?;
        if !cwd.is_dir() {
            return Err("终端工作目录不是目录".to_owned());
        }
        if self.sessions.lock().contains_key(&id) {
            return Err("终端已经存在".to_owned());
        }
        if self.sessions.lock().len() >= MAX_SESSIONS {
            return Err("终端数量超过上限".to_owned());
        }
        let (cols, rows) =
            bounded_dimensions(cols.unwrap_or(DEFAULT_COLS), rows.unwrap_or(DEFAULT_ROWS));
        let settings = self.settings();
        let shell = if matches!(shell, PtyShell::Auto) {
            settings.configured_shell()
        } else {
            shell
        };
        let shell = resolve_shell(shell);
        let shell_cwd = shell_working_directory(&shell, &cwd)?;
        let mut command = shell_command_with_profile(shell, settings.inherit_system_profile)?;
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("创建终端失败：{error}"))?;
        command.cwd(&shell_cwd);
        for (name, value) in env.into_iter().flatten() {
            command.env(name, value);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| format!("启动系统 Shell 失败：{error}"))?;
        drop(pair.slave);
        let writer = match pair.master.take_writer() {
            Ok(writer) => Arc::new(Mutex::new(writer)),
            Err(error) => {
                let mut child = child;
                reap_child(&mut child);
                return Err(format!("打开终端输入失败：{error}"));
            }
        };
        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                let mut child = child;
                reap_child(&mut child);
                return Err(format!("打开终端输出失败：{error}"));
            }
        };
        let pid = child.process_id();
        let killer = child.clone_killer();
        let terminal = Arc::new(Mutex::new(TerminalState::new(cols, rows)));
        let session = Session {
            writer,
            master: pair.master,
            child: Arc::new(Mutex::new(child)),
            killer: Arc::new(Mutex::new(killer)),
            pid,
            cwd: cwd.clone(),
            terminal: Arc::clone(&terminal),
            output_thread: None,
            watcher_thread: None,
        };
        let mut sessions = self.sessions.lock();
        if sessions.contains_key(&id) {
            drop(sessions);
            reap_session(session);
            return Err("终端已经存在".to_owned());
        }
        sessions.insert(id.clone(), session);
        // 读取线程只把每个 PTY 块解析进固定屏幕，并发送有界事件；不会复制完整历史。
        let manager = self.clone();
        let output_id = id.clone();
        let output_thread = thread::spawn(move || {
            let mut buffer = [0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(size) => {
                        let bytes = &buffer[..size];
                        let byte_offset = {
                            let mut terminal = terminal.lock();
                            terminal.process(bytes);
                            terminal.byte_offset
                        };
                        manager.emit(PtyEvent::Output {
                            id: output_id.clone(),
                            byte_offset,
                        });
                    }
                }
            }
        });
        let manager = self.clone();
        let watcher_id = id.clone();
        let watcher_thread = thread::spawn(move || manager.watch_exit(watcher_id));
        let session = sessions.get_mut(&id).expect("刚插入的终端会话必须存在");
        session.output_thread = Some(output_thread);
        session.watcher_thread = Some(watcher_thread);
        Ok(())
    }

    pub fn snapshot(&self, id: &str) -> Result<Option<PtySnapshot>, String> {
        let sessions = self.sessions.lock();
        let Some(session) = sessions.get(id) else {
            return Ok(None);
        };
        let terminal = session.terminal.lock();
        let cols = terminal.term.grid().columns();
        let rows = terminal.term.grid().screen_lines();
        let rendered = render_grid(&terminal.term);
        Ok(Some(PtySnapshot {
            id: id.to_owned(),
            cwd: session.cwd.clone(),
            pid: session.pid,
            cells: rendered.cells,
            palette: rendered.palette,
            cursor: rendered.cursor,
            selection: rendered.selection,
            mode: rendered.mode,
            display_offset: rendered.display_offset,
            byte_offset: terminal.byte_offset,
            exited: terminal.exited,
            exit_code: terminal.exit_code,
            generation: terminal.generation,
            cols: u16::try_from(cols).unwrap_or(MAX_COLS),
            rows: u16::try_from(rows).unwrap_or(MAX_ROWS),
        }))
    }

    /// 滚动当前终端的有限回滚，并通过事件让 UI 重新读取 cell viewport。
    pub fn scroll(&self, id: &str, lines: i32) -> Result<(), String> {
        if lines == 0 {
            return Ok(());
        }
        let sessions = self.sessions.lock();
        let session = sessions.get(id).ok_or("终端不存在或已经退出")?;
        let mut terminal = session.terminal.lock();
        terminal
            .term
            .scroll_display(alacritty_terminal::grid::Scroll::Delta(lines));
        self.emit(PtyEvent::Scrolled {
            id: id.to_owned(),
            byte_offset: terminal.byte_offset,
        });
        Ok(())
    }

    /// 返回当前仍由宿主持有的 PTY 快照；面板刷新使用该入口，不复制内部会话状态。
    pub fn snapshots(&self) -> Result<Vec<PtySnapshot>, String> {
        let ids = self.sessions.lock().keys().cloned().collect::<Vec<_>>();
        let mut snapshots = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(snapshot) = self.snapshot(&id)? {
                snapshots.push(snapshot);
            }
        }
        snapshots.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(snapshots)
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
        if data.len() > MAX_TERMINAL_INPUT_BYTES {
            return Err("终端输入超过 1 MiB 限制".to_owned());
        }
        let writer = self
            .sessions
            .lock()
            .get(id)
            .map(|session| Arc::clone(&session.writer))
            .ok_or("终端不存在或已经退出")?;
        let mut writer = writer.lock();
        writer
            .write_all(data)
            .and_then(|_| writer.flush())
            .map_err(|error| format!("写入终端失败：{error}"))
    }

    /// 终端输入会回到底部并清除本地选择，保持键盘、IME 和粘贴行为一致。
    pub fn input(&self, id: &str, data: &[u8]) -> Result<(), String> {
        if data.len() > MAX_TERMINAL_INPUT_BYTES {
            return Err("终端输入超过 1 MiB 限制".to_owned());
        }
        let (writer, terminal) = {
            let sessions = self.sessions.lock();
            let session = sessions.get(id).ok_or("终端不存在或已经退出")?;
            (Arc::clone(&session.writer), Arc::clone(&session.terminal))
        };
        let mut writer = writer.lock();
        writer
            .write_all(data)
            .and_then(|_| writer.flush())
            .map_err(|error| format!("写入终端失败：{error}"))?;
        drop(writer);
        let mut terminal = terminal.lock();
        terminal.term.scroll_display(Scroll::Bottom);
        terminal.term.selection = None;
        Ok(())
    }

    /// 在 Alacritty 网格中建立本地选择；调用方需用 viewport offset 转换鼠标行号。
    pub fn start_selection(
        &self,
        id: &str,
        kind: SelectionType,
        point: CellPoint,
        side: Side,
    ) -> Result<(), String> {
        let sessions = self.sessions.lock();
        let session = sessions.get(id).ok_or("终端不存在或已经退出")?;
        let mut terminal = session.terminal.lock();
        terminal.term.selection = Some(Selection::new(kind, point, side));
        Ok(())
    }

    /// 更新当前选择的锚点，释放鼠标后仍由终端状态保留该范围。
    pub fn update_selection(&self, id: &str, point: CellPoint, side: Side) -> Result<(), String> {
        let sessions = self.sessions.lock();
        let session = sessions.get(id).ok_or("终端不存在或已经退出")?;
        let mut terminal = session.terminal.lock();
        if let Some(selection) = terminal.term.selection.as_mut() {
            selection.update(point, side);
        }
        Ok(())
    }

    /// 使用 Alacritty 的宽字符、折行和选择类型规则导出文本。
    pub fn selection_to_string(&self, id: &str) -> Result<Option<String>, String> {
        let sessions = self.sessions.lock();
        let session = sessions.get(id).ok_or("终端不存在或已经退出")?;
        let terminal = session.terminal.lock();
        Ok(terminal.term.selection_to_string())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let sessions = self.sessions.lock();
        let session = sessions.get(id).ok_or("终端不存在或已经退出")?;
        let (cols, rows) = bounded_dimensions(cols, rows);
        session
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| format!("调整终端尺寸失败：{error}"))?;
        let mut terminal = session.terminal.lock();
        terminal
            .term
            .resize(TermSize::new(usize::from(cols), usize::from(rows)));
        // resize 不会自动重算历史容量；按新的列宽更新配置，保持 cell 预算有界。
        terminal.term.set_options(Config {
            scrolling_history: bounded_scrollback_lines(cols),
            ..Config::default()
        });
        Ok(())
    }

    pub fn clear_history(&self, id: &str) -> Result<u64, String> {
        let sessions = self.sessions.lock();
        let session = sessions.get(id).ok_or("终端不存在")?;
        let mut terminal = session.terminal.lock();
        terminal.clear();
        let offset = terminal.byte_offset;
        self.emit(PtyEvent::Cleared {
            id: id.to_owned(),
            byte_offset: offset,
            generation: terminal.generation,
        });
        Ok(offset)
    }

    pub fn close(&self, id: &str) -> Result<(), String> {
        if let Some(session) = self.sessions.lock().remove(id) {
            reap_session(session);
        }
        Ok(())
    }

    /// 收割所有仍在运行的 PTY；窗口卸载不会调用此方法，只有宿主退出时调用。
    pub fn shutdown(&self) {
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .drain()
            .map(|(_, session)| session)
            .collect();
        for session in sessions {
            reap_session(session);
        }
        self.subscribers.lock().clear();
    }

    fn emit(&self, event: PtyEvent) {
        self.subscribers.lock().retain(|(_, subscriber)| {
            match subscriber.try_send(event.clone()) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        });
    }

    fn watch_exit(&self, id: String) {
        let (child, terminal) = {
            let sessions = self.sessions.lock();
            let Some(session) = sessions.get(&id) else {
                return;
            };
            (Arc::clone(&session.child), Arc::clone(&session.terminal))
        };
        let status = {
            let mut child = child.lock();
            match child.wait() {
                Ok(status) => status,
                Err(error) => {
                    tracing::warn!(%error, terminal_id = %id, "等待终端退出失败");
                    return;
                }
            }
        };
        let code = status.exit_code();
        let should_emit = {
            let mut terminal = terminal.lock();
            let should_emit = !terminal.exited;
            terminal.exited = true;
            terminal.exit_code = Some(code);
            should_emit
        };
        if should_emit && self.sessions.lock().contains_key(&id) {
            self.emit(PtyEvent::Exited {
                id,
                exit_code: code,
            });
        }
    }
}

impl Default for NativePtyManager {
    fn default() -> Self {
        let paths = crate::native_paths::NativePaths {
            data_root: PathBuf::new(),
            home_dir: PathBuf::new(),
            documents_dir: PathBuf::new(),
        };
        Self::new(Arc::new(paths))
    }
}

impl Drop for NativePtyManager {
    fn drop(&mut self) {
        if Arc::strong_count(&self.sessions) != 1 {
            return;
        }
        self.shutdown();
    }
}

struct RenderedGrid {
    cells: Vec<PtyCell>,
    palette: Vec<Option<PtyRgb>>,
    cursor: Option<PtyCursor>,
    selection: Option<PtySelection>,
    mode: u32,
    display_offset: usize,
}

fn render_grid(term: &Term<VoidListener>) -> RenderedGrid {
    let cols = term.grid().columns();
    let rows = term.grid().screen_lines();
    let content = term.renderable_content();
    let mut cells = (0..cols.saturating_mul(rows))
        .map(|_| PtyCell {
            character: ' ',
            combining: Vec::new(),
            foreground: PtyColor::Named {
                value: NamedColor::Foreground as u16,
            },
            background: PtyColor::Named {
                value: NamedColor::Background as u16,
            },
            flags: 0,
        })
        .collect::<Vec<_>>();
    for indexed in content.display_iter {
        let row = indexed.point.line.0 + content.display_offset as i32;
        let Ok(row) = usize::try_from(row) else {
            continue;
        };
        let column = indexed.point.column.0;
        if row >= rows || column >= cols {
            continue;
        }
        let cell = indexed.cell;
        cells[row * cols + column] = PtyCell {
            character: cell.c,
            combining: cell.zerowidth().unwrap_or_default().to_vec(),
            foreground: snapshot_color(cell.fg),
            background: snapshot_color(cell.bg),
            flags: cell.flags.bits(),
        };
    }
    let cursor = match content.cursor.shape {
        CursorShape::Hidden => None,
        shape => {
            let row = content.cursor.point.line.0 + content.display_offset as i32;
            let column = content.cursor.point.column.0;
            (row >= 0 && (row as usize) < rows && column < cols).then_some(PtyCursor {
                row: row as u16,
                column: column as u16,
                shape: match shape {
                    CursorShape::Block => PtyCursorShape::Block,
                    CursorShape::HollowBlock => PtyCursorShape::HollowBlock,
                    CursorShape::Underline => PtyCursorShape::Underline,
                    CursorShape::Beam => PtyCursorShape::Beam,
                    CursorShape::Hidden => PtyCursorShape::Hidden,
                },
            })
        }
    };
    let palette = (0..COUNT)
        .map(|index| term.colors()[index].map(snapshot_rgb))
        .collect();
    RenderedGrid {
        cells,
        palette,
        cursor,
        selection: content.selection.map(|selection| PtySelection {
            start_row: selection.start.line.0
                + i32::try_from(content.display_offset).unwrap_or(i32::MAX),
            start_column: i32::try_from(selection.start.column.0).unwrap_or(i32::MAX),
            end_row: selection.end.line.0
                + i32::try_from(content.display_offset).unwrap_or(i32::MAX),
            end_column: i32::try_from(selection.end.column.0).unwrap_or(i32::MAX),
            is_block: selection.is_block,
        }),
        mode: content.mode.bits(),
        display_offset: content.display_offset,
    }
}

fn snapshot_color(color: Color) -> PtyColor {
    match color {
        Color::Named(name) => PtyColor::Named { value: name as u16 },
        Color::Indexed(value) => PtyColor::Indexed { value },
        Color::Spec(rgb) => PtyColor::Rgb {
            red: rgb.r,
            green: rgb.g,
            blue: rgb.b,
        },
    }
}

fn snapshot_rgb(rgb: Rgb) -> PtyRgb {
    PtyRgb {
        red: rgb.r,
        green: rgb.g,
        blue: rgb.b,
    }
}

/// 仅保留给终端网格回归测试；生产快照直接发布 cell，避免重复生成文本。
#[cfg(test)]
fn render_screen(term: &Term<VoidListener>) -> Vec<u8> {
    let grid = render_grid(term);
    render_cells_text(
        &grid.cells,
        term.grid().columns(),
        term.grid().screen_lines(),
    )
}

#[cfg(test)]
fn render_cells_text(cells: &[PtyCell], cols: usize, rows: usize) -> Vec<u8> {
    let mut text = String::with_capacity(cols.saturating_add(1).saturating_mul(rows));
    for row in 0..rows {
        let mut line = String::new();
        for col in 0..cols {
            let Some(cell) = cells.get(row.saturating_mul(cols).saturating_add(col)) else {
                continue;
            };
            let flags = Flags::from_bits_retain(cell.flags);
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                // 保留宽字符的第二个终端列，避免快照文本把后续字符左移。
                line.push(' ');
                continue;
            }
            line.push(cell.character);
            line.extend(cell.combining.iter().copied());
        }
        text.push_str(line.trim_end_matches(' '));
        if row + 1 < rows {
            text.push('\n');
        }
    }
    text.into_bytes()
}

fn bounded_scrollback_lines(cols: u16) -> usize {
    let bytes_per_line = usize::from(cols)
        .saturating_mul(size_of::<alacritty_terminal::term::cell::Cell>())
        .max(1);
    MAX_SCROLLBACK_LINES
        .min(MAX_SCROLLBACK_BYTES / bytes_per_line)
        .max(1)
}

/// 将 GPUI 原生键盘事件编码为 xterm 输入字节；终端不再依赖隐藏 TextInput 模拟逐行提交。
pub fn terminal_key_bytes(keystroke: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let held = &keystroke.modifiers;
    if held.platform {
        return None;
    }
    if !held.control
        && !held.function
        && let Some(character) = keystroke.key_char.as_deref()
        && !character.is_empty()
    {
        let mut bytes = Vec::with_capacity(character.len() + usize::from(held.alt));
        if held.alt {
            bytes.push(0x1b);
        }
        bytes.extend_from_slice(character.as_bytes());
        return Some(bytes);
    }
    let code = 1 + u8::from(held.shift) + 2 * u8::from(held.alt) + 4 * u8::from(held.control);
    let cursor = |end: char| match (code, app_cursor) {
        (1, true) => format!("\x1bO{end}"),
        (1, false) => format!("\x1b[{end}"),
        _ => format!("\x1b[1;{code}{end}"),
    };
    let tilde = |number: u8| match code {
        1 => format!("\x1b[{number}~"),
        _ => format!("\x1b[{number};{code}~"),
    };
    let function = |end: char| match code {
        1 => format!("\x1bO{end}"),
        _ => format!("\x1b[1;{code}{end}"),
    };
    let sent = match keystroke.key.as_str() {
        "enter" if held.alt => "\x1b\r".to_owned(),
        "enter" => "\r".to_owned(),
        "backspace" if held.control => "\x08".to_owned(),
        "backspace" if held.alt => "\x1b\x7f".to_owned(),
        "backspace" => "\x7f".to_owned(),
        "tab" if held.shift => "\x1b[Z".to_owned(),
        "tab" => "\t".to_owned(),
        "escape" => "\x1b".to_owned(),
        "space" if held.control => return control_key("space", held.alt),
        "space" => " ".to_owned(),
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => function('P'),
        "f2" => function('Q'),
        "f3" => function('R'),
        "f4" => function('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        key if held.control => return control_key(key, held.alt),
        _ => return None,
    };
    Some(sent.into_bytes())
}

/// 将剪贴板文本转换为终端输入；换行统一为 CR，括号粘贴时剥离原文 ESC。
pub fn terminal_paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut normalized = String::with_capacity(text.len());
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                normalized.push('\r');
            }
            '\n' => normalized.push('\r'),
            character => normalized.push(character),
        }
    }
    if bracketed {
        normalized.retain(|character| character != '\x1b');
        format!("\x1b[200~{normalized}\x1b[201~").into_bytes()
    } else {
        normalized.into_bytes()
    }
}

fn control_key(key: &str, alt: bool) -> Option<Vec<u8>> {
    let code = match key {
        "space" | "@" | "2" => 0,
        "[" | "3" => 0x1b,
        "\\" | "4" => 0x1c,
        "]" | "5" => 0x1d,
        "^" | "6" => 0x1e,
        "_" | "-" | "7" => 0x1f,
        "?" | "8" => 0x7f,
        letter if letter.len() == 1 && letter.as_bytes()[0].is_ascii_lowercase() => {
            letter.as_bytes()[0] - b'a' + 1
        }
        _ => return None,
    };
    Some(if alt { vec![0x1b, code] } else { vec![code] })
}

fn bounded_dimensions(cols: u16, rows: u16) -> (u16, u16) {
    (cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_ROWS))
}

/// 将 canonicalize 产生的 Win32 扩展路径转换成当前 Shell 能接受的进程 cwd。
///
/// `cmd.exe` 能使用普通盘符路径，但会把 `\\?\D:\...` 当成不支持的 UNC
/// 路径并回退到 `C:\Windows`。UNC 没有可跨 Shell 伪造的盘符：PowerShell 和
/// Git Bash 使用标准 UNC 形式；CMD 明确拒绝 UNC，避免静默回退到 `C:\Windows`。
/// 未知自定义 Shell 不假设其能力，保留调用方的真实路径。
fn shell_working_directory(_shell: &PtyShell, path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy().replace('/', "\\");
        const EXTENDED_UNC_PREFIX: &str = r"\\?\UNC\";
        const EXTENDED_PREFIX: &str = r"\\?\";

        let extended_unc = strip_ascii_case_insensitive_prefix(&text, EXTENDED_UNC_PREFIX);
        let extended_drive = strip_ascii_case_insensitive_prefix(&text, EXTENDED_PREFIX)
            .filter(|local| local.as_bytes().get(1) == Some(&b':'));
        let standard_unc = text.starts_with(r"\\")
            && !text.starts_with(EXTENDED_PREFIX)
            && !text.starts_with(r"\\.\");
        if matches!(_shell, PtyShell::Auto | PtyShell::Cmd)
            && (extended_unc.is_some() || standard_unc)
        {
            return Err("CMD不支持UNC工作目录，请选择PowerShell".to_owned());
        }

        if let Some(unc) = extended_unc {
            return match _shell {
                PtyShell::PowerShell | PtyShell::PowerShell7 | PtyShell::GitBash => {
                    Ok(PathBuf::from(format!(r"\\{unc}")))
                }
                PtyShell::Auto | PtyShell::Cmd | PtyShell::Custom { .. } => Ok(path.to_path_buf()),
            };
        }
        if let Some(local) = extended_drive {
            return Ok(PathBuf::from(local));
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(windows)]
fn strip_ascii_case_insensitive_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

#[cfg(windows)]
fn resolve_shell(shell: PtyShell) -> PtyShell {
    if !matches!(shell, PtyShell::Auto) {
        return shell;
    }
    // Auto 只在已确认可执行的候选中选择，顺序与设置页展示的能力一致。
    if executable_on_path("pwsh.exe").is_some() {
        PtyShell::PowerShell7
    } else if executable_on_path("powershell.exe").is_some() {
        PtyShell::PowerShell
    } else if git_bash_path().is_some() {
        PtyShell::GitBash
    } else {
        PtyShell::Cmd
    }
}

#[cfg(not(windows))]
fn resolve_shell(shell: PtyShell) -> PtyShell {
    if matches!(shell, PtyShell::Auto) {
        PtyShell::Custom {
            path: std::env::var_os("SHELL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/bin/sh")),
        }
    } else {
        shell
    }
}

fn shell_command_with_profile(
    shell: PtyShell,
    inherit_system_profile: bool,
) -> Result<CommandBuilder, String> {
    #[cfg(windows)]
    {
        let (path, args) = match shell {
            PtyShell::Auto | PtyShell::Cmd => (
                std::env::var_os("COMSPEC")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("cmd.exe")),
                (!inherit_system_profile)
                    .then(|| "/d".to_owned())
                    .into_iter()
                    .collect(),
            ),
            PtyShell::PowerShell => (
                PathBuf::from("powershell.exe"),
                (!inherit_system_profile)
                    .then(|| "-NoProfile".to_owned())
                    .into_iter()
                    .collect(),
            ),
            PtyShell::PowerShell7 => (
                PathBuf::from("pwsh.exe"),
                (!inherit_system_profile)
                    .then(|| "-NoProfile".to_owned())
                    .into_iter()
                    .collect(),
            ),
            PtyShell::GitBash => (
                git_bash_path().ok_or("Git Bash 当前不可用")?,
                [
                    (!inherit_system_profile).then(|| "--noprofile".to_owned()),
                    Some("-i".to_owned()),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ),
            PtyShell::Custom { path } => (path, Vec::new()),
        };
        let mut command = CommandBuilder::new(path);
        command.args(args);
        Ok(command)
    }
    #[cfg(not(windows))]
    {
        let _ = inherit_system_profile;
        let path = match shell {
            PtyShell::Custom { path } => path,
            _ => std::env::var_os("SHELL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/bin/sh")),
        };
        let mut command = CommandBuilder::new(path);
        command.arg("-i");
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        Ok(command)
    }
}

fn reap_child(child: &mut Box<dyn portable_pty::Child + Send + Sync>) {
    let _ = child.kill();
    let _ = child.wait();
}

fn reap_session(session: Session) {
    let Session {
        master,
        child,
        killer,
        mut output_thread,
        mut watcher_thread,
        ..
    } = session;
    let _ = killer.lock().kill();
    let mut child = child.lock();
    let _ = child.wait();
    drop(child);
    drop(master);
    if let Some(thread) = output_thread.take() {
        let _ = thread.join();
    }
    if let Some(thread) = watcher_thread.take() {
        let _ = thread.join();
    }
}

fn validate_environment(
    env: Option<&std::collections::HashMap<String, String>>,
) -> Result<(), String> {
    let Some(env) = env else {
        return Ok(());
    };
    if env.len() > 256
        || env
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 1024 * 1024
    {
        return Err("终端环境变量超过大小限制".to_owned());
    }
    if env.iter().any(|(key, value)| {
        key.is_empty()
            || key.len() > 256
            || key.contains(['=', '\0'])
            || value.len() > 65_536
            || value.contains('\0')
    }) {
        return Err("终端环境变量格式无效".to_owned());
    }
    Ok(())
}

#[cfg(windows)]
fn executable_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(';')
        .find_map(|directory| {
            let path = Path::new(directory).join(name);
            path.is_file().then_some(path)
        })
}

#[cfg(windows)]
fn git_bash_path() -> Option<PathBuf> {
    let candidates = [
        std::env::var_os("ProgramFiles").map(|root| PathBuf::from(root).join("Git/bin/bash.exe")),
        std::env::var_os("ProgramFiles(x86)")
            .map(|root| PathBuf::from(root).join("Git/bin/bash.exe")),
        std::env::var_os("LOCALAPPDATA")
            .map(|root| PathBuf::from(root).join("Programs/Git/bin/bash.exe")),
    ];
    candidates.into_iter().flatten().find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::{
        index::{Column, Line, Point as CellPoint, Side},
        selection::{Selection, SelectionType},
    };
    use std::collections::HashMap;
    #[cfg(windows)]
    use std::{ffi::OsString, path::Path};

    #[cfg(windows)]
    #[test]
    fn cmd_spawn_receives_regular_drive_cwd_for_extended_project_path() {
        let extended = Path::new(r"\\?\D:\projects\keen-code");
        for shell in [PtyShell::Auto, PtyShell::Cmd] {
            let shell_cwd = shell_working_directory(&shell, extended).expect("盘符路径应可规范化");
            let mut command = shell_command_with_profile(shell, false).expect("CMD 命令应可构造");
            command.cwd(&shell_cwd);

            assert_eq!(
                command.get_cwd(),
                Some(&OsString::from(r"D:\projects\keen-code"))
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn shell_working_directory_uses_unc_per_shell_capability() {
        let extended_unc = Path::new(r"\\?\UNC\server\share\project");
        let regular_unc = Path::new(r"\\server\share\project");
        assert_eq!(
            shell_working_directory(&PtyShell::PowerShell, extended_unc)
                .expect("PowerShell 应可使用 UNC 工作目录"),
            regular_unc
        );
        assert_eq!(
            shell_working_directory(&PtyShell::GitBash, extended_unc)
                .expect("Git Bash 应可使用 UNC 工作目录"),
            regular_unc
        );
        assert_eq!(
            shell_working_directory(
                &PtyShell::Custom {
                    path: PathBuf::from("custom"),
                },
                extended_unc,
            )
            .expect("未知 Shell 应保留原路径"),
            extended_unc
        );
    }

    #[cfg(windows)]
    #[test]
    fn cmd_rejects_standard_and_extended_unc_working_directory() {
        let paths = [
            Path::new(r"\\server\share\project"),
            Path::new(r"\\?\UNC\server\share\project"),
        ];
        for shell in [PtyShell::Auto, PtyShell::Cmd] {
            for path in paths {
                assert_eq!(
                    shell_working_directory(&shell, path),
                    Err("CMD不支持UNC工作目录，请选择PowerShell".to_owned())
                );
            }
        }
    }

    #[test]
    fn create_requires_an_existing_directory_before_spawning() {
        let manager = NativePtyManager::default();
        let temporary = tempfile::tempdir().expect("创建临时目录");
        let missing = temporary.path().join("missing");
        let error = manager
            .create("terminal", &missing, None, None, PtyShell::Auto, None)
            .expect_err("不存在的终端目录必须被拒绝");
        assert!(error.contains("终端目录不可访问"));
    }

    #[test]
    fn session_operations_require_a_registered_id() {
        let manager = NativePtyManager::default();
        assert!(manager.write("missing", b"input").is_err());
        assert!(manager.resize("missing", 0, 0).is_err());
        assert!(manager.clear_history("missing").is_err());
        assert!(manager.close("missing").is_ok());
    }

    #[test]
    fn dropping_pty_subscription_disconnects_its_receiver() {
        let manager = NativePtyManager::default();
        let (receiver, subscription) = manager.subscribe().expect("测试订阅应成功");
        drop(subscription);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn pty_subscriptions_are_bounded() {
        let manager = NativePtyManager::default();
        let subscriptions = (0..MAX_SUBSCRIBERS)
            .map(|_| manager.subscribe().expect("订阅应在上限内成功"))
            .collect::<Vec<_>>();
        assert!(manager.subscribe().is_err());
        drop(subscriptions);
        assert!(manager.subscribe().is_ok());
    }

    #[test]
    fn environment_rejects_control_characters() {
        let mut environment = HashMap::new();
        environment.insert("BAD\0NAME".to_owned(), "value".to_owned());
        assert!(validate_environment(Some(&environment)).is_err());
    }

    #[test]
    fn terminal_dimensions_are_bounded() {
        assert_eq!(bounded_dimensions(0, 0), (1, 1));
        assert_eq!(bounded_dimensions(u16::MAX, u16::MAX), (MAX_COLS, MAX_ROWS));
    }

    #[test]
    fn terminal_scrollback_stays_nonzero_and_bounded_by_cell_budget() {
        let narrow = bounded_scrollback_lines(40);
        let wide = bounded_scrollback_lines(MAX_COLS);
        assert!((1..=MAX_SCROLLBACK_LINES).contains(&narrow));
        assert!((1..=MAX_SCROLLBACK_LINES).contains(&wide));
        assert!(wide <= narrow);
    }

    #[test]
    fn terminal_keys_encode_control_cursor_and_typed_alt_input() {
        let ctrl_c = Keystroke::parse("ctrl-c").expect("ctrl-c 应可解析");
        assert_eq!(terminal_key_bytes(&ctrl_c, false), Some(vec![0x03]));

        let cursor = Keystroke::parse("shift-up").expect("shift-up 应可解析");
        assert_eq!(
            terminal_key_bytes(&cursor, true),
            Some(b"\x1b[1;2A".to_vec())
        );

        let typed = Keystroke::parse("alt-a->å").expect("带输入字符的 Alt 键应可解析");
        assert_eq!(
            terminal_key_bytes(&typed, false),
            Some("\x1bå".as_bytes().to_vec())
        );
    }

    #[test]
    fn terminal_paste_normalizes_line_endings_and_brackets_safely() {
        assert_eq!(terminal_paste_bytes("a\r\nb\nc\r", false), b"a\rb\rc\r");
        assert_eq!(
            terminal_paste_bytes("a\x1b[201~b", true),
            b"\x1b[200~a[201~b\x1b[201~"
        );
    }

    #[test]
    fn snapshot_selection_uses_viewport_coordinates() {
        let mut state = TerminalState::new(12, 4);
        state.process(b"abc");
        state.term.selection = Some(Selection::new(
            SelectionType::Simple,
            CellPoint::new(Line(0), Column(0)),
            Side::Left,
        ));
        state
            .term
            .selection
            .as_mut()
            .expect("选择应存在")
            .update(CellPoint::new(Line(0), Column(2)), Side::Right);

        let selection = render_grid(&state.term).selection.expect("快照应包含选择");
        assert_eq!(selection.start_row, 0);
        assert_eq!(selection.start_column, 0);
        assert_eq!(selection.end_row, 0);
        assert_eq!(selection.end_column, 2);
        assert!(!selection.is_block);
    }

    #[test]
    fn alacritty_grid_preserves_unicode_width_and_sgr_attributes() {
        let mut state = TerminalState::new(12, 4);
        state.process("中a".as_bytes());
        let rendered = String::from_utf8(render_screen(&state.term)).expect("屏幕应为 UTF-8");
        assert_eq!(
            rendered
                .lines()
                .next()
                .unwrap()
                .chars()
                .take(3)
                .collect::<String>(),
            "中 a"
        );

        state.process(b"\x1b[31mR");
        assert!(matches!(
            state.term.grid()[Line(0)][Column(3)].fg,
            alacritty_terminal::vte::ansi::Color::Named(
                alacritty_terminal::vte::ansi::NamedColor::Red
            )
        ));
    }

    #[test]
    fn ansi_screen_handles_cr_backspace_cursor_and_clear() {
        let mut state = TerminalState::new(12, 4);
        state.process(b"hello\rworld");
        assert!(
            String::from_utf8(render_screen(&state.term))
                .expect("屏幕应为 UTF-8")
                .starts_with("world")
        );

        state.process(b"\x1b[2;3HX");
        let rendered = String::from_utf8(render_screen(&state.term)).expect("屏幕应为 UTF-8");
        assert_eq!(rendered.lines().nth(1).unwrap().chars().nth(2), Some('X'));

        state.process(b"\x1b[2K\x1b[3;1HZ\x08Y");
        let rendered = String::from_utf8(render_screen(&state.term)).expect("屏幕应为 UTF-8");
        assert_eq!(
            rendered.lines().nth(2).unwrap(),
            "Y",
            "退格会把光标移回 Z 所在列，后续字符覆盖 Z"
        );
        assert_eq!(
            state.term.grid().cursor.point,
            CellPoint::new(Line(2), Column(1))
        );
        assert_eq!(state.term.grid()[Line(2)][Column(1)].c, ' ');
    }

    #[test]
    fn clear_history_clears_the_visible_screen_without_scrollback() {
        let mut state = TerminalState::new(12, 4);
        state.process(b"visible");
        state.clear();

        let rendered = String::from_utf8(render_screen(&state.term)).expect("屏幕应为 UTF-8");
        assert!(
            rendered
                .chars()
                .all(|character| character == '\n' || character == ' ')
        );
        assert_eq!(state.generation, 1);
    }

    #[test]
    fn alternate_screen_does_not_destroy_main_screen() {
        let mut state = TerminalState::new(10, 3);
        state.process(b"main\x1b[?1049halt\x1b[?1049l");
        let rendered = String::from_utf8(render_screen(&state.term)).expect("屏幕应为 UTF-8");
        assert!(rendered.starts_with("main"));
        assert!(!rendered.contains("alt"));
    }
}
