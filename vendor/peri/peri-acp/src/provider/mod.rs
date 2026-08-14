//! LLM Provider and model configuration.
//!
//! Manages provider configuration, model alias resolution, and LLM factory creation.
//! Decoupled from TUI-specific types.

pub mod config;
pub mod store;

use std::sync::Arc;

pub use config::{AppConfig, PeriConfig, ProfileConfig, Profiles, ProviderConfig, ProviderModels};
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

/// Agent/Workflow 模型选择的显式解析结果。
///
/// `Inherit` 只表示调用方应沿用当前会话 Provider；配置或语法错误必须使用
/// `Error`，禁止与“未配置档位”混为一谈后静默回退。
pub enum AgentModelResolution {
    /// 未指定模型、显式 `inherit`，或四档未配置具体模型时继承会话 Provider。
    Inherit,
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
                let model = std::env::var("ANTHROPIC_MODEL")
                    .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
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
                    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
                        let model = std::env::var("ANTHROPIC_MODEL")
                            .unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
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

    /// 从 PeriConfig 按 active_alias 对应的 Profile 构造 LlmProvider
    pub fn from_config(cfg: &config::PeriConfig) -> Option<Self> {
        Self::from_config_for_alias(cfg, &cfg.config.active_alias)
    }

    /// 绕过四档 Profile 抽象，直接由 `provider_id` + 具体 `model` 构造 LlmProvider。
    ///
    /// 用于会话级 provider 隔离：每个 session 直接持有 `"{provider_id}::{model}"`，
    /// 不再经过 `active_alias`/`Profiles` 间接层。`effort`/`max_tokens`/`context_1m`/
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

    /// 解析 Agent/Workflow 的模型选择。
    ///
    /// 支持 KeenCode `provider_id::model`、上游四档和 Claude 插件裸具体模型。
    /// 四档只有在 Profile 显式给出模型，或目标 Provider 给出对应档位映射时
    /// 才生效；KeenCode 生成的无 Profile 配置因此继承当前会话 Provider，绝不
    /// 从 `providers[0]` 猜测厂商默认模型。语法和已声明配置错误返回 `Error`，
    /// 供 embedded、stdio 与 workflow 在任何网络请求前统一拒绝。
    pub fn resolve_agent_model(
        cfg: &config::PeriConfig,
        inherited: &Self,
        model_selection: &str,
    ) -> AgentModelResolution {
        if model_selection.chars().any(char::is_control) {
            return AgentModelResolution::Error("模型选择不能包含控制字符".to_string());
        }
        let selection = model_selection.trim();
        if selection.is_empty() || selection.eq_ignore_ascii_case("inherit") {
            return AgentModelResolution::Inherit;
        }

        let qualified = match peri_acp_types::agents::split_provider_model(model_selection) {
            Ok(value) => value,
            Err(error) => return AgentModelResolution::Error(error.to_string()),
        };
        if let Some((provider_id, model)) = qualified {
            return resolve_configured_provider(
                cfg,
                provider_id,
                model,
                inherited.effort().map(str::to_owned),
                32_000,
                false,
                None,
            );
        }

        let tier = selection.to_ascii_lowercase();
        if peri_acp_types::agents::MODEL_TIERS.contains(&tier.as_str()) {
            return resolve_tier_for_agent(cfg, &tier);
        }

        AgentModelResolution::Resolved(inherited.with_model_name(selection.to_string()))
    }

