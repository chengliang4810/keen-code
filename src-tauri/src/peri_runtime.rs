//! peri ACP 运行时：进程内装配、事件泵与多 Session 生命周期。
//!
//! KeenCode 直接使用 MpscTransport 驱动 peri ACP。每个 Session 的工作目录、标题、
//! 状态、错误和待回答请求都按 Session ID 隔离；界面焦点只决定
//! `session_get_state` 展示哪个快照，不参与命令授权。

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use peri_acp::host::assemble::{EmbeddedHostAssemblyInput, assemble_embedded_server_config};
use peri_acp::host::run_acp_server;
use peri_acp::provider::{LlmProvider, PeriConfig};
use peri_acp::session::SessionManager;
use peri_acp::transport::AcpTransport;
use peri_acp::transport::mpsc::mpsc_transport_pair;
use peri_acp::transport::types::{IncomingMessage, RequestId};
use peri_acp_types::ports::McpPoolPort;
use peri_acp_types::store::ThreadStore;
use peri_agent::agent::model_bridge::AgentModelBridge;
use peri_agent::agent::react::ReactLLM;
use peri_agent::messages::BaseMessage;
use peri_middlewares::mcp::{McpClientPool, McpConfigFile, McpInitStatus};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::diagnostics::Diagnostics;
use crate::providers;

/// 桌面端后台任务面板中的单个运行中任务。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTaskInfo {
    /// 拥有该后台任务的根 Session。
    pub session_id: String,
    /// Peri TaskManager 分配的稳定任务标识。
    pub task_id: String,
    /// 后台任务类别，用于界面区分 Shell 与子 Agent。
    pub kind: peri_acp_types::tasks::BgTaskKind,
    /// 子 Agent 线程标识；Shell 为 null。
    pub child_thread_id: Option<String>,
    /// 任务启动时记录的单行摘要。
    pub summary: String,
    /// 任务启动时间（UTC RFC 3339）。
    pub started_at: String,
    /// 查询时已经运行的毫秒数。
    pub duration_ms: u64,
    /// 仅后台 Shell 具有的系统进程标识。
    pub pid: Option<u32>,
}

/// 只把仍在运行的 Peri 后台任务投影到桌面端 DTO。
fn running_background_task(
    session_id: &str,
    task: peri_agent::agent::async_tasks::BgTaskInfo,
) -> Option<BackgroundTaskInfo> {
    if !matches!(
        task.status,
        peri_agent::agent::async_tasks::BackgroundTaskStatus::Running
    ) {
        return None;
    }
    Some(BackgroundTaskInfo {
        session_id: session_id.to_owned(),
        task_id: task.task_id,
        kind: task.kind,
        child_thread_id: task.child_thread_id,
        summary: task.summary,
        started_at: task.started_at,
        duration_ms: task.duration_ms,
        pid: task.pid,
    })
}

/// 前端可见的会话状态（与 api.ts SessionSnapshot.state 对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// 已登记但尚未加载或当前没有界面焦点。
    Idle,
    /// 正在加载已有 Session。
    Connecting,
    /// 已加载且当前没有执行回合。
    Ready,
    /// 当前 Session 正在执行模型回合。
    Streaming,
    /// ACP transport 或 Session 加载已断开。
    Disconnected,
}

/// Host 已接受 turn 后、`session/prompt` 真正进入 Peri 前的派发阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnDispatchState {
    /// Host 已确认 Streaming，后台仍在准备设置、记忆与个性化上下文。
    Preparing,
    /// 后台已取得派发权，`session/prompt` 正在运行或等待响应。
    Dispatched,
    /// stop 已绑定本 turn 并向 Peri 发出取消；重复 stop 不得再次发送。
    CancelRequested,
}

/// `session_stop` 在 Session 锁内绑定当前 turn 后得到的唯一动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStopAction {
    /// prompt 尚未派发，Host 已本地收口该 turn。
    CompleteLocally(String),
    /// prompt 已取得派发权，需要向 Peri 发送一次 cancel。
    NotifyRuntime(String),
    /// 同一 turn 已请求取消，不重复发送。
    AlreadyRequested(String),
}

/// 一个已登记 Session 的独立运行时状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSession {
    /// peri ThreadStore 分配的唯一标识。
    pub session_id: String,
    /// 创建 Session 时已经授权且规范化的工作目录。
    pub cwd: String,
    /// 当前标题；尚未生成标题时为空。
    pub title: Option<String>,
    /// 当前 Session 独立的执行状态。
    pub state: SessionState,
    /// 当前 Session 最近一次运行时错误。
    pub last_error: Option<String>,
    /// 此 Session 是否已经加载进当前 ACP server 进程。
    loaded: bool,
    /// 已被 Host 接受、尚未收到完成边界的前台 client turn 标识。
    active_turn_id: Option<String>,
    /// 最近一个已经收口的前台 turn；只用于 WebView 恢复窗口校验 done。
    last_completed_turn_id: Option<String>,
    /// 当前前台 turn 的 prompt 派发/取消阶段；与 active_turn_id 同生共灭。
    active_turn_dispatch: Option<TurnDispatchState>,
}

impl RuntimeSession {
    /// 构造一个经过目录授权的 Session 运行时记录。
    pub fn new(
        session_id: String,
        cwd: String,
        title: Option<String>,
        state: SessionState,
        loaded: bool,
    ) -> Self {
        Self {
            session_id,
            cwd,
            title,
            state,
            last_error: None,
            loaded,
            active_turn_id: None,
            last_completed_turn_id: None,
            active_turn_dispatch: None,
        }
    }

    /// 返回此 Session 是否已经加载进 ACP server。
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }
}

/// 多 Session 登记表与当前界面焦点。
#[derive(Default)]
struct RuntimeSessions {
    /// 当前界面聚焦的 Session；不作为权限依据。
    focused_session_id: Option<String>,
    /// 按 Session ID 保存的独立运行时记录。
    by_id: HashMap<String, RuntimeSession>,
}

/// MCP 按需初始化与配置热重载状态。
#[derive(Default)]
struct McpRuntimeState {
    /// 最近一次已经应用到连接池的配置内容摘要。
    applied_fingerprint: Option<[u8; 32]>,
}

impl McpRuntimeState {
    /// 判断指定配置内容是否已经应用，避免每个任务重复初始化连接池。
    fn is_current(&self, fingerprint: &[u8; 32]) -> bool {
        self.applied_fingerprint.as_ref() == Some(fingerprint)
    }
}

impl RuntimeSessions {
    /// 返回全部仍在运行的前台 turn，供 WebView 重挂载后一次性恢复关联。
    fn active_turns(&self) -> Vec<ActiveTurnSnapshot> {
        let mut turns = self
            .by_id
            .values()
            .filter_map(|session| {
                session
                    .active_turn_id
                    .as_ref()
                    .map(|turn_id| ActiveTurnSnapshot {
                        session_id: session.session_id.clone(),
                        turn_id: turn_id.clone(),
                    })
            })
            .collect::<Vec<_>>();
        turns.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        turns
    }

    /// 返回每个 Session 最近完成的 turn，供恢复窗口精确识别终止事件。
    fn completed_turns(&self) -> Vec<ActiveTurnSnapshot> {
        let mut turns = self
            .by_id
            .values()
            .filter_map(|session| {
                session
                    .last_completed_turn_id
                    .as_ref()
                    .map(|turn_id| ActiveTurnSnapshot {
                        session_id: session.session_id.clone(),
                        turn_id: turn_id.clone(),
                    })
            })
            .collect::<Vec<_>>();
        turns.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        turns
    }

    /// 同步持久元数据，并拒绝在运行时静默替换 Session 工作目录。
    fn sync_metadata(
        &mut self,
        session_id: String,
        cwd: String,
        title: Option<String>,
    ) -> Result<()> {
        if let Some(session) = self.by_id.get_mut(&session_id) {
            if session.cwd != cwd {
                anyhow::bail!("Session 运行目录与持久元数据不一致：{session_id}");
            }
            session.title = title;
            return Ok(());
        }
        self.by_id.insert(
            session_id.clone(),
            RuntimeSession::new(session_id, cwd, title, SessionState::Idle, false),
        );
        Ok(())
    }

    /// 原子接受一个新的前台 turn；同一 Session 同时只允许一个活跃 turn。
    fn begin_turn(&mut self, session_id: &str, turn_id: String) -> Result<String> {
        let session = self
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        if turn_id.trim().is_empty() {
            anyhow::bail!("requestId 不能为空");
        }
        if !session.loaded {
            anyhow::bail!("Session 尚未加载：{session_id}");
        }
        if session.active_turn_id.is_some() || session.state == SessionState::Streaming {
            anyhow::bail!("Session 正在运行：{session_id}");
        }
        if session.state != SessionState::Ready {
            anyhow::bail!(
                "Session 当前不能接收消息：{session_id} ({:?})",
                session.state
            );
        }
        if session.last_completed_turn_id.as_deref() == Some(turn_id.as_str()) {
            anyhow::bail!("requestId 不能复用上一轮标识：{turn_id}");
        }
        session.last_completed_turn_id = None;
        session.active_turn_id = Some(turn_id.clone());
        session.active_turn_dispatch = Some(TurnDispatchState::Preparing);
        session.state = SessionState::Streaming;
        session.last_error = None;
        Ok(turn_id)
    }

    /// 完成当前 turn；指定 expected 时只允许对应后台请求收口，避免覆盖下一轮。
    fn finish_turn(
        &mut self,
        session_id: &str,
        expected: Option<&str>,
        error: Option<String>,
    ) -> Result<Option<String>> {
        let session = self
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        let Some(active_turn_id) = session.active_turn_id.as_deref() else {
            return Ok(None);
        };
        if expected.is_some_and(|expected| expected != active_turn_id) {
            return Ok(None);
        }
        let active_turn_id = active_turn_id.to_owned();
        session.active_turn_id = None;
        session.last_completed_turn_id = Some(active_turn_id.clone());
        session.active_turn_dispatch = None;
        if let Some(error) = error {
            session.last_error = Some(error);
        }
        if session.state != SessionState::Disconnected {
            session.state = SessionState::Ready;
        }
        Ok(Some(active_turn_id))
    }

