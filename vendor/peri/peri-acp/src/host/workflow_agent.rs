//! Workflow agent 装配面薄壳（p1-wa 收口）。
//!
//! 执行体已随 p1-wa 物理迁入 `peri_agent::agent::workflow`（`agent.rs` /
//! `factory.rs`——session 运行单元归 Agent 层，§2）；中间件链 / 工具 /
//! error_suggest / tool resolver / session 级 WorkflowMiddleware 装配经
//! [`WorkflowMiddlewareFactory`] 端口注入（peri-middlewares 实现，ACP 宿主
//! 装配点注入）。
//!
//! 本模块保留 ACP 装配面职责（§0 边 2：ACP 不再持有 middlewares/workflow
//! 引用——`scripts/import-exemptions.conf` 的 L5 豁免随本任务移除）：
//!
//! 1. `create_session_workflow_middleware`：session 级 WorkflowMiddleware
//!    装配编排（executor 构造 + progress channel + 端口装配）；
//! 2. 注入面构造 helpers（provider/peri_config 投影模型工厂、publish hook、
//!    forwarder launcher、system prompt fallback）——构造点收敛在本模块，
//!    防注入面漂移（`host/requests.rs` / `host/stdio` 装配面共用）。

use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::{
    agents::AgentOverrides,
    compact::CompactConfig,
    permission::PermissionMode,
    ports::{SkillsPort, WorkflowMiddlewarePort},
    workflow::{AgentExecutor, ProgressEvent, WorkflowTaskResult},
};
use peri_agent::agent::workflow::{
    create_executor, WorkflowAgentContext, WorkflowAgentPromptBuilder, WorkflowMiddlewareFactory,
    WorkflowModel, WorkflowModelFactory, WorkflowPublishHook, WorkflowSystemPromptFallback,
};
use peri_agent::session::exec::executor_helpers::ForwarderLauncherFn;

use crate::provider::{AgentModelResolution, LlmProvider, PeriConfig};
use crate::session::executor::FrozenSessionData;

/// 模型工厂构造：provider / peri_config 投影。
///
/// provider 经 `Arc<RwLock<>>` 共享——provider/model 切换后自动感知，无需
/// 重建 executor（与迁移前 `WorkflowAgentContext.provider` 语义一致）；
/// retry observer 由执行体按 run 传入（重试观测翻译为 LlmRetrying 交给
/// 本 run handler）。
///
/// 池化分支（迁移前 `ctx.agent_pool`）：未文档化契约——与主 builder 的
/// `ctx.pool` 必须是同一 `Arc<Mutex<AgentPool>>`，池化模型烘焙的 observer
/// 才会与主链路共享同一转发器；迁移前 4 处入口均传 `agent_pool: None`
/// （死路径）。若未来接线，池化烘焙在本工厂内实现。
pub(crate) fn build_model_factory(
    provider: &Arc<RwLock<LlmProvider>>,
    peri_config: &RwLock<PeriConfig>,
) -> WorkflowModelFactory {
    build_model_factory_with_request_observer(provider, peri_config, None)
}

