use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures_util::{Stream, StreamExt};
use keencode_model::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
    ProviderCapabilities, ProviderProtocol, TokenUsage,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use reqwest::{Client, Method};

use crate::adapters::Adapter;
use crate::catalog::{ModelCatalog, ModelCatalogFailure, fetch_model_catalog};
use crate::config::{ProviderConfig, ProviderConfigError};
use crate::http::{
    decode_error_response, decode_success_response, redact_model_error, transport_error,
};
#[cfg(feature = "live-test-trace")]
use crate::trace::{WireTraceCollector, WireTraceSink};
use crate::{
    REQUEST_METADATA_AGENT_ID, REQUEST_METADATA_PURPOSE, REQUEST_METADATA_SESSION_ID,
    REQUEST_METADATA_TURN_ID, RequestErrorKind, RequestMode, RequestObservation,
    RequestObservationScope, RequestObservationState, RequestObserver,
};

/// 在线调用失败时把已经脱敏的统一错误同时绑定到当前线级交换。
#[inline]
fn record_terminal_error(
    #[cfg(feature = "live-test-trace")] trace: Option<&WireTraceSink>,
    error: ModelError,
) -> ModelError {
    #[cfg(feature = "live-test-trace")]
    if let Some(trace) = trace {
        trace.record_terminal_error(&error);
    }
    error
}

/// 通过统一模型接口调用一个固定协议端点的 HTTP Provider。
#[derive(Clone)]
pub struct ProviderClient {
    config: Arc<ProviderConfig>,
    http: Client,
    /// 可选的生产请求观测器，只接收不含正文和凭据的短元数据。
    observer: Option<Arc<dyn RequestObserver>>,
    /// 显式启用时收集不含认证 Header 的线级证据。
    #[cfg(feature = "live-test-trace")]
    trace: Option<WireTraceCollector>,
}

impl fmt::Debug for ProviderClient {
    /// 只展示脱敏配置以及可选能力是否启用，绝不展开观测器或线级正文。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("ProviderClient");
        debug
            .field("config", &self.config)
            .field("observer_enabled", &self.observer.is_some());
        #[cfg(feature = "live-test-trace")]
        debug.field("trace_enabled", &self.trace.is_some());
        debug.finish()
    }
}

impl ProviderClient {
    /// 按 Provider 超时和 TLS 配置创建共享连接池。
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderConfigError> {
        config.validate()?;
        let mut builder = Client::builder()
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .redirect(reqwest::redirect::Policy::none());
        if let Some(timeout) = config.request_timeout {
            builder = builder.timeout(timeout);
        }
        let http = builder
            .build()
            .map_err(|error| ProviderConfigError::HttpClient {
                message: config.api_key().map_or_else(
                    || error.to_string(),
                    |api_key| api_key.redact(&error.to_string()),
                ),
            })?;
        Ok(Self {
            config: Arc::new(config),
            http,
            observer: None,
            #[cfg(feature = "live-test-trace")]
            trace: None,
        })
    }

    /// 创建显式启用线级证据收集的客户端与独立收集器。
    #[cfg(feature = "live-test-trace")]
    pub fn new_traced(
        config: ProviderConfig,
    ) -> Result<(Self, WireTraceCollector), ProviderConfigError> {
        let collector = WireTraceCollector::default();
        let mut client = Self::new(config)?;
        client.trace = Some(collector.clone());
        Ok((client, collector))
    }

