//! run_session_loop 完整装配路径测试（L5：executor 迁入 peri-agent 后留在
//! ACP 宿主侧的流程测试）。
//!
//! 归属说明：完整装配路径（turn 终态唯一）需要 stage 装配
//! 注入面（ACP 桥 + middlewares + prompt 渲染），frozen 渲染测试需要 ACP
//! 渲染面（`SessionManager::build_frozen_data`）——按归属留 ACP；keepgoing
//! 短路纯函数测试随 `run_session_loop` 迁入
//! peri-agent（`session::exec::executor_test.rs`）。
//!
//! Mock 命名遵循 CLAUDE.md：`make_` 前缀（函数），`Mock` 前缀（结构体）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use peri_acp_types::{
    event::ExecutorEvent,
    interaction::{InteractionContext, InteractionResponse, UserInteractionBroker},
    messages::{BaseMessage, MessageContent},
    store::ThreadStore,
};
use peri_agent::session::exec::executor_helpers::{ForwarderLauncherFn, StageBuildFn};
use peri_agent::thread::FilesystemThreadStore;
use tokio_util::sync::CancellationToken as AgentCancellationToken;

use crate::session::executor::{
    run_session_loop, PromptStopReason, SessionContext, SubagentLlmFactory, TurnInput,
};
use crate::{
    provider::{LlmProvider, PeriConfig, ProviderConfig},
    session::{agent_pool::AgentPool, event_sink::EventSink, SessionManager},
};
use peri_middlewares::{host_ports::SkillsProvider, tool_search::ToolSearchIndex};

// ── Mock EventSink ─────────────────────────────────────────────────────────

/// Mock EventSink，记录所有 push_done 调用（含 request_id）与事件流。
struct MockEventSink {
    push_done_count: Mutex<usize>,
    push_done_stop_reasons: Mutex<Vec<String>>,
    pushed_events: Mutex<Vec<String>>,
}

impl MockEventSink {
    fn new() -> Self {
        Self {
            push_done_count: Mutex::new(0),
            push_done_stop_reasons: Mutex::new(Vec::new()),
            pushed_events: Mutex::new(Vec::new()),
        }
    }

    fn push_done_count(&self) -> usize {
        *self.push_done_count.lock().unwrap()
    }
}

#[async_trait]
impl EventSink for MockEventSink {
    async fn push_event(&self, _session_id: &str, event: &ExecutorEvent, _context_window: u32) {
        let json = serde_json::to_string(event).unwrap_or_default();
        self.pushed_events.lock().unwrap().push(json);
    }

    async fn push_done(
        &self,
        _session_id: &str,
        stop_reason: &str,
        _request_id: Option<&str>,
        _done_kind: peri_acp_types::event::DoneKind,
    ) {
        *self.push_done_count.lock().unwrap() += 1;
        self.push_done_stop_reasons
            .lock()
            .unwrap()
            .push(stop_reason.to_string());
    }
}

/// 空操作 broker：测试路径不会触发真实交互。
struct NoopBroker;

#[async_trait]
impl UserInteractionBroker for NoopBroker {
    async fn request(&self, _ctx: InteractionContext) -> InteractionResponse {
        InteractionResponse::Rejected
    }
}

// ── Helper 工厂函数 ─────────────────────────────────────────────────────────

