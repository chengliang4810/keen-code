//! ACP Host — transport-agnostic request handler（自 peri-tui 迁出归位）。
//!
//! Accepts any [`AcpTransport`] implementation (the embedded host uses mpsc),
//! builds and executes ReAct agents, and pushes [`SessionUpdate`] notifications
//! back through the transport. ACP Host = 部署单元（`docs/top-level.md` §7/§19）：
//! 由 cli/TUI 作为部署装配点启动，TUI 进程不再持有控制面。
//!
//! **Cancel architecture**: `session/prompt` execution is spawned into a
//! background tokio task so the main server loop remains responsive to
//! `session/cancel` notifications. Sessions are shared via
//! `Arc<tokio::sync::Mutex<HashMap>>`.
//!
//! **多读者 + 单 writer lease**（[`lease`]）：每个 session 的 writer 唯一
//! （可提交输入/取消），观察者只读。策略先行，协议级扩展另立 issue。

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use crate::dispatch::prompt::extract_prompt_params;
pub use crate::session::state_builders::build_config_options;
use crate::transport::types::{AcpError, IncomingMessage};
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::SettingsHooksPort;
use peri_acp_types::interaction::ChannelState;
use peri_acp_types::messages::BaseMessage;
use peri_acp_types::plugin::PluginManagerPort;
use peri_acp_types::ports::{LspPoolPort, McpPoolPort, SkillsPort, ToolSearchPort};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::provider::{LlmProvider, PeriConfig};
use peri_acp_types::event_data::PredictionAction;

pub mod assemble;
pub(crate) mod compact_config;
pub mod controller_ports;
#[cfg(test)]
#[path = "executor_flow_test.rs"]
mod executor_flow_tests;
mod goal_requests;
pub mod lease;
mod model_factory;
#[cfg(test)]
#[path = "model_factory_test.rs"]
mod model_factory_tests;
mod notify;
mod prompt;
pub mod prompt_handle;
mod requests;
pub mod stage_builder;

pub(crate) use goal_requests::{
    handle_goal_clear, handle_goal_get, handle_goal_transition, handle_goal_upsert,
};
pub(crate) use notify::{extract_session_id, handle_notification, send_session_info_update};
pub(crate) use prompt::run_prompt;
pub(crate) use requests::handle_request;

// ── Session state ────────────────────────────────────────────────────────────

pub(crate) struct SessionState {
    #[allow(dead_code)] // session 标识字段，保留供调试
    session_id: String,
    thread_id: String,
    cwd: String,
    pub(crate) history: Vec<BaseMessage>,
    cancel_token: Option<CancellationToken>,
    // ── Frozen session data (populated at creation, immutable thereafter) ──
    pub(crate) frozen: Option<crate::session::executor::FrozenSessionData>,
    /// Recall items from previous turn (injected as <system-reminder> in next user message).
    pub(crate) recall_items: Vec<String>,
    /// Session-scoped agent component pool for reusing heavy objects across prompts.
    pub(crate) agent_pool: crate::session::agent_pool::AgentPool,
    /// 当前会话独立的模型供应商快照；切换模型只更新本会话。
    pub(crate) provider: Arc<parking_lot::RwLock<LlmProvider>>,
    /// 当前会话选择的供应商稳定标识；供应商热更新时据此重建快照。
    pub(crate) provider_id: String,
    /// 当前会话独立的 deferred 工具注册表与搜索索引。
    ///
    /// Host 级的实现对象只作为装配模板保留；运行时工具必须绑定到此处，
    /// 否则一个会话构造的 cwd 工具会被另一个会话的 Search/ExecuteExtraTool
    /// 看见。该对象跨 turn 复用，但绝不跨 Session 共享。
    pub(crate) tool_registry: SessionToolRegistry,
    /// Session 级 LSP 服务器池（session/new 时创建，跨 turn 复用；H1）。
    pub(crate) lsp_pool: Option<Arc<dyn LspPoolPort>>,
    // ── Prediction 写入的会话元数据（MVP：仅存储，不展示）──
    /// 预测生成的会话标题（未来 /rename 与标题栏显示使用）。
    pub(crate) title: Option<String>,
    /// 预测生成的会话标签（未来按标签检索使用）。
    pub(crate) tags: Vec<String>,
    /// 多读者 + 单 writer lease：session 创建方（writer）唯一可提交输入/取消。
    ///
    /// 协议无客户端身份字段（`clientId` 属协议级扩展，另立 issue），writer 恒为
    /// `"default"`；prompt/cancel 入口经 [`lease::WriterLease::is_writer`] 校验。
    pub(crate) lease: lease::WriterLease,
}

