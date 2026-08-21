//! LLM Provider and model configuration.
//!
//! Manages provider configuration, explicit model resolution, and LLM factory creation.
//! Decoupled from TUI-specific types.

pub mod config;
pub mod store;

use std::sync::Arc;

pub use config::{AppConfig, PeriConfig, ProviderConfig, ProviderModels};
use peri_model::{
    AnthropicConfig, AnthropicModel, OpenAiConfig, OpenAiModel, ResponsesConfig, ResponsesModel,
};
pub use store::{
    config_path, load, load_from, save, save_to, set_global_config_path, workspace_config_path,
};
use url::Url;

const MIN_ANTHROPIC_THINKING_BUDGET: u32 = 1_024;
const DEFAULT_ANTHROPIC_THINKING_BUDGET: u32 = 10_000;

#[derive(Clone)]
pub enum LlmProvider {
    /// OpenAI 兼容 Provider。`base_url` 需要 `/v1` 后缀。
    OpenAi {
        api_key: String,
        base_url: String,
        model: String,
        /// 思考强度 "low".."max"；None 表示不启用 extended thinking
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        /// 手工配置的上下文窗口大小；None 回退 200K 默认
        context_window: Option<u32>,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    },
    /// OpenAI Responses API（`POST /v1/responses`，SSE 流式）。
    /// 字段语义与 [`LlmProvider::OpenAi`] 一致；`base_url` 需要 `/v1` 后缀。
    OpenAiResponses {
        api_key: String,
        base_url: String,
        model: String,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        context_window: Option<u32>,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    },
    Anthropic {
        api_key: String,
        model: String,
        base_url: Option<String>,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        context_window: Option<u32>,
        retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    },
}

/// Agent 模型选择的显式解析结果。
///
/// 子 Agent 的省略模型由调用方直接以 `None` 表示跟随当前会话；此枚举只
/// 表示已解析的 `provider_id::model` 或显式错误。
pub enum AgentModelResolution {
    /// 已解析出可直接构造模型的 Provider。
    Resolved(LlmProvider),
    /// 用户可修复的模型选择或 Provider 配置错误。
    Error(String),
}

impl LlmProvider {
    pub fn from_env() -> Option<Self> {
        let provider_hint = std::env::var("MODEL_PROVIDER").unwrap_or_default();

        match provider_hint.to_lowercase().as_str() {
            "anthropic" => {
                let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
                let model = std::env::var("ANTHROPIC_MODEL").ok()?;
                let base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
                Some(Self::Anthropic {
                    api_key,
                    model,
                    base_url,
                    effort: None,
                    max_tokens: 32000,
                    context_1m: false,
                    context_window: None,
                    retry_observer: None,
                })
            }
            "openai" | "" => {
                if provider_hint.is_empty() {
                    if let (Ok(api_key), Ok(model)) = (
                        std::env::var("ANTHROPIC_API_KEY"),
                        std::env::var("ANTHROPIC_MODEL"),
                    ) {
                        let base_url = std::env::var("ANTHROPIC_BASE_URL").ok();
                        return Some(Self::Anthropic {
                            api_key,
                            model,
                            base_url,
                            effort: None,
                            max_tokens: 32000,
                            context_1m: false,
                            context_window: None,
                            retry_observer: None,
                        });
                    }
                }
                let api_key = std::env::var("OPENAI_API_KEY").ok()?;
                let base_url = std::env::var("OPENAI_API_BASE")
                    .or_else(|_| std::env::var("OPENAI_BASE_URL"))
                    .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
                let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
                Some(Self::OpenAi {
                    api_key,
                    base_url,
                    model,
                    effort: None,
                    max_tokens: 32000,
                    context_1m: false,
                    context_window: None,
                    retry_observer: None,
                })
            }
            _ => {
                let api_key = std::env::var("OPENAI_API_KEY").ok()?;
                let base_url = std::env::var("OPENAI_API_BASE")
                    .or_else(|_| std::env::var("OPENAI_BASE_URL"))
                    .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
                let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
                Some(Self::OpenAi {
                    api_key,
                    base_url,
                    model,
                    effort: None,
                    max_tokens: 32000,
                    context_1m: false,
                    context_window: None,
                    retry_observer: None,
                })
            }
        }
    }

    /// 从配置中的第一个带有明确模型元数据的 Provider 构造 LlmProvider。
    ///
    /// 桌面端通常通过 [`LlmProvider::from_provider_config`] 直接传入
    /// `provider_id::model`；此方法仅服务于没有会话级选择的 stdio/env 启动。
    pub fn from_config(cfg: &config::PeriConfig) -> Option<Self> {
        let provider = cfg
            .config
            .providers
            .iter()
            .find(|provider| !provider.api_key.is_empty())?;
        let model = provider
            .extra
            .get("model")
            .and_then(|value| value.as_str())
            .or_else(|| provider.models.models.keys().next().map(String::as_str))?;
        Self::from_provider_config(
            cfg,
            &provider.id,
            model,
            Some("high".to_string()),
            32_000,
            false,
            None,
        )
    }