    /// 返回不包含认证信息的 Provider 配置。
    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    /// 为后续模型请求安装同步短元数据观测器。
    pub fn with_request_observer(mut self, observer: Arc<dyn RequestObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// 请求并解析全部模型目录分页。
    pub async fn list_models(&self) -> Result<ModelCatalog, ModelError> {
        fetch_model_catalog(self)
            .await
            .map_err(|failure| failure.error)
    }

    /// 请求全部模型目录，并在失败时保留已经成功解析的分页事实。
    pub async fn list_models_with_partial(&self) -> Result<ModelCatalog, ModelCatalogFailure> {
        fetch_model_catalog(self).await
    }

    /// 创建协议请求；有凭据时添加敏感认证 Header，无凭据时保持显式匿名。
    pub(crate) fn authenticated_request(
        &self,
        method: Method,
        url: reqwest::Url,
    ) -> Result<reqwest::RequestBuilder, ModelError> {
        let request = self
            .http
            .request(method, url)
            .header(ACCEPT, "application/json, text/event-stream");
        let request = match self.config.protocol {
            ProviderProtocol::Messages => request.header("anthropic-version", "2023-06-01"),
            ProviderProtocol::ChatCompletions | ProviderProtocol::Responses => request,
        };
        let Some(api_key) = self.config.api_key() else {
            return Ok(request);
        };
        let mut credential = match self.config.protocol {
            ProviderProtocol::Messages => HeaderValue::from_str(api_key.expose()),
            ProviderProtocol::ChatCompletions | ProviderProtocol::Responses => {
                HeaderValue::from_str(&format!("Bearer {}", api_key.expose()))
            }
        }
        .map_err(|_| ModelError::InvalidRequest {
            message: "API Key 包含不能用于 HTTP Header 的字符".to_owned(),
        })?;
        credential.set_sensitive(true);
        Ok(match self.config.protocol {
            ProviderProtocol::Messages => request.header("x-api-key", credential),
            ProviderProtocol::ChatCompletions | ProviderProtocol::Responses => {
                request.header(AUTHORIZATION, credential)
            }
        })
    }
}

/// 进程内请求标识的单调后缀；与当前毫秒组合后不依赖随机源。
static NEXT_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
/// 单个观测身份或错误说明允许保留的最大字符数。
const MAX_OBSERVATION_TEXT_CHARS: usize = 1_000;
/// Provider 常见的不敏感请求标识响应头。
const PROVIDER_REQUEST_ID_HEADERS: [&str; 2] = ["x-request-id", "request-id"];

/// 从逻辑开始到响应流终态维持一次请求的观测状态。
struct RequestLifecycle {
    /// 接收同步短元数据的观察者。
    observer: Arc<dyn RequestObserver>,
    /// 仅用于拒绝 Provider 响应标识回显当前凭据的内存中比较值。
    api_key: Option<crate::ApiKey>,
    /// 一次逻辑调用的稳定标识。
    logical_request_id: String,
    /// 请求使用的模型。
    model: String,
    /// 请求使用的协议。
    protocol: ProviderProtocol,
    /// 请求采用的响应方式。
    mode: RequestMode,
    /// 不含凭据的完整协议端点。
    endpoint: String,
    /// 可选的 Session 标识。
    session_id: Option<String>,
    /// 可选的 Turn 标识。
    turn_id: Option<String>,
    /// 可选的 Agent 标识。
    agent_id: Option<String>,
    /// 可选的调用用途。
    purpose: Option<String>,
    /// 逻辑请求开始时间。
    logical_started_at_ms: u64,
    /// 实际 HTTP 尝试开始时间。
    attempt_started_at_ms: Option<u64>,
    /// 首次收到响应头的时间。
    response_headers_at_ms: Option<u64>,
    /// 已收到的 HTTP 状态。
    http_status: Option<u16>,
    /// Provider 返回的安全请求标识。
    provider_request_id: Option<String>,
    /// 流中最后一次合并后的 Token 用量。
    usage: TokenUsage,
    /// 已经发送唯一终态后为真。
    terminal: bool,
}

impl RequestLifecycle {
    /// 创建逻辑请求观测并立即发送开始事件。
    fn start(
        observer: Arc<dyn RequestObserver>,
        config: &ProviderConfig,
        request: &ModelRequest,
        endpoint: String,
    ) -> Self {
        let now = observation_now_ms();
        let sequence = NEXT_REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut lifecycle = Self {
            observer,
            api_key: config.api_key().cloned(),
            logical_request_id: format!("request-{now}-{sequence}"),
            model: request.model.clone(),
            protocol: config.protocol,
            mode: if config.response_mode.is_streaming() {
                RequestMode::Stream
            } else {
                RequestMode::Buffered
            },
            endpoint,
            session_id: observation_metadata(request, REQUEST_METADATA_SESSION_ID),
            turn_id: observation_metadata(request, REQUEST_METADATA_TURN_ID),
            agent_id: observation_metadata(request, REQUEST_METADATA_AGENT_ID),
            purpose: observation_metadata(request, REQUEST_METADATA_PURPOSE),
            logical_started_at_ms: now,
            attempt_started_at_ms: None,
            response_headers_at_ms: None,
            http_status: None,
            provider_request_id: None,
            usage: TokenUsage::unknown(),
            terminal: false,
        };
        lifecycle.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Started,
            0,
            now,
            None,
            None,
        );
        lifecycle
    }