/// 构造最小 SessionContext（flow 测试走预取消中断路径；stage 装配桥经
/// 真实 ACP 桥注入——与生产 host/prompt.rs 同模式；LLM 工厂从测试
/// LlmProvider + AgentPool 烘焙，装配路径实际调用）。
fn make_session_context(session_id: &str) -> SessionContext {
    // 事件广播宿主：发射端（EventPublisher 适配）与订阅端（subscribe 工厂）
    // 共享同一 Controller 实例，保持迁移前「publish/subscribe 同一广播」语义。
    let controller = Arc::new(peri_controller::Controller::new(
        Arc::new(FilesystemThreadStore::new(
            std::env::temp_dir().join(format!("peri-exec-flow-{}", uuid::Uuid::new_v4())),
        )) as Arc<dyn ThreadStore>,
    ));
    // 测试 LlmProvider + AgentPool + PeriConfig（与迁移前 executor_test 同源）
    let provider = LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://api.example.com/v1".to_string(),
        model: "gpt-4o".to_string(),
        effort: None,
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
        supports_vision: true,
        retry_observer: None,
    };
    let pool = Arc::new(parking_lot::Mutex::new(AgentPool::new()));
    let mut peri_config = PeriConfig::default();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        ..Default::default()
    }];
    let peri_config = Arc::new(peri_config);
    let retry_events = pool.lock().retry_events.clone();

    // stage 装配 LLM 工厂（与生产 host/prompt.rs 同源：AgentPool 缓存 +
    // RetryObserver 烘焙；subagent 工厂烘焙 with_session_id）
    let primary_llm_factory: Option<Arc<dyn Fn() -> Arc<dyn peri_model::Model> + Send + Sync>> = {
        let pool = Arc::clone(&pool);
        let provider = provider.clone();
        let retry_events = retry_events.clone();
        Some(Arc::new(move || {
            let fp = crate::session::agent_pool::fingerprint(&provider);
            crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(&pool, &fp, || {
                provider
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model()
            })
        }))
    };
    let subagent_llm_factory: Option<SubagentLlmFactory> = {
        let provider = provider.clone();
        let peri_config = Arc::clone(&peri_config);
        let pool = Arc::clone(&pool);
        let retry_events = retry_events.clone();
        let sid = session_id.to_string();
        Some(Arc::new(move |model_selection: Option<&str>| {
            let (p, fp) = if let Some(selection) = model_selection {
                let configured = selection.split_once("::").and_then(|(provider_id, model)| {
                    LlmProvider::from_provider_config(
                        &peri_config,
                        provider_id,
                        model,
                        provider.effort().map(str::to_owned),
                        32_000,
                        false,
                        None,
                    )
                });
                match configured {
                    Some(p) => {
                        let fp = crate::session::agent_pool::fingerprint(&p);
                        (Some(p), fp)
                    }
                    None => {
                        let fp = crate::session::agent_pool::fingerprint(&provider);
                        (None, fp)
                    }
                }
            } else {
                let fp = crate::session::agent_pool::fingerprint(&provider);
                (None, fp)
            };
            let model: Arc<dyn peri_model::Model> =
                crate::session::agent_pool::AgentPool::get_or_create_subagent_llm(
                    &pool,
                    &fp,
                    || match &p {
                        Some(p) => p
                            .clone()
                            .with_retry_observer(Some(retry_events.as_retry_observer()))
                            .into_model(),
                        None => provider
                            .clone()
                            .with_retry_observer(Some(retry_events.as_retry_observer()))
                            .into_model(),
                    },
                );
            let mut llm = peri_agent::agent::model_bridge::AgentModelBridge::from_arc(model);
            llm = llm.with_session_id(sid.clone());
            Box::new(llm)
        }))
    };

    SessionContext {
        cwd: "/tmp".to_string(),
        provider_name: "OpenAI:gpt-4o".to_string(),
        provider_model_name: "gpt-4o".to_string(),
        provider_fp: "openai:gpt-4o".to_string(),
        effective_context_window: 200_000,
        claude_md_excludes: None,
        language: None,
        compact_config: Default::default(),
        get_cached_llm: None,
        fresh_auxiliary_model: None,
        store_llm: None,
        retry_events: Some(Arc::new(retry_events)),
        primary_llm_factory,
        subagent_llm_factory,
        session_id: session_id.to_string(),
        cancel: AgentCancellationToken::new(),
        broker: Arc::new(NoopBroker),
        session_access: None,
        thread_store: None,
        thread_id: None,
        plugin_skill_roots: vec![],
        plugin_agent_dirs: vec![],
        plugin_loaded: vec![],
        hook_groups: vec![],
        cron_scheduler: None,
        mcp_pool: None,
        channel_state: None,
        tool_search_index: Arc::new(ToolSearchIndex::default()),
        shared_tools: Arc::new(parking_lot::RwLock::new(Default::default())),
        lsp_servers: vec![],
        lsp_pool: None,
        skills: Arc::new(SkillsProvider),
        event_publisher: Arc::new(crate::host::controller_ports::ControllerEventPublisher(
            controller.clone(),
        )),
        // 订阅端与发射端必须共享同一 Controller 广播，确保保留的运行时事件
        // 都经同一事件泵抵达协议出口。
        subscribe: {
            let controller = Arc::clone(&controller);
            Arc::new(move || {
                Box::new(
                    crate::host::controller_ports::ControllerSubscriptionAdapter(
                        controller.subscribe(),
                    ),
                )
            })
        },
        command_lookup: Arc::new(|_| None),
        compact_config_loader: Arc::new(Default::default),
        tool_invocation_resolver: Arc::new(
            peri_middlewares::tool_search::ExecuteExtraToolResolver::default(),
        ),
        session_start_source: None,
        developer_context: None, // 流程测试默认不注入本轮开发者上下文
        request_id: None,
        allow_await_wake: false,
        frozen_fallback_builder: None,
    }
}

