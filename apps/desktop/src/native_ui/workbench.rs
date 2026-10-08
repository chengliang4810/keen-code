//! 原生工作台服务聚合。
//!
//! NativeHost 创建并持有一个工作台实例，GPUI 面板通过快照方法和事件订阅
//! 获取数据。所有需要访问本地文件的服务显式接收同一份 `NativePaths`，避免
//! 在服务内部重新推导 HOME、数据目录或工作区路径。

use std::sync::Arc;

use crate::native_paths::NativePaths;

pub mod dev_servers;
pub mod diff;
pub mod editors;
pub mod files;
pub mod git;
pub mod panel;
pub mod profile;
pub mod skills;
pub mod terminal;
pub mod worktrees;

pub use dev_servers::{
    DevServerEvent, DevServerEventSink, DevServerInput, DevServerSnapshot, NativeDevServers,
};
pub use diff::{DiffDocument, DiffOptions, DiffService, ReviewComment, ReviewSide};
pub use editors::{EditorInfo, EditorService, OpenEditorResult};
pub use files::{
    ContentMatch, ContentSearchInput, ContentSearchResult, FileEntry, FileKind, FileSearchInput,
    FileSearchResult, FileService, TextDocument,
};
pub use git::{GitAction, GitFileStatus, GitService, GitStatus, StashEntry};
pub use panel::{NativeWorkbenchPanel, NativeWorkbenchPanelHandle, WorkbenchPane};
pub use profile::{NativeProfile, ProfileService};
pub use skills::{NativeSkill, SkillService};
pub use terminal::{
    MAX_TERMINAL_INPUT_BYTES, NativePtyManager, NativeTerminalSettings, PtyCell, PtyColor,
    PtyCursorShape, PtyEvent, PtyRgb, PtySelection, PtyShell, PtySnapshot, PtySubscription,
    terminal_key_bytes, terminal_paste_bytes,
};
pub use worktrees::{NativeWorktrees, WorktreeEntry, WorktreeRequest, WorktreeResult};

/// 原生工作台的长期服务集合。PTY 与开发命令服务被 Arc 持有，面板卸载不会
/// 自动关闭后台进程；只有显式调用 stop/close/shutdown 才会释放它们。
pub struct NativeWorkbench {
    pub files: FileService,
    pub diff: DiffService,
    pub git: GitService,
    pub worktrees: NativeWorktrees,
    pub terminals: Arc<NativePtyManager>,
    pub dev_servers: Arc<NativeDevServers>,
    pub editors: EditorService,
    pub skills: SkillService,
    pub profile: ProfileService,
}

impl NativeWorkbench {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self {
            files: FileService::new(Arc::clone(&paths)),
            diff: DiffService::new(Arc::clone(&paths)),
            git: GitService::new(Arc::clone(&paths)),
            worktrees: NativeWorktrees::new(Arc::clone(&paths)),
            terminals: Arc::new(NativePtyManager::new(Arc::clone(&paths))),
            dev_servers: Arc::new(NativeDevServers::new(Arc::clone(&paths))),
            editors: EditorService::new(Arc::clone(&paths)),
            skills: SkillService::new(Arc::clone(&paths)),
            profile: ProfileService::new(Arc::clone(&paths)),
        }
    }

    /// 让工作台通过现有 typed 设置入口刷新终端配置；PTY 会话本身保持不变。
    pub fn sync_terminal_settings(&self) -> Result<(), String> {
        self.terminals.sync_settings()
    }

    pub fn terminal_settings(&self) -> NativeTerminalSettings {
        self.terminals.settings()
    }

    /// 关闭工作台持有的外部进程；窗口面板销毁不会调用此方法，宿主退出时显式调用。
    pub async fn shutdown(&self) -> Result<(), String> {
        self.terminals.shutdown();
        self.dev_servers.shutdown().await
    }
}