    /// 标记第一次也是当前唯一一次真实 HTTP 尝试已经开始。
    fn start_attempt(&mut self) {
        let now = observation_now_ms();
        self.attempt_started_at_ms = Some(now);
        self.emit(
            RequestObservationScope::Attempt,
            RequestObservationState::Started,
            1,
            now,
            None,
            None,
        );
    }

    /// 保存不含响应正文和 Header 内容的 HTTP 响应头事实。
    fn response_head(&mut self, response: &reqwest::Response, api_key: Option<&crate::ApiKey>) {
        self.response_headers_at_ms = Some(observation_now_ms());
        self.http_status = Some(response.status().as_u16());
        self.provider_request_id = PROVIDER_REQUEST_ID_HEADERS.iter().find_map(|name| {
            response
                .headers()
                .get(*name)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| safe_provider_request_id(value, api_key))
        });
    }

    /// 合并 Provider 报告的可空用量字段。
    fn update_usage(&mut self, usage: &TokenUsage) {
        self.usage.update_from(usage);
    }

    /// 响应事件开始时补充 Provider 响应标识；HTTP Header 值优先。
    fn update_response_id(&mut self, response_id: Option<&str>) {
        if self.provider_request_id.is_none() {
            self.provider_request_id = response_id
                .and_then(|value| safe_provider_request_id(value, self.api_key.as_ref()));
        }
    }

    /// 在实际 HTTP 尝试前形成唯一逻辑失败终态。
    fn fail_before_attempt(&mut self, error: &ModelError) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let now = observation_now_ms();
        self.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Failed,
            0,
            now,
            Some(now.saturating_sub(self.logical_started_at_ms)),
            Some(error),
        );
    }

    /// 为实际 HTTP 尝试和逻辑请求依次形成唯一失败终态。
    fn fail(&mut self, error: &ModelError) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let now = observation_now_ms();
        if let Some(started_at_ms) = self.attempt_started_at_ms {
            self.emit(
                RequestObservationScope::Attempt,
                RequestObservationState::Failed,
                1,
                now,
                Some(now.saturating_sub(started_at_ms)),
                Some(error),
            );
        }
        self.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Failed,
            self.attempt_started_at_ms.map_or(0, |_| 1),
            now,
            Some(now.saturating_sub(self.logical_started_at_ms)),
            Some(error),
        );
    }

    /// 收到协议终态后立即形成唯一成功终态，不要求 Agent 继续轮询传输 EOF。
    fn complete(&mut self) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let now = observation_now_ms();
        if let Some(started_at_ms) = self.attempt_started_at_ms {
            self.emit(
                RequestObservationScope::Attempt,
                RequestObservationState::Completed,
                1,
                now,
                Some(now.saturating_sub(started_at_ms)),
                None,
            );
        }
        self.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Completed,
            self.attempt_started_at_ms.map_or(0, |_| 1),
            now,
            Some(now.saturating_sub(self.logical_started_at_ms)),
            None,
        );
    }

    /// Future 或流被提前丢弃时形成唯一取消终态。
    fn cancel(&mut self) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        let now = observation_now_ms();
        if let Some(started_at_ms) = self.attempt_started_at_ms {
            self.emit(
                RequestObservationScope::Attempt,
                RequestObservationState::Cancelled,
                1,
                now,
                Some(now.saturating_sub(started_at_ms)),
                None,
            );
        }
        self.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Cancelled,
            self.attempt_started_at_ms.map_or(0, |_| 1),
            now,
            Some(now.saturating_sub(self.logical_started_at_ms)),
            None,
        );
    }

    /// 组装并同步投递一条不含敏感正文的不可变观测。
    fn emit(
        &mut self,
        scope: RequestObservationScope,
        state: RequestObservationState,
        attempt: u32,
        at_ms: u64,
        duration_ms: Option<u64>,
        error: Option<&ModelError>,
    ) {
        let observation = RequestObservation {
            scope,
            state,
            logical_request_id: self.logical_request_id.clone(),
            attempt,
            max_attempts: 1,
            model: self.model.clone(),
            protocol: self.protocol,
            mode: self.mode,
            endpoint: self.endpoint.clone(),
            at_ms,
            duration_ms,
            response_headers_at_ms: self.response_headers_at_ms,
            http_status: self.http_status,
            provider_request_id: self.provider_request_id.clone(),
            usage: self.usage.clone(),
            error_kind: error.map(classify_request_error),
            error_summary: error.map(|error| bounded_text(&error.to_string())),
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            agent_id: self.agent_id.clone(),
            purpose: self.purpose.clone(),
        };
        let observer = Arc::clone(&self.observer);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observer.on_request(observation);
        }));
    }
}

