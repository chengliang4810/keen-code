//! ACP Prompt execution — builds and executes the agent via crate::executor.
//! Extracted from original acp_server.rs (2026-05-20 split).

use std::sync::Arc;

use crate::{
    broker::AcpTransportBroker,
    session::{event_sink::TransportEventSink, executor},
    transport::types::AcpError,
};
use agent_client_protocol::schema::v1::{PromptResponse, StopReason};
use parking_lot::RwLock;
use peri_acp_types::cron::CronSchedulerPort;
use peri_acp_types::hooks::RegisteredHook;
use peri_acp_types::interaction::ChannelState;
use peri_acp_types::ports::McpPoolPort;
use serde_json::Value;
use tracing::info;

use peri_agent::session::exec::executor_helpers::{
    CommandLookupFn, ForwarderLauncherFn, StageBuildFn,
};
use peri_agent::session::exec::stage_builder::CachedLlmInstances;

use super::SharedSessions;
use crate::provider::{LlmProvider, PeriConfig};

// ── Prompt execution (spawned into background task) ──────────────────────────

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_prompt(
    params: Value,
    sessions: &SharedSessions,
    _default_provider: &Arc<RwLock<LlmProvider>>,
    request_observer: Option<Arc<dyn peri_model::RequestObserver>>,
    peri_config: &Arc<RwLock<PeriConfig>>,
    cron_scheduler: Option<Arc<dyn CronSchedulerPort>>,
    plugin_skill_roots: &[peri_acp_types::skills::SkillRoot],
    plugin_agent_dirs: &[std::path::PathBuf],
    plugin_loaded: &[peri_acp_types::plugin::LoadedPlugin],
    hook_groups: &[Vec<peri_acp_types::hooks::RegisteredHook>],
    mcp_pool: Option<Arc<dyn McpPoolPort>>,
    channel_state: Option<Arc<ChannelState>>,
    skills: Arc<dyn peri_acp_types::ports::SkillsPort>,
    plugin_lsp_servers: &[peri_acp_types::lsp::LspServerConfig],
    transport: &Arc<dyn crate::transport::AcpTransport>,
    thread_store: &Arc<dyn peri_acp_types::store::ThreadStore>,
    controller: &Arc<peri_controller::Controller>,
    pool: Arc<parking_lot::Mutex<crate::session::agent_pool::AgentPool>>,
    session_manager: crate::session::SessionManager,
    // 内部 continuation 通知通道（注入 SessionContext，供 on_bg_complete
    // 闭包通知 server 的 continuation scheduler）。无 scheduler 场景为 None。
    cont_tx: Option<
        tokio::sync::mpsc::UnboundedSender<crate::session::executor::ContinuationRequest>,
    >,
    // 内部 AsyncContinuation（bg 完成唤醒被取消的 turn）：不 push 空 user
    // prompt、不触发 keepgoing 语义。仅由 continuation scheduler 调用。
    continuation: bool,
) -> Result<Value, AcpError> {
    let session_id = params
        .get("sessionId")
        .or_else(|| params.get("session_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AcpError::new(-32602, "missing sessionId"))?
        .to_string();
    // v2 路径下 MessageQueue 由 run_session_loop 从 session_manager.v2_message_queue
    // 解析（executor.rs:368），不再作为 PromptExecutionContext 字段传入。
    let message = params
        .get("message")
        .ok_or_else(|| AcpError::new(-32602, "missing message"))?;
    let content: peri_acp_types::messages::MessageContent = message
        .get("content")
        .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
        .unwrap_or_default();

    // Parse optional background task results for synthetic tool_use + tool_result injection
    let bg_results: Vec<peri_acp_types::event::BackgroundTaskResult> = params
        .get("bgResults")
        .map(|v| serde_json::from_value(v.clone()).unwrap_or_default())
        .unwrap_or_default();
    let mut developer_context = extract_developer_context(&params);

    // Issue 2026-08-05 返工：requestId 透传——TUI 提交时生成、随 prompt RPC 到达，
    // 服务器随 turn 结束事件（peri/agent_event_done）原样回带，供 TUI 侧 stale
    // TurnInterrupted 的 request_id 配对判定。缺失路径（continuation 等）为 None。
    let request_id = params
        .get("requestId")
        .and_then(|v| v.as_str())
        .map(String::from);

    // Create cancel token and register in sessions.
    // `AgentCancellationToken` 即 `tokio_util::sync::CancellationToken` 别名
    // （peri-agent re-export；ACP 协议面直接使用底层类型，不经业务 crate）。
    let cancel = tokio_util::sync::CancellationToken::new();
    {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        state.cancel_token = Some(cancel.clone());
    }

    // Read session data under lock, then release immediately.
    let (
        cwd,
        history,
        is_empty,
        thread_id,
        frozen,
        incoming_recalls,
        lsp_pool,
        session_provider,
        tool_registry,
    ) = {
        let mut sessions = sessions.lock().await;
        let state = sessions
            .get_mut(&session_id)
            .ok_or_else(|| AcpError::new(-32602, "session not found"))?;
        (
            state.cwd.clone(),
            state.history.clone(),
            state.history.is_empty(),
            state.thread_id.clone(),
            state.frozen.clone(),
            // [AsyncContinuation] 续跑不 take recall：上一轮留给用户 prompt 的
            // recall 必须保留在 SessionState（后续用户 prompt 注入），续跑自身
            // 也不注入（见 executor::run_session_loop 的 continuation 分支）。
            take_recall_for_turn(&mut state.recall_items, continuation),
            state.lsp_pool.clone(),
            Arc::clone(&state.provider),
            state.tool_registry.clone(),
        )
    };
    // The registry is session-scoped. Never use AcpServerConfig's host-level
    // template here: its map/index would let one session search or execute
    // another session's cwd-bound tools.
    // Build a fresh per-session tool snapshot for this serialized turn.
    tool_registry.reset_for_turn();
    let tool_search_index = tool_registry.tool_search_index;
    let shared_tools = tool_registry.shared_tools;
    let history_len = history.len();
    if has_incomplete_last_turn(&history) {
        developer_context = Some(merge_developer_context(
            developer_context.as_deref(),
            "The previous turn did not complete successfully. The last assistant message may contain partial output. Continue from the preserved progress and do not repeat completed actions.",
        ));
    }
    // Save message IDs for compact persistence path (history is moved into run_session_loop below).
    let history_ids: Vec<peri_acp_types::messages::MessageId> =
        history.iter().map(|m| m.id()).collect();

    let broker: Arc<dyn peri_acp_types::interaction::UserInteractionBroker> = Arc::new(
        AcpTransportBroker::new(Arc::clone(transport), session_id.clone().into()),
    );
    let event_sink = Arc::new(TransportEventSink::new(
        Arc::clone(transport),
        session_manager.caps_registry(),
        request_id.clone(),
    ));

    let provider_snapshot = session_provider.read().clone();
    let peri_config_snapshot = Arc::new(peri_config.read().clone());

    // Track first history message ID for cancel-with-progress path (history is moved below)
    // Uses Option<MessageId> (16 bytes) instead of cloning the entire history.
    let first_history_id = history.first().map(|m| m.id());

    // ── L5：SessionContext 投影（provider / peri_config / pool / SessionManager /
    //    Controller 端口化——执行体迁入 peri-agent 后由本宿主构造注入面）──
    let provider_name = provider_snapshot.display_name().to_string();
    let provider_model_name = provider_snapshot.model_name().to_string();
    let provider_fp = crate::session::agent_pool::fingerprint(&provider_snapshot);
    let effective_context_window = if provider_snapshot.context_1m() {
        1_000_000
    } else {
        provider_snapshot.context_window()
    };
    let claude_md_excludes = peri_config_snapshot.config.claude_md_excludes.clone();
    let language = peri_config_snapshot.config.language.clone();
    let mut compact_config = peri_config_snapshot
        .config
        .compact
        .clone()
        .unwrap_or_default();
    compact_config.apply_env_overrides();
    let retry_events = pool.lock().retry_events.clone();

    // 主 LLM 缓存读取（AgentPool has_valid_cache + get_cached_llm 语义）
    let get_cached_llm: Option<Arc<dyn Fn() -> Option<CachedLlmInstances> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider_snapshot.clone();
        Some(Arc::new(move || {
            let guard = pool.lock();
            if guard.has_valid_cache(&provider) {
                guard.get_cached_llm().cloned()
            } else {
                None
            }
        }))
    };
    // fresh auxiliary model（缓存缺失时；retry observer 烘焙）
    let fresh_auxiliary_model: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider_snapshot.clone();
        let request_observer = request_observer.clone();
        Some(Arc::new(move || {
            let provider = provider
                .clone()
                .with_retry_observer(Some(pool.lock().retry_events.as_retry_observer()));
            provider
                .into_model_with_request_observer(request_observer.clone())
                .into()
        }))
    };
    // LLM 缓存回写（AgentPool store_llm 语义）
    let store_llm: Option<Arc<dyn Fn(CachedLlmInstances) + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        Some(Arc::new(move |cache: CachedLlmInstances| {
            pool.lock().store_llm(cache);
        }))
    };
    // stage 装配 LLM 工厂（主 LLM / 子 agent；与迁移前 stage_builder
    // 桥内构造同源——AgentPool 缓存 + RetryObserver 烘焙）
    let primary_llm_factory: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider_snapshot.clone();
        let retry_events = retry_events.clone();
        let request_observer = request_observer.clone();
        Some(Arc::new(move || {
            let fp = crate::session::agent_pool::fingerprint(&provider);
            crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(&pool, &fp, || {
                provider
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model_with_request_observer(request_observer.clone())
            })
        }))
    };
    let subagent_llm_factory: Option<executor::SubagentLlmFactory> = {
        Some(
            crate::host::model_factory::build_subagent_llm_factory_with_request_observer(
                provider_snapshot.clone(),
                Arc::clone(&peri_config_snapshot),
                Arc::clone(&pool),
                retry_events.clone(),
                session_id.clone(),
                request_observer.clone(),
            ),
        )
    };

    // 事件端口（Controller 适配）
    let event_publisher: Arc<dyn peri_acp_types::event::EventPublisher> = Arc::new(
        crate::host::controller_ports::ControllerEventPublisher(Arc::clone(controller)),
    );
    let subscribe: Arc<dyn Fn() -> Box<dyn peri_acp_types::event::EventSubscriber> + Send + Sync> = {
        let controller = Arc::clone(controller);
        Arc::new(move || {
            Box::new(
                crate::host::controller_ports::ControllerSubscriptionAdapter(
                    controller.subscribe(),
                ),
            )
        })
    };

    // 命令拦截注入面（ACP 协议面注册表 / compact 配置）
    let command_lookup: CommandLookupFn = Arc::new(|text: &str| {
        crate::session::command::default_prompt_command_registry().find_arc(text)
    });
    let compact_config_loader: Arc<
        dyn Fn() -> peri_acp_types::compact::CompactConfig + Send + Sync,
    > = {
        let peri_config = Arc::clone(&peri_config_snapshot);
        Arc::new(move || crate::host::compact_config::load_compact_config(&peri_config))
    };
    let tool_invocation_resolver: Arc<dyn peri_agent::tools::ToolInvocationResolver> =
        Arc::new(peri_middlewares::tool_search::ExecuteExtraToolResolver::default());

    // 防御性 frozen 构建器（turn.frozen=None 回落；生产不可达）
    let frozen_fallback_builder: Option<executor::FrozenFallbackBuilder> = {
        let sm = session_manager.clone();
        let roots = plugin_skill_roots.to_vec();
        let dirs = plugin_agent_dirs.to_vec();
        Some(Arc::new(move |cwd, _language| {
            sm.build_frozen_data(cwd, &roots, &dirs)
        }))
    };

    let ctx = executor::SessionContext {
        cwd,
        provider_name,
        provider_model_name,
        provider_fp,
        effective_context_window,
        claude_md_excludes,
        language,
        compact_config,
        get_cached_llm,
        fresh_auxiliary_model,
        store_llm,
        retry_events: Some(Arc::new(retry_events)),
        primary_llm_factory,
        subagent_llm_factory,
        session_id: session_id.clone(),
        cancel,
        broker,
        session_access: Some(
            Arc::new(session_manager) as Arc<dyn peri_acp_types::session::SessionAccessPort>
        ),
        thread_store: Some(Arc::clone(thread_store)),
        thread_id: Some(thread_id.clone()),
        plugin_skill_roots: plugin_skill_roots.to_vec(),
        plugin_agent_dirs: plugin_agent_dirs.to_vec(),
        plugin_loaded: plugin_loaded.to_vec(),
        hook_groups: hook_groups.to_vec(),
        cron_scheduler,
        mcp_pool,
        channel_state,
        tool_search_index,
        skills,
        shared_tools,
        lsp_servers: plugin_lsp_servers.to_vec(),
        lsp_pool,
        event_publisher,
        subscribe,
        command_lookup,
        compact_config_loader,
        tool_invocation_resolver,
        session_start_source: if !continuation && is_empty {
            Some("startup".to_string())
        } else {
            None
        },
        developer_context,
        request_id,
        allow_await_wake: true,
        continuation_notify: cont_tx,
        frozen_fallback_builder,
    };

    // stage 装配桥：从 SessionContext 投影 StageBuildInput 并补齐注入面
    let ctx_for_stage = ctx.clone();
    let stage_build: StageBuildFn = Arc::new(move |sbr| {
        // compact hook 闭包在每次装配时构造（hook_groups 非空才产生动作；
        // 与迁移前 stage_builder 内构造时机逐次一致）
        let (compact_pre_hook, compact_post_hook) = crate::host::prompt::build_compact_hooks(
            &ctx_for_stage.hook_groups,
            &ctx_for_stage.cwd,
            &ctx_for_stage.session_id,
            &ctx_for_stage.provider_model_name,
        );
        crate::host::stage_builder::build_stage_context(
            &ctx_for_stage,
            &peri_middlewares::assembly::ProductionChainAssembler, // ZST 装配器
            compact_pre_hook,
            compact_post_hook,
            sbr.cached_llm.as_ref(),
            sbr.system_prompt,
            sbr.subagent_system_prompt,
            sbr.frozen,
            sbr.event_handler,
            sbr.agent_overrides,
            sbr.preload_skills,
            sbr.child_handler_factory,
            sbr.auxiliary_model,
            sbr.thread_persistence,
            sbr.goal_controller,
            sbr.task_manager,
            sbr.on_bg_complete,
        )
    });

    // EventBus forwarder 启动器；事件映射与转发保持在 ACP 事件层。
    let forwarder_launcher: ForwarderLauncherFn = Arc::new(|handles, _agent_id, on_event| {
        crate::event::spawn_eventbus_forwarder(handles, on_event);
    });

    let turn = executor::TurnInput {
        event_sink,
        content,
        continuation,
        frozen,
        history,
        incoming_recalls,
        bg_results,
        stage_build,
        forwarder_launcher,
    };

    // 3.0 批 2：执行发起经 Controller（控制面第四步 run Session）。
    // 本轮执行句柄（PromptHandle）注册进 Runtime 映射（注册或替换，
    // 不递增 epoch/seq）→ `Controller::run_session` 经 Runtime 查映射发起 →
    // `run_session_loop` 执行完成（返回时结果已就绪）→ take_result。
    // L5：执行体固定为 `run_session_loop`（句柄内部直接调用，无需 runner 注入）。
    let handle = Arc::new(crate::host::prompt_handle::PromptHandle::new(ctx, turn));
    controller.register_session(&session_id, Arc::clone(&handle));
    controller
        .run_session(&session_id)
        .await
        .map_err(|e| AcpError::new(-32603, format!("run_session failed: {e}")))?;
    let result = handle.take_result();

    // Persist new messages to ThreadStore and update in-memory state.
    {
        let mut sessions = sessions.lock().await;
        if let Some(state) = sessions.get_mut(&session_id) {
            if result.ok {
                info!(session_id = %session_id, messages = result.messages.len(), "Agent execution completed");
                // Persist only the newly added messages.
                if history_len < result.messages.len() {
                    let new_msgs = &result.messages[history_len..];
                    if let Err(e) = thread_store.append_messages(&thread_id, new_msgs).await {
                        tracing::warn!(error = %e, "Failed to persist messages to ThreadStore");
                    }
                } else if result.messages.len() < history_len {
                    // Compact replaced own messages with a condensed summary.
                    // Delete old messages from ThreadStore and persist compacted state,
                    // otherwise session restore loads old + new messages causing duplication.
                    info!(
                        session_id = %session_id,
                        old_count = history_len,
                        new_count = result.messages.len(),
                        "Compact detected: updating ThreadStore"
                    );
                    if let Err(e) = thread_store.delete_messages(&thread_id, &history_ids).await {
                        tracing::warn!(
                            error = %e,
                            "Failed to delete pre-compact messages from ThreadStore"
                        );
                    }
                    if let Err(e) = thread_store
                        .append_messages(&thread_id, &result.messages)
                        .await
                    {
                        tracing::warn!(
                            error = %e,
                            "Failed to persist compacted messages to ThreadStore"
                        );
                    }
                }
                state.history = result.messages;
            } else if result.history_replaced_by_compaction
                || result.messages.len() > history_len + 1
            {
                // Error/cancel but agent made progress (user msg + AI/tool messages beyond
                // just the user message). Preserve history so the agent remembers the
                // interrupted round's context on the next prompt. Covers all error paths:
                // LLM stream errors, HTTP errors, tool failures, middleware errors,
                // MaxIterationsExceeded, and Ctrl+C cancel.
                //
                // NOTE: execute() skips cleanup_prepended on error paths (? propagation),
                // so result.messages may contain leaked system prepends at the beginning.
                // A committed Full Compact intentionally removes prior visible IDs and appends
                // its summary to ThreadStore. Only that explicit executor signal may replace
                // history without its original first message; other partial results are rejected.
                if let Some(cleaned) = strip_leaked_prepends(
                    &result.messages,
                    first_history_id,
                    result.history_replaced_by_compaction,
                ) {
                    let new_count = cleaned.len().saturating_sub(history_len);
                    // Persist newly added messages to ThreadStore
                    if new_count > 0 && history_len < cleaned.len() {
                        let new_msgs = &cleaned[history_len..];
                        if let Err(e) = thread_store.append_messages(&thread_id, new_msgs).await {
                            tracing::warn!(error = %e, "Failed to persist cancelled-round messages");
                        }
                    }
                    state.history = cleaned;
                    info!(
                        session_id = %session_id,
                        history_len,
                        new_count,
                        "Agent cancelled with progress, preserving history"
                    );
                } else {
                    tracing::warn!(
                        session_id = %session_id,
                        history_len,
                        result_messages = result.messages.len(),
                        "Cancelled result omitted existing history; preserving prior in-memory history"
                    );
                }
            } else {
                // Execution failed, cancelled early (no AI output), or MaxIterationsExceeded.
                // Roll back LLM-side history to pre-submit state.
                // The TUI's TurnInterrupted handler detects zero AI output (current_turn empty)
                // and performs the corresponding UI rollback: removes the user bubble from
                // committed + restores text to the input area via INPUT_RESTORE_TEXT storage
                // + RENDER_HEARTBEAT trigger.
                state.history.truncate(history_len);
                info!(session_id = %session_id, history_len, "Agent execution failed/cancelled, rolled back history");
            }
            // [AsyncContinuation] 续跑结束不回写 recall：保留续跑开始前
            // SessionState 中的 recall（上一轮留给用户 prompt 的），续跑自身
            // 产生的 recall 不覆盖它；用户 prompt 正常回写。
            if recall_overwrite_allowed(continuation) {
                state.recall_items = result.recall_items;
            }
            state.cancel_token = None;
        }
    }

    let acp_stop_reason = match result.stop_reason {
        executor::PromptStopReason::Cancelled => StopReason::Cancelled,
        executor::PromptStopReason::MaxTurnRequests => StopReason::MaxTurnRequests,
        executor::PromptStopReason::EndTurn => StopReason::EndTurn,
    };
    let resp = PromptResponse::new(acp_stop_reason);
    serde_json::to_value(resp).map_err(|e| AcpError::new(-32603, format!("Serialize failed: {e}")))
}