/// 构造带宿主请求观测器的 Workflow 模型工厂。
pub(crate) fn build_model_factory_with_request_observer(
    provider: &Arc<RwLock<LlmProvider>>,
    peri_config: &RwLock<PeriConfig>,
    request_observer: Option<Arc<dyn peri_model::RequestObserver>>,
) -> WorkflowModelFactory {
    let provider = Arc::clone(provider);
    let peri_config = Arc::new(peri_config.read().clone());
    let request_observer = request_observer.clone();
    Arc::new(
        move |model: Option<&str>, max_tokens: Option<u32>, observer| {
            // 合并 provider 读取为一次（display/model 同源，避免中间切换导致
            // 不一致——与迁移前 execute() 块作用域语义一致）。如 workflow 脚本
            // 指定了 model 参数：
            //   1) provider_id::model → 按 KeenCode provider 配置解析；
            //   2) haiku/sonnet/opus/fable → 按上游档位 Profile 解析；
            //   3) 其他裸值 → 保留上游“具体模型名”语义，仅替换当前 provider model。
            // 输入边界已拒绝残缺限定模型/控制字符；此处仍防御性校验，错误
            // 必须返回执行体并在构造模型前终止。`tier` 仅在档位解析成功时有值。
            let (effective, tier) = {
                let provider_read = provider.read();
                let requested_tier = model.and_then(|selection| {
                    let normalized = selection.trim().to_ascii_lowercase();
                    peri_acp_types::agents::MODEL_TIERS
                        .contains(&normalized.as_str())
                        .then_some(normalized)
                });
                let resolution = model
                    .map(|selection| {
                        LlmProvider::resolve_agent_model(&peri_config, &provider_read, selection)
                    })
                    .unwrap_or(AgentModelResolution::Inherit);
                match resolution {
                    AgentModelResolution::Inherit => (provider_read.clone(), None),
                    AgentModelResolution::Resolved(provider) => (provider, requested_tier),
                    AgentModelResolution::Error(error) => return Err(error),
                }
            };
            // `maxTokens` 是单次 workflow agent 调用的输出上限；提供时覆盖 profile，
            // 未提供时保留 profile/provider 的默认值。
            let effective = max_tokens
                .map(|max_tokens| effective.with_max_tokens(max_tokens))
                .unwrap_or(effective);
            let model_name = effective.model_name().to_string();
            Ok(WorkflowModel {
                model: Arc::from(
                    effective
                        .with_retry_observer(Some(observer))
                        .into_model_with_request_observer(request_observer.clone()),
                ),
                model_name,
                tier,
            })
        },
    )
}

/// 事件发射钩子构造（`Controller::publish_event` 适配；事件三层化统一出口，
/// workflow agent 的 v2 事件经此进入协议化路径，与主 executor 同一出口）。
pub(crate) fn build_publish_hook(
    controller: &Arc<peri_controller::Controller>,
) -> WorkflowPublishHook {
    let controller = Arc::clone(controller);
    Arc::new(move |sid: &str, source, ev| controller.publish_event(sid, source, ev.clone()))
}

/// EventBus forwarder 启动器构造（workflow 专用：bridge = None——workflow 的
/// Langfuse 处理在外部事件旁路处理器，与迁移前 `spawn_eventbus_forwarder`
/// 调用点一致；biased select 顺序不变量单点保持在 `crate::event`）。
pub(crate) fn build_workflow_forwarder_launcher() -> ForwarderLauncherFn {
    Arc::new(|handles, _agent_id, on_event| {
        crate::event::spawn_eventbus_forwarder(handles, on_event, None);
    })
}

/// system prompt fallback 渲染闭包构造（`PromptTemplate` 渲染面；skills 经
/// 注入的 [`SkillsPort`] 访问——与宿主装配点注入的端口实现同一类型）。
///
/// workflow agent 链不注册 WorkflowTool（无嵌套 workflow），fallback 渲染
/// 关闭 workflow section，与工具注册一致（P2-2026-08-02）。
pub(crate) fn build_workflow_system_prompt_fallback(
    skills: Arc<dyn SkillsPort>,
) -> WorkflowSystemPromptFallback {
    Arc::new(
        move |cwd: &str, frozen_date: Option<&str>, frozen_language: Option<&str>| {
            let features =
                crate::prompt::PromptFeatures::detect_without_workflow(PermissionMode::Bypass);
            let template = crate::prompt::PromptTemplate::new();
            let env = if let Some(date) = frozen_date {
                crate::prompt::PromptEnv::with_frozen_date(cwd, date)
            } else {
                crate::prompt::PromptEnv::detect(cwd)
            };
            template.render(&env, &features, skills.as_ref(), &[], frozen_language)
        },
    )
}