/// Session-scoped deferred-tool state.
///
/// Both fields are intentionally owned by the SessionState/SessionInfo that
/// uses them. Keeping the index and the executable map together prevents a
/// search result from resolving against a different session's tool instance.
#[derive(Clone)]
pub(crate) struct SessionToolRegistry {
    pub(crate) tool_search_index: Arc<dyn ToolSearchPort>,
    pub(crate) shared_tools:
        Arc<parking_lot::RwLock<BTreeMap<String, Arc<dyn peri_agent::tools::BaseTool>>>>,
}

impl SessionToolRegistry {
    pub(crate) fn new() -> Self {
        Self {
            tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
            shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        }
    }

    /// Start a new foreground turn with an empty tool snapshot.
    ///
    /// Prompt dispatch serializes turns for a session, so this only mutates
    /// this session's registry at its boundary. It never clears a host-wide
    /// map and cannot affect another session's tools.
    pub(crate) fn reset_for_turn(&self) {
        self.shared_tools.write().clear();
        self.tool_search_index.clear();
    }
}

// ── Server config ────────────────────────────────────────────────────────────

/// All cross-session configuration needed by the ACP server.
pub struct AcpServerConfig {
    pub provider: Arc<parking_lot::RwLock<LlmProvider>>,
    /// 宿主级模型请求观测器，随 provider 动态/缓存工厂显式传递。
    pub request_observer: Option<Arc<dyn peri_model::RequestObserver>>,
    pub peri_config: Arc<parking_lot::RwLock<PeriConfig>>,
    pub cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    pub mcp_pool: Option<Arc<dyn McpPoolPort>>,
    /// OAuth 授权事件通道（host 级，跨 session）：装配点创建 (tx, rx) 并注入
    /// tx（MCP 授权回调经此转发 AcpEvent），run_acp_server take rx 后 spawn
    /// 消费者 task，以 `peri/agent_event` notification（sessionId 为空串，
    /// host 级事件不做 session 过滤）送达 TUI。
    pub oauth_event_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::event::AcpEvent>>,
    pub(crate) oauth_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::event::AcpEvent>>,
    pub channel_state: Option<Arc<ChannelState>>,
    /// 宿主可热替换的插件 Skill 根目录；每次请求读取一致性快照。
    pub plugin_skill_roots: Arc<parking_lot::RwLock<Vec<peri_acp_types::skills::SkillRoot>>>,
    pub plugin_agent_dirs: Vec<std::path::PathBuf>,
    pub plugin_hooks: Vec<peri_acp_types::hooks::RegisteredHook>,
    /// 仅插件 hooks（不含 settings hooks；`plugin/list` 命令面数据源——
    /// TUI hooks 面板经 ACP 拿数据，M-TUI 收口）。
    pub plugin_hooks_only: Arc<parking_lot::RwLock<Vec<peri_acp_types::hooks::RegisteredHook>>>,
    pub plugin_loaded: Vec<peri_acp_types::plugin::LoadedPlugin>,
    pub hook_groups: Vec<Vec<peri_acp_types::hooks::RegisteredHook>>,
    pub plugin_lsp_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
    /// Skills 扫描端口（available-commands / agents 扫描经此访问）。
    pub skills: Arc<dyn SkillsPort>,
    /// 插件管理端口（plugin/* 命令面经此访问）。
    pub plugin_manager: Arc<dyn PluginManagerPort>,
    /// Settings hooks 加载端口（hook 组装配经此访问）。
    pub settings_hooks: Arc<dyn SettingsHooksPort>,
    /// 是否按会话 cwd 在每轮执行前重新加载 global/project/local settings Hooks。
    /// bare Host 关闭；桌面嵌入式 Host 必须开启，避免多项目会话共用启动目录。
    pub settings_hooks_enabled: bool,
    pub thread_store: Arc<dyn peri_acp_types::store::ThreadStore>,
    /// Controller 层宿主：dispatch 存储操作（load/list/fork/execute-command/rewind）
    /// 经此访问持久化存储（ARC-BOUNDARY-001 方向，不再直操 `thread_store`）；
    /// 3.0 批 2：事件发射（`publish_event`）/ 执行发起（`run_session`）亦经此宿主。
    pub controller: Arc<peri_controller::Controller>,
    pub config_path: std::path::PathBuf,
    /// 共享 SessionManager：用于支撑后台任务与 goal_state。
    ///
    /// TUI 本地仍维护 SessionState（history/frozen/agent_pool 等），但 SubAgent
    /// 注册/注销与 goal_state 通过 SessionManager 中的 AcpSession 记录管理，
    /// 保证 `run_session_loop` 可访问会话级任务、消息与目标状态。
    pub session_manager: crate::session::SessionManager,
}