impl Drop for RequestLifecycle {
    /// 确保取消 Future 或提前丢弃响应流不会遗留永久 running 记录。
    fn drop(&mut self) {
        self.cancel();
    }
}

/// 给真实 Provider 事件流附加用量、失败、EOF 和取消观测。
struct ObservedModelStream {
    /// 原始统一事件流。
    inner: ModelStream,
    /// 尚未形成终态的请求生命周期。
    lifecycle: RequestLifecycle,
    /// 是否已经观察到协议的 MessageEnd。
    saw_message_end: bool,
}

impl Stream for ObservedModelStream {
    type Item = Result<ModelStreamEvent, ModelError>;

    /// 先更新短元数据，再原样返回同一个 Provider 中立事件。
    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(event))) => {
                match &event {
                    ModelStreamEvent::MessageStart { metadata } => this
                        .lifecycle
                        .update_response_id(metadata.response_id.as_deref()),
                    ModelStreamEvent::Usage { usage } => this.lifecycle.update_usage(usage),
                    ModelStreamEvent::MessageEnd { .. } => {
                        this.saw_message_end = true;
                        // Agent Runner 在 MessageEnd 后完成当前 Round，不会为观测器
                        // 额外轮询一次 EOF；协议 Adapter 已保证终态事件合法。
                        this.lifecycle.complete();
                    }
                    ModelStreamEvent::DecodeTiming { .. }
                    | ModelStreamEvent::TextDelta { .. }
                    | ModelStreamEvent::ReasoningDelta { .. }
                    | ModelStreamEvent::ReasoningSummaryDelta { .. }
                    | ModelStreamEvent::ReasoningContinuation { .. }
                    | ModelStreamEvent::ToolCallStart { .. }
                    | ModelStreamEvent::ToolCallArgumentsDelta { .. }
                    | ModelStreamEvent::ToolCallEnd { .. } => {}
                }
                Poll::Ready(Some(Ok(event)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.lifecycle.fail(&error);
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                if this.saw_message_end {
                    this.lifecycle.complete();
                } else {
                    let error = ModelError::StreamInterrupted {
                        message: "模型事件流在协议终态前关闭".to_owned(),
                        retryable: true,
                    };
                    this.lifecycle.fail(&error);
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// 仅对真实流式响应计时；将结果先于终态交给同一收集器，保持取消与背压语义。
struct TimedModelStream {
    inner: ModelStream,
    origin: Instant,
    first_output_ms: Option<u64>,
    pending_end: Option<ModelStreamEvent>,
}

/// 空增量与 Usage/响应头不代表输出；工具参数也属于模型生成内容。
fn is_output_delta(event: &ModelStreamEvent) -> bool {
    match event {
        ModelStreamEvent::TextDelta { delta, .. }
        | ModelStreamEvent::ReasoningDelta { delta, .. }
        | ModelStreamEvent::ReasoningSummaryDelta { delta, .. }
        | ModelStreamEvent::ToolCallArgumentsDelta { delta, .. } => !delta.is_empty(),
        ModelStreamEvent::ToolCallStart { .. } => true,
        _ => false,
    }
}

impl Stream for TimedModelStream {
    type Item = Result<ModelStreamEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(end) = this.pending_end.take() {
            return Poll::Ready(Some(Ok(end)));
        }
        match this.inner.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(event))) => {
                let now = u64::try_from(this.origin.elapsed().as_millis()).unwrap_or(u64::MAX);
                if is_output_delta(&event) && this.first_output_ms.is_none() {
                    this.first_output_ms = Some(now);
                }
                if matches!(event, ModelStreamEvent::MessageEnd { .. }) {
                    if let Some(first) = this.first_output_ms {
                        this.pending_end = Some(event);
                        return Poll::Ready(Some(Ok(ModelStreamEvent::DecodeTiming {
                            duration_ms: now.saturating_sub(first),
                        })));
                    }
                }
                Poll::Ready(Some(Ok(event)))
            }
            other => other,
        }
    }
}

/// 返回当前 Unix Epoch 毫秒，系统时间异常时安全退化为零。
fn observation_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// 读取一个由 Runtime 写入的有界非空追踪字段。
fn observation_metadata(request: &ModelRequest, key: &str) -> Option<String> {
    request
        .metadata
        .get(key)
        .and_then(|value| bounded_non_empty(value))
}