/// workflow `agentType` 指定时的 subagent prompt 渲染器。
///
/// 与主链注入的 `system_builder` 使用相同的 PromptTemplate override 语义，但始终
/// 关闭 workflow section，确保 prompt 声明与 workflow agent 的工具集一致。
pub(crate) fn build_workflow_agent_prompt_builder(
    skills: Arc<dyn SkillsPort>,
) -> WorkflowAgentPromptBuilder {
    Arc::new(
        move |overrides: Option<&AgentOverrides>, cwd, frozen_date, frozen_language| {
            let features =
                crate::prompt::PromptFeatures::detect_without_workflow(PermissionMode::Bypass);
            let template = overrides.map_or_else(
                crate::prompt::PromptTemplate::new,
                crate::prompt::PromptTemplate::with_overrides,
            );
            let env = frozen_date.map_or_else(
                || crate::prompt::PromptEnv::detect(cwd),
                |date| crate::prompt::PromptEnv::with_frozen_date(cwd, date),
            );
            template.render(&env, &features, skills.as_ref(), &[], frozen_language)
        },
    )
}

/// 创建 session 级 WorkflowMiddleware（session/new / load / resume 共用，GAP-05）。
///
/// 编排：构造 executor（`WorkflowAgentContext` 注入面）+ progress 通道 +
/// 经 [`WorkflowMiddlewareFactory`] 端口装配 `WorkflowMiddleware` 实例；
/// 返回端口句柄，host/stdio 命令面与 host/requests 命令面只持
/// `Arc<dyn WorkflowMiddlewarePort>`（3.0 批 2 波 2 装配边界收口）。
///
/// 事件发布：session 级路径与迁移前一致（`controller: None`），不启用事件
/// 发布——publish_hook 传 None，workflow 事件仅由内部 handler 消费
/// （usage/progress），不进入协议化事件流。TUI/stdio 主会话 session/new 均
/// 走此路径；每-turn executor 调用点（`host/prompt.rs` /
/// `host/stdio/session/prompt_exec.rs`）仍传 Some（与迁移前一致）。统一发射
/// 接线留待单独裁定。
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_session_workflow_middleware(
    provider: Arc<RwLock<LlmProvider>>,
    peri_config: &RwLock<PeriConfig>,
    cwd: &str,
    session_id: &str,
    frozen_data: &FrozenSessionData,
    middleware_factory: Arc<dyn WorkflowMiddlewareFactory>,
    publish_hook: Option<WorkflowPublishHook>,
    skills: Arc<dyn SkillsPort>,
) -> Option<Arc<dyn WorkflowMiddlewarePort>> {
    let mut compact_config = CompactConfig::default();
    compact_config.apply_env_overrides();
    let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
    let wf_executor = create_executor(WorkflowAgentContext {
        cwd: cwd.to_string(),
        frozen_claude_md: frozen_data.claude_md().map(|s| s.to_string()),
        frozen_claude_local_md: frozen_data.claude_local_md().map(|s| s.to_string()),
        frozen_skill_summary: frozen_data.skill_summary().map(|s| s.to_string()),
        session_id: Some(session_id.to_string()),
        compact_config: Some(compact_config),
        cancel: None,
        // 无 16_workflow 版本（P2-2026-08-02）：workflow agent 链不
        // 注册 WorkflowTool，不得复用带 workflow 声明的主 prompt。
        system_prompt: Some(frozen_data.subagent_system_prompt().to_string()),
        broker: None,
        permission_mode: None,
        frozen_date: Some(frozen_data.date().to_string()),
        frozen_language: frozen_data.language().map(|s| s.to_string()),
        thread_store: None,
        progress_tx: Some(progress_tx),
        subagent_ctx_builder: None,
        agent_prompt_builder: build_workflow_agent_prompt_builder(Arc::clone(&skills)),
        model_factory: build_model_factory(&provider, peri_config),
        middleware_factory: Arc::clone(&middleware_factory),
        system_prompt_fallback: build_workflow_system_prompt_fallback(skills),
        forwarder_launcher: build_workflow_forwarder_launcher(),
        publish_hook,
        // Langfuse 观测：与迁移前一致（调用点均传 None，workflow agent 路径
        // 未启用遥测；注入面预留，未来接线经 LangfuseHooks 构造）。
        langfuse_hooks: None,
        langfuse_event_handler: None,
    });
    let (notification_tx, _) = tokio::sync::broadcast::channel(32);
    Some(middleware_factory.build_workflow_middleware(
        wf_executor,
        cwd,
        notification_tx,
        Some(progress_rx),
    ))
}