/// 构造 stage 装配桥（真实 ACP 桥，与生产 host/prompt.rs 同模式：ZST
/// ProductionChainAssembler + build_compact_hooks（测试 ctx hook_groups 为空
/// → (None, None)）。
fn make_stage_build(ctx: &SessionContext) -> StageBuildFn {
    let ctx_for_stage = ctx.clone();
    Arc::new(move |sbr| {
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
    })
}

/// 构造 forwarder 启动器（真实 spawn_eventbus_forwarder）。
fn make_forwarder_launcher() -> ForwarderLauncherFn {
    Arc::new(|handles, _agent_id, on_event| {
        crate::event::spawn_eventbus_forwarder(handles, on_event);
    })
}

fn make_turn_input(
    event_sink: Arc<dyn EventSink>,
    content: MessageContent,
    history: Vec<BaseMessage>,
    stage_build: StageBuildFn,
) -> TurnInput {
    TurnInput {
        event_sink,
        content,
        frozen: None,
        history,
        incoming_recalls: vec![],
        bg_results: vec![],
        stage_build,
        forwarder_launcher: make_forwarder_launcher(),
    }
}

/// 预取消的 turn 只通过 ACP `push_done` 发出一次终态；不再依赖已删除的
/// TurnStarted/TurnEnded 中间事件。
#[tokio::test]
async fn test_cancelled_turn_pushes_single_terminal_signal() {
    // Arrange：预取消 token，进入管线后立即中断（不触发真实 LLM 调用）
    let mock_sink = Arc::new(MockEventSink::new());
    let ctx = make_session_context("test-turn-terminal");
    ctx.cancel.cancel();
    let stage_build = make_stage_build(&ctx);
    let turn = make_turn_input(
        Arc::clone(&mock_sink) as Arc<dyn EventSink>,
        MessageContent::text("cancelled turn"),
        vec![],
        stage_build,
    );

    // Act
    let result = run_session_loop(ctx, turn).await;

    // Assert：终态唯一（Interrupted）
    assert!(!result.ok);
    assert_eq!(result.stop_reason, PromptStopReason::Cancelled);

    // 协议出口终态唯一。
    assert_eq!(
        mock_sink.push_done_count(),
        1,
        "终态信号（push_done）必须恰好一次"
    );
    assert_eq!(
        mock_sink
            .push_done_stop_reasons
            .lock()
            .unwrap()
            .last()
            .cloned(),
        Some("cancelled".to_string()),
        "取消路径必须映射为 cancelled"
    );
}

// ── FrozenSessionData 渲染测试（L5：渲染面留 ACP，经 build_frozen_data）───