/// 丢弃空值并限制不可信短文本长度。
fn bounded_non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| bounded_text(trimmed))
}

/// 只保留不会回显当前认证凭据的有界 Provider 请求标识。
fn safe_provider_request_id(value: &str, api_key: Option<&crate::ApiKey>) -> Option<String> {
    let value = bounded_non_empty(value)?;
    if value.chars().any(char::is_control)
        || api_key.is_some_and(|api_key| value.contains(api_key.expose()))
    {
        return None;
    }
    Some(value)
}

/// 按 Unicode 字符边界截断观测文本。
fn bounded_text(value: &str) -> String {
    value.chars().take(MAX_OBSERVATION_TEXT_CHARS).collect()
}

/// 把 Provider 中立错误映射为稳定请求记录分类。
fn classify_request_error(error: &ModelError) -> RequestErrorKind {
    match error {
        ModelError::Transport { .. } => RequestErrorKind::Transport,
        ModelError::StreamInterrupted { .. } => RequestErrorKind::StreamInterrupted,
        ModelError::Protocol { .. } | ModelError::ProtocolUnsupported { .. } => {
            RequestErrorKind::Protocol
        }
        ModelError::Cancelled { .. } => RequestErrorKind::Cancelled,
        ModelError::Authentication { .. }
        | ModelError::Authorization { .. }
        | ModelError::QuotaExceeded { .. }
        | ModelError::ModelNotFound { .. }
        | ModelError::RateLimited { .. }
        | ModelError::ProviderUnavailable { .. } => RequestErrorKind::HttpStatus,
        ModelError::ContextLengthExceeded { .. }
        | ModelError::InvalidRequest { .. }
        | ModelError::UnsupportedCapability { .. }
        | ModelError::StructuredOutput { .. } => RequestErrorKind::Other,
    }
}

impl ModelProvider for ProviderClient {
    /// 返回指定模型的配置能力快照。
    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        self.config.capabilities_for(model)
    }

    /// 编码并发送一次真实流式模型请求。
    fn stream(&self, request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>> {
        let client = self.clone();
        Box::pin(async move {
            let url = client
                .config
                .protocol_url()
                .map_err(|error| ModelError::InvalidRequest {
                    message: error.to_string(),
                })?;
            let mut lifecycle = client.observer.as_ref().map(|observer| {
                RequestLifecycle::start(
                    Arc::clone(observer),
                    &client.config,
                    &request,
                    url.as_str().to_owned(),
                )
            });
            if let Err(error) = request.validate() {
                if let Some(lifecycle) = &mut lifecycle {
                    lifecycle.fail_before_attempt(&error);
                }
                return Err(error);
            }
            let mut adapter = Adapter::new(client.config.protocol);
            adapter.configure_chat_output_tokens(client.config.chat_output_token_field);
            let body = match adapter
                .encode_request(&request, client.config.response_mode.is_streaming())
            {
                Ok(body) => body,
                Err(error) => {
                    if let Some(lifecycle) = &mut lifecycle {
                        lifecycle.fail_before_attempt(&error);
                    }
                    return Err(error);
                }
            };
            #[cfg(feature = "live-test-trace")]
            let trace = client.trace.as_ref().map(|collector| {
                collector.begin(request.clone(), client.config.max_event_bytes, body.clone())
            });
            let request_builder = match client.authenticated_request(Method::POST, url) {
                Ok(request_builder) => request_builder.json(&body),
                Err(error) => {
                    let error = record_terminal_error(
                        #[cfg(feature = "live-test-trace")]
                        trace.as_ref(),
                        error,
                    );
                    if let Some(lifecycle) = &mut lifecycle {
                        lifecycle.fail_before_attempt(&error);
                    }
                    return Err(error);
                }
            };
            if let Some(lifecycle) = &mut lifecycle {
                lifecycle.start_attempt();
            }
            let response = match request_builder.send().await {
                Ok(response) => response,
                Err(error) => {
                    let error = transport_error(error, client.config.api_key());
                    let error = record_terminal_error(
                        #[cfg(feature = "live-test-trace")]
                        trace.as_ref(),
                        error,
                    );
                    if let Some(lifecycle) = &mut lifecycle {
                        lifecycle.fail(&error);
                    }
                    return Err(error);
                }
            };
            if let Some(lifecycle) = &mut lifecycle {
                lifecycle.response_head(&response, client.config.api_key());
            }
            #[cfg(feature = "live-test-trace")]
            if let Some(trace) = &trace {
                let content_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                trace.record_response_head(response.status().as_u16(), content_type);
            }
            if !response.status().is_success() {
                let error = decode_error_response(
                    response,
                    client.config.api_key(),
                    client.config.max_event_bytes,
                    #[cfg(feature = "live-test-trace")]
                    trace.clone(),
                )
                .await;
                let error = record_terminal_error(
                    #[cfg(feature = "live-test-trace")]
                    trace.as_ref(),
                    error,
                );
                if let Some(lifecycle) = &mut lifecycle {
                    lifecycle.fail(&error);
                }
                return Err(error);
            }
            let is_sse = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
            let stream = match decode_success_response(
                response,
                adapter,
                client.config.max_event_bytes,
                client.config.max_response_bytes,
                #[cfg(feature = "live-test-trace")]
                trace.clone(),
            )
            .await
            {
                Ok(stream) => stream,
                Err(error) => {
                    let error = redact_model_error(error, client.config.api_key());
                    let error = record_terminal_error(
                        #[cfg(feature = "live-test-trace")]
                        trace.as_ref(),
                        error,
                    );
                    if let Some(lifecycle) = &mut lifecycle {
                        lifecycle.fail(&error);
                    }
                    return Err(error);
                }
            };
            let api_key = client.config.api_key().cloned();
            #[cfg(feature = "live-test-trace")]
            let stream_trace = trace;
            let stream: ModelStream = Box::pin(stream.map(move |item| {
                item.map_err(|error| redact_model_error(error, api_key.as_ref()))
                    .map_err(|error| {
                        record_terminal_error(
                            #[cfg(feature = "live-test-trace")]
                            stream_trace.as_ref(),
                            error,
                        )
                    })
            }));
            // 缓冲 JSON 是一次性交付，不能把本地解析速度当成模型输出速度。
            let stream: ModelStream = if is_sse {
                Box::pin(TimedModelStream {
                    inner: stream,
                    origin: Instant::now(),
                    first_output_ms: None,
                    pending_end: None,
                })
            } else {
                stream
            };
            Ok(match lifecycle {
                Some(lifecycle) => Box::pin(ObservedModelStream {
                    inner: stream,
                    lifecycle,
                    saw_message_end: false,
                }) as ModelStream,
                None => stream,
            })
        })
    }
}