// 类型锚点：确认端口装配方法的入参类型与编排处一致（防签名漂移）。
#[allow(dead_code)]
fn _type_anchor(
    f: Arc<dyn WorkflowMiddlewareFactory>,
    e: Arc<dyn AgentExecutor>,
    n: tokio::sync::broadcast::Sender<WorkflowTaskResult>,
    p: Option<tokio::sync::mpsc::UnboundedReceiver<ProgressEvent>>,
) -> Arc<dyn WorkflowMiddlewarePort> {
    f.build_workflow_middleware(e, "cwd", n, p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_model_factory_applies_concrete_model_name() {
        let provider = Arc::new(RwLock::new(LlmProvider::OpenAi {
            api_key: String::new(),
            base_url: "http://localhost".into(),
            model: "parent-model".into(),
            effort: None,
            max_tokens: 1024,
            context_1m: false,
            context_window: None,
            retry_observer: None,
        }));
        let config = RwLock::new(PeriConfig::default());
        let factory = build_model_factory(&provider, &config);
        let built = factory(Some("workflow-model"), None, Arc::new(|_| {}))
            .expect("裸具体模型应沿用当前 Provider");

        assert_eq!(built.model_name, "workflow-model");
        assert_eq!(built.tier, None);
    }

    /// Workflow 工厂与 embedded/stdio 子 Agent 共享限定模型和档位解析。
    #[test]
    fn workflow_model_factory_resolves_provider_model_and_tier() {
        let provider = Arc::new(RwLock::new(LlmProvider::OpenAi {
            api_key: "parent-key".into(),
            base_url: "http://localhost".into(),
            model: "parent-model".into(),
            effort: Some("high".into()),
            max_tokens: 1024,
            context_1m: false,
            context_window: None,
            retry_observer: None,
        }));
        let config = RwLock::new(PeriConfig {
            config: crate::provider::AppConfig {
                profiles: crate::provider::Profiles {
                    haiku: crate::provider::ProfileConfig {
                        provider: "provider-b".into(),
                        model: Some("tier-model".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                providers: vec![crate::provider::ProviderConfig {
                    id: "provider-b".into(),
                    provider_type: "anthropic".into(),
                    api_key: "provider-key".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        });
        let factory = build_model_factory(&provider, &config);

        let qualified = factory(Some("provider-b::direct-model"), None, Arc::new(|_| {}))
            .expect("有效限定模型应解析成功");
        assert_eq!(qualified.model_name, "direct-model");
        assert_eq!(qualified.tier, None);
        let prepared = qualified
            .model
            .prepare_request(&peri_model::ModelRequest::default())
            .unwrap();
        assert!(matches!(
            prepared.protocol(),
            peri_model::ProviderProtocol::Anthropic
        ));

        let tier = factory(Some("HAIKU"), None, Arc::new(|_| {})).expect("已配置档位应解析成功");
        assert_eq!(tier.model_name, "tier-model");
        assert_eq!(tier.tier.as_deref(), Some("haiku"));

        let invalid = factory(Some("provider-b::"), None, Arc::new(|_| {}));
        assert!(invalid.is_err(), "残缺限定模型必须 fail closed");
    }

    /// KeenCode 无 Profile 配置的四档必须继承会话模型，不能猜测 Provider 默认模型。
    #[test]
    fn workflow_model_factory_inherits_unconfigured_tier() {
        let provider = Arc::new(RwLock::new(LlmProvider::OpenAi {
            api_key: "parent-key".into(),
            base_url: "http://localhost".into(),
            model: "parent-model".into(),
            effort: None,
            max_tokens: 1024,
            context_1m: false,
            context_window: None,
            retry_observer: None,
        }));
        let config = RwLock::new(PeriConfig {
            config: crate::provider::AppConfig {
                providers: vec![crate::provider::ProviderConfig {
                    id: "provider-a".into(),
                    provider_type: "openai".into(),
                    api_key: "provider-key".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        });
        let factory = build_model_factory(&provider, &config);

        let built =
            factory(Some("haiku"), None, Arc::new(|_| {})).expect("未配置档位应继承当前 Provider");
        assert_eq!(built.model_name, "parent-model");
        assert_eq!(built.tier, None);
    }
}