    /// 后台准备完成后原子取得 prompt 派发权；已取消或过期 turn 返回 false。
    fn begin_prompt_dispatch(&mut self, session_id: &str, expected: &str) -> Result<bool> {
        let session = self
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        if session.active_turn_id.as_deref() != Some(expected) {
            return Ok(false);
        }
        if session.active_turn_dispatch != Some(TurnDispatchState::Preparing) {
            return Ok(false);
        }
        session.active_turn_dispatch = Some(TurnDispatchState::Dispatched);
        Ok(true)
    }

    /// 将 stop 原子绑定到调用时的 active turn，并决定本地收口还是通知 Peri。
    fn request_stop(&mut self, session_id: &str, expected: &str) -> Result<SessionStopAction> {
        let session = self
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        let Some(turn_id) = session.active_turn_id.as_deref() else {
            anyhow::bail!("Session 当前没有运行中的 turn：{session_id}");
        };
        if turn_id != expected {
            anyhow::bail!(
                "Session turn 已变化：{session_id}（expected={expected}, active={turn_id}）"
            );
        };
        match session.active_turn_dispatch {
            Some(TurnDispatchState::Preparing) => {
                let turn_id = turn_id.to_owned();
                session.active_turn_id = None;
                session.last_completed_turn_id = Some(turn_id.clone());
                session.active_turn_dispatch = None;
                session.last_error = None;
                if session.state != SessionState::Disconnected {
                    session.state = SessionState::Ready;
                }
                Ok(SessionStopAction::CompleteLocally(turn_id))
            }
            Some(TurnDispatchState::Dispatched) => {
                session.active_turn_dispatch = Some(TurnDispatchState::CancelRequested);
                Ok(SessionStopAction::NotifyRuntime(turn_id.to_owned()))
            }
            Some(TurnDispatchState::CancelRequested) => {
                Ok(SessionStopAction::AlreadyRequested(turn_id.to_owned()))
            }
            None => anyhow::bail!("Session active turn 缺少派发状态：{session_id}"),
        }
    }

    /// cancel 通知发送失败时允许同一 turn 重试；过期 turn 不做任何修改。
    fn rollback_stop_request(&mut self, session_id: &str, expected: &str) -> Result<()> {
        let session = self
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        if session.active_turn_id.as_deref() == Some(expected)
            && session.active_turn_dispatch == Some(TurnDispatchState::CancelRequested)
        {
            session.active_turn_dispatch = Some(TurnDispatchState::Dispatched);
        }
        Ok(())
    }
}

/// 前端会话快照（session_get_state 返回）。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    /// 当前快照对应的 Session ID；idle 快照为空。
    pub session_id: Option<String>,
    /// 当前 Session 的独立执行状态。
    pub state: SessionState,
    /// 当前唯一运行中的 client turn；WebView 重挂载后可恢复 stop/done 关联。
    pub active_turn_id: Option<String>,
    /// 当前唯一使用的后端类型。
    pub backend: &'static str,
    /// Session 已授权的项目根目录。
    pub project_path: Option<String>,
    /// Session 当前标题。
    pub title: Option<String>,
    /// Session 最近一次运行时错误。
    pub last_error: Option<String>,
    /// 当前诊断日志文件路径。
    pub diagnostics_path: String,
}

/// 一个 Session 当前仍在运行的前台 turn。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTurnSnapshot {
    pub session_id: String,
    pub turn_id: String,
}

/// WebView 启动时的一致运行时快照：焦点状态与全部活跃 turn 共用一次读锁。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStateSnapshot {
    pub focused_session: SessionSnapshot,
    pub active_turns: Vec<ActiveTurnSnapshot>,
    pub completed_turns: Vec<ActiveTurnSnapshot>,
}

/// 进程内 peri 运行时句柄。
pub struct PeriRuntime {
    /// 客户端侧传输（命令经它发 JSON-RPC；recv 循环收通知/请求）。
    transport: Arc<dyn AcpTransport>,
    /// ACP server 共用的 SessionManager，用于退出时发现后台终端任务。
    session_manager: SessionManager,
    /// 会话持久化（SQLite）。
    pub thread_store: Arc<dyn ThreadStore>,
    /// 当前 LlmProvider；未配置供应商时为占位（空密钥），由 provider_configured 区分。
    provider: Arc<RwLock<LlmProvider>>,
    /// 当前完整 peri 配置（供应商 + 逐模型元数据）；保存设置后整体替换。
    peri_config: Arc<RwLock<PeriConfig>>,
    /// 所有独立后台模型调用共享的请求观测器。
    request_observer: Arc<crate::analytics::AnalyticsRecorder>,
    /// 当前是否已有有效供应商配置。
    provider_configured: std::sync::atomic::AtomicBool,
    /// 退出清理开始后拒绝新的 turn 与 MCP 初始化，避免关闭后重新拉起资源。
    shutting_down: std::sync::atomic::AtomicBool,
    /// 已启用插件声明的 Skill 根；插件变更后整体热替换。
    plugin_skill_roots: Arc<RwLock<Vec<peri_middlewares::skills::SkillRoot>>>,
    /// 已启用插件声明的 Hooks；每轮任务开始时读取最新快照。
    plugin_hooks: Arc<RwLock<Vec<peri_middlewares::hooks::RegisteredHook>>>,
    /// KeenCode 合并用户与插件配置后生成的唯一 MCP 运行时文件。
    mcp_config_path: RwLock<PathBuf>,
    /// 当前项目的完整 MCP 配置；插件敏感 userConfig 只存在于此进程内结构。
    mcp_runtime_config: RwLock<McpConfigFile>,
    /// Host 与 Session 中间件共享的具体 MCP 连接池。
    mcp_pool: Arc<McpClientPool>,
    /// 串行化首次初始化与配置指纹切换，防止并发任务重复拉起连接。
    mcp_runtime_state: tokio::sync::Mutex<McpRuntimeState>,
    /// 多 Session 独立状态与当前界面焦点。
    sessions: RwLock<RuntimeSessions>,
    /// 按 Session ID 隔离的待回答 elicitation 请求。
    pending_by_session: Mutex<HashMap<String, HashMap<i64, RequestId>>>,
    /// 后端诊断日志句柄。
    diagnostics: Arc<Diagnostics>,
    /// peri tracing 文件输出生命周期。
    _tracing_guard: Option<peri_agent::telemetry::TracingGuard>,
}

impl PeriRuntime {
    /// 在 Tauri setup 中装配完整运行时（同步入口，内部 block_on）。
    pub fn build(app: &AppHandle) -> Result<Arc<Self>> {
        let diagnostics = app.state::<Arc<Diagnostics>>().inner().clone();
        diagnostics.log("info", "runtime.build", "开始装配 PeriRuntime");
        match tauri::async_runtime::block_on(Self::build_async(app, diagnostics.clone())) {
            Ok(runtime) => {
                eprintln!("[keencode] PeriRuntime build 成功");
                diagnostics.log("info", "runtime.build", "PeriRuntime build 成功");
                Ok(runtime)
            }
            Err(e) => {
                eprintln!("[keencode] PeriRuntime build 失败: {e:#}");
                diagnostics.error("runtime.build", format!("PeriRuntime build 失败: {e:#}"));
                Err(e)
            }
        }
    }