/// 读取桌面宿主随本轮 prompt 传入的隐藏开发者上下文。
fn extract_developer_context(params: &Value) -> Option<String> {
    params
        .get("developerContext")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn merge_developer_context(existing: Option<&str>, reminder: &str) -> String {
    match existing.map(str::trim).filter(|value| !value.is_empty()) {
        Some(existing) => format!("{existing}\n\n{reminder}"),
        None => reminder.to_owned(),
    }
}

fn has_incomplete_last_turn(history: &[peri_acp_types::messages::BaseMessage]) -> bool {
    history
        .iter()
        .rev()
        .find_map(|message| message.turn_metadata())
        .is_some_and(|(_, _, incomplete, _)| incomplete)
}

/// [AsyncContinuation] 读取本轮 recall 的策略：
///
/// - 用户 prompt：`mem::take`（消费上一轮 recall 注入本轮的 system-reminder，
///   结束时由 `recall_overwrite_allowed` 回写新 recall）；
/// - 内部续跑：**clone 而非 take**——上一轮留给用户 prompt 的 recall 必须
///   保留在 `SessionState`（续跑自身也不注入，见 executor 的 continuation
///   分支），避免续跑"吞掉"用户 prompt 应得的 recall。
fn take_recall_for_turn(recall_items: &mut Vec<String>, continuation: bool) -> Vec<String> {
    if continuation {
        recall_items.clone()
    } else {
        std::mem::take(recall_items)
    }
}

/// 构造 compact plugin hook 回调（宿主装配面职责，L5 归位自
/// host/stage_builder.rs：hook_groups 非空时构造 `fire_pre_compact` /
/// `fire_post_compact` 转发闭包；语义同迁移前——tokio::spawn 转发、不阻塞
/// 管线；hook_groups 为空返回 `(None, None)`）。
#[allow(clippy::type_complexity)]
pub(crate) fn build_compact_hooks(
    hook_groups: &[Vec<RegisteredHook>],
    cwd: &str,
    session_id: &str,
    model: &str,
) -> (
    Option<Arc<dyn Fn() + Send + Sync>>,
    Option<Arc<dyn Fn(bool, usize) + Send + Sync>>,
) {
    let hook_groups_flat: Vec<RegisteredHook> = hook_groups.iter().flatten().cloned().collect();
    if hook_groups_flat.is_empty() {
        return (None, None);
    }
    let cwd = cwd.to_string();
    let sid = session_id.to_string();
    let model = model.to_string();
    let pre: Arc<dyn Fn() + Send + Sync> = {
        let hooks = hook_groups_flat.clone();
        let cwd = cwd.clone();
        let sid = sid.clone();
        let model = model.clone();
        Arc::new(move || {
            let hooks = hooks.clone();
            let cwd = cwd.clone();
            let sid = sid.clone();
            let model = model.clone();
            tokio::spawn(async move {
                peri_middlewares::hooks::stage_firing::fire_pre_compact(
                    &hooks, &cwd, &sid, "", &model, 0,
                )
                .await;
            });
        })
    };
    let post: Arc<dyn Fn(bool, usize) + Send + Sync> = {
        let hooks = hook_groups_flat.clone();
        let cwd = cwd.clone();
        let sid = sid.clone();
        let model = model.clone();
        Arc::new(move |_compacted: bool, affected_count: usize| {
            let hooks = hooks.clone();
            let cwd = cwd.clone();
            let sid = sid.clone();
            let model = model.clone();
            tokio::spawn(async move {
                peri_middlewares::hooks::stage_firing::fire_post_compact(
                    &hooks,
                    &cwd,
                    &sid,
                    "",
                    &model,
                    affected_count,
                )
                .await;
            });
        })
    };
    (Some(pre), Some(post))
}

/// 本轮结束时是否允许用 `result.recall_items` 覆盖 `SessionState.recall_items`。
///
/// 续跑结束时**不改变** SessionState 中的 recall（保留续跑开始前的值给后续
/// 用户 prompt）；用户 prompt 正常回写本轮产生的 recall。
fn recall_overwrite_allowed(continuation: bool) -> bool {
    !continuation
}

/// Returns `None` when a partial result omits existing history. A committed Full Compact
/// explicitly replaces prior visible messages with its persisted summary, so it is accepted.
fn strip_leaked_prepends(
    result_messages: &[peri_acp_types::messages::BaseMessage],
    first_history_id: Option<peri_acp_types::messages::MessageId>,
    full_compaction_committed: bool,
) -> Option<Vec<peri_acp_types::messages::BaseMessage>> {
    match first_history_id {
        Some(first_id) => {
            // Find where original history starts in result (skip leaked prepends).
            if let Some(start) = result_messages.iter().position(|m| m.id() == first_id) {
                Some(result_messages[start..].to_vec())
            } else if full_compaction_committed {
                Some(
                    result_messages
                        .iter()
                        .skip_while(|m| m.is_system())
                        .cloned()
                        .collect(),
                )
            } else {
                None
            }
        }
        None => {
            // Original history was empty — strip leading system messages (all prepends).
            Some(
                result_messages
                    .iter()
                    .skip_while(|m| m.is_system())
                    .cloned()
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