    /// 直接由 `provider_id` + 具体 `model` 构造 LlmProvider。
    ///
    /// 用于会话级 provider 隔离：每个 session 直接持有 `"{provider_id}::{model}"`，
    /// 不经过间接配置层。`effort`/`max_tokens`/`context_1m`/
    /// `context_window` 由调用方显式传入（KeenCode 侧目前固定 effort="high"、
    /// max_tokens=32000，context_1m/context_window 取自逐模型手工配置）。
    pub fn from_provider_config(
        cfg: &config::PeriConfig,
        provider_id: &str,
        model: &str,
        effort: Option<String>,
        max_tokens: u32,
        context_1m: bool,
        context_window: Option<u32>,
    ) -> Option<Self> {
        let provider = cfg.config.providers.iter().find(|p| p.id == provider_id)?;
        if provider.api_key.is_empty() || model.is_empty() {
            return None;
        }

        match provider.provider_type.as_str() {
            "anthropic" => Some(Self::Anthropic {
                api_key: provider.api_key.clone(),
                model: model.to_string(),
                base_url: if provider.base_url.is_empty() {
                    None
                } else {
                    Some(provider.base_url.clone())
                },
                effort,
                max_tokens,
                context_1m,
                context_window,
                retry_observer: None,
            }),
            "openai_responses" => Some(Self::OpenAiResponses {
                api_key: provider.api_key.clone(),
                base_url: if provider.base_url.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    provider.base_url.clone()
                },
                model: model.to_string(),
                effort,
                max_tokens,
                context_1m,
                context_window,
                retry_observer: None,
            }),
            _ => Some(Self::OpenAi {
                api_key: provider.api_key.clone(),
                base_url: if provider.base_url.is_empty() {
                    "https://api.openai.com/v1".to_string()
                } else {
                    provider.base_url.clone()
                },
                model: model.to_string(),
                effort,
                max_tokens,
                context_1m,
                context_window,
                retry_observer: None,
            }),
        }
    }

    /// 解析 Agent 的显式模型选择。
    ///
    /// 只接受 KeenCode `provider_id::model`。省略模型由宿主在调用工厂时
    /// 直接沿用当前会话 Provider；任何裸模型名或无效值均返回 `Error`。
    pub fn resolve_agent_model(
        cfg: &config::PeriConfig,
        inherited: &Self,
        model_selection: &str,
    ) -> AgentModelResolution {
        if model_selection.chars().any(char::is_control) {
            return AgentModelResolution::Error("模型选择不能包含控制字符".to_string());
        }
        let qualified = match peri_acp_types::agents::split_provider_model(model_selection) {
            Ok(value) => value,
            Err(error) => return AgentModelResolution::Error(error.to_string()),
        };
        let Some((provider_id, model)) = qualified else {
            return AgentModelResolution::Error(
                "Agent 模型必须使用 provider_id::model；省略 model 表示跟随当前会话".to_string(),
            );
        };
        resolve_configured_provider(
            cfg,
            provider_id,
            model,
            inherited.effort().map(str::to_owned),
            32_000,
            false,
            None,
        )
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::OpenAi { .. } => "OpenAI",
            Self::OpenAiResponses { .. } => "OpenAI Responses",
            Self::Anthropic { .. } => "Anthropic",
        }
    }

    pub fn model_name(&self) -> &str {
        match self {
            Self::OpenAi { model, .. } | Self::OpenAiResponses { model, .. } => model,
            Self::Anthropic { model, .. } => model,
        }
    }

    pub fn context_1m(&self) -> bool {
        match self {
            Self::OpenAi { context_1m, .. }
            | Self::OpenAiResponses { context_1m, .. }
            | Self::Anthropic { context_1m, .. } => *context_1m,
        }
    }

    /// 思考强度稳定标识，用于 fingerprint；None 时返回空字符串
    pub fn effort_key(&self) -> String {
        match self {
            Self::OpenAi { effort, .. }
            | Self::OpenAiResponses { effort, .. }
            | Self::Anthropic { effort, .. } => effort
                .as_ref()
                .map(|e| format!(":effort={e}"))
                .unwrap_or_default(),
        }
    }

    /// 当前 provider 的推理强度。
    pub fn effort(&self) -> Option<&str> {
        match self {
            Self::OpenAi { effort, .. }
            | Self::OpenAiResponses { effort, .. }
            | Self::Anthropic { effort, .. } => effort.as_deref(),
        }
    }

    /// 替换推理强度，保持当前会话的供应商、模型及其他配置不变。
    pub fn with_effort(&self, effort: String) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::OpenAi {
                effort: current, ..
            }
            | Self::OpenAiResponses {
                effort: current, ..
            }
            | Self::Anthropic {
                effort: current, ..
            } => *current = Some(effort),
        }
        clone
    }

    /// 替换模型名，保持其他配置不变
    pub fn with_model_name(&self, model: String) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::OpenAi { model: m, .. } | Self::OpenAiResponses { model: m, .. } => *m = model,
            Self::Anthropic { model: m, .. } => *m = model,
        }
        clone
    }

    /// 替换当前会话的 1M 上下文开关，保持供应商、模型和其他配置不变。
    pub fn with_context_1m(&self, enabled: bool) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::OpenAi { context_1m, .. }
            | Self::OpenAiResponses { context_1m, .. }
            | Self::Anthropic { context_1m, .. } => *context_1m = enabled,
        }
        clone
    }

    /// 覆盖单次调用的输出 token 上限；provider 在本次模型构造后即被消费。
    pub fn with_max_tokens(&self, max_tokens: u32) -> Self {
        let mut clone = self.clone();
        match &mut clone {
            Self::OpenAi {
                max_tokens: configured_max_tokens,
                ..
            }
            | Self::OpenAiResponses {
                max_tokens: configured_max_tokens,
                ..
            }
            | Self::Anthropic {
                max_tokens: configured_max_tokens,
                ..
            } => *configured_max_tokens = max_tokens,
        }
        clone
    }

    /// 绑定 retry observer；`into_model()` 构造模型时注入 runtime。
    pub fn with_retry_observer(
        mut self,
        observer: Option<Arc<dyn peri_model::RetryObserver>>,
    ) -> Self {
        match &mut self {
            Self::OpenAi { retry_observer, .. }
            | Self::OpenAiResponses { retry_observer, .. }
            | Self::Anthropic { retry_observer, .. } => {
                *retry_observer = observer;
            }
        }
        self
    }

    /// 使用宿主提供的请求观测器构造模型。
    ///
    /// 请求观测器与 retry observer 分开注入：前者记录每个 logical call 和
    /// physical attempt 的安全元数据，后者只负责把重试进度转发给 ACP。
    /// 保留无参数的 [`Self::into_model`] 供纯库调用和测试使用；桌面宿主的
    /// 所有生产模型工厂都应调用此入口，以免动态模型切换或缓存模型丢失观测器。
    pub fn into_model_with_request_observer(
        self,
        request_observer: Option<Arc<dyn peri_model::RequestObserver>>,
    ) -> Box<dyn peri_model::Model> {
        match self {
            Self::OpenAi {
                api_key,
                base_url,
                model,
                effort,
                max_tokens,
                retry_observer,
                ..
            } => {
                let endpoint =
                    parse_endpoint(&base_url, "https://api.openai.com/v1", "openai base_url");
                let mut config = OpenAiConfig::new(endpoint, api_key, model);
                if let Some(e) = effort.as_ref() {
                    config = config.with_reasoning_effort(e);
                    config = config.with_thinking_enabled(true);
                }
                config = config.with_max_tokens(max_tokens);
                config =
                    config.with_runtime(runtime_config(true, retry_observer, request_observer));
                Box::new(OpenAiModel::new(config))
            }
            Self::OpenAiResponses {
                api_key,
                base_url,
                model,
                effort,
                max_tokens,
                retry_observer,
                ..
            } => {
                let endpoint =
                    parse_endpoint(&base_url, "https://api.openai.com/v1", "responses base_url");
                let mut config = ResponsesConfig::new(endpoint, api_key, model);
                if let Some(e) = effort.as_ref() {
                    config = config.with_reasoning_effort(e);
                }
                config = config.with_max_tokens(max_tokens);
                config =
                    config.with_runtime(runtime_config(false, retry_observer, request_observer));
                Box::new(ResponsesModel::new(config))
            }
            Self::Anthropic {
                api_key,
                model,
                base_url,
                effort,
                max_tokens,
                retry_observer,
                ..
            } => {
                let endpoint = match base_url {
                    Some(url) => {
                        parse_endpoint(&url, "https://api.anthropic.com", "anthropic base_url")
                    }
                    None => Url::parse("https://api.anthropic.com").expect("静态默认 endpoint"),
                };
                let output_max_tokens = max_tokens;
                let mut config = AnthropicConfig::new(endpoint, api_key, model);
                if let Some(e) = effort
                    .as_ref()
                    .filter(|_| output_max_tokens > MIN_ANTHROPIC_THINKING_BUDGET)
                {
                    let thinking_budget =
                        DEFAULT_ANTHROPIC_THINKING_BUDGET.min(output_max_tokens.saturating_sub(1));
                    config = config.with_extended_thinking(thinking_budget, e);
                }
                config = config.with_max_tokens(output_max_tokens);
                config =
                    config.with_runtime(runtime_config(true, retry_observer, request_observer));
                Box::new(AnthropicModel::new(config))
            }
        }
    }

    /// 获取模型的上下文窗口大小（不消费 self）。
    ///
    /// 历史实现通过 `into_model().context_window()` 取值，OpenAI 与 Anthropic
    /// provider 均返回 200_000；`peri_model::Model` 不暴露 context_window，
    /// 此处保持配置级常量语义（1M 窗口由 `context_1m()` 标志在调用侧覆盖）。
    /// 会话调用方可通过 `from_provider_config` 传入手工上下文窗口。
    pub fn context_window(&self) -> u32 {
        match self {
            Self::OpenAi { context_window, .. }
            | Self::OpenAiResponses { context_window, .. }
            | Self::Anthropic { context_window, .. } => context_window.unwrap_or(200_000),
        }
    }

    pub fn into_model(self) -> Box<dyn peri_model::Model> {
        self.into_model_with_request_observer(None)
    }
}

