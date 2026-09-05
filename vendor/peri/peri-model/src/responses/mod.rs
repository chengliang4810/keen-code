//! OpenAI Responses API provider（`POST /v1/responses`，SSE 流式）。
//!
//! 上游重构时未随 `openai_compatible` 迁移 Responses 协议；本模块从旧
//! `peri-agent::llm::responses` 移植并适配 `peri-model` 的流式优先 `Model` trait。
//! 与 Chat Completions 的关键差异：
//! - 请求体使用 `input` items 与 `instructions` 字段；
//! - 中转网关只接受 `stream: true` 请求，非流式直接返回 400；
//! - 工具调用参数在 `response.output_item.done` 事件中完整给出，无需增量累积。
//!
//! 本模块只产生和消费标准 `peri-model` 协议；不会引用 Agent 事件或类型。

mod request;
mod response;
mod stream;

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    runtime::{
        start_logical_request,
        stream::{runtime_http_sse_stream_with_lifecycle, HttpSseRequest},
        RequestObservationContext,
    },
    transport::{HttpRequest, HttpTransport, ReqwestTransport},
    ModelCapabilities, ModelError, ModelRequest, ModelResult, ModelRuntimeConfig, ModelStream,
    PreparedModelRequest, ProviderProtocol,
};

use request::BuiltResponsesRequest;

const PROVIDER_NAME: &str = "openai-responses";
const DEFAULT_MAX_TOKENS: u32 = 32_000;

/// OpenAI Responses API 的强类型配置。
///
/// 认证凭据仅保存在此配置和模型内部；其 `Debug` 实现永不输出凭据。
pub struct ResponsesConfig {
    endpoint: Url,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
    max_tokens: u32,
    supports_vision: bool,
    runtime: ModelRuntimeConfig,
}

impl ResponsesConfig {
    /// 显式创建配置；环境变量解析属于上层应用，不在协议 crate 中提供。
    pub fn new(endpoint: Url, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            endpoint,
            api_key: api_key.into(),
            model: model.into(),
            reasoning_effort: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            supports_vision: true,
            runtime: ModelRuntimeConfig::default(),
        }
    }

    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_vision_support(mut self, supported: bool) -> Self {
        self.supports_vision = supported;
        self
    }

    pub fn with_runtime(mut self, runtime: ModelRuntimeConfig) -> Self {
        self.runtime = runtime;
        self
    }
}

impl fmt::Debug for ResponsesConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponsesConfig")
            .field("endpoint", &debug_endpoint_projection(&self.endpoint))
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("max_tokens", &self.max_tokens)
            .field("runtime", &self.runtime)
            .finish()
    }
}

fn debug_endpoint_projection(endpoint: &Url) -> String {
    match endpoint.host() {
        Some(host) => format!("{}://{host}/[REDACTED]", endpoint.scheme()),
        None => format!("{}://[REDACTED]", endpoint.scheme()),
    }
}

/// OpenAI Responses API 模型。
pub struct ResponsesModel {
    config: ResponsesConfig,
    transport: Arc<dyn HttpTransport>,
    client: reqwest::Client,
}

impl ResponsesModel {
    /// 从显式、强类型配置创建模型。
    ///
    /// transport 与 native request path 共享同一个 `reqwest::Client`（clone 仅
    /// 增加引用计数，连接池 / TLS session cache 复用），不再创建双 client。
    pub fn new(config: ResponsesConfig) -> Self {
        let client = reqwest::Client::new();
        Self {
            config,
            transport: Arc::new(ReqwestTransport::new(client.clone())),
            client,
        }
    }

    #[cfg(test)]
    fn with_transport(config: ResponsesConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            transport,
            client: reqwest::Client::new(),
        }
    }

    /// 所有 public request path 共用的私有构造结果。
    fn build_request(&self, request: &ModelRequest) -> ModelResult<BuiltResponsesRequest> {
        request::build_request(&self.config, request)
    }

    fn native_http_request(
        client: &reqwest::Client,
        api_key: &str,
        built: &BuiltResponsesRequest,
    ) -> ModelResult<HttpRequest> {
        let request = client
            .post(built.endpoint.clone())
            .bearer_auth(api_key)
            .json(&built.body)
            .build()
            .map_err(|_| ModelError::protocol(crate::ProtocolErrorKind::Provider))?;
        Ok(HttpRequest::new(request))
    }
}

#[async_trait]
impl crate::Model for ResponsesModel {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            supports_tools: true,
            supports_reasoning: true,
            supports_vision: self.config.supports_vision,
            supports_streaming: true,
        }
    }

    fn prepare_request(&self, request: &ModelRequest) -> ModelResult<PreparedModelRequest> {
        self.build_request(request)?.observe(&self.config.runtime)
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<ModelStream> {
        let context = RequestObservationContext::from_request(
            ProviderProtocol::Other {
                value: "openai_responses".into(),
            },
            self.config.model.clone(),
            &self.config.endpoint,
            &request,
        );
        let lifecycle = start_logical_request(&self.config.runtime, context.clone());
        if cancellation.is_cancelled() {
            lifecycle.finish_cancelled();
            return Err(ModelError::cancelled());
        }
        let built = match self.build_request(&request) {
            Ok(built) => Arc::new(built),
            Err(error) => {
                lifecycle.finish_error(&error);
                return Err(error);
            }
        };
        let client = self.client.clone();
        let api_key = self.config.api_key.clone();
        let request_factory = {
            let built = Arc::clone(&built);
            Arc::new(move || Self::native_http_request(&client, &api_key, &built))
        };

        Ok(runtime_http_sse_stream_with_lifecycle(
            &self.config.runtime,
            cancellation,
            HttpSseRequest::new(
                Arc::clone(&self.transport),
                request_factory,
                Arc::<str>::from(PROVIDER_NAME),
                stream::decoders(),
            ),
            context,
            lifecycle,
        ))
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod mod_test;