#[cfg(test)]
mod decode_timing_tests {
    use super::*;
    use keencode_model::{ResponseMetadata, StopReason};

    /// 首段输出识别与终态前单次投递，共用生产包装器；不需要真实模型或墙钟 sleep。
    #[test]
    fn timing_is_emitted_once_before_end_and_missing_output_stays_unknown() {
        for has_output in [false, true] {
            let mut events = vec![ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            }];
            if has_output {
                events.push(ModelStreamEvent::TextDelta {
                    index: 0,
                    delta: "ok".into(),
                });
            }
            events.push(ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            });
            let stream = TimedModelStream {
                inner: Box::pin(futures_util::stream::iter(events.into_iter().map(Ok))),
                origin: Instant::now(),
                first_output_ms: None,
                pending_end: None,
            };
            let result = futures_executor::block_on(stream.collect::<Vec<_>>());
            assert!(matches!(
                result.last(),
                Some(Ok(ModelStreamEvent::MessageEnd { .. }))
            ));
            let count = result
                .iter()
                .filter(|event| matches!(event, Ok(ModelStreamEvent::DecodeTiming { .. })))
                .count();
            assert_eq!(count, usize::from(has_output));
            if has_output {
                assert!(matches!(
                    result[result.len() - 2],
                    Ok(ModelStreamEvent::DecodeTiming { .. })
                ));
            }
        }
    }

    #[test]
    fn output_start_matches_harness_text_reasoning_and_tool_deltas() {
        assert!(!is_output_delta(&ModelStreamEvent::TextDelta {
            index: 0,
            delta: String::new()
        }));
        assert!(!is_output_delta(&ModelStreamEvent::Usage {
            usage: TokenUsage::unknown()
        }));
        assert!(is_output_delta(&ModelStreamEvent::ReasoningDelta {
            index: 0,
            delta: "thinking".into()
        }));
        assert!(is_output_delta(&ModelStreamEvent::ToolCallStart {
            index: 0,
            id: "a".into(),
            name: "Read".into()
        }));
        assert!(is_output_delta(&ModelStreamEvent::ToolCallArgumentsDelta {
            index: 0,
            id: "a".into(),
            delta: "{}".into()
        }));
    }
}