/// 构造带 SkillsProvider 的 SessionManager（frozen 渲染输入）。
fn make_manager(tmp: &tempfile::TempDir) -> SessionManager {
    let thread_store = Arc::new(FilesystemThreadStore::new(tmp.path().join("threads")));
    let mut peri_config = PeriConfig::default();
    peri_config.config.providers = vec![ProviderConfig {
        id: "a".to_string(),
        provider_type: "openai".to_string(),
        api_key: "sk-test".to_string(),
        ..Default::default()
    }];
    let default_provider = LlmProvider::from_provider_config(
        &peri_config,
        "a",
        "gpt-4o",
        Some("high".to_string()),
        32_000,
        false,
        None,
    )
    .expect("测试 provider 应可构造");
    SessionManager::new(
        thread_store,
        default_provider,
        Arc::new(peri_config),
        None,
        None,
        None,
        Arc::new(SkillsProvider),
    )
}

/// [回归测试] 同一 frozen 输入必须产生字节相同的 system prompt。
///
/// 历史背景（ARC-FROZEN-001）：system prompt 在 session/new 时一次性冻结；
/// 相同会话输入（cwd/language/skill roots/date）若因调用方上下文差异产生不同
/// 前缀，会破坏 Anthropic 前缀缓存，并使主 agent 与
/// subagent 看到不一致的策略。本测试固定全部输入，验证
/// `build_frozen_data` 是确定性的。
#[tokio::test]
async fn test_frozen_session_data_build_is_deterministic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp);
    let cwd = "/tmp";

    let a = mgr.build_frozen_data(cwd, &[], &[]);
    let b = mgr.build_frozen_data(cwd, &[], &[]);

    assert_eq!(
        a.system_prompt(),
        b.system_prompt(),
        "相同 frozen 输入两次 build 应产生相同 system prompt"
    );
    assert_eq!(
        a.skill_summary(),
        b.skill_summary(),
        "相同 frozen 输入两次 build 应产生相同 skill 摘要"
    );
}

/// [回归测试] 已冻结的 system prompt 与 skill 摘要不受会话中途磁盘变化影响。
///
/// 历史背景（ARC-FROZEN-001 / 审计 prompt-sections-audit.md P2-11）：skill
/// 摘要与 system prompt 在 session/new 冻结；会话内磁盘 skill 增删不得改变
/// 已冻结产物（冻结是前缀缓存稳定性的有意权衡，不能按需重扫）。
#[tokio::test]
async fn test_frozen_system_prompt_immune_to_disk_changes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mgr = make_manager(&tmp);
    let cwd = tmp.path().to_str().unwrap();

    // 冻结前：cwd 含 skill-a
    let skills_dir_a = tmp.path().join(".agents").join("skills").join("skill-a");
    std::fs::create_dir_all(&skills_dir_a).unwrap();
    std::fs::write(
        skills_dir_a.join("SKILL.md"),
        "---\nname: 'skill-a'\ndescription: 'A test skill'\n---\n\nbody",
    )
    .unwrap();

    let frozen = mgr.build_frozen_data(cwd, &[], &[]);

    let frozen_prompt = frozen.system_prompt().to_string();
    let frozen_summary = frozen.skill_summary().map(|s| s.to_string());
    assert!(
        frozen_summary.as_deref().unwrap_or("").contains("skill-a"),
        "冻结摘要应包含冻结时的 skill-a"
    );

    // 会话中途：删除 skill-a，新增 skill-b 与 CLAUDE.md
    std::fs::remove_dir_all(&skills_dir_a).unwrap();
    let skills_dir_b = tmp.path().join(".agents").join("skills").join("skill-b");
    std::fs::create_dir_all(&skills_dir_b).unwrap();
    std::fs::write(
        skills_dir_b.join("SKILL.md"),
        "---\nname: 'skill-b'\ndescription: 'B test skill'\n---\n\nbody",
    )
    .unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# New CLAUDE.md").unwrap();

    // 已冻结产物不变（不按需重读磁盘）
    assert_eq!(
        frozen.system_prompt(),
        frozen_prompt,
        "已冻结 system prompt 不应受会话中途磁盘变化影响"
    );
    assert_eq!(
        frozen.skill_summary().map(|s| s.to_string()),
        frozen_summary,
        "已冻结 skill 摘要不应随磁盘重扫"
    );
}