fn runtime_config(
    full_observation: bool,
    retry_observer: Option<Arc<dyn peri_model::RetryObserver>>,
    request_observer: Option<Arc<dyn peri_model::RequestObserver>>,
) -> peri_model::ModelRuntimeConfig {
    let mut runtime = if full_observation {
        peri_model::ModelRuntimeConfig::with_full_observation()
    } else {
        peri_model::ModelRuntimeConfig::default()
    };
    if let Some(observer) = retry_observer {
        runtime = runtime.with_retry_observer(observer);
    }
    if let Some(observer) = request_observer {
        runtime = runtime.with_request_observer(observer);
    }
    runtime
}

/// 解析显式 Provider 配置并在构造模型前完成可用性校验。
#[allow(clippy::too_many_arguments)]
fn resolve_configured_provider(
    cfg: &config::PeriConfig,
    provider_id: &str,
    model: &str,
    effort: Option<String>,
    max_tokens: u32,
    context_1m: bool,
    context_window: Option<u32>,
) -> AgentModelResolution {
    let Some(provider) = cfg
        .config
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
    else {
        return AgentModelResolution::Error(format!(
            "模型选择引用了不存在的 Provider '{provider_id}'"
        ));
    };

    if let Err(error) = validate_agent_provider(provider, model, max_tokens) {
        return AgentModelResolution::Error(error);
    }

    match LlmProvider::from_provider_config(
        cfg,
        provider_id,
        model,
        effort,
        max_tokens,
        context_1m,
        context_window,
    ) {
        Some(provider) => AgentModelResolution::Resolved(provider),
        None => {
            AgentModelResolution::Error(format!("Provider '{provider_id}' 无法构造模型 '{model}'"))
        }
    }
}

