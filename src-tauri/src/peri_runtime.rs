//! peri ACP 运行时：进程内装配、事件泵与多 Session 生命周期。
//!
//! KeenCode 直接使用 MpscTransport 驱动 peri ACP。每个 Session 的工作目录、标题、
//! 状态、错误和待回答请求都按 Session ID 隔离；界面焦点只决定
//! `session_get_state` 展示哪个快照，不参与命令授权。

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use parking_lot::{Mutex, RwLock};
use peri_acp::provider::{LlmProvider, PeriConfig};
use peri_acp::server::{AcpServerConfig, run_acp_server};
use peri_acp::session::SessionManager;
use peri_acp::transport::AcpTransport;
use peri_acp::transport::mpsc::mpsc_transport_pair;
use peri_acp::transport::types::{IncomingMessage, RequestId};
use peri_middlewares::hitl::SharedPermissionMode;
use peri_middlewares::mcp::McpClientPool;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};

use crate::diagnostics::Diagnostics;
use crate::providers;

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

impl RuntimeSessions {
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
}

/// 前端会话快照（session_get_state 返回）。
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    /// 当前快照对应的 Session ID；idle 快照为空。
    pub session_id: Option<String>,
    /// 当前 Session 的独立执行状态。
    pub state: SessionState,
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

/// 进程内 peri 运行时句柄。
pub struct PeriRuntime {
    /// 客户端侧传输（命令经它发 JSON-RPC；recv 循环收通知/请求）。
    transport: Arc<dyn AcpTransport>,
    /// ACP server 共用的 SessionManager，用于退出时发现后台终端任务。
    session_manager: SessionManager,
    /// 会话持久化（SQLite）。
    pub thread_store: Arc<dyn peri_agent::thread::ThreadStore>,
    /// 当前 LlmProvider；未配置供应商时为占位（空密钥），由 provider_configured 区分。
    provider: Arc<RwLock<LlmProvider>>,
    /// 当前完整 peri 配置（供应商 + 四档 Profile）；保存设置后整体替换。
    peri_config: Arc<RwLock<PeriConfig>>,
    /// 当前是否已有有效供应商配置。
    provider_configured: std::sync::atomic::AtomicBool,
    /// 已启用插件声明的 Skill 根；插件变更后整体热替换。
    plugin_skill_roots: Arc<RwLock<Vec<peri_middlewares::skills::SkillRoot>>>,
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
        let thread_store: Arc<dyn peri_agent::thread::ThreadStore> = Arc::new(
            peri_agent::thread::SqliteThreadStore::new(threads_dir.join("threads.db"))
                .await
                .context("打开会话数据库失败")?,
        );
        diagnostics.log("info", "runtime.storage", "会话存储初始化完成");

        let permission_mode =
            SharedPermissionMode::new(peri_middlewares::hitl::PermissionMode::Bypass);
        let session_manager = SessionManager::new(
            Arc::clone(&thread_store),
            provider_runtime.read().clone(),
            Arc::new(peri_config_runtime.read().clone()),
            Arc::clone(&permission_mode),
            None,
        );

        // ── 装配 AcpServerConfig（复刻上游 launch 顺序）──
        // 只读取并校验配置路径，不在应用启动阶段拉起 MCP 子进程或 HTTP 连接；
        // 首个真正执行的任务会通过 McpClientPool 按需初始化。
        let project_dir = std::env::current_dir().map_err(anyhow::Error::msg)?;
        let snapshot = crate::extensions::claude_runtime_snapshot(app, &project_dir)
            .map_err(anyhow::Error::msg)?;
        let mcp_pool = Some(Arc::new(McpClientPool::new_pending()));
        let plugin_skill_roots = Arc::new(RwLock::new(
            crate::extensions::runtime_skill_roots(app).map_err(anyhow::Error::msg)?,
        ));
        let plugin_agent_dirs =
            crate::extensions::runtime_plugin_agent_dirs(app).map_err(anyhow::Error::msg)?;
        let shared_tools = Arc::new(parking_lot::RwLock::new(std::collections::BTreeMap::new()));