    /// 异步装配（block_on 包装，便于直接 await 内部初始化）。
    async fn build_async(app: &AppHandle, diagnostics: Arc<Diagnostics>) -> Result<Arc<Self>> {
        diagnostics.log("info", "runtime.build", "解析供应商配置");
        let (provider, peri_config, configured) = Self::resolve_provider(app)?;
        let provider_runtime = Arc::new(RwLock::new(provider));
        let peri_config_runtime = Arc::new(RwLock::new(peri_config));
        diagnostics.log(
            "info",
            "runtime.provider",
            if configured {
                "供应商配置解析完成（密钥已隐藏）"
            } else {
                "尚未配置供应商，运行时等待设置"
            },
        );

        // 会话只写入当前用户的 KeenCode 统一目录；目录或 SQLite 初始化失败即终止启动。
        let runtime_root = crate::storage::root_dir(app)?;
        let threads_dir = runtime_root.join("threads");
        std::fs::create_dir_all(&threads_dir)
            .with_context(|| format!("创建会话数据目录失败：{}", threads_dir.display()))?;
        diagnostics.log(
            "info",
            "runtime.storage",
            format!("会话数据目录={}", threads_dir.display()),
        );
        let thread_store: Arc<dyn ThreadStore> = Arc::new(
            peri_resources::sessions::SqliteThreadStore::new(threads_dir.join("threads.db"))
                .await
                .context("打开会话数据库失败")?,
        );
        diagnostics.log("info", "runtime.storage", "会话存储初始化完成");

        // ── 装配 AcpServerConfig（复刻上游 launch 顺序）──
        // 只读取并校验配置路径，不在应用启动阶段拉起 MCP 子进程或 HTTP 连接；
        // 首个真正执行的任务会通过 McpClientPool 按需初始化。
        let project_dir = std::env::current_dir().map_err(anyhow::Error::msg)?;
        let snapshot = crate::extensions::claude_runtime_snapshot(app, &project_dir)
            .unwrap_or_else(|error| {
                diagnostics.log(
                    "warn",
                    "runtime.plugins",
                    format!("插件运行时快照无效，已隔离并按无插件继续：{error}"),
                );
                crate::claude_plugins::PluginRuntimeSnapshot::default()
            });
        let mcp_config_path =
            crate::extensions::prepare_mcp_runtime_config(app).unwrap_or_else(|error| {
                diagnostics.log(
                    "warn",
                    "runtime.mcp",
                    format!("MCP 运行时快照准备失败，按空配置继续：{error}"),
                );
                runtime_root.join("mcp-runtime.json")
            });
        let mcp_runtime_config = crate::extensions::runtime_mcp_config(app, &project_dir)
            .unwrap_or_else(|error| {
                diagnostics.log(
                    "warn",
                    "runtime.mcp",
                    format!("MCP 进程内配置准备失败，按空配置继续：{error}"),
                );
                McpConfigFile::default()
            });
        let mcp_pool = Arc::new(McpClientPool::new_pending());
        let mcp_pool_port: Arc<dyn McpPoolPort> = mcp_pool.clone();
        let plugin_skill_roots = Arc::new(RwLock::new(
            crate::extensions::runtime_skill_roots(app, &project_dir).unwrap_or_else(|error| {
                diagnostics.log(
                    "warn",
                    "runtime.plugins",
                    format!("插件 Skill 快照无效，已按无插件 Skill 继续：{error}"),
                );
                Vec::new()
            }),
        ));
        let mut plugin_agent_dirs = crate::extensions::runtime_plugin_agent_dirs(app)
            .unwrap_or_else(|error| {
                diagnostics.log(
                    "warn",
                    "runtime.plugins",
                    format!("插件 Agent 快照无效，已按无插件 Agent 继续：{error}"),
                );
                Vec::new()
            });
        let global_agents_dir = runtime_root.join("agents");
        if !plugin_agent_dirs.contains(&global_agents_dir) {
            plugin_agent_dirs.push(global_agents_dir);
        }
        let agent_search_path =
            std::env::join_paths(&plugin_agent_dirs).context("拼接插件与全局 Agent 目录失败")?;
        // Agent 目录展示与执行必须使用同一份有序路径，避免插件 Agent 只可见不可运行。
        // 仅在启动早期设置一次；项目与内置定义仍由 Peri 保持更高优先级。
        unsafe {
            std::env::set_var("PERI_AGENT_DIRS", agent_search_path);
        }
        // 请求观测器必须在 Host/SessionManager 装配之前进入所有模型工厂，
        // 否则动态模型和缓存模型会丢失请求记录。
        let request_observer = Arc::new(crate::analytics::AnalyticsRecorder::new(app)?);
        app.manage(Arc::clone(&request_observer));
        // Peri 3.6.5 在 Host 创建时为每个 Session 建立惰性 LSP 池；该配置不是
        // 热更新引用，因此插件 LSP 的启停和 userConfig 变更统一在下次启动生效。
        let plugin_lsp_servers = snapshot
            .plugins
            .iter()
            .flat_map(|plugin| plugin.lsp_servers.iter().cloned())
            .collect::<Vec<_>>();
        diagnostics.log(
            "info",
            "runtime.plugins",
            format!("已装配 {} 个插件 LSP Server", plugin_lsp_servers.len()),
        );
        let plugin_hooks = Arc::new(RwLock::new(snapshot.plugin_hooks));
        let server_config = assemble_embedded_server_config(EmbeddedHostAssemblyInput {
            provider: Arc::clone(&provider_runtime),
            request_observer: Some(request_observer.clone()),
            peri_config: Arc::clone(&peri_config_runtime),
            mcp_pool: Some(mcp_pool_port),
            plugin_skill_roots: Arc::clone(&plugin_skill_roots),
            plugin_agent_dirs,
            plugin_hooks: Arc::clone(&plugin_hooks),
            plugin_lsp_servers,
            thread_store: Arc::clone(&thread_store),
            config_path: crate::storage::root_dir(app)?.join("peri-settings.json"),
        });
        let session_manager = server_config.session_manager.clone();

        // 沙箱写工具（SandboxWrite）的外部基目录：只读子代理的方案/报告统一写入
        // `~/.keencode/plans/<项目键>/`，项目目录内不再产生 `.peri/` 等写入。
        // 须在 ACP server 任务启动前设置；构造工具时按会话 cwd 派生项目子目录。
        if std::env::var_os("PERI_SANDBOX_WRITE_BASE").is_none() {
            let sandbox_base = runtime_root.join("plans");
            // Rust 2024 将修改进程环境标记为 unsafe；这里仅在启动早期设置一次。
            unsafe {
                std::env::set_var("PERI_SANDBOX_WRITE_BASE", sandbox_base);
            }
        }

        // 内置子智能体模型覆盖表：`agent_update` 对内置名写入
        // `agent_id -> providerId::model`，peri 在加载内置定义与目录扫描时
        // 套用（每次派发重读，UI 修改后无需重启即生效）。
        if std::env::var_os("PERI_AGENT_MODEL_OVERRIDES").is_none() {
            let overrides_path = runtime_root.join("agent-model-overrides.json");
            // 同上：仅在启动早期设置一次。
            unsafe {
                std::env::set_var("PERI_AGENT_MODEL_OVERRIDES", overrides_path);
            }
        }

        // 将 peri 内部 tracing 也落到同一日志目录，便于查看 agent、MCP 和工具链细节。
        if std::env::var_os("RUST_LOG_FILE").is_none() {
            let tracing_path = diagnostics.path().with_file_name("peri.log");
            // Rust 2024 将修改进程环境标记为 unsafe；这里仅在启动早期设置一次日志路径。
            unsafe {
                std::env::set_var("RUST_LOG_FILE", tracing_path);
            }
        }
        let tracing_guard =
            std::panic::catch_unwind(|| peri_agent::telemetry::init_tracing("peri")).ok();
        if tracing_guard.is_some() {
            diagnostics.log("info", "runtime.logging", "peri tracing 已初始化");
        } else {
            diagnostics.error(
                "runtime.logging",
                "peri tracing 初始化失败，保留 KeenCode 诊断日志",
            );
        }

        // ── 传输对 + 服务器 ──
        let (client_transport, server_transport) = mpsc_transport_pair();
        let server_arc: Arc<dyn AcpTransport> = Arc::new(server_transport);
        let server_diagnostics = Arc::clone(&diagnostics);
        tauri::async_runtime::spawn(async move {
            server_diagnostics.log("info", "acp.server", "ACP server task 启动");
            run_acp_server(server_arc, server_config).await;
            server_diagnostics.error("acp.server", "ACP server task 已退出");
        });

        let runtime = Arc::new(Self {
            transport: Arc::new(client_transport),
            session_manager,
            thread_store,
            provider: provider_runtime,
            peri_config: peri_config_runtime,
            request_observer,
            provider_configured: std::sync::atomic::AtomicBool::new(configured),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
            plugin_skill_roots,
            plugin_hooks,
            mcp_config_path: RwLock::new(mcp_config_path),
            mcp_runtime_config: RwLock::new(mcp_runtime_config),
            mcp_pool,
            mcp_runtime_state: tokio::sync::Mutex::new(McpRuntimeState::default()),
            sessions: RwLock::new(RuntimeSessions::default()),
            pending_by_session: Mutex::new(HashMap::new()),
            diagnostics,
            _tracing_guard: tracing_guard,
        });
        runtime.spawn_event_pump(app.clone());
        // 桌面端通过进程内 MpscTransport 仍遵循 ACP initialize 契约；错误、回合完成、
        // token 统计与重放等扩展事件必须先显式声明，不能依赖服务端猜测客户端类型。
        runtime
            .send_request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {
                        "_meta": {
                            "peri.tokenStats": true,
                            "peri.skillNames": true,
                            "peri.replay": true,
                            "peri.sourceAgentId": true,
                            "peri.contextUsage": true,
                            "peri.agentEvent": true,
                            "peri.agentEventDone": true,
                            "peri.unstableEvent": true,
                        }
                    }
                }),
            )
            .await
            .context("初始化 ACP 桌面客户端能力失败")?;
        Ok(runtime)
    }

    /// 根据 KeenCode 当前设置解析 provider/config 原子状态。
    ///
    /// `peri_config.config.providers` 总是包含全部已保存供应商：会话级模型切换
    /// （`session/set_config_option` 的 `"{provider_id}::{model}"` 值）依赖它按
    /// `provider_id` 查找任意供应商（Q1 决策，见 [`providers::build_peri_config_all`]）。
    /// `provider` 与 `configured` 只反映"新会话默认值"（当前激活供应商 + 当前模型）。
    ///
    /// 未配置供应商时返回占位 LlmProvider（空密钥），`configured=false`；
    /// LlmProvider 存在性不足以区分配置态，必须配合 configured 标志使用。
    fn resolve_provider(app: &AppHandle) -> Result<(LlmProvider, PeriConfig, bool)> {
        let listed = providers::list(app)?;
        let language = crate::app_settings::get(app)?
            .interface_language
            .as_code()
            .to_owned();
        let build_config = |providers| {
            let mut config = providers::build_peri_config_all(providers);
            config.config.language = Some(language.clone());
            config
        };
        let Some(active_id) = listed.active_provider_id.as_deref() else {
            return Ok((
                placeholder_provider(),
                build_config(listed.providers),
                false,
            ));
        };
        // 与 providers::select_model 相同的语义：当前模型必须属于激活供应商。
        let Some(active) = listed.providers.iter().find(|p| p.id == active_id) else {
            return Ok((
                placeholder_provider(),
                build_config(listed.providers),
                false,
            ));
        };
        let Some(model) = listed
            .default_model
            .as_deref()
            .filter(|model| active.models.iter().any(|m| m == model))
        else {
            return Ok((
                placeholder_provider(),
                build_config(listed.providers),
                false,
            ));
        };
        let (context_1m, context_window) = providers::resolve_context(active, model);
        let peri_config = build_config(listed.providers);
        let build_default = || {
            LlmProvider::from_provider_config(
                &peri_config,
                active_id,
                model,
                Some("high".to_string()),
                32_000,
                context_1m,
                context_window,
            )
        };
        let configured = build_default().is_some();
        let provider = build_default().unwrap_or_else(placeholder_provider);
        Ok((provider, peri_config, configured))
    }

    /// 从当前供应商元数据和密钥重新构造运行时快照。
    ///
    /// 供应商配置仍由 KeenCode 自己持久化；这里只更新内存中的 ACP 共享引用，
    /// 不写入 peri 的默认设置文件，也不会暴露 API Key。
    pub fn reload_provider(&self, app: &AppHandle) -> Result<()> {
        self.replace_provider_state(app)
    }

    /// 从当前插件清单热替换后续任务使用的 Skills、Hooks 与 MCP 配置。
    pub fn reload_plugins(&self, app: &AppHandle) -> Result<()> {
        // 先切换 MCP 快照路径：即使后续 Skill 解析失败，也不能让下一轮继续
        // 读取上一次插件状态留下的旧 MCP 快照。
        self.reload_mcp_snapshot(app)?;
        let project_dir = std::env::current_dir().map_err(anyhow::Error::msg)?;
        let roots = crate::extensions::runtime_skill_roots(app, &project_dir)
            .map_err(anyhow::Error::msg)?;
        let hooks = crate::extensions::claude_runtime_snapshot(app, &project_dir)
            .map_err(anyhow::Error::msg)?
            .plugin_hooks;
        *self.plugin_skill_roots.write() = roots;
        *self.plugin_hooks.write() = hooks;
        self.diagnostics
            .log("info", "runtime.plugins", "插件 Skills 与 Hooks 热加载完成");
        Ok(())
    }

    /// 发布最新 MCP 运行时快照路径；生成失败时路径会切到空配置或隔离路径。
    pub fn reload_mcp_snapshot(&self, app: &AppHandle) -> Result<PathBuf> {
        let path = crate::extensions::mcp_config_path(app).map_err(anyhow::Error::msg)?;
        let project_dir = std::env::current_dir().map_err(anyhow::Error::msg)?;
        let config =
            crate::extensions::runtime_mcp_config(app, &project_dir).unwrap_or_else(|error| {
                self.diagnostics.log(
                    "warn",
                    "runtime.mcp",
                    format!("MCP 插件配置热加载失败，按空配置继续：{error}"),
                );
                McpConfigFile::default()
            });
        *self.mcp_config_path.write() = path.clone();
        *self.mcp_runtime_config.write() = config;
        Ok(path)
    }

    /// 在首次真实任务或 OAuth 请求前，从 KeenCode 唯一配置文件初始化 MCP。
    ///
    /// 相同内容摘要只初始化一次；用户或插件改写运行时文件后，下一次任务会先
    /// 关闭旧连接，再使用同一个连接池重建状态。查看 `mcp/list` 不触发连接。
    pub async fn ensure_mcp_initialized(&self) -> Result<()> {
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            anyhow::bail!("应用正在退出，拒绝初始化 MCP");
        }
        let mcp_config_path = self.mcp_config_path.read().clone();
        let mcp_runtime_config = self.mcp_runtime_config.read().clone();
        // McpConfigFile 内部使用 HashMap；先按名称转成 BTreeMap，保证同一
        // 配置不会因 HashMap 迭代顺序变化而在每轮任务中重复重载连接。
        let canonical_servers = mcp_runtime_config
            .mcp_servers
            .iter()
            .map(|(name, server)| (name, server))
            .collect::<BTreeMap<_, _>>();
        let config_bytes =
            serde_json::to_vec(&canonical_servers).context("序列化进程内 MCP 运行时配置失败")?;
        let fingerprint = mcp_config_fingerprint(&config_bytes);
        let mut state = self.mcp_runtime_state.lock().await;
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            anyhow::bail!("应用正在退出，拒绝初始化 MCP");
        }
        if state.is_current(&fingerprint) {
            return Ok(());
        }

        self.diagnostics.log(
            "info",
            "runtime.mcp",
            format!(
                "开始按需加载 MCP 配置 path={} reload={}",
                mcp_config_path.display(),
                state.applied_fingerprint.is_some()
            ),
        );
        self.mcp_pool.reset_for_reinitialize().await;
        let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);
        McpClientPool::run_initialize_from_config(
            self.mcp_pool.clone(),
            mcp_runtime_config,
            status_tx,
            None,
            None,
        )
        .await;
        state.applied_fingerprint = Some(fingerprint);
        self.diagnostics
            .log("info", "runtime.mcp", "MCP 按需初始化完成");
        Ok(())
    }

    /// 从当前持久化配置构造并提交一个完整运行时快照。
    fn replace_provider_state(&self, app: &AppHandle) -> Result<()> {
        self.diagnostics
            .log("info", "runtime.provider", "开始热加载供应商配置");
        let (provider, peri_config, configured) = Self::resolve_provider(app)?;
        *self.provider.write() = provider;
        *self.peri_config.write() = peri_config;
        self.provider_configured
            .store(configured, std::sync::atomic::Ordering::Relaxed);
        self.diagnostics.log(
            "info",
            "runtime.provider",
            if configured {
                "供应商配置热加载完成"
            } else {
                "供应商已清空，运行时进入未配置状态"
            },
        );
        Ok(())
    }

    /// 拒绝在尚未配置模型供应商时进入任何 LLM 请求。
    pub fn ensure_provider_configured(&self) -> Result<()> {
        if self
            .provider_configured
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            Ok(())
        } else {
            anyhow::bail!("请先在设置中添加并选择模型供应商")
        }
    }

    /// 返回当前是否可以发起模型调用，供低优先级后台能力静默跳过。
    pub fn provider_is_configured(&self) -> bool {
        self.provider_configured
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 使用当前供应商执行一次无工具、无主会话历史的后台模型请求。
    pub async fn generate_isolated(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        timeout_secs: u64,
    ) -> Result<String> {
        self.ensure_provider_configured()?;
        let provider = self.provider.read().clone();
        let llm = AgentModelBridge::new(Arc::from(
            provider.into_model_with_request_observer(Some(self.request_observer.clone())),
        ))
        .with_purpose("background");
        let messages = vec![
            BaseMessage::system(system_prompt.to_owned()),
            BaseMessage::human(user_prompt.to_owned()),
        ];
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            llm.generate_reasoning(&messages, &[], None),
        )
        .await
        .context("后台模型请求超时")??;
        result
            .final_answer
            .or_else(|| result.source_message.map(|message| message.content()))
            .filter(|value| !value.trim().is_empty())
            .context("后台模型请求返回空内容")
    }

    /// 事件泵：客户端 recv 循环，把服务器通知和 elicitation 转发为 Tauri 事件。
    fn spawn_event_pump(self: &Arc<Self>, app: AppHandle) {
        let transport = Arc::clone(&self.transport);
        let runtime = Arc::downgrade(self);
        let diagnostics = Arc::clone(&self.diagnostics);
        tauri::async_runtime::spawn(async move {
            diagnostics.log("info", "acp.transport", "客户端事件泵启动");
            while let Some(msg) = transport.recv().await {
                match msg {
                    IncomingMessage::Request { id, method, params } => {
                        if let Some(runtime) = runtime.upgrade() {
                            runtime.diagnostics.rpc("recv", &method, &params);
                        }
                        if method == "elicitation/create" {
                            match (request_id_number(&id), elicitation_session_id(&params)) {
                                (Ok(rpc_id), Ok(session_id)) => {
                                    if let Some(runtime) = runtime.upgrade() {
                                        if !runtime
                                            .session(&session_id)
                                            .is_some_and(|session| session.is_loaded())
                                        {
                                            let _ = transport
                                                .send_response(
                                                    id,
                                                    Err(peri_acp::transport::types::AcpError::new(
                                                        -32602,
                                                        "elicitation 指向未登记的 Session",
                                                    )),
                                                )
                                                .await;
                                            continue;
                                        }
                                        let task_title = runtime
                                            .session(&session_id)
                                            .and_then(|session| session.title);
                                        runtime
                                            .pending_by_session
                                            .lock()
                                            .entry(session_id.clone())
                                            .or_default()
                                            .insert(rpc_id, id);
                                        app.state::<Arc<crate::task_notifications::TaskNotifications>>()
                                            .notify_needs_confirmation(
                                                &app,
                                                &session_id,
                                                rpc_id,
                                                task_title.as_deref(),
                                            );
                                    }
                                    let _ = app.emit(
                                        "acp://elicitation",
                                        json!({ "method": method, "rpcId": rpc_id, "params": params }),
                                    );
                                }
                                (Err(error), _) | (_, Err(error)) => {
                                    let _ = transport.send_response(id, Err(error)).await;
                                }
                            }
                        } else {
                            // 未知请求：直接回方法不存在
                            let _ = transport
                                .send_response(
                                    id,
                                    Err(peri_acp::transport::types::AcpError::new(
                                        -32601,
                                        format!("Method not found: {method}"),
                                    )),
                                )
                                .await;
                        }
                    }
                    IncomingMessage::Notification { method, mut params } => {
                        if let Some(runtime) = runtime.upgrade() {
                            runtime.diagnostics.rpc("recv", &method, &params);
                        }
                        let event = match method.as_str() {
                            "session/update" => Some("acp://session-update"),
                            "peri/agent_event" => Some("acp://agent-event"),
                            "peri/agent_event_done" => Some("acp://agent-done"),
                            "session/recovery" => Some("acp://recovery-status"),
                            "peri/unstable-event" => Some("acp://unstable-event"),
                            _ => None,
                        };
                        // Peri 3.6.5 只在 turn done 回带 prompt requestId；其余实时
                        // 通知由 Host 用当前 Session 的唯一活跃请求补齐同一关联键。
                        // 没有活跃请求时不猜测，历史重放与后台事件保持无关联。
                        if matches!(
                            method.as_str(),
                            "session/update" | "peri/agent_event" | "peri/unstable-event"
                        ) && let Some(session_id) =
                            params.get("sessionId").and_then(Value::as_str)
                            && let Some(request_id) = runtime
                                .upgrade()
                                .and_then(|runtime| runtime.active_turn_id(session_id))
                        {
                            // Peri 3.6.5 的部分通知缺少 requestId，需要用当前活跃
                            // 回合补齐；已经带 requestId 的通知必须原样保留，迟到的
                            // 上一轮事件不能被错误重标为当前回合。
                            attach_request_id_if_missing(&mut params, &request_id);
                        }
                        let mut should_emit = true;
                        if method == "peri/agent_event"
                            && let (Some(session_id), Some(event_json)) = (
                                params.get("sessionId").and_then(Value::as_str),
                                params.get("event_json").and_then(Value::as_str),
                            )
                            && let Some(turn_id) = params.get("requestId").and_then(Value::as_str)
                            && let Some(runtime) = runtime.upgrade()
                            && runtime.is_active_session_turn(session_id, turn_id)
                        {
                            if let Some(message) = agent_execution_failure(event_json) {
                                let _ =
                                    runtime.set_session_error(session_id, Some(message.to_owned()));
                            }
                            app.state::<Arc<crate::task_notifications::TaskNotifications>>()
                                .observe_agent_event(session_id, turn_id, event_json);
                        }
                        if method == "peri/agent_event_done" {
                            // 只有 Peri 原样回带、且仍匹配 Host 当前活跃请求的 done
                            // 才是本轮完成边界；无 requestId 的命令/后台通知不收口。
                            should_emit = false;
                            if let (Some(session_id), Some(turn_id), Some(stop_reason)) = (
                                params
                                    .get("sessionId")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                params
                                    .get("requestId")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                                params
                                    .get("stopReason")
                                    .and_then(Value::as_str)
                                    .map(str::to_owned),
                            ) {
                                let completed_at_ms = crate::analytics::now_ms();
                                let runtime = runtime.upgrade();
                                let task_title = runtime
                                    .as_ref()
                                    .and_then(|runtime| runtime.session(&session_id))
                                    .and_then(|session| session.title);
                                if let Some(runtime) = runtime
                                    && let Ok(Some(finished_turn_id)) = runtime.finish_session_turn(
                                        &session_id,
                                        Some(&turn_id),
                                        None,
                                    )
                                {
                                    let notifications = app
                                        .state::<Arc<crate::task_notifications::TaskNotifications>>(
                                        );
                                    notifications.notify_done(
                                        &app,
                                        &session_id,
                                        &finished_turn_id,
                                        task_title.as_deref(),
                                        &stop_reason,
                                    );
                                    if let Some(object) = params.as_object_mut() {
                                        object.insert(
                                            "_keencode".to_owned(),
                                            json!({ "completedAtMs": completed_at_ms }),
                                        );
                                    }
                                    should_emit = true;
                                }
                            }
                        }
                        if should_emit && let Some(event) = event {
                            let _ = app.emit(event, json!({ "method": method, "params": params }));
                        } else if event.is_none()
                            && let Some(runtime) = runtime.upgrade()
                        {
                            runtime.diagnostics.error(
                                "acp.notification",
                                format!("收到未声明的 ACP 通知：{method}"),
                            );
                        }
                    }
                    IncomingMessage::Response { .. } => {
                        // mpsc transport 的 router 已按 request id 分发
                    }
                }
            }
            diagnostics.error("acp.transport", "ACP transport 已断开");
            if let Some(runtime) = runtime.upgrade() {
                for (session_id, turn_id) in
                    runtime.mark_transport_disconnected("ACP transport 已断开")
                {
                    app.state::<Arc<crate::task_notifications::TaskNotifications>>()
                        .discard_turn(&session_id, &turn_id);
                }
            }
            let _ = app.emit("acp://closed", json!({}));
        });
    }

    /// 发送 JSON-RPC 请求并等待响应。
    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
        if method == "mcp/oauth_start" {
            self.ensure_mcp_initialized().await?;
        }
        let started = std::time::Instant::now();
        self.diagnostics.rpc("send", method, &params);
        let result = self
            .transport
            .send_request(method, params)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.message));
        match &result {
            Ok(response) => self.diagnostics.log(
                "info",
                "acp.rpc",
                format!(
                    "direction=response method={} elapsed_ms={} result={}",
                    method,
                    started.elapsed().as_millis(),
                    crate::diagnostics::summarize_value_for_log(response)
                ),
            ),
            Err(error) => self.diagnostics.error(
                "acp.rpc",
                format!(
                    "direction=response method={} elapsed_ms={} error={error}",
                    method,
                    started.elapsed().as_millis()
                ),
            ),
        }
        result
    }

    /// 发送 JSON-RPC 通知。
    pub async fn send_notification(&self, method: &str, params: Value) -> Result<()> {
        self.diagnostics.rpc("send-notification", method, &params);
        self.transport
            .send_notification(method, params)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.message))
    }

    /// 返回所有仍在执行回合的 Session ID。
    pub fn active_session_ids(&self) -> Vec<String> {
        let mut session_ids = self
            .sessions
            .read()
            .by_id
            .values()
            .filter(|session| session.state == SessionState::Streaming)
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>();
        for session in self.session_manager.inner_sessions().iter() {
            let has_background_tasks = (&*session.task_manager as &dyn std::any::Any)
                .downcast_ref::<peri_agent::agent::async_tasks::TaskManager>()
                .is_some_and(|manager| {
                    manager.list_tasks_full().iter().any(|task| {
                        matches!(
                            task.status,
                            peri_agent::agent::async_tasks::BackgroundTaskStatus::Running
                        )
                    })
                });
            if has_background_tasks && !session_ids.contains(session.key()) {
                session_ids.push(session.key().clone());
            }
        }
        session_ids
    }

    /// 同步终止指定 Session 的 Agent 与后台终端任务。
    pub fn cancel_session_work(&self, session_id: &str) {
        if let Some(session) = self.session_manager.get_session(session_id) {
            peri_acp_types::session::cancel_all_agents(session.active_agents.values());
            session.cancel_token.cancel();
            session.task_manager.cancel_all();
        }
    }

    /// 标记退出流程已开始；之后不再接受新 turn 或 MCP 初始化。
    pub fn begin_shutdown(&self) {
        self.shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        // 与 begin_session_turn 共用一次写锁屏障：若某个 turn 已在标记前通过
        // 检查，等待它完成登记后再继续退出；标记后的 turn 则会在锁内拒绝。
        drop(self.sessions.write());
    }

    /// 应用退出前关闭全部 Session 级资源，再停止共享 MCP 连接。
    ///
    /// 当前 Peri 的 `close_session` 会发出取消并移除会话资源，但没有等待所有
    /// 子任务完成的契约；这里只按现有能力有序关闭，不虚构完成保证。
    pub async fn shutdown_for_exit(&self) {
        self.begin_shutdown();
        let _mcp_state_guard = self.mcp_runtime_state.lock().await;
        let session_ids = self
            .session_manager
            .inner_sessions()
            .iter()
            .map(|session| session.key().clone())
            .collect::<Vec<_>>();
        for session_id in session_ids {
            if let Err(error) = self.session_manager.close_session(&session_id).await {
                self.diagnostics.log(
                    "warn",
                    "runtime.shutdown",
                    format!("关闭 Session 资源失败 session_id={session_id}: {error:#}"),
                );
            }
        }
        self.mcp_pool.shutdown().await;
    }

    /// 返回全部 Session 中仍在运行的后台任务。
    pub fn background_tasks(&self) -> Vec<BackgroundTaskInfo> {
        let mut tasks = Vec::new();
        for session in self.session_manager.inner_sessions().iter() {
            let Some(manager) = (&*session.task_manager as &dyn std::any::Any)
                .downcast_ref::<peri_agent::agent::async_tasks::TaskManager>()
            else {
                continue;
            };
            tasks.extend(
                manager
                    .list_tasks_full()
                    .into_iter()
                    .filter_map(|task| running_background_task(session.key(), task)),
            );
        }
        tasks.sort_by(|left, right| left.started_at.cmp(&right.started_at));
        tasks
    }

    // ── 会话状态 ────────────────────────────────────────────────────────────

    /// 返回当前界面聚焦 Session 的快照；没有焦点时返回健康的 idle 快照。
    pub fn snapshot(&self) -> SessionSnapshot {
        let sessions = self.sessions.read();
        match sessions
            .focused_session_id
            .as_deref()
            .and_then(|session_id| sessions.by_id.get(session_id))
        {
            Some(session) => self.snapshot_from_session(session),
            None => SessionSnapshot {
                session_id: None,
                state: SessionState::Idle,
                active_turn_id: None,
                backend: "peri_acp",
                project_path: None,
                title: None,
                last_error: None,
                diagnostics_path: self.diagnostics.path().display().to_string(),
            },
        }
    }

    /// 返回焦点 Session 与全部活跃 turn 的同一时刻快照。
    pub fn runtime_state_snapshot(&self) -> RuntimeStateSnapshot {
        let sessions = self.sessions.read();
        let focused_session = match sessions
            .focused_session_id
            .as_deref()
            .and_then(|session_id| sessions.by_id.get(session_id))
        {
            Some(session) => self.snapshot_from_session(session),
            None => SessionSnapshot {
                session_id: None,
                state: SessionState::Idle,
                active_turn_id: None,
                backend: "peri_acp",
                project_path: None,
                title: None,
                last_error: None,
                diagnostics_path: self.diagnostics.path().display().to_string(),
            },
        };
        RuntimeStateSnapshot {
            focused_session,
            active_turns: sessions.active_turns(),
            completed_turns: sessions.completed_turns(),
        }
    }

    /// 返回指定 Session 的独立快照。
    pub fn snapshot_for(&self, session_id: &str) -> Result<SessionSnapshot> {
        let sessions = self.sessions.read();
        let session = sessions
            .by_id
            .get(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        Ok(self.snapshot_from_session(session))
    }

    /// 把单个运行时记录转换成前端快照。
    fn snapshot_from_session(&self, session: &RuntimeSession) -> SessionSnapshot {
        SessionSnapshot {
            session_id: Some(session.session_id.clone()),
            state: session.state,
            active_turn_id: session.active_turn_id.clone(),
            backend: "peri_acp",
            project_path: Some(session.cwd.clone()),
            title: session.title.clone(),
            last_error: session.last_error.clone(),
            diagnostics_path: self.diagnostics.path().display().to_string(),
        }
    }

    /// 登记或替换一个已经完成目录授权的 Session。
    pub fn register_session(&self, session: RuntimeSession) {
        self.diagnostics.log(
            "info",
            "runtime.session",
            format!(
                "register session_id={} state={:?} loaded={}",
                session.session_id, session.state, session.loaded
            ),
        );
        self.sessions
            .write()
            .by_id
            .insert(session.session_id.clone(), session);
    }

    /// 登记持久化元数据；已运行的 Session 只同步目录和标题，不重置状态。
    pub fn sync_session_metadata(
        &self,
        session_id: String,
        cwd: String,
        title: Option<String>,
    ) -> Result<()> {
        self.sessions.write().sync_metadata(session_id, cwd, title)
    }

    /// 返回指定 Session 的运行时记录副本。
    pub fn session(&self, session_id: &str) -> Option<RuntimeSession> {
        self.sessions.read().by_id.get(session_id).cloned()
    }

    /// 删除已持久化 Session 后清理对应运行时登记与待回答请求。
    pub fn forget_session(&self, session_id: &str) {
        let mut sessions = self.sessions.write();
        sessions.by_id.remove(session_id);
        if sessions.focused_session_id.as_deref() == Some(session_id) {
            sessions.focused_session_id = None;
        }
        drop(sessions);
        self.pending_by_session.lock().remove(session_id);
    }

    /// 把指定 Session 设为当前界面焦点；焦点不参与后续权限校验。
    pub fn focus_session(&self, session_id: &str) -> Result<()> {
        let mut sessions = self.sessions.write();
        if !sessions.by_id.contains_key(session_id) {
            anyhow::bail!("Session 尚未登记：{session_id}");
        }
        sessions.focused_session_id = Some(session_id.to_owned());
        Ok(())
    }

    /// 清除界面焦点，但保留所有前后台 Session 的运行状态。
    pub fn clear_focus(&self) {
        self.sessions.write().focused_session_id = None;
    }

    /// 检查通知是否仍属于当前 active client turn；缺失/迟到事件一律 fail closed。
    pub fn is_active_session_turn(&self, session_id: &str, turn_id: &str) -> bool {
        self.sessions
            .read()
            .by_id
            .get(session_id)
            .and_then(|session| session.active_turn_id.as_deref())
            == Some(turn_id)
    }

    /// 返回指定 Session 当前唯一活跃的 prompt requestId。
    fn active_turn_id(&self, session_id: &str) -> Option<String> {
        self.sessions
            .read()
            .by_id
            .get(session_id)
            .and_then(|session| session.active_turn_id.clone())
    }

    /// 更新指定 Session 的执行状态。
    pub fn set_session_state(&self, session_id: &str, state: SessionState) -> Result<()> {
        let mut sessions = self.sessions.write();
        let session = sessions
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        session.state = state;
        self.diagnostics.log(
            "info",
            "runtime.state",
            format!("session_id={session_id} state={state:?}"),
        );
        Ok(())
    }

    /// Host 完成所有同步校验后原子接受一个前台 turn，并立即进入 Streaming。
    pub fn begin_session_turn(&self, session_id: &str, turn_id: String) -> Result<String> {
        let mut sessions = self.sessions.write();
        if self
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            anyhow::bail!("应用正在退出，不能开始新任务");
        }
        let turn_id = sessions.begin_turn(session_id, turn_id)?;
        drop(sessions);
        self.diagnostics.log(
            "info",
            "runtime.state",
            format!(
                "session_id={session_id} turn_id={turn_id} state={:?}",
                SessionState::Streaming
            ),
        );
        Ok(turn_id)
    }

    /// 完成 MCP 等异步前置准备后，为指定 client turn 原子取得 prompt 派发权。
    ///
    /// stop 在准备期间仍按 `Preparing` 本地收口；只有全部前置工作完成后才切到
    /// `Dispatched`，避免 cancel 先于 prompt 进入 Peri 而被静默丢弃。
    pub async fn prepare_session_prompt_dispatch(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<bool> {
        self.ensure_mcp_initialized().await?;
        self.sessions
            .write()
            .begin_prompt_dispatch(session_id, turn_id)
    }

    /// 把 stop 严格绑定到前端传入的 client turn；不允许按 Session 猜测当前回合。
    pub fn request_session_stop(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<SessionStopAction> {
        self.sessions.write().request_stop(session_id, turn_id)
    }

    /// cancel 通知未能进入 transport 时恢复该 turn 的可重试状态。
    pub fn rollback_session_stop_request(&self, session_id: &str, turn_id: &str) -> Result<()> {
        self.sessions
            .write()
            .rollback_stop_request(session_id, turn_id)
    }

    /// 收口当前前台 turn；expected 为 None 时接受 ACP done，为 Some 时供后台请求兜底。
    pub fn finish_session_turn(
        &self,
        session_id: &str,
        expected: Option<&str>,
        error: Option<String>,
    ) -> Result<Option<String>> {
        let finished = self
            .sessions
            .write()
            .finish_turn(session_id, expected, error)?;
        if let Some(ref turn_id) = finished {
            self.diagnostics.log(
                "info",
                "runtime.state",
                format!(
                    "session_id={session_id} turn_id={turn_id} state={:?}",
                    SessionState::Ready
                ),
            );
        }
        Ok(finished)
    }

    /// 后台 `session/prompt` 在 ACP 未发完成边界前失败时，复用现有 Agent 事件投影。
    pub fn emit_prompt_failure(
        &self,
        app: &AppHandle,
        session_id: &str,
        turn_id: &str,
        message: &str,
        completed_at_ms: u64,
    ) {
        let event_json = json!({
            "type": "agent_execution_failed",
            "value": { "code": "runtime_error", "message": message },
        })
        .to_string();
        let notifications = app.state::<Arc<crate::task_notifications::TaskNotifications>>();
        notifications.observe_agent_event(session_id, turn_id, &event_json);
        let _ = app.emit(
            "acp://agent-event",
            json!({
                "method": "peri/agent_event",
                "params": {
                    "sessionId": session_id,
                    "requestId": turn_id,
                    "event_json": event_json
                },
            }),
        );
        self.emit_local_turn_done(app, session_id, turn_id, "end_turn", completed_at_ms);
    }

    /// Host 本地收口尚未派发或早期失败的 turn，并复用唯一 agent-done 投影。
    pub fn emit_local_turn_done(
        &self,
        app: &AppHandle,
        session_id: &str,
        turn_id: &str,
        stop_reason: &str,
        completed_at_ms: u64,
    ) {
        let notifications = app.state::<Arc<crate::task_notifications::TaskNotifications>>();
        let task_title = self.session(session_id).and_then(|session| session.title);
        notifications.notify_done(app, session_id, turn_id, task_title.as_deref(), stop_reason);
        let _ = app.emit(
            "acp://agent-done",
            json!({
                "method": "peri/agent_event_done",
                "params": {
                    "sessionId": session_id,
                    "requestId": turn_id,
                    "stopReason": stop_reason,
                    "_meta": { "doneKind": "turn" },
                    "_keencode": { "completedAtMs": completed_at_ms },
                },
            }),
        );
    }

    /// 标记指定 Session 是否已经加载进当前 ACP server。
    pub fn set_session_loaded(&self, session_id: &str, loaded: bool) -> Result<()> {
        let mut sessions = self.sessions.write();
        let session = sessions
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        session.loaded = loaded;
        Ok(())
    }

    /// 更新指定 Session 的最近错误。
    pub fn set_session_error(&self, session_id: &str, error: Option<String>) -> Result<()> {
        if let Some(value) = &error {
            self.diagnostics.error(
                "runtime.state",
                format!("session_id={session_id} last_error={value}"),
            );
        }
        let mut sessions = self.sessions.write();
        let session = sessions
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        session.last_error = error;
        Ok(())
    }

    /// 会话重命名后只同步目标 Session 的标题。
    pub fn set_session_title(&self, session_id: &str, title: String) -> Result<()> {
        let mut sessions = self.sessions.write();
        let session = sessions
            .by_id
            .get_mut(session_id)
            .with_context(|| format!("Session 尚未登记：{session_id}"))?;
        session.title = Some(title);
        Ok(())
    }

    /// ACP transport 断开时将全部已登记 Session 独立标记为断开。
    fn mark_transport_disconnected(&self, error: &str) -> Vec<(String, String)> {
        // 传输已经不可恢复，继续运行模型或后台任务只会产生用户无法接收的结果，
        // 并可能继续消耗网络与额度。先触发共享取消 token，再清理界面投影。
        for session_id in self.active_session_ids() {
            self.cancel_session_work(&session_id);
        }
        let mut sessions = self.sessions.write();
        let mut interrupted_turns = Vec::new();
        for session in sessions.by_id.values_mut() {
            if let Some(turn_id) = session.active_turn_id.take() {
                interrupted_turns.push((session.session_id.clone(), turn_id));
            }
            session.active_turn_dispatch = None;
            session.state = SessionState::Disconnected;
            session.last_error = Some(error.to_owned());
            session.loaded = false;
        }
        self.pending_by_session.lock().clear();
        interrupted_turns
    }

    /// 写入运行时所属的诊断日志。
    pub fn log(&self, level: &str, component: &str, message: impl AsRef<str>) {
        self.diagnostics.log(level, component, message);
    }

    // ── Elicitation 应答 ─────────────────────────────────────────────────────

    /// 取出并消费一个挂起的 ACP 请求。
    fn take_pending(&self, rpc_id: i64) -> Option<RequestId> {
        let mut pending = self.pending_by_session.lock();
        take_pending_by_rpc(&mut pending, rpc_id)
    }

    /// 前端回答 elicitation 后回送 ACP 响应。
    pub async fn respond_rpc(&self, rpc_id: i64, result: Value) -> Result<()> {
        let request_id = self
            .take_pending(rpc_id)
            .with_context(|| format!("未知 rpcId：{rpc_id}（可能已超时或重复响应）"))?;
        self.transport
            .send_response(request_id, Ok(result))
            .await
            .map_err(|error| anyhow::anyhow!(error.message))
    }

    /// 停止回合时取消目标 Session 挂起的 elicitation 请求。
    pub async fn cancel_pending_for(&self, session_id: &str) {
        let pending = {
            let mut requests = self.pending_by_session.lock();
            requests
                .remove(session_id)
                .map(|requests| requests.into_values().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        for request_id in pending {
            let _ = self
                .transport
                .send_response(request_id, Ok(json!({"action": "cancel"})))
                .await;
        }
    }
}

/// 计算 MCP 运行时配置的稳定 SHA-256 内容摘要。
fn mcp_config_fingerprint(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// 未配置供应商时的占位 LlmProvider（空密钥，`configured=false` 区分）。
fn placeholder_provider() -> LlmProvider {
    LlmProvider::OpenAi {
        api_key: String::new(),
        base_url: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o".to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
        supports_vision: false,
        retry_observer: None,
    }
}

/// 从当前 ACP elicitation 参数读取唯一的 Session ID。
fn elicitation_session_id(params: &Value) -> Result<String, peri_acp::transport::types::AcpError> {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            peri_acp::transport::types::AcpError::new(-32602, "elicitation/create 缺少 sessionId")
        })
}

/// 按全局唯一 rpcId 从 Session 分区中取出一个挂起请求。
fn take_pending_by_rpc(
    pending: &mut HashMap<String, HashMap<i64, RequestId>>,
    rpc_id: i64,
) -> Option<RequestId> {
    let session_id = pending.iter().find_map(|(session_id, requests)| {
        requests.contains_key(&rpc_id).then(|| session_id.clone())
    })?;
    let request_id = pending
        .get_mut(&session_id)
        .and_then(|requests| requests.remove(&rpc_id));
    if pending.get(&session_id).is_some_and(HashMap::is_empty) {
        pending.remove(&session_id);
    }
    request_id
}

/// KeenCode 当前前端契约只接受 ACP 数字请求标识；其他结构直接报协议错误。
fn request_id_number(id: &RequestId) -> Result<i64, peri_acp::transport::types::AcpError> {
    match id {
        RequestId::Number(number) => Ok(*number),
        RequestId::String(_) => Err(peri_acp::transport::types::AcpError::new(
            -32600,
            "KeenCode 只接受数字 ACP 请求标识",
        )),
    }
}

/// 从当前 peri Agent 事件中读取会话错误正文。
fn agent_execution_failure(event_json: &str) -> Option<String> {
    let event: Value = serde_json::from_str(event_json).ok()?;
    (event.get("type").and_then(Value::as_str) == Some("agent_execution_failed"))
        .then(|| {
            event
                .get("value")?
                .get("message")?
                .as_str()
                .map(str::to_owned)
        })
        .flatten()
}

/// 仅为缺少 requestId 的实时通知补齐当前回合关联，不覆盖已有关联。
fn attach_request_id_if_missing(params: &mut Value, request_id: &str) {
    if params
        .get("requestId")
        .and_then(Value::as_str)
        .is_some_and(|existing| !existing.trim().is_empty())
    {
        return;
    }
    if let Some(object) = params.as_object_mut() {
        object.insert("requestId".to_owned(), Value::String(request_id.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EmbeddedHostAssemblyInput, McpRuntimeState, RuntimeSession, RuntimeSessions,
        SessionSnapshot, SessionState, SessionStopAction, agent_execution_failure,
        assemble_embedded_server_config, attach_request_id_if_missing, elicitation_session_id,
        mcp_config_fingerprint, placeholder_provider, running_background_task, take_pending_by_rpc,
    };
    use peri_acp::transport::types::RequestId;
    use peri_acp_types::store::ThreadStore;
    use peri_acp_types::tasks::BgTaskKind;
    use peri_agent::agent::async_tasks::{BackgroundTaskStatus, BgTaskInfo};
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// 嵌入式 Host 必须与桌面热更新逻辑共享插件 Skills 与 Hooks 事实源。
    #[test]
    fn embedded_host_shares_hot_reload_plugin_state() {
        let plugin_skill_roots = Arc::new(parking_lot::RwLock::new(Vec::new()));
        let plugin_hooks = Arc::new(parking_lot::RwLock::new(Vec::new()));
        let concrete_mcp_pool = Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
        let mcp_pool: Arc<dyn peri_acp_types::ports::McpPoolPort> = concrete_mcp_pool;
        let thread_store: Arc<dyn ThreadStore> =
            Arc::new(peri_resources::sessions::FilesystemThreadStore::new(
                std::env::temp_dir().join("keencode-embedded-host-test"),
            ));
        let config = assemble_embedded_server_config(EmbeddedHostAssemblyInput {
            provider: Arc::new(parking_lot::RwLock::new(placeholder_provider())),
            request_observer: None,
            peri_config: Arc::new(parking_lot::RwLock::new(Default::default())),
            mcp_pool: Some(mcp_pool),
            plugin_skill_roots: Arc::clone(&plugin_skill_roots),
            plugin_agent_dirs: Vec::new(),
            plugin_hooks: Arc::clone(&plugin_hooks),
            plugin_lsp_servers: Vec::new(),
            thread_store,
            config_path: std::env::temp_dir().join("keencode-embedded-host-settings.json"),
        });

        assert!(Arc::ptr_eq(&plugin_skill_roots, &config.plugin_skill_roots));
        assert!(Arc::ptr_eq(&plugin_hooks, &config.plugin_hooks_only));
        assert!(config.oauth_event_tx.is_some());
        assert!(
            config.settings_hooks_enabled,
            "桌面嵌入式 Host 必须按会话 cwd 加载普通 settings Hooks"
        );
    }

    /// MCP 配置内容不变时保持同一指纹，内容变化后才触发下一任务重载。
    #[test]
    fn mcp_runtime_state_tracks_applied_fingerprint() {
        let first = mcp_config_fingerprint(br#"{"mcpServers":{}}"#);
        let second = mcp_config_fingerprint(br#"{"mcpServers":{"new":{}}}"#);
        let mut state = McpRuntimeState::default();

        assert!(!state.is_current(&first));
        state.applied_fingerprint = Some(first);
        assert!(state.is_current(&first));
        assert!(!state.is_current(&second));
    }

    /// 会话状态必须按当前前端契约序列化为 snake_case。
    #[test]
    fn session_state_serializes_with_current_contract() {
        assert_eq!(
            serde_json::to_string(&SessionState::Disconnected).unwrap(),
            "\"disconnected\""
        );
    }

    /// 运行时快照不再伪造当前模型字段。
    #[test]
    fn session_snapshot_has_no_fake_model_field() {
        let snapshot = SessionSnapshot {
            session_id: Some("session-1".to_string()),
            state: SessionState::Ready,
            active_turn_id: None,
            backend: "peri_acp",
            project_path: Some("/tmp/demo".to_string()),
            title: Some("Demo".to_string()),
            last_error: None,
            diagnostics_path: "/tmp/keencode.log".to_string(),
        };

        let value = serde_json::to_value(snapshot).unwrap();
        assert!(
            value
                .get("activeTurnId")
                .is_some_and(|value| value.is_null())
        );
        assert!(value.get("modelId").is_none());
    }

    /// 后台任务投影必须保留 Agent 类别，并排除已经结束的登记项。
    #[test]
    fn background_task_projection_keeps_kind_and_running_state() {
        let running = running_background_task(
            "session-a",
            BgTaskInfo {
                task_id: "agent-task-1".to_owned(),
                kind: BgTaskKind::Agent,
                child_thread_id: Some("child-thread-1".to_owned()),
                summary: "检查实现".to_owned(),
                status: BackgroundTaskStatus::Running,
                started_at: "2026-08-14T08:00:00Z".to_owned(),
                duration_ms: 320,
                pid: None,
                output_preview: None,
            },
        )
        .expect("运行中的 Agent 任务必须可见");
        let serialized = serde_json::to_value(running).unwrap();
        assert_eq!(serialized["sessionId"], "session-a");
        assert_eq!(serialized["taskId"], "agent-task-1");
        assert_eq!(serialized["kind"], "agent");
        assert_eq!(serialized["childThreadId"], "child-thread-1");

        assert!(
            running_background_task(
                "session-a",
                BgTaskInfo {
                    task_id: "done-task".to_owned(),
                    kind: BgTaskKind::Shell,
                    child_thread_id: None,
                    summary: "已完成".to_owned(),
                    status: BackgroundTaskStatus::Completed,
                    started_at: "2026-08-14T08:00:00Z".to_owned(),
                    duration_ms: 640,
                    pid: Some(12),
                    output_preview: Some("done".to_owned()),
                },
            )
            .is_none()
        );
    }

    /// 多 Session 状态必须按 ID 完全隔离，焦点切换不得改写后台状态。
    #[test]
    fn runtime_sessions_keep_independent_state() {
        let mut sessions = RuntimeSessions::default();
        sessions.by_id.insert(
            "session-a".to_owned(),
            RuntimeSession::new(
                "session-a".to_owned(),
                "/tmp/a".to_owned(),
                Some("A".to_owned()),
                SessionState::Streaming,
                true,
            ),
        );
        sessions.by_id.insert(
            "session-b".to_owned(),
            RuntimeSession::new(
                "session-b".to_owned(),
                "/tmp/b".to_owned(),
                Some("B".to_owned()),
                SessionState::Ready,
                true,
            ),
        );
        sessions.focused_session_id = Some("session-b".to_owned());
        sessions.by_id.get_mut("session-a").unwrap().last_error = Some("A failed".to_owned());

        assert_eq!(sessions.by_id["session-a"].state, SessionState::Streaming);
        assert_eq!(sessions.by_id["session-b"].state, SessionState::Ready);
        assert_eq!(
            sessions.by_id["session-a"].last_error.as_deref(),
            Some("A failed")
        );
        assert_eq!(sessions.by_id["session-b"].last_error, None);
        assert_eq!(sessions.focused_session_id.as_deref(), Some("session-b"));
    }

    /// WebView 恢复快照必须覆盖全部并行 Session，且不暴露已完成的 turn。
    #[test]
    fn runtime_active_turn_snapshot_covers_all_running_sessions() {
        let mut sessions = RuntimeSessions::default();
        for session_id in ["session-b", "session-a", "session-ready"] {
            sessions.by_id.insert(
                session_id.to_owned(),
                RuntimeSession::new(
                    session_id.to_owned(),
                    format!("/tmp/{session_id}"),
                    None,
                    SessionState::Ready,
                    true,
                ),
            );
        }
        sessions
            .begin_turn("session-b", "turn-b".to_owned())
            .unwrap();
        sessions
            .begin_turn("session-a", "turn-a".to_owned())
            .unwrap();

        let turns = sessions.active_turns();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].session_id, "session-a");
        assert_eq!(turns[0].turn_id, "turn-a");
        assert_eq!(turns[1].session_id, "session-b");
        assert_eq!(turns[1].turn_id, "turn-b");

        sessions
            .finish_turn("session-a", Some("turn-a"), None)
            .unwrap();
        let active = sessions.active_turns();
        let completed = sessions.completed_turns();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].turn_id, "turn-b");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].session_id, "session-a");
        assert_eq!(completed[0].turn_id, "turn-a");
    }

    /// Host 接受必须原子切换 Streaming；重复 send 与过期后台收口不能破坏下一轮。
    #[test]
    fn runtime_turn_acceptance_is_atomic_and_correlated() {
        let mut sessions = RuntimeSessions::default();
        sessions.by_id.insert(
            "session-a".to_owned(),
            RuntimeSession::new(
                "session-a".to_owned(),
                "/tmp/a".to_owned(),
                Some("A".to_owned()),
                SessionState::Ready,
                true,
            ),
        );

        let first = sessions
            .begin_turn("session-a", "turn-1".to_owned())
            .unwrap();
        assert_eq!(first, "turn-1");
        assert_eq!(sessions.by_id["session-a"].state, SessionState::Streaming);
        assert!(
            sessions
                .begin_turn("session-a", "duplicate".to_owned())
                .is_err()
        );
        assert_eq!(
            sessions
                .finish_turn("session-a", Some("stale"), None)
                .unwrap(),
            None
        );
        assert_eq!(sessions.by_id["session-a"].state, SessionState::Streaming);

        assert_eq!(
            sessions
                .finish_turn("session-a", Some(&first), None)
                .unwrap(),
            Some(first.clone())
        );
        assert!(sessions.begin_turn("session-a", first.clone()).is_err());
        let second = sessions
            .begin_turn("session-a", "turn-2".to_owned())
            .unwrap();
        assert_eq!(second, "turn-2");
        assert_eq!(
            sessions
                .finish_turn("session-a", Some(&first), None)
                .unwrap(),
            None
        );
        assert_eq!(
            sessions.by_id["session-a"].active_turn_id.as_deref(),
            Some(second.as_str())
        );
        assert_eq!(sessions.by_id["session-a"].state, SessionState::Streaming);
    }

    /// 本轮失败事件写入的错误必须跨 done 保留；下一 turn 接受时才清理旧错。
    #[test]
    fn turn_done_preserves_observed_error_until_next_turn() {
        let mut sessions = RuntimeSessions::default();
        sessions.by_id.insert(
            "session-a".to_owned(),
            RuntimeSession::new(
                "session-a".to_owned(),
                "/tmp/a".to_owned(),
                None,
                SessionState::Ready,
                true,
            ),
        );
        let failed_turn = sessions
            .begin_turn("session-a", "turn-failed".to_owned())
            .unwrap();
        sessions.by_id.get_mut("session-a").unwrap().last_error =
            Some("provider failed".to_owned());
        sessions
            .finish_turn("session-a", Some(&failed_turn), None)
            .unwrap();
        assert_eq!(
            sessions.by_id["session-a"].last_error.as_deref(),
            Some("provider failed")
        );

        sessions
            .begin_turn("session-a", "turn-next".to_owned())
            .unwrap();
        assert_eq!(sessions.by_id["session-a"].last_error, None);
    }

    /// ack 后仍在准备上下文时 stop 必须本地收口，旧后台不得再派发 prompt。
    #[test]
    fn stop_during_preparation_prevents_stale_prompt_dispatch() {
        let mut sessions = RuntimeSessions::default();
        sessions.by_id.insert(
            "session-a".to_owned(),
            RuntimeSession::new(
                "session-a".to_owned(),
                "/tmp/a".to_owned(),
                None,
                SessionState::Ready,
                true,
            ),
        );
        let turn = sessions
            .begin_turn("session-a", "turn-a".to_owned())
            .unwrap();
        assert_eq!(
            sessions.request_stop("session-a", &turn).unwrap(),
            SessionStopAction::CompleteLocally(turn.clone())
        );
        assert_eq!(sessions.by_id["session-a"].state, SessionState::Ready);
        assert!(!sessions.begin_prompt_dispatch("session-a", &turn).unwrap());

        let next = sessions
            .begin_turn("session-a", "turn-b".to_owned())
            .unwrap();
        assert!(sessions.begin_prompt_dispatch("session-a", &next).unwrap());
        assert!(sessions.request_stop("session-a", &turn).is_err());
        assert_eq!(
            sessions.by_id["session-a"].active_turn_id.as_deref(),
            Some("turn-b")
        );
    }

    /// cancel 通知本身不是完成边界；旧 turn 在 done 前必须继续阻止新消息。
    #[test]
    fn cancelled_turn_stays_active_until_done_boundary() {
        let mut sessions = RuntimeSessions::default();
        sessions.by_id.insert(
            "session-a".to_owned(),
            RuntimeSession::new(
                "session-a".to_owned(),
                "/tmp/a".to_owned(),
                None,
                SessionState::Ready,
                true,
            ),
        );
        let cancelled_turn = sessions
            .begin_turn("session-a", "turn-cancel".to_owned())
            .unwrap();
        assert!(
            sessions
                .begin_prompt_dispatch("session-a", &cancelled_turn)
                .unwrap()
        );
        assert_eq!(
            sessions.request_stop("session-a", &cancelled_turn).unwrap(),
            SessionStopAction::NotifyRuntime(cancelled_turn.clone())
        );

        // session/cancel 已发送，但尚未收到 agent_event_done：状态不得提前改成 Ready。
        assert_eq!(sessions.by_id["session-a"].state, SessionState::Streaming);
        assert_eq!(
            sessions.by_id["session-a"].active_turn_id.as_deref(),
            Some(cancelled_turn.as_str())
        );
        assert!(
            sessions
                .begin_turn("session-a", "too-early".to_owned())
                .is_err()
        );

        assert_eq!(
            sessions.finish_turn("session-a", None, None).unwrap(),
            Some(cancelled_turn)
        );
        assert_eq!(sessions.by_id["session-a"].state, SessionState::Ready);
        assert!(
            sessions
                .begin_turn("session-a", "turn-next".to_owned())
                .is_ok()
        );
    }

    /// 持久元数据同步可以更新标题，但不得把已登记 Session 切到另一目录。
    #[test]
    fn runtime_session_metadata_rejects_cwd_replacement() {
        let mut sessions = RuntimeSessions::default();
        sessions
            .sync_metadata(
                "session-a".to_owned(),
                "/tmp/a".to_owned(),
                Some("A".to_owned()),
            )
            .unwrap();
        sessions.by_id.get_mut("session-a").unwrap().state = SessionState::Streaming;

        sessions
            .sync_metadata(
                "session-a".to_owned(),
                "/tmp/a".to_owned(),
                Some("Renamed".to_owned()),
            )
            .unwrap();
        assert_eq!(
            sessions.by_id["session-a"].title.as_deref(),
            Some("Renamed")
        );
        assert_eq!(sessions.by_id["session-a"].state, SessionState::Streaming);
        assert!(
            sessions
                .sync_metadata(
                    "session-a".to_owned(),
                    "/tmp/b".to_owned(),
                    Some("Tampered".to_owned()),
                )
                .is_err()
        );
        assert_eq!(sessions.by_id["session-a"].cwd, "/tmp/a");
    }

    /// 停止一个 Session 只能取走该 Session 的挂起问题。
    #[test]
    fn pending_elicitation_is_cancelled_per_session() {
        let mut pending = HashMap::from([
            (
                "session-a".to_owned(),
                HashMap::from([(1, RequestId::Number(1))]),
            ),
            (
                "session-b".to_owned(),
                HashMap::from([(2, RequestId::Number(2))]),
            ),
        ]);

        assert_eq!(
            pending
                .remove("session-a")
                .unwrap()
                .into_values()
                .collect::<Vec<_>>(),
            vec![RequestId::Number(1)],
        );
        assert_eq!(pending.len(), 1);
        assert_eq!(pending["session-b"][&2], RequestId::Number(2));
        assert_eq!(
            take_pending_by_rpc(&mut pending, 2),
            Some(RequestId::Number(2))
        );
        assert!(pending.is_empty());
    }

    /// elicitation 必须显式携带当前 ACP 契约的 Session ID。
    #[test]
    fn elicitation_requires_explicit_session_id() {
        assert_eq!(
            elicitation_session_id(&json!({"sessionId": "session-a"})).unwrap(),
            "session-a"
        );
        assert!(elicitation_session_id(&json!({"session_id": "session-a"})).is_err());
        assert!(elicitation_session_id(&json!({"sessionId": "  "})).is_err());
    }

    /// 后台早期错误沿用现有 agent_execution_failed 事件形状。
    #[test]
    fn prompt_failure_parser_matches_agent_event_contract() {
        assert_eq!(
            agent_execution_failure(
                r#"{"type":"agent_execution_failed","value":{"code":"runtime_error","message":"upstream failed"}}"#,
            )
            .as_deref(),
            Some("upstream failed"),
        );
        assert!(agent_execution_failure(r#"{"type":"llm_retrying"}"#).is_none());
    }

    #[test]
    fn request_id_fallback_does_not_overwrite_an_existing_turn() {
        let mut missing = json!({"sessionId": "session-a"});
        attach_request_id_if_missing(&mut missing, "turn-current");
        assert_eq!(missing["requestId"], "turn-current");

        let mut empty = json!({"sessionId": "session-a", "requestId": null});
        attach_request_id_if_missing(&mut empty, "turn-current");
        assert_eq!(empty["requestId"], "turn-current");

        let mut delayed = json!({
            "sessionId": "session-a",
            "requestId": "turn-old",
        });
        attach_request_id_if_missing(&mut delayed, "turn-current");
        assert_eq!(delayed["requestId"], "turn-old");
    }
}