// ── Main server loop ────────────────────────────────────────────────────────

type SharedSessions = Arc<tokio::sync::Mutex<HashMap<String, SessionState>>>;
/// Per-session prompt serialization lock map.
pub(crate) type PromptLocks = Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;

/// Main ACP server loop. Accepts any `AcpTransport` (the embedded host uses mpsc).
///
/// `session/prompt` is spawned into a background task so the loop stays
/// responsive to `session/cancel` and other incoming messages.
pub async fn run_acp_server(
    transport: Arc<dyn crate::transport::AcpTransport>,
    mut cfg: AcpServerConfig,
) {
    // OAuth 授权事件消费者：host 级事件（无 session 归属），以空 sessionId
    // 的 peri/agent_event notification 送达 TUI（pump 侧对空 sessionId 放行）。
    let oauth_event_rx = cfg.oauth_event_rx.take();
    let cfg = Arc::new(cfg);
    if let Some(mut rx) = oauth_event_rx {
        let oauth_transport = Arc::clone(&transport);
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let event_json = match serde_json::to_string(&event) {
                    Ok(json) => json,
                    Err(e) => {
                        tracing::error!(error = %e, "OAuth event serialize failed");
                        continue;
                    }
                };
                if let Err(e) = oauth_transport
                    .send_notification(
                        "peri/agent_event",
                        serde_json::json!({
                            "sessionId": "",
                            "event_json": event_json,
                        }),
                    )
                    .await
                {
                    tracing::debug!(error = %e, "OAuth event notification send failed");
                }
            }
        });
    }
    let sessions: SharedSessions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    // Per-session prompt serialization lock: ensures that when a prompt completes
    // (state.history updated) the next prompt for the same session sees the updated history.
    let prompt_locks: PromptLocks = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    while let Some(msg) = transport.recv().await {
        match msg {
            IncomingMessage::Request { id, method, params } => {
                if method == "session/prompt" {
                    // Spawn long-running prompt execution so the server loop
                    // continues processing session/cancel notifications.
                    let prompt_session_id = extract_session_id(&params, "").to_string();
                    let sessions = sessions.clone();
                    let transport = Arc::clone(&transport);
                    let prompt_locks = prompt_locks.clone();
                    let cfg = Arc::clone(&cfg);
                    tokio::spawn(async move {
                        let result = dispatch_prompt_turn(
                            params,
                            &sessions,
                            &prompt_locks,
                            &transport,
                            &cfg,
                        )
                        .await;
                        let _ = transport.send_response(id, result).await;
                        if !prompt_session_id.is_empty() {
                            send_session_info_update(transport.as_ref(), &prompt_session_id).await;
                        }
                    });
                } else {
                    let mut sessions = sessions.lock().await;
                    let result =
                        handle_request(&method, &params, &cfg, &mut sessions, &transport).await;
                    let new_session_id = (method == "session/new")
                        .then(|| {
                            result
                                .as_ref()
                                .ok()?
                                .get("sessionId")?
                                .as_str()
                                .map(str::to_owned)
                        })
                        .flatten();
                    let response_sent = transport.send_response(id, result).await.is_ok();
                    if response_sent {
                        if let Some(session_id) = new_session_id {
                            notify::send_new_session_commands(
                                transport.as_ref(),
                                &cfg,
                                &sessions,
                                &session_id,
                            )
                            .await;
                        }
                    }
                }
            }
            IncomingMessage::Notification { method, params } => {
                let mut sessions = sessions.lock().await;
                handle_notification(&method, &params, &mut sessions, &cfg);
            }
            IncomingMessage::Response { .. } => {
                // Responses are routed internally by the transport's pending map.
            }
        }
    }

    // ── 宿主退出：优雅关闭所有会话的 LSP 服务器池（H1 shutdown 钩子）──
    // transport 关闭 = 宿主退出。sessions 即将 drop；每 turn 装配复用的是
    // 会话级 pool，此处显式 shutdown，避免 LSP 服务器子进程随进程残留。
    {
        let sessions = sessions.lock().await;
        for state in sessions.values() {
            if let Some(pool) = state.lsp_pool.as_ref() {
                pool.shutdown().await;
            }
        }
    }
}

