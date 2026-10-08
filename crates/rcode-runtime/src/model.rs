use rcode_model::{ProviderProtocol, ReasoningCapability};
use rcode_provider::{ApiKey, ProviderClient, ProviderConfig, ReasoningBody};

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelConfig {
    pub provider_id: String,
    pub protocol: ProviderProtocol,
    pub base_url: String,
    pub model: String,
    pub context_limit: u64,
    pub max_output_tokens: Option<u32>,
    #[serde(default = "default_image_input")]
    pub image_input: bool,
    pub reasoning_body: Option<ReasoningBody>,
    pub allow_private_network: bool,
}

impl ModelConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.model.trim().is_empty()
            || self.model.len() > 256
            || !(8192..=4_000_000).contains(&self.context_limit)
            || self.max_output_tokens.is_some_and(|limit| {
                limit == 0 || u64::from(limit) > self.context_limit || limit > 1_000_000
            })
        {
            return Err("模型配置无效或超过大小上限".into());
        }
        crate::network::validate_network_endpoint(&self.base_url)
    }

    pub fn apply_capabilities(&self, config: &mut ProviderConfig) {
        config.reasoning_body = self.reasoning_body.clone();
        config.default_capabilities.streaming = true;
        config.default_capabilities.tool_calling = true;
        config.default_capabilities.parallel_tool_calls = true;
        config.default_capabilities.reasoning = if self.reasoning_body.is_some() {
            ReasoningCapability::Configurable
        } else {
            ReasoningCapability::OutputOnly
        };
        config.default_capabilities.image_input = self.image_input;
        config.default_capabilities.max_context_tokens = Some(self.context_limit);
        config.default_capabilities.max_output_tokens = self.max_output_tokens.map(u64::from);
    }

    pub async fn provider(&self, key: Option<ApiKey>) -> Result<ProviderClient, String> {
        self.validate()?;
        let mut config = match key {
            Some(key) => ProviderConfig::new(&self.provider_id, self.protocol, &self.base_url, key),
            None if matches!(self.provider_id.as_str(), "openai" | "anthropic") => {
                return Err("尚未配置模型供应商密钥".into());
            }
            None => ProviderConfig::new_unauthenticated(
                &self.provider_id,
                self.protocol,
                &self.base_url,
            ),
        }
        .map_err(|error| error.to_string())?;
        self.apply_capabilities(&mut config);
        let http =
            crate::network::agent_http_client(&self.base_url, self.allow_private_network).await?;
        ProviderClient::new(config)
            .map(|provider| provider.with_http_client(http))
            .map_err(|error| error.to_string())
    }
}

fn default_image_input() -> bool {
    true
}