/// 校验 Agent 显式选择所依赖的 Provider 配置。
fn validate_agent_provider(
    provider: &ProviderConfig,
    model: &str,
    max_tokens: u32,
) -> Result<(), String> {
    if provider.api_key.trim().is_empty() {
        return Err(format!("Provider '{}' 缺少 API Key", provider.id));
    }
    if model.trim().is_empty() {
        return Err(format!("Provider '{}' 的模型名不能为空", provider.id));
    }
    if max_tokens == 0 {
        return Err(format!(
            "Provider '{}' 的 max_tokens 必须大于 0",
            provider.id
        ));
    }
    if !matches!(
        provider.provider_type.as_str(),
        "anthropic" | "openai" | "openai_responses"
    ) {
        return Err(format!(
            "Provider '{}' 的类型 '{}' 不受支持",
            provider.id, provider.provider_type
        ));
    }
    if !provider.base_url.trim().is_empty() {
        let endpoint = Url::parse(&provider.base_url)
            .map_err(|error| format!("Provider '{}' 的 Base URL 无效: {error}", provider.id))?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(format!(
                "Provider '{}' 的 Base URL 必须是有效的 HTTP(S) 地址",
                provider.id
            ));
        }
    }
    Ok(())
}

/// 解析 provider endpoint；非法 URL 时记录告警并回落到默认值，
/// 保持 provider 构造期不失败（fail-soft）的语义。
/// 真正无效的 endpoint 会在 prepare/stream 时由 `peri-model` 返回
/// `InvalidEndpoint` 错误（fail closed）。
fn parse_endpoint(raw: &str, fallback: &str, label: &str) -> Url {
    Url::parse(raw).unwrap_or_else(|error| {
        tracing::warn!(
            %error,
            %label,
            raw,
            "provider endpoint 非法，回落到默认 endpoint"
        );
        Url::parse(fallback).expect("默认 endpoint 必须可解析")
    })
}

#[cfg(test)]
#[path = "provider_test.rs"]
mod tests;