    /// 从 PeriConfig 按指定档位（"fable"/"opus"/"sonnet"/"haiku"）构造 LlmProvider。
    /// Profile 是唯一事实源：provider/model/effort/max_tokens/context_1m 全部取自
    /// `profiles[alias]`，model 空时回退 provider.models 同档位映射（fable 空回退 opus）。
    pub fn from_config_for_alias(cfg: &config::PeriConfig, alias: &str) -> Option<Self> {
        let app = &cfg.config;
        let (provider, profile) = resolve_profile(app, alias)?;

        if provider.api_key.is_empty() {
            return None;
        }

        let model = resolve_model_name(provider, alias, profile);
        let effort = Some(profile.effort.clone());
        let max_tokens = profile.max_tokens;
        let context_1m = profile.context_1m;
        let context_window = profile.context_window;

        match provider.provider_type.as_str() {
            "anthropic" => Some(Self::Anthropic {
                api_key: provider.api_key.clone(),
                model,
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
                model,
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
                model,
                effort,
                max_tokens,
                context_1m,
                context_window,
                retry_observer: None,
            }),
        }
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

    /// 获取模型的上下文窗口大小（不消费 self）。
    ///
    /// 历史实现通过 `into_model().context_window()` 取值，OpenAI 与 Anthropic
    /// provider 均返回 200_000；`peri_model::Model` 不暴露 context_window，
    /// 此处保持配置级常量语义（1M 窗口由 `context_1m()` 标志在调用侧覆盖）。
    /// 客户端可通过 `ProfileConfig.context_window` 手工覆盖该默认值。
    pub fn context_window(&self) -> u32 {
        match self {
            Self::OpenAi { context_window, .. }
            | Self::OpenAiResponses { context_window, .. }
            | Self::Anthropic { context_window, .. } => context_window.unwrap_or(200_000),
        }
    }

    pub fn into_model(self) -> Box<dyn peri_model::Model> {
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
                // 全量观测：langfuse input 与实际发送请求体一致（敏感键/data URI 仍强制脱敏）
                config = config.with_runtime(match retry_observer {
                    Some(observer) => peri_model::ModelRuntimeConfig::with_full_observation()
                        .with_retry_observer(observer),
                    None => peri_model::ModelRuntimeConfig::with_full_observation(),
                });
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
                if let Some(observer) = retry_observer {
                    config = config.with_runtime(
                        peri_model::ModelRuntimeConfig::default().with_retry_observer(observer),
                    );
                }
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
                // 全量观测：langfuse input 与实际发送请求体一致（敏感键/data URI 仍强制脱敏）
                config = config.with_runtime(match retry_observer {
                    Some(observer) => peri_model::ModelRuntimeConfig::with_full_observation()
                        .with_retry_observer(observer),
                    None => peri_model::ModelRuntimeConfig::with_full_observation(),
                });
                Box::new(AnthropicModel::new(config))
            }
        }
    }
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

/// 解析四档 Profile；完全未配置的档位继承会话 Provider。
fn resolve_tier_for_agent(cfg: &config::PeriConfig, tier: &str) -> AgentModelResolution {
    let Some(profile) = cfg.config.profiles.get(tier) else {
        return AgentModelResolution::Inherit;
    };
    let explicit_model = profile
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty());

    let provider = if profile.provider.trim().is_empty() {
        cfg.config.providers.first()
    } else {
        match cfg
            .config
            .providers
            .iter()
            .find(|provider| provider.id == profile.provider)
        {
            Some(provider) => Some(provider),
            None => {
                return if explicit_model.is_some() {
                    AgentModelResolution::Error(format!(
                        "模型档位 '{tier}' 引用了不存在的 Provider '{}'",
                        profile.provider
                    ))
                } else {
                    AgentModelResolution::Inherit
                };
            }
        }
    };

    let Some(provider) = provider else {
        return if explicit_model.is_some() {
            AgentModelResolution::Error(format!(
                "模型档位 '{tier}' 已配置模型，但没有可用 Provider"
            ))
        } else {
            AgentModelResolution::Inherit
        };
    };
    let mapped_model = provider
        .models
        .get_model(tier)
        .map(str::trim)
        .filter(|model| !model.is_empty());
    let Some(model) = explicit_model.or(mapped_model) else {
        return AgentModelResolution::Inherit;
    };

    resolve_configured_provider(
        cfg,
        &provider.id,
        model,
        Some(profile.effort.clone()),
        profile.max_tokens,
        profile.context_1m,
        profile.context_window,
    )
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

/// 解析 active profile → (provider, profile)。
/// profile.provider 为空时回退第一个可用 provider；provider 找不到返回 None。
fn resolve_profile<'a>(
    app: &'a config::AppConfig,
    alias: &str,
) -> Option<(&'a ProviderConfig, &'a config::ProfileConfig)> {
    let profile = app.profiles.get(alias)?;
    let provider = if profile.provider.is_empty() {
        app.providers.first()
    } else {
        app.providers.iter().find(|p| p.id == profile.provider)
    }?;
    Some((provider, profile))
}

/// 解析最终 model 名：Profile.model > ProviderModels 同档位（fable 空回退 opus）> 厂商默认
fn resolve_model_name(
    provider: &ProviderConfig,
    alias: &str,
    profile: &config::ProfileConfig,
) -> String {
    if let Some(m) = profile.model.as_ref().filter(|m| !m.is_empty()) {
        return m.clone();
    }
    provider
        .models
        .get_model(alias)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match provider.provider_type.as_str() {
            "anthropic" => "claude-sonnet-4-6".to_string(),
            _ => "gpt-4o".to_string(),
        })
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
