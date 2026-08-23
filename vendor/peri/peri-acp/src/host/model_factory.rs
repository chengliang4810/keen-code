//! Host 共享模型工厂：统一子 Agent 模型解析。

use std::sync::Arc;

use peri_agent::agent::{model_bridge::AgentModelBridge, react::ReactLLM};

use crate::{
    provider::{AgentModelResolution, LlmProvider, PeriConfig},
    session::{
        agent_pool::{fingerprint, AgentPool},
        executor::SubagentLlmFactory,
        retry_events::RetryEventForwarder,
    },
};

/// 解析子 Agent 的模型选择；解析失败时告警并回退会话 Provider。
///
/// 覆盖 KeenCode 的删除场景：子 Agent 定义或工具参数引用的
/// `provider_id::model` 在供应商/模型被删除后失效，此时沿用当前会话
/// Provider 继续执行（[`resolve_agent_model`] 仍如实返回 `Error`，
/// 回退是宿主工厂的产品策略，不是解析层语义）。
pub(crate) fn resolve_subagent_provider(
    inherited: &LlmProvider,
    peri_config: &PeriConfig,
    selection: &str,
) -> LlmProvider {
    match LlmProvider::resolve_agent_model(peri_config, inherited, selection) {
        AgentModelResolution::Resolved(provider) => provider,
        AgentModelResolution::Error(error) => {
            tracing::warn!(selection, error, "子 Agent 模型选择无效，回退会话 Provider");
            inherited.clone()
        }
    }
}

/// 构造 Host 共用的子 Agent LLM 工厂。
///
/// 模型选择解析失败（引用的供应商/模型已删除等）时告警并回退会话
/// Provider，不中断子 Agent 派发。
#[cfg(test)]
pub(crate) fn build_subagent_llm_factory(
    inherited: LlmProvider,
    peri_config: Arc<PeriConfig>,
    pool: Arc<parking_lot::Mutex<AgentPool>>,
    retry_events: RetryEventForwarder,
    session_id: String,
) -> SubagentLlmFactory {
    build_subagent_llm_factory_with_request_observer(
        inherited,
        peri_config,
        pool,
        retry_events,
        session_id,
        None,
    )
}

/// 构造带宿主请求观测器的子 Agent LLM 工厂。
pub(crate) fn build_subagent_llm_factory_with_request_observer(
    inherited: LlmProvider,
    peri_config: Arc<PeriConfig>,
    pool: Arc<parking_lot::Mutex<AgentPool>>,
    retry_events: RetryEventForwarder,
    session_id: String,
    request_observer: Option<Arc<dyn peri_model::RequestObserver>>,
) -> SubagentLlmFactory {
    Arc::new(move |model_selection: Option<&str>| {
        let effective = match model_selection {
            None => inherited.clone(),
            Some(selection) => resolve_subagent_provider(&inherited, &peri_config, selection),
        };

        let provider_fingerprint = fingerprint(&effective);
        let retry_events = retry_events.clone();
        let request_observer = request_observer.clone();
        let model =
            AgentPool::get_or_create_subagent_llm(&pool, &provider_fingerprint, move || {
                effective
                    .clone()
                    .with_retry_observer(Some(retry_events.as_retry_observer()))
                    .into_model_with_request_observer(request_observer.clone())
            });
        let llm = AgentModelBridge::from_arc(model)
            .with_session_id(session_id.clone())
            .with_purpose("subagent");
        Box::new(llm) as Box<dyn ReactLLM + Send + Sync>
    })
}