/// 用户 prompt 的共享执行路径。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_prompt_turn(
    params: Value,
    sessions: &SharedSessions,
    prompt_locks: &PromptLocks,
    transport: &Arc<dyn crate::transport::AcpTransport>,
    cfg: &AcpServerConfig,
) -> Result<Value, AcpError> {
    let prompt_session_id = extract_session_id(&params, "").to_string();

    // 多读者 + 单 writer lease：prompt 是写入操作，仅 writer 可提交。
    // 协议无客户端身份字段，writer 恒为 session 创建方（"default"）——
    // 未来引入 clientId 后此处按请求方判定即可（见 lease 模块文档）。
    {
        let sessions = sessions.lock().await;
        if let Some(state) = sessions.get(&prompt_session_id) {
            if !state.lease.is_writer("default") {
                return Err(AcpError::new(
                    -32603,
                    "read-only observer cannot submit prompt",
                ));
            }
        }
    }

    // 挂起注入：session 当前在 await_wake 挂起（turn 在途但 idle，通常因 bg
    // 任务活跃——executor 在 run_react_loop 挂起期间置 idle_suspended 标志）。
    // 若在此等待 per-session prompt lock，注入会阻塞至当前 turn 完成——bg 任务
    // 可能长达数分钟，用户输入表现为"nothing happen"（TUI 侧 submit_consumer
    // 串行 await prompt RPC，被挂起的 RPC 卡住，后续提交全部排队）。
    // 正确语义：直接把用户消息推入 session inbox（Prompt + wake），挂起的
    // run_react_loop 醒来后由 Receive drain_all 消费，在**同一 turn** 内继续。
    // 注入后立即返回——当前 turn 的 TurnDone 会携带原 request_id（挂起时
    // 该 turn 已在执行），TUI 侧仅用 request_id 做 stale TurnInterrupted 配对，
    // TurnDone 路径不比对（见 peri-tui acp_events/turn.rs）。
    // prompt_with_bg_results 的 bgResults 在 run_session_loop 内 push Defer——挂起注入路径不携带
    // bgResults（该 RPC 仅无需挂起的会话路径使用，allow_await_wake=false 永不挂起）。
    if cfg.session_manager.is_idle_suspended(&prompt_session_id) {
        let (_, content, _attachments) = extract_prompt_params(&params)?;
        if let Some(inbox) = cfg.session_manager.session_inbox_for(&prompt_session_id) {
            inbox.handle().push_prompt(
                peri_acp_types::session::MessageSource::UserInput,
                BaseMessage::human(content),
            );
            tracing::info!(
                session_id = %prompt_session_id,
                "prompt injected while turn suspended (await_wake); loop will wake and consume"
            );
            return Ok(serde_json::json!({}));
        }
    }

    let prompt_lock = {
        let mut locks = prompt_locks.lock().await;
        locks
            .entry(prompt_session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };

    // Serialize prompts per session: wait for any in-flight prompt to finish
    // so that state.history is up-to-date when this prompt reads it.
    let _guard = prompt_lock.lock().await;

    // Extract AgentPool from session, wrap in Arc<Mutex> for
    // in-place modification inside executor.
    //
    // 取出必须在 prompt lock 之内：若在锁外取出，并发的用户 prompt 会取走被 `mem::replace`
    // 换出的空池并先行归还，导致两轮共享同一缓存的两个池实例互相覆盖、
    // 缓存丢失（跨轮次热缓存是本池的核心价值）。归还仍在锁内（函数末尾）。
    let pool_arc = {
        let mut sessions = sessions.lock().await;
        let pool = sessions
            .get_mut(&prompt_session_id)
            .map(|s| std::mem::take(&mut s.agent_pool))
            .unwrap_or_default();
        Arc::new(parking_lot::Mutex::new(pool))
    };

    // 每轮只克隆一次 Skill 根快照，避免扫描期间持有写锁并保留热更新语义。
    let plugin_skill_roots = cfg.plugin_skill_roots.read().clone();
    let hook_groups = if cfg.settings_hooks_enabled {
        let cwd = {
            let sessions = sessions.lock().await;
            sessions
                .get(&prompt_session_id)
                .map(|state| state.cwd.clone())
                .ok_or_else(|| AcpError::new(-32602, "session not found"))?
        };
        let plugin_hooks = cfg.plugin_hooks_only.read().clone();
        assemble::assemble_hook_groups(&plugin_hooks, cfg.settings_hooks.as_ref(), &cwd, false)
    } else {
        cfg.hook_groups.clone()
    };
    let result = run_prompt(
        params,
        sessions,
        &cfg.provider,
        cfg.request_observer.clone(),
        &cfg.peri_config,
        cfg.cron_scheduler.clone(),
        &plugin_skill_roots,
        &cfg.plugin_agent_dirs,
        &cfg.plugin_loaded,
        &hook_groups,
        cfg.mcp_pool.clone(),
        cfg.channel_state.clone(),
        cfg.skills.clone(),
        &cfg.plugin_lsp_servers,
        transport,
        &cfg.thread_store,
        &cfg.controller,
        pool_arc.clone(),
        cfg.session_manager.clone(),
    )
    .await;

    // Prediction: agent 成功完成后发起预测输入请求。
    if result.is_ok() {
        let pred_transport = Arc::clone(transport);
        let pred_session_id = prompt_session_id.clone();
        let pred_provider = cfg.provider.clone();
        let pred_sessions = sessions.clone();
        let pred_thread_store = cfg.thread_store.clone();
        let pred_caps_registry = cfg.session_manager.caps_registry();
        let pred_request_observer = cfg.request_observer.clone();

        tokio::spawn(async move {
            tracing::debug!("Prediction task started");
            // 从 session 获取最新历史与当前标题
            let (history, cwd, current_title) = {
                let sessions = pred_sessions.lock().await;
                match sessions.get(&pred_session_id) {
                    Some(s) => (s.history.clone(), s.cwd.clone(), s.title.clone()),
                    None => {
                        tracing::debug!("Prediction: session not found");
                        return;
                    }
                }
            };

            // 取最近 10 条消息作为上下文（排除 System 消息）
            let recent: Vec<_> = history
                .iter()
                .rev()
                .filter(|m| !m.is_system())
                .take(10)
                .cloned()
                .collect();
            let recent: Vec<_> = recent.into_iter().rev().collect();

            if recent.is_empty() {
                tracing::debug!("Prediction: no recent messages");
                return;
            }
            tracing::debug!(count = recent.len(), "Prediction: got messages");

            // 直接复用已构建的 LlmProvider（绕过 from_config）
            let llm_provider = pred_provider.read().clone();
            tracing::debug!("Prediction: LLM provider ready");

            // Facade：agent 构建与执行统一由 peri-acp executor 承担，
            // TUI 层不再直接构建 Agent（遵守 CLAUDE.md [TRAP]）。
            // L5：LLM 构造（AgentModelBridge）在协议面完成，执行体只收 ReactLLM。
            let llm: Box<dyn peri_agent::agent::react::ReactLLM + Send + Sync> = Box::new(
                peri_agent::agent::model_bridge::AgentModelBridge::new(Arc::from(
                    llm_provider.into_model_with_request_observer(pred_request_observer),
                ))
                .with_session_id(pred_session_id.clone())
                .with_purpose("prediction"),
            );
            let result = crate::session::executor::execute_prediction(
                llm,
                recent,
                &cwd,
                current_title.as_deref(),
            )
            .await;

            match result {
                Ok(actions) => {
                    if actions.is_empty() {
                        tracing::debug!("Prediction: empty actions");
                        return;
                    }
                    // 元数据动作写入 session 状态；标题变更待持久化并推送
                    let mut applied_title: Option<String> = None;
                    {
                        let mut sessions = pred_sessions.lock().await;
                        if let Some(state) = sessions.get_mut(&pred_session_id) {
                            for action in &actions {
                                match action {
                                    PredictionAction::SetTitle { title } => {
                                        let title = title.trim();
                                        if !title.is_empty() {
                                            state.title = Some(title.to_string());
                                            applied_title = Some(title.to_string());
                                        }
                                    }
                                    PredictionAction::AddTag { tag }
                                        if !state.tags.contains(tag) =>
                                    {
                                        state.tags.push(tag.clone());
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    // 标题变更：持久化到 thread store，并推送 session/update
                    // 供标题栏与外部客户端刷新（与 session/rename 行为一致）
                    if let Some(title) = applied_title {
                        if let Err(e) = pred_thread_store
                            .update_title(&pred_session_id, &title)
                            .await
                        {
                            tracing::warn!(
                                session_id = %pred_session_id,
                                error = %e,
                                "Prediction: failed to persist title"
                            );
                        }
                        notify::send_session_info_update_with_title(
                            pred_transport.as_ref(),
                            &pred_session_id,
                            Some(&title),
                        )
                        .await;
                    }
                    let caps = pred_caps_registry
                        .get(&pred_session_id)
                        .map(|r| r.clone())
                        .unwrap_or_default();
                    if caps.prediction {
                        // text 字段取首个 Placeholder（兼容旧消费方）
                        let text = actions
                            .iter()
                            .find_map(|a| match a {
                                PredictionAction::Placeholder { text } => Some(text.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let actions_json: Vec<serde_json::Value> = actions
                            .iter()
                            .filter_map(|a| serde_json::to_value(a).ok())
                            .collect();
                        tracing::debug!(
                            count = actions.len(),
                            "Prediction ready, sending notification"
                        );
                        let _ = pred_transport
                            .send_notification(
                                "peri/prediction_ready",
                                serde_json::json!({
                                    "sessionId": pred_session_id,
                                    "text": text,
                                    "actions": actions_json,
                                }),
                            )
                            .await;
                    } else {
                        tracing::debug!(
                            "Prediction ready but cap not declared, suppressing notification"
                        );
                    }
                }
                Err(crate::session::executor::PredictionError::Failed(e)) => {
                    tracing::debug!(error = %e, "Prediction fork failed");
                }
                Err(crate::session::executor::PredictionError::Timeout) => {
                    tracing::debug!("Prediction fork timed out (30s)");
                }
            }
        });
    }

    // Restore AgentPool back into session (still inside the per-session prompt
    // lock — see the take-out comment above).
    {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&prompt_session_id) {
            if let Ok(mutex) = Arc::try_unwrap(pool_arc) {
                state.agent_pool = mutex.into_inner();
            }
        }
    }

    result
}