        let server_config = AcpServerConfig {
            provider: Arc::clone(&provider_runtime),
            peri_config: Arc::clone(&peri_config_runtime),
            permission_mode: Arc::clone(&permission_mode),
            cron_scheduler: None,
            mcp_pool,
            channel_state: None,
            plugin_skill_roots: Arc::clone(&plugin_skill_roots),
            plugin_agent_dirs,
            plugin_hooks: snapshot.plugin_hooks.clone(),
            plugin_loaded: Vec::new(),
            hook_groups: vec![snapshot.plugin_hooks],
            plugin_lsp_servers: Vec::new(),
            // 为所有会话共享当前运行时的工具搜索索引。
            tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
            shared_tools,
            thread_store: Arc::clone(&thread_store),
            langfuse_session: None,
            config_path: crate::storage::root_dir(app)?.join("peri-settings.json"),
            session_manager: session_manager.clone(),
        };

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
            provider_configured: std::sync::atomic::AtomicBool::new(configured),
            plugin_skill_roots,
            sessions: RwLock::new(RuntimeSessions::default()),
            pending_by_session: Mutex::new(HashMap::new()),
            diagnostics,
            _tracing_guard: tracing_guard,
        });
        // 用量统计必须先进入 Tauri state，事件泵才能在收到 usage_update 时读取。
        app.manage(Arc::new(crate::analytics::AnalyticsRecorder::new(app)?));
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
        let Some(active_id) = listed.active_provider_id.as_deref() else {
            return Ok((
                placeholder_provider(),
                providers::build_peri_config_all(listed.providers),
                false,
            ));
        };
        // 与 providers::select_model 相同的语义：当前模型必须属于激活供应商。
        let Some(active) = listed.providers.iter().find(|p| p.id == active_id) else {
            return Ok((
                placeholder_provider(),
                providers::build_peri_config_all(listed.providers),
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
                providers::build_peri_config_all(listed.providers),
                false,
            ));
        };
        let (context_1m, context_window) = providers::resolve_context(active, model);
        let peri_config = providers::build_peri_config_all(listed.providers);
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

    /// 从当前插件清单热替换后续任务使用的 Skill 根。
    pub fn reload_plugin_skills(&self, app: &AppHandle) -> Result<()> {
        let roots = crate::extensions::runtime_skill_roots(app).map_err(anyhow::Error::msg)?;
        *self.plugin_skill_roots.write() = roots;
        // 插件状态变化同时重写合并后的 MCP 运行时文件；McpClientPool 会在下一次
        // 任务按文件指纹重连，不需要重启桌面进程。
        let _ = crate::extensions::mcp_config_path(app).map_err(anyhow::Error::msg)?;
        self.diagnostics
            .log("info", "runtime.plugins", "插件 Skills 热加载完成");
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
                    IncomingMessage::Notification { method, params } => {
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
                        if method == "peri/agent_event"
                            && let (Some(session_id), Some(event_json)) = (
                                params.get("sessionId").and_then(Value::as_str),
                                params.get("event_json").and_then(Value::as_str),
                            )
                        {
                            app.state::<Arc<crate::task_notifications::TaskNotifications>>()
                                .observe_agent_event(session_id, event_json);
                        }
                        if method == "peri/agent_event_done"
                            && let (Some(session_id), Some(stop_reason)) = (
                                params.get("sessionId").and_then(Value::as_str),
                                params.get("stopReason").and_then(Value::as_str),
                            )
                        {
                            let task_title = runtime
                                .upgrade()
                                .and_then(|runtime| runtime.session(session_id))
                                .and_then(|session| session.title);
                            app.state::<Arc<crate::task_notifications::TaskNotifications>>()
                                .notify_done(&app, session_id, task_title.as_deref(), stop_reason);
                        }
                        if method == "session/update"
                            && let (Some(session_id), Some(update)) = (
                                params.get("sessionId").and_then(Value::as_str),
                                params.get("update").cloned(),
                            )
                            && update.get("type").and_then(Value::as_str) == Some("usage_update")
                            && update.get("_meta").is_some()
                        {
                            app.state::<Arc<crate::analytics::AnalyticsRecorder>>()
                                .observe_usage_update(session_id, &update);
                        }
                        if let Some(event) = event {
                            let _ = app.emit(event, json!({ "method": method, "params": params }));
                        } else if let Some(runtime) = runtime.upgrade() {
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
                runtime.mark_transport_disconnected("ACP transport 已断开");
            }
            let _ = app.emit("acp://closed", json!({}));
        });
    }

    /// 发送 JSON-RPC 请求并等待响应。
    pub async fn send_request(&self, method: &str, params: Value) -> Result<Value> {
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
        for session_id in self.session_manager.sessions_with_background_tasks() {
            if !session_ids.contains(&session_id) {
                session_ids.push(session_id);
            }
        }
        session_ids
    }

    /// 同步终止指定 Session 的 Agent 与后台终端任务。
    pub fn cancel_session_for_exit(&self, session_id: &str) {
        self.session_manager.cancel_session_for_exit(session_id);
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
                backend: "peri_acp",
                project_path: None,
                title: None,
                last_error: None,
                diagnostics_path: self.diagnostics.path().display().to_string(),
            },
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
    fn mark_transport_disconnected(&self, error: &str) {
        let mut sessions = self.sessions.write();
        for session in sessions.by_id.values_mut() {
            session.state = SessionState::Disconnected;
            session.last_error = Some(error.to_owned());
            session.loaded = false;
        }
        self.pending_by_session.lock().clear();
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

#[cfg(test)]
mod tests {
    use super::{
        RuntimeSession, RuntimeSessions, SessionSnapshot, SessionState, elicitation_session_id,
        take_pending_by_rpc,
    };
    use peri_acp::transport::types::RequestId;
    use serde_json::json;
    use std::collections::HashMap;

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
            backend: "peri_acp",
            project_path: Some("/tmp/demo".to_string()),
            title: Some("Demo".to_string()),
            last_error: None,
            diagnostics_path: "/tmp/keencode.log".to_string(),
        };

        let value = serde_json::to_value(snapshot).unwrap();
        assert!(value.get("modelId").is_none());
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
}
