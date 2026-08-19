//! Host 共享模型工厂：统一 embedded 与 stdio 的子 Agent 模型解析。

use std::sync::Arc;

use peri_agent::agent::{
    model_bridge::AgentModelBridge,
    react::{ReactLLM, RejectingReactLLM},
};

use crate::{
    provider::{AgentModelResolution, LlmProvider, PeriConfig},
    session::{
        agent_pool::{fingerprint, AgentPool},
        executor::SubagentLlmFactory,
        retry_events::RetryEventForwarder,
    },
};

/// 构造 embedded 与 stdio 共用的子 Agent LLM 工厂。
///
/// 解析错误直接返回 [`RejectingReactLLM`]；该分支位于 fingerprint、AgentPool
/// 和底层模型构造之前，保证无父模型回退、无缓存写入、无网络请求。
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
            Some(selection) => {
                match LlmProvider::resolve_agent_model(&peri_config, &inherited, selection) {
                    AgentModelResolution::Inherit => inherited.clone(),
                    AgentModelResolution::Resolved(provider) => provider,
                    AgentModelResolution::Error(error) => {
                        return Box::new(RejectingReactLLM::new(format!("模型选择无效: {error}")))
                            as Box<dyn ReactLLM + Send + Sync>;
                    }
                }
            }
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
