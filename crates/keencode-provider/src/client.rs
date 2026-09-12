use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{Stream, StreamExt};
use keencode_model::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelStream, ModelStreamEvent,
    ProviderCapabilities, ProviderProtocol, TokenUsage,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use reqwest::{Client, Method};

use crate::adapters::Adapter;
use crate::catalog::{ModelCatalog, ModelCatalogFailure, fetch_model_catalog};
use crate::config::{ProviderConfig, ProviderConfigError, RetryConfig};
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

    /// 执行一次真实 HTTP 尝试并返回统一事件流；失败携带失败点捕获的头部事实。
    ///
    /// 本方法不触碰请求生命周期：尝试的开始、失败与终态观测全部由重试
    /// 状态机统一记录，保证「上一次尝试 fail、新尝试 start_attempt」的语义。
    async fn perform_attempt(
        &self,
        request_builder: reqwest::RequestBuilder,
        #[cfg(feature = "live-test-trace")] trace: Option<WireTraceSink>,
    ) -> Result<AttemptStream, AttemptFailure> {
        let mut adapter = Adapter::new(self.config.protocol);
        adapter.configure_chat_output_tokens(self.config.chat_output_token_field);
        let response = match request_builder.send().await {
            Ok(response) => response,
            Err(error) => {
                let error = transport_error(error, self.config.api_key());
                let error = record_terminal_error(
                    #[cfg(feature = "live-test-trace")]
                    trace.as_ref(),
                    error,
                );
                return Err(AttemptFailure { error, head: None });
            }
        };
        let head = capture_attempt_head(&response, self.config.api_key());
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
                self.config.api_key(),
                self.config.max_event_bytes,
                #[cfg(feature = "live-test-trace")]
                trace.clone(),
            )
            .await;
            let error = record_terminal_error(
                #[cfg(feature = "live-test-trace")]
                trace.as_ref(),
                error,
            );
            return Err(AttemptFailure {
                error,
                head: Some(head),
            });
        }
        let is_sse = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
        let stream = match decode_success_response(
            response,
            adapter,
            self.config.max_event_bytes,
            self.config.max_response_bytes,
            #[cfg(feature = "live-test-trace")]
            trace.clone(),
        )
        .await
        {
            Ok(stream) => stream,
            Err(error) => {
                let error = redact_model_error(error, self.config.api_key());
                let error = record_terminal_error(
                    #[cfg(feature = "live-test-trace")]
                    trace.as_ref(),
                    error,
                );
                return Err(AttemptFailure {
                    error,
                    head: Some(head),
                });
            }
        };
        let api_key = self.config.api_key().cloned();
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
        // 流空闲看门狗在计时包装之外再包一层：每次尝试的事件流超过空闲
        // 时长未收到任何事件（含非可见事件）即按可重试流中断结束本次尝试，
        // 由重试状态机的既有失败判定决定静默重试或原样交给下游。缓冲响应
        // 的事件流一次性就绪、从不挂起，包装层为透明的直通结构。
        let idle_timeout = Duration::from_millis(self.config.stream_idle_timeout_ms);
        let stream: ModelStream = Box::pin(IdleWatchdogStream {
            inner: stream,
            idle_timeout,
            timer: None,
            finished: false,
        });
        Ok(AttemptStream { stream, head })
    }
}

/// 进程内请求标识的单调后缀；与当前毫秒组合后不依赖随机源。
static NEXT_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);
/// 单个观测身份或错误说明允许保留的最大字符数。
const MAX_OBSERVATION_TEXT_CHARS: usize = 1_000;
/// Provider 常见的不敏感请求标识响应头。
const PROVIDER_REQUEST_ID_HEADERS: [&str; 2] = ["x-request-id", "request-id"];
/// 重试等待的对称抖动幅度；实际延迟在指数退避结果上偏移 -25%..+25%。
const RETRY_JITTER_FRACTION: f64 = 0.25;
/// Provider 报告的 retry_after 建议在重试等待中允许的最大毫秒数。
const RETRY_AFTER_CAP_MS: u64 = 120 * 1000;

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
    /// 当前请求允许的最大 HTTP 尝试次数。
    max_attempts: u32,
    /// 已经开始的 HTTP 尝试计数。
    attempt: u32,
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
            max_attempts: config.retry.max_attempts,
            attempt: 0,
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

    /// 标记下一次真实 HTTP 尝试已经开始；自动重试时序号随之递增。
    fn start_attempt(&mut self) {
        self.attempt = self.attempt.saturating_add(1);
        let now = observation_now_ms();
        self.attempt_started_at_ms = Some(now);
        self.emit(
            RequestObservationScope::Attempt,
            RequestObservationState::Started,
            self.attempt,
            now,
            None,
            None,
        );
    }

    /// 保存一次尝试在响应头到达时捕获的不含响应正文和 Header 内容的事实。
    fn record_attempt_head(&mut self, head: &AttemptHead) {
        self.response_headers_at_ms = Some(head.headers_at_ms);
        self.http_status = Some(head.http_status);
        self.provider_request_id = head.provider_request_id.clone();
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

    /// 记录一次将被自动重试的尝试失败；不形成逻辑终态，随后等待退避并重新开始尝试。
    fn fail_attempt(&mut self, error: &ModelError) {
        if self.terminal {
            return;
        }
        let now = observation_now_ms();
        if let Some(started_at_ms) = self.attempt_started_at_ms {
            self.emit(
                RequestObservationScope::Attempt,
                RequestObservationState::Failed,
                self.attempt,
                now,
                Some(now.saturating_sub(started_at_ms)),
                Some(error),
            );
        }
    }

    /// 为当前 HTTP 尝试和逻辑请求依次形成唯一失败终态。
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
                self.attempt,
                now,
                Some(now.saturating_sub(started_at_ms)),
                Some(error),
            );
        }
        self.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Failed,
            self.attempt,
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
                self.attempt,
                now,
                Some(now.saturating_sub(started_at_ms)),
                None,
            );
        }
        self.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Completed,
            self.attempt,
            now,
            Some(now.saturating_sub(self.logical_started_at_ms)),
            None,
        );
    }

    /// 确保取消 Future 或提前丢弃响应流不会遗留永久 running 记录。
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
                self.attempt,
                now,
                Some(now.saturating_sub(started_at_ms)),
                None,
            );
        }
        self.emit(
            RequestObservationScope::Logical,
            RequestObservationState::Cancelled,
            self.attempt,
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
            max_attempts: self.max_attempts,
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

/// 一次真实 HTTP 尝试在响应头到达时捕获的短元数据事实。
struct AttemptHead {
    /// 响应头到达的 Unix 毫秒时间。
    headers_at_ms: u64,
    /// 远端返回的 HTTP 状态。
    http_status: u16,
    /// Provider 返回的安全请求标识；不含可回显当前凭据的值。
    provider_request_id: Option<String>,
}

/// 从真实 HTTP 响应捕获不含正文的头部事实，供生命周期观测记录。
fn capture_attempt_head(
    response: &reqwest::Response,
    api_key: Option<&crate::ApiKey>,
) -> AttemptHead {
    AttemptHead {
        headers_at_ms: observation_now_ms(),
        http_status: response.status().as_u16(),
        provider_request_id: PROVIDER_REQUEST_ID_HEADERS.iter().find_map(|name| {
            response
                .headers()
                .get(*name)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| safe_provider_request_id(value, api_key))
        }),
    }
}

/// 单次 HTTP 尝试产出的统一事件流及捕获的头部事实。
struct AttemptStream {
    /// 已按媒体类型包装好计时与错误脱敏的统一事件流。
    stream: ModelStream,
    /// 响应头到达时捕获的短元数据。
    head: AttemptHead,
}

/// 单次尝试失败及失败点已知的状态事实，供重试判定与观测记录使用。
struct AttemptFailure {
    /// 已脱敏的 Provider 中立错误。
    error: ModelError,
    /// 响应头事实；请求未能送达远端时为 `None`。
    head: Option<AttemptHead>,
}

/// 单次尝试失败后的处理去向。
enum FailureAction {
    /// 已安排退避重试；状态机继续轮询退避等待。
    RetryScheduled,
    /// 终态失败；错误按原样交给下游。
    Terminal(ModelError),
}

/// 退避结束后正在执行的下一次尝试 Future。
type PendingAttemptFuture =
    Pin<Box<dyn Future<Output = Result<AttemptStream, AttemptFailure>> + Send>>;

/// 外包统一事件流的重试状态机；对下游维持一次逻辑请求的完整事件序列。
///
/// 某次尝试失败时，若尚未向下游转发任何事件且失败属于可重试类别，
/// 则按指数退避静默重新发起 HTTP 尝试；否则错误按原样交给下游。
/// 取消不在此层处理：调用方丢弃流即取消，退避等待随 [`BackoffSleep`] 释放，
/// 运行时的取消信号在 Runner 层 select，本层不引入新的取消通道。
struct RetryModelStream {
    /// 克隆的 Provider 客户端；重试时用同一配置重新发起 HTTP 尝试。
    client: ProviderClient,
    /// 首次构造的认证请求模板；每次尝试通过 `try_clone` 复用同一协议正文。
    template: reqwest::RequestBuilder,
    /// 可选的线级证据捕获槽位；同一逻辑请求的多次尝试聚合记录在同一交换内。
    #[cfg(feature = "live-test-trace")]
    trace: Option<WireTraceSink>,
    /// 尚未形成终态的请求生命周期。
    lifecycle: Option<RequestLifecycle>,
    /// 当前尝试的统一事件流。
    inner: Option<ModelStream>,
    /// 是否已经观察到协议的 MessageEnd。
    saw_message_end: bool,
    /// 是否已经向下游转发任意 Ok 事件；为真后不再自动重试。
    ///
    /// 静默重试只允许发生在尚未转发任何事件时：一旦下游收到过
    /// MessageStart 等结构性事件，第二次尝试会再次转发同一事件并
    /// 触发「一个响应只能包含一次开始事件」的协议错误。
    forwarded_output: bool,
    /// 已经开始的尝试次数。
    attempts_started: u32,
    /// 退避等待。
    backoff: Option<BackoffSleep>,
    /// 退避结束后正在执行的下一次尝试。
    pending_attempt: Option<PendingAttemptFuture>,
}

impl RetryModelStream {
    /// 按当前策略判定一次失败后的去向，并记录对应的观测事件。
    fn handle_failure(&mut self, error: ModelError, http_status: Option<u16>) -> FailureAction {
        let policy = self.client.config.retry.clone();
        let will_retry = !self.forwarded_output
            && self.attempts_started < policy.max_attempts
            && is_retryable_failure(&error, http_status, &policy);
        if let Some(lifecycle) = self.lifecycle.as_mut() {
            if will_retry {
                lifecycle.fail_attempt(&error);
            } else {
                lifecycle.fail(&error);
            }
        }
        if !will_retry {
            self.inner = None;
            return FailureAction::Terminal(error);
        }
        // 每次退避调度的 attempt、总次数与等待毫秒都可由 RequestObserver 的
        // Attempt/Failed -> Attempt/Started 事件对观测；后续接线时由 Runtime
        // 事件桥投影为 keencode-acp 的 `ModelRetryScheduled` 事件（上限
        // 32 次、延迟 10 分钟），本层只负责 observation，不直接接 UI。
        let delay = retry_delay(
            &policy,
            self.attempts_started,
            retry_after_ms(&error),
            jitter_factor(jitter_seed(self.attempts_started)),
        );
        self.inner = None;
        self.backoff = Some(BackoffSleep::new(delay));
        FailureAction::RetryScheduled
    }

    /// 在退避结束后启动下一次真实 HTTP 尝试；模板不可克隆时返回终态错误。
    fn begin_next_attempt(&mut self) -> Option<ModelError> {
        self.attempts_started = self.attempts_started.saturating_add(1);
        if let Some(lifecycle) = self.lifecycle.as_mut() {
            lifecycle.start_attempt();
        }
        let Some(builder) = self.template.try_clone() else {
            // JSON 正文模板始终可克隆；该分支仅为防御异常实现保留。
            let error = ModelError::Protocol {
                message: "模型请求模板无法复用于自动重试".to_owned(),
            };
            if let Some(lifecycle) = self.lifecycle.as_mut() {
                lifecycle.fail(&error);
            }
            self.inner = None;
            return Some(error);
        };
        let client = self.client.clone();
        #[cfg(feature = "live-test-trace")]
        let trace = self.trace.clone();
        self.pending_attempt = Some(Box::pin(async move {
            client
                .perform_attempt(
                    builder,
                    #[cfg(feature = "live-test-trace")]
                    trace,
                )
                .await
        }));
        None
    }
}

impl Stream for RetryModelStream {
    type Item = Result<ModelStreamEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(mut sleep) = this.backoff.take() {
                match Pin::new(&mut sleep).poll(context) {
                    Poll::Ready(()) => {
                        if let Some(error) = this.begin_next_attempt() {
                            return Poll::Ready(Some(Err(error)));
                        }
                    }
                    Poll::Pending => {
                        this.backoff = Some(sleep);
                        return Poll::Pending;
                    }
                }
            }
            if let Some(mut attempt) = this.pending_attempt.take() {
                match attempt.as_mut().poll(context) {
                    Poll::Ready(Ok(attempted)) => {
                        if let Some(lifecycle) = this.lifecycle.as_mut() {
                            lifecycle.record_attempt_head(&attempted.head);
                        }
                        this.inner = Some(attempted.stream);
                    }
                    Poll::Ready(Err(failure)) => {
                        let http_status = failure.head.as_ref().map(|head| head.http_status);
                        if let (Some(head), Some(lifecycle)) =
                            (failure.head.as_ref(), this.lifecycle.as_mut())
                        {
                            lifecycle.record_attempt_head(head);
                        }
                        match this.handle_failure(failure.error, http_status) {
                            FailureAction::RetryScheduled => continue,
                            FailureAction::Terminal(error) => {
                                return Poll::Ready(Some(Err(error)));
                            }
                        }
                    }
                    Poll::Pending => {
                        this.pending_attempt = Some(attempt);
                        return Poll::Pending;
                    }
                }
            }
            let Some(mut stream) = this.inner.take() else {
                // 没有活动尝试也没有挂起重试：仅在终态已记录后可能的防御性 EOF。
                return Poll::Ready(None);
            };
            match stream.as_mut().poll_next(context) {
                Poll::Ready(Some(Ok(event))) => {
                    match &event {
                        ModelStreamEvent::MessageStart { metadata } => {
                            if let Some(lifecycle) = this.lifecycle.as_mut() {
                                lifecycle.update_response_id(metadata.response_id.as_deref());
                            }
                        }
                        ModelStreamEvent::Usage { usage } => {
                            if let Some(lifecycle) = this.lifecycle.as_mut() {
                                lifecycle.update_usage(usage);
                            }
                        }
                        ModelStreamEvent::MessageEnd { .. } => {
                            this.saw_message_end = true;
                            if let Some(lifecycle) = this.lifecycle.as_mut() {
                                // Agent Runner 在 MessageEnd 后完成当前 Round，不会为观测器
                                // 额外轮询一次 EOF；协议 Adapter 已保证终态事件合法。
                                lifecycle.complete();
                            }
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
                    // 守卫以「已向下游转发的 Ok 事件」为准：只要转发过任意
                    // 事件（含 MessageStart、Usage 等结构性事件），本次尝试
                    // 的失败就不再自动重试；否则第二次尝试会重复转发同一
                    // 序列并触发下游协议错误。静默重试因此只覆盖尚未转发
                    // 任何事件的失败（连接失败、HTTP 状态失败、零事件中断）。
                    this.forwarded_output = true;
                    this.inner = Some(stream);
                    return Poll::Ready(Some(Ok(event)));
                }
                Poll::Ready(Some(Err(error))) => match this.handle_failure(error, None) {
                    FailureAction::RetryScheduled => {}
                    FailureAction::Terminal(error) => {
                        return Poll::Ready(Some(Err(error)));
                    }
                },
                Poll::Ready(None) => {
                    if this.saw_message_end {
                        return Poll::Ready(None);
                    }
                    let error = ModelError::StreamInterrupted {
                        message: "模型事件流在协议终态前关闭".to_owned(),
                        retryable: true,
                    };
                    match this.handle_failure(error, None) {
                        FailureAction::RetryScheduled => {}
                        FailureAction::Terminal(error) => {
                            return Poll::Ready(Some(Err(error)));
                        }
                    }
                }
                Poll::Pending => {
                    this.inner = Some(stream);
                    return Poll::Pending;
                }
            }
        }
    }
}

/// 到期唤醒线程的共享计时原语；退避等待与流空闲看门狗共用。
///
/// Provider 层运行在任意执行器上，无法假设 Tokio 定时器可用；为低频计时
/// 等待引入外部定时器依赖不值得。线程按短分片睡眠并在持有者被丢弃后
/// 及时退出，满足空闲资源约束；唤醒只会唤醒已注册的执行器任务。
struct DeadlineTimer {
    /// 到期时刻。
    deadline: Instant,
    /// 是否已经注册到期唤醒线程。
    registered: bool,
    /// 持有者被丢弃后通知等待线程提前退出。
    cancelled: Option<Arc<AtomicBool>>,
}

impl DeadlineTimer {
    /// 计时等待的分片睡眠间隔；丢弃后的残留等待最多持续一个分片。
    const POLL_INTERVAL: Duration = Duration::from_millis(50);

    /// 创建在指定延迟后到期的计时器。
    fn after(delay: Duration) -> Self {
        Self::at(Instant::now() + delay)
    }

    /// 创建在指定时刻到期的计时器。
    fn at(deadline: Instant) -> Self {
        Self {
            deadline,
            registered: false,
            cancelled: None,
        }
    }

    /// 在指定名称的线程上注册到期唤醒并等待到期。
    ///
    /// 到期或线程注册失败都返回 `Ready`；两者都没有后续唤醒源，调用方
    /// 必须把 `Ready` 按各自语义收敛：退避等待按零等待放行，流空闲看门狗
    /// 立即按超时切断，任何方向都不会永久挂起。
    fn poll_until(&mut self, context: &mut Context<'_>, thread_name: &str) -> Poll<()> {
        poll_deadline_wait(
            self.deadline,
            &mut self.registered,
            &mut self.cancelled,
            context,
            |deadline, waker| spawn_deadline_thread(thread_name, deadline, waker),
        )
    }
}

impl Drop for DeadlineTimer {
    /// 持有者在到期前被丢弃时通知等待线程提前退出，避免残留后台线程。
    fn drop(&mut self) {
        if let Some(cancelled) = &self.cancelled {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

/// 在独立线程上按分片睡眠等待到期并唤醒执行器任务；线程创建失败返回 `None`。
fn spawn_deadline_thread(
    thread_name: &str,
    deadline: Instant,
    waker: Waker,
) -> Option<Arc<AtomicBool>> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let thread_cancelled = Arc::clone(&cancelled);
    let spawned = std::thread::Builder::new()
        .name(thread_name.to_owned())
        .spawn(move || {
            while Instant::now() < deadline {
                if thread_cancelled.load(Ordering::Relaxed) {
                    return;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(DeadlineTimer::POLL_INTERVAL));
            }
            waker.wake();
        });
    // 线程创建失败时没有注册任何唤醒源；返回 None 让调用方按到期退化。
    spawned.ok().map(|_| cancelled)
}

/// 到期等待的共享轮询逻辑；到期或注册失败都返回 `Ready`。
///
/// 未注册唤醒源时先通过 `register` 注册，注册失败（线程创建失败，之后
/// 不再有任何唤醒源）时同样返回 `Ready`，由调用方决定退化方向：退避
/// 按零等待放行，看门狗按超时切断，避免流永久挂起。`register` 返回
/// `Some(取消标志)` 表示注册成功，持有者被丢弃时借助该标志通知等待
/// 线程提前退出。
fn poll_deadline_wait(
    deadline: Instant,
    registered: &mut bool,
    cancelled: &mut Option<Arc<AtomicBool>>,
    context: &mut Context<'_>,
    register: impl FnOnce(Instant, Waker) -> Option<Arc<AtomicBool>>,
) -> Poll<()> {
    if Instant::now() >= deadline {
        return Poll::Ready(());
    }
    if !*registered {
        *registered = true;
        let Some(flag) = register(deadline, context.waker().clone()) else {
            // 没有唤醒源就没有下一次轮询：按到期退化是唯一不会挂死的选择。
            return Poll::Ready(());
        };
        *cancelled = Some(flag);
    }
    Poll::Pending
}

/// 用按需创建、到期即结束的线程唤醒执行器的极简退避等待。
///
/// 计时原语与流空闲看门狗共享 [`DeadlineTimer`]；注册失败时没有唤醒源，
/// 按零等待退化为立即完成，避免流永久挂起。
struct BackoffSleep {
    /// 共享的到期计时原语。
    timer: DeadlineTimer,
}

impl BackoffSleep {
    /// 创建在指定延迟后到期的退避等待。
    fn new(delay: Duration) -> Self {
        Self {
            timer: DeadlineTimer::after(delay),
        }
    }
}

impl Future for BackoffSleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut()
            .timer
            .poll_until(context, "keencode-retry-backoff")
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

/// 流空闲看门狗：每次尝试的事件流超过空闲时长未收到任何事件时，
/// 按可重试的流中断结束当前尝试。
///
/// 看门狗包装在每次尝试的内部流外、重试状态机转发之内：超时错误沿用
/// [`ModelError::StreamInterrupted`] 语义进入 [`RetryModelStream`] 的既有
/// 失败判定，零事件阶段被切断可静默重试（正是「流挂起」最需要救的场景），
/// 已转发事件后切断按原样交给下游。计时按「空闲」而非总时长：deadline
/// 只在事件之后重新起算，任何事件（含 MessageStart、Usage 等非可见事件）
/// 都会终止当前空闲窗口并取消挂起的计时线程；同一空闲窗口内至多存在
/// 一个计时线程，重复轮询不会重新注册。空闲窗口从首次观察到挂起起算，
/// 下游背压造成的暂停不计入流的空闲时间。
struct IdleWatchdogStream {
    /// 被看守的当前尝试事件流。
    inner: ModelStream,
    /// 允许的最大空闲时长；零表示禁用看门狗。
    idle_timeout: Duration,
    /// 当前空闲窗口的计时器；仅在流挂起期间存在。
    timer: Option<DeadlineTimer>,
    /// 已发出切断错误或流结束后为真，随后按 EOF 收敛。
    finished: bool,
}

impl Stream for IdleWatchdogStream {
    type Item = Result<ModelStreamEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(context) {
            Poll::Ready(item) => {
                // 事件、错误与流结束都终止当前空闲窗口并取消挂起的计时线程。
                this.timer = None;
                this.finished = item.is_none();
                Poll::Ready(item)
            }
            Poll::Pending => {
                if this.idle_timeout.is_zero() {
                    return Poll::Pending;
                }
                let mut timer = match this.timer.take() {
                    Some(timer) => timer,
                    None => DeadlineTimer::after(this.idle_timeout),
                };
                match timer.poll_until(context, "keencode-stream-watchdog") {
                    Poll::Ready(()) => {
                        // 到期，或计时线程注册失败（之后没有唤醒源能再唤醒
                        // 本任务）：看门狗的失败退化方向与退避相反，必须
                        // 立即切断而不是放行，否则挂起流会永久等待。注册
                        // 失败分支的 deadline 由本次轮询刚创建、必然尚未
                        // 到达，据此区分两种来源并使用如实的切断文案。
                        this.finished = true;
                        let message = if Instant::now() >= timer.deadline {
                            format!(
                                "模型事件流超过 {} ms 未收到任何事件",
                                this.idle_timeout.as_millis()
                            )
                        } else {
                            // 计时线程创建失败：没有任何等待发生，「超过
                            // X ms 未收到事件」的描述会失真，改用保守切断
                            // 文案；错误变体与重试语义保持不变。
                            "看门狗计时线程创建失败，按超时保守切断".to_owned()
                        };
                        Poll::Ready(Some(Err(ModelError::StreamInterrupted {
                            message,
                            retryable: true,
                        })))
                    }
                    Poll::Pending => {
                        this.timer = Some(timer);
                        Poll::Pending
                    }
                }
            }
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

/// 看门狗切断错误说明的稳定特征；两个特征分别对应空闲到期切断与计时
/// 线程注册失败的保守切断。特征与消息构造同在 [`IdleWatchdogStream`] 中
/// 维护，`idle_watchdog_tests` 钉死两侧契约；看门狗切断据此在观测分类上
/// 与真实 EOF 截断（「模型事件流在协议终态前关闭」）区分开。
const WATCHDOG_CUT_MESSAGE_MARKERS: [&str; 2] = ["未收到任何事件", "按超时保守切断"];

/// 判断流中断错误说明是否来自流空闲看门狗切断而非真实流截断。
fn is_watchdog_idle_cut(message: &str) -> bool {
    WATCHDOG_CUT_MESSAGE_MARKERS
        .iter()
        .any(|marker| message.contains(marker))
}

/// 把 Provider 中立错误映射为稳定请求记录分类。
///
/// 看门狗切断沿用 `ModelError::StreamInterrupted` 变体以保持既有重试判定
/// 语义，但观测分类按「请求超过明确超时」归入 [`RequestErrorKind::Timeout`]，
/// 使看门狗切断与真实 EOF 截断在线上记录中可区分；[`ModelError`] 变体与
/// 重试拍板均不受该分类影响。
fn classify_request_error(error: &ModelError) -> RequestErrorKind {
    match error {
        ModelError::Transport { .. } => RequestErrorKind::Transport,
        ModelError::StreamInterrupted { message, .. } if is_watchdog_idle_cut(message) => {
            RequestErrorKind::Timeout
        }
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

/// 读取限流错误携带的服务器建议等待；其他错误不采用该建议。
fn retry_after_ms(error: &ModelError) -> Option<u64> {
    match error {
        ModelError::RateLimited {
            retry_after_ms: Some(server_ms),
            ..
        } => Some(*server_ms),
        _ => None,
    }
}

/// 判定一次尚未向下游转发任何事件的失败是否允许自动重试。
///
/// 分类建立在现有 [`ModelError`] 变体与 [`classify_request_error`] 之上：
/// 传输失败与流中断沿用归一化阶段的 `retryable` 标记；HTTP 408、429 与 5xx
/// 已归类为携带 `retryable: true` 的 `RateLimited` 或 `ProviderUnavailable`；
/// HTTP 409 冲突在线上归类为 `InvalidRequest`，仅在失败点确实观察到 409
/// 状态时重试。取消、上下文超限、认证授权与其他 4xx 一律不重试。
///
/// 与拍板清单「408/409/429/5xx 可重试」相比的两处已接受偏差（均为保守
/// 方向、继承既有分类器，不在此处扩大或收窄分类）：
///
/// - HTTP 425 被既有分类器与 429 一同归入 `RateLimited`，会按
///   `retry_http_status` 重试。拍板清单未列出 425，但「Too Early」语义上
///   同属限速类瞬时失败，多试一次方向保守，故保留既有分类。
/// - 501、505-508、510、511 虽属 5xx，但既有分类器的默认分支给出
///   `retryable: false` 的 `ProviderUnavailable`，走不可重试路径。这些
///   状态码表达服务器不支持或拒绝当前请求形态，重试无收益，故沿用
///   既有分类器而不为「5xx 全重试」新增特判。
fn is_retryable_failure(
    error: &ModelError,
    http_status: Option<u16>,
    policy: &RetryConfig,
) -> bool {
    match error {
        // 取消表达调用方意志，绝不自动重试。
        ModelError::Cancelled { .. } => false,
        ModelError::Transport { retryable, .. } => *retryable && policy.retry_transport,
        ModelError::StreamInterrupted { retryable, .. } => {
            *retryable && policy.retry_stream_interrupted
        }
        ModelError::RateLimited { .. } => policy.retry_http_status,
        ModelError::ProviderUnavailable { retryable, .. } => *retryable && policy.retry_http_status,
        ModelError::InvalidRequest { .. } => policy.retry_http_status && http_status == Some(409),
        _ => false,
    }
}

/// 计算一次失败后的重试等待：服务器建议优先并封顶 120 秒，否则按
/// `base_delay × 2^(失败次数-1)` 指数退避并封顶 `max_delay`，最后叠加对称抖动。
fn retry_delay(
    policy: &RetryConfig,
    failed_attempt: u32,
    retry_after_ms: Option<u64>,
    jitter: f64,
) -> Duration {
    let jitter = jitter.clamp(-RETRY_JITTER_FRACTION, RETRY_JITTER_FRACTION);
    let base_ms = match retry_after_ms {
        Some(server_ms) => server_ms.min(RETRY_AFTER_CAP_MS),
        None => {
            let max_delay_ms = u64::try_from(policy.max_delay.as_millis()).unwrap_or(u64::MAX);
            let mut delay_ms = u64::try_from(policy.base_delay.as_millis()).unwrap_or(u64::MAX);
            // 逐次翻倍避免移位溢出；到达上限后提前结束。
            for _ in 1..failed_attempt.max(1) {
                delay_ms = delay_ms.saturating_mul(2);
                if delay_ms >= max_delay_ms {
                    delay_ms = max_delay_ms;
                    break;
                }
            }
            delay_ms.min(max_delay_ms)
        }
    };
    let jittered_ms = (base_ms as f64 * (1.0 + jitter)).round().max(0.0);
    Duration::from_millis(jittered_ms as u64)
}

/// 从系统时钟低位噪声与尝试序号派生抖动种子的确定性混洗。
///
/// 抖动只需要打散同一毫秒内成批失败请求的重试节奏，没有密码学需求；
/// 为此引入外部随机依赖不符合项目的最少依赖约束，时钟低位噪声已经足够。
fn jitter_seed(failed_attempt: u32) -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    u64::from(nanos) ^ (u64::from(failed_attempt) << 32) ^ 0x9E37_79B9_7F4A_7C15
}

/// 用 xorshift64 把种子展开为 `[-0.25, 0.25)` 内的对称抖动比例。
fn jitter_factor(seed: u64) -> f64 {
    let mut state = seed | 1;
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    let unit = (state >> 11) as f64 / (1u64 << 53) as f64;
    unit * 2.0 * RETRY_JITTER_FRACTION - RETRY_JITTER_FRACTION
}

impl ModelProvider for ProviderClient {
    /// 返回指定模型的配置能力快照。
    fn capabilities(&self, model: &str) -> ProviderCapabilities {
        self.config.capabilities_for(model)
    }

    /// 编码并发送一次可自动重试的流式模型请求。
    ///
    /// 校验与编码等确定性失败不重试；真实 HTTP 尝试失败时，若尚未向下游
    /// 转发任何事件且错误属于可重试类别，则按 [`retry_delay`] 退避后静默重试。
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
            // 提示缓存断点跟随模型能力快照（与 ModelProvider::capabilities 同源）；
            // 只有 Messages Adapter 消费该开关，其他协议保持标准线格式。
            adapter.configure_prompt_caching(
                client
                    .config
                    .capabilities_for(&request.model)
                    .prompt_caching,
            );
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
                // 同一逻辑请求的多次尝试聚合记录在同一交换内：响应正文按到达
                // 顺序追加，响应头与终态以最后一次捕获为准。
                collector.begin(request.clone(), client.config.max_event_bytes, body.clone())
            });
            let template = match client.authenticated_request(Method::POST, url) {
                Ok(template) => template.json(&body),
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
            let mut attempts_started = 0_u32;
            let attempted = loop {
                attempts_started += 1;
                if let Some(lifecycle) = &mut lifecycle {
                    lifecycle.start_attempt();
                }
                let Some(builder) = template.try_clone() else {
                    // JSON 正文模板始终可克隆；该分支仅为防御异常实现保留。
                    let error = ModelError::Protocol {
                        message: "模型请求模板无法复用于自动重试".to_owned(),
                    };
                    if let Some(lifecycle) = &mut lifecycle {
                        lifecycle.fail(&error);
                    }
                    return Err(error);
                };
                match client
                    .perform_attempt(
                        builder,
                        #[cfg(feature = "live-test-trace")]
                        trace.clone(),
                    )
                    .await
                {
                    Ok(attempted) => {
                        if let Some(lifecycle) = &mut lifecycle {
                            lifecycle.record_attempt_head(&attempted.head);
                        }
                        break attempted;
                    }
                    Err(failure) => {
                        let http_status = failure.head.as_ref().map(|head| head.http_status);
                        if let (Some(head), Some(lifecycle)) =
                            (failure.head.as_ref(), lifecycle.as_mut())
                        {
                            lifecycle.record_attempt_head(head);
                        }
                        let policy = &client.config.retry;
                        let will_retry = attempts_started < policy.max_attempts
                            && is_retryable_failure(&failure.error, http_status, policy);
                        if let Some(lifecycle) = &mut lifecycle {
                            if will_retry {
                                // 观测语义：上一次尝试 fail，退避后新尝试 start_attempt。
                                lifecycle.fail_attempt(&failure.error);
                            } else {
                                lifecycle.fail(&failure.error);
                            }
                        }
                        if !will_retry {
                            return Err(failure.error);
                        }
                        BackoffSleep::new(retry_delay(
                            policy,
                            attempts_started,
                            retry_after_ms(&failure.error),
                            jitter_factor(jitter_seed(attempts_started)),
                        ))
                        .await;
                    }
                }
            };
            Ok(Box::pin(RetryModelStream {
                client,
                template,
                #[cfg(feature = "live-test-trace")]
                trace,
                lifecycle,
                inner: Some(attempted.stream),
                saw_message_end: false,
                forwarded_output: false,
                attempts_started,
                backoff: None,
                pending_attempt: None,
            }) as ModelStream)
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

#[cfg(test)]
mod retry_tests {
    use super::*;

    /// 默认策略下的指数退避序列按 2 的幂翻倍并在 max_delay 封顶。
    #[test]
    fn backoff_sequence_doubles_and_caps_at_max_delay() {
        let policy = RetryConfig::default();
        let expected_ms = [500, 1000, 2000, 4000, 8000, 16000, 32000, 32000, 32000];
        for (failed_attempt, expected) in expected_ms.iter().enumerate() {
            let delay = retry_delay(&policy, failed_attempt as u32 + 1, None, 0.0);
            assert_eq!(delay, Duration::from_millis(*expected));
        }
    }

    /// 自定义上限先于继续翻倍生效，base_delay 本身也受 max_delay 约束。
    #[test]
    fn backoff_caps_use_configured_max_delay() {
        let policy = RetryConfig {
            max_attempts: 10,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_millis(3000),
            ..RetryConfig::default()
        };
        let expected_ms = [500, 1000, 2000, 3000, 3000];
        for (failed_attempt, expected) in expected_ms.iter().enumerate() {
            let delay = retry_delay(&policy, failed_attempt as u32 + 1, None, 0.0);
            assert_eq!(delay, Duration::from_millis(*expected));
        }
    }

    /// 服务器 retry_after 建议优先于指数退避，封顶 120 秒后仍叠加抖动。
    #[test]
    fn retry_after_takes_precedence_and_caps_at_120_seconds() {
        let policy = RetryConfig::default();
        // 建议值不受尝试次数影响，即使指数退避已经超过建议值。
        for failed_attempt in [1_u32, 5, 9] {
            let delay = retry_delay(&policy, failed_attempt, Some(5000), 0.0);
            assert_eq!(delay, Duration::from_millis(5000));
        }
        assert_eq!(
            retry_delay(&policy, 1, Some(500_000), 0.0),
            Duration::from_millis(120_000)
        );
        // 抖动 ±25% 应用于封顶后的建议值。
        assert_eq!(
            retry_delay(&policy, 1, Some(500_000), -RETRY_JITTER_FRACTION),
            Duration::from_millis(90_000)
        );
        assert_eq!(
            retry_delay(&policy, 1, Some(500_000), RETRY_JITTER_FRACTION),
            Duration::from_millis(150_000)
        );
        // 服务器建议零等待时立即重试。
        assert_eq!(retry_delay(&policy, 1, Some(0), 0.0), Duration::ZERO);
    }

    /// 抖动比例有界且确定性；超范围输入被收敛回 ±25% 边界。
    #[test]
    fn jitter_stays_within_symmetric_bounds() {
        for seed in 0..10_000_u64 {
            let jitter = jitter_factor(seed);
            assert!((-RETRY_JITTER_FRACTION..RETRY_JITTER_FRACTION).contains(&jitter));
        }
        assert_eq!(jitter_factor(0), jitter_factor(0));
        // 抖动只影响等待下界，不会把延迟推成负数或翻倍以上。
        let policy = RetryConfig::default();
        let delay = retry_delay(&policy, 1, None, f64::MAX);
        assert_eq!(delay, Duration::from_millis(625));
    }

    /// 种子随尝试序号确定性变化，避免连续重试使用同一抖动比例。
    #[test]
    fn jitter_seed_varies_with_attempt() {
        assert_ne!(jitter_seed(1), jitter_seed(2));
        assert_ne!(jitter_seed(2), jitter_seed(3));
    }

    /// 唤醒线程注册失败时退避按零等待立即完成，不会留下永久挂起。
    #[test]
    fn backoff_wait_returns_ready_when_wake_thread_registration_fails() {
        // 永不到期的期限加注定失败的注册器，验证失败路径零等待放行。
        let deadline = Instant::now() + Duration::from_secs(3600);
        let mut registered = false;
        let mut cancelled: Option<Arc<AtomicBool>> = None;
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let poll = poll_deadline_wait(
            deadline,
            &mut registered,
            &mut cancelled,
            &mut context,
            |_, _| None, // 模拟线程创建失败：不注册任何唤醒源。
        );
        assert!(
            matches!(poll, Poll::Ready(())),
            "注册失败必须零等待放行而不是 Pending"
        );
        assert!(registered, "失败也应标记为已注册，避免重复注册尝试");
        assert!(cancelled.is_none(), "注册失败不应留下取消标志");
    }

    /// 重试分类只放行传输失败、408/409/429/5xx 与可见输出前的流中断，
    /// 并受逐类开关约束；取消、上下文超限、认证授权与其他 4xx 一律拒绝。
    #[test]
    fn retry_classification_follows_error_variants_and_flags() {
        let policy = RetryConfig::default();
        let retryable =
            |error: &ModelError, status: Option<u16>| is_retryable_failure(error, status, &policy);
        let transport = |retryable| ModelError::Transport {
            message: "transport".to_owned(),
            retryable,
        };
        let unavailable = |retryable, status| ModelError::ProviderUnavailable {
            message: "unavailable".to_owned(),
            status_code: status,
            retryable,
        };
        assert!(retryable(&transport(true), None));
        assert!(!retryable(&transport(false), None));
        assert!(retryable(
            &ModelError::StreamInterrupted {
                message: "interrupted".to_owned(),
                retryable: true
            },
            None
        ));
        assert!(!retryable(
            &ModelError::StreamInterrupted {
                message: "interrupted".to_owned(),
                retryable: false
            },
            None
        ));
        assert!(retryable(
            &ModelError::RateLimited {
                message: "rate limited".to_owned(),
                retry_after_ms: Some(1000),
                status_code: Some(429)
            },
            None
        ));
        assert!(retryable(&unavailable(true, Some(408)), None));
        assert!(retryable(&unavailable(true, Some(500)), None));
        assert!(!retryable(&unavailable(false, Some(418)), None));
        assert!(retryable(
            &ModelError::InvalidRequest {
                message: "conflict".to_owned()
            },
            Some(409)
        ));
        assert!(!retryable(
            &ModelError::InvalidRequest {
                message: "bad request".to_owned()
            },
            Some(400)
        ));
        assert!(!retryable(
            &ModelError::InvalidRequest {
                message: "bad request".to_owned()
            },
            None
        ));
        let non_retryable: [ModelError; 7] = [
            ModelError::Cancelled {
                message: "cancelled".to_owned(),
            },
            ModelError::ContextLengthExceeded {
                message: "too long".to_owned(),
            },
            ModelError::Authentication {
                message: "auth".to_owned(),
                status_code: Some(401),
            },
            ModelError::Authorization {
                message: "forbidden".to_owned(),
                status_code: Some(403),
            },
            ModelError::QuotaExceeded {
                message: "quota".to_owned(),
                status_code: Some(402),
            },
            ModelError::Protocol {
                message: "protocol".to_owned(),
            },
            ModelError::StructuredOutput {
                enforcement: keencode_model::StructuredOutputEnforcement::Native,
                failure: keencode_model::StructuredOutputFailureKind::InvalidJson,
                message: "structured".to_owned(),
            },
        ];
        for error in &non_retryable {
            assert!(!retryable(error, Some(500)), "{error:?} 不应重试");
        }

        // 逐类开关可以独立关闭对应类别。
        let muted = RetryConfig {
            retry_transport: false,
            retry_http_status: false,
            retry_stream_interrupted: false,
            ..RetryConfig::default()
        };
        assert!(!is_retryable_failure(&transport(true), None, &muted));
        assert!(!is_retryable_failure(
            &ModelError::RateLimited {
                message: "rate limited".to_owned(),
                retry_after_ms: None,
                status_code: Some(429)
            },
            None,
            &muted
        ));
        assert!(!is_retryable_failure(
            &unavailable(true, Some(503)),
            None,
            &muted
        ));
        assert!(!is_retryable_failure(
            &ModelError::StreamInterrupted {
                message: "interrupted".to_owned(),
                retryable: true
            },
            None,
            &muted
        ));
        assert!(!is_retryable_failure(
            &ModelError::InvalidRequest {
                message: "conflict".to_owned()
            },
            Some(409),
            &muted
        ));
    }

    /// 配置校验拒绝零尝试次数与超过观测事件上限的尝试次数。
    #[test]
    fn retry_config_validates_attempt_bounds() {
        let zero = RetryConfig {
            max_attempts: 0,
            ..RetryConfig::default()
        };
        let base_url = "https://bounds.example.invalid/v1";
        let config = |retry: RetryConfig| {
            let mut config = ProviderConfig::new_unauthenticated(
                "provider-retry-bounds",
                ProviderProtocol::Responses,
                base_url,
            )
            .expect("边界测试配置应有效");
            config.retry = retry;
            config
        };
        assert!(matches!(
            config(zero).validate(),
            Err(ProviderConfigError::InvalidRetryConfig { .. })
        ));
        let oversized = RetryConfig {
            max_attempts: 33,
            ..RetryConfig::default()
        };
        assert!(matches!(
            config(oversized).validate(),
            Err(ProviderConfigError::InvalidRetryConfig { .. })
        ));
        assert!(config(RetryConfig::default()).validate().is_ok());
        assert!(
            config(RetryConfig {
                max_attempts: 32,
                ..RetryConfig::default()
            })
            .validate()
            .is_ok()
        );
    }
}

#[cfg(test)]
mod idle_watchdog_tests {
    use super::*;
    use std::task::Wake;

    /// 记录唤醒事实的测试 Waker；用于观察计时线程是否真的到期唤醒。
    struct RecordingWaker(Arc<AtomicBool>);

    impl Wake for RecordingWaker {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// 永不产出任何事件的内部流，模拟「连接在、无数据、无关闭」的挂起。
    fn pending_stream() -> ModelStream {
        Box::pin(futures_util::stream::pending::<
            Result<ModelStreamEvent, ModelError>,
        >())
    }

    /// 到期线程在到期后唤醒已注册的 Waker；对照组，证明线程确实运行。
    #[test]
    fn deadline_timer_fires_and_wakes_registered_waker() {
        let woken = Arc::new(AtomicBool::new(false));
        let waker = Waker::from(Arc::new(RecordingWaker(Arc::clone(&woken))));
        let mut context = Context::from_waker(&waker);
        let mut timer = DeadlineTimer::after(Duration::from_millis(80));
        assert!(matches!(
            timer.poll_until(&mut context, "keencode-test-deadline"),
            Poll::Pending
        ));
        assert!(!woken.load(Ordering::SeqCst), "到期前不应唤醒");
        std::thread::sleep(Duration::from_millis(400));
        assert!(woken.load(Ordering::SeqCst), "到期后必须唤醒已注册的 Waker");
    }

    /// 丢弃计时器后等待线程经取消标志及时退出：期限（80ms）早已过去的
    /// 时刻仍无任何唤醒，证明线程没有睡到期末而是被 Drop 取消退出。
    /// 「至多一个分片内退出」由线程循环结构保证：取消标志按 50ms 分片
    /// 检查，因此取消后的残留睡眠不会超过一个分片。
    #[test]
    fn deadline_timer_drop_cancels_wait_thread_before_deadline() {
        let woken = Arc::new(AtomicBool::new(false));
        let waker = Waker::from(Arc::new(RecordingWaker(Arc::clone(&woken))));
        let mut context = Context::from_waker(&waker);
        let mut timer = DeadlineTimer::after(Duration::from_millis(80));
        assert!(matches!(
            timer.poll_until(&mut context, "keencode-test-deadline"),
            Poll::Pending
        ));
        drop(timer);
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            !woken.load(Ordering::SeqCst),
            "丢弃后线程必须取消退出而不是睡到期末唤醒"
        );
    }

    /// 永久挂起的内部流在看门狗到期时以可重试流中断结束该尝试。
    #[test]
    fn watchdog_cuts_pending_stream_after_idle_timeout() {
        let stream = IdleWatchdogStream {
            inner: pending_stream(),
            idle_timeout: Duration::from_millis(100),
            timer: None,
            finished: false,
        };
        let mut result = futures_executor::block_on(stream.collect::<Vec<_>>());
        assert_eq!(result.len(), 1, "看门狗切断后流应立即以错误结束");
        assert!(matches!(
            result.pop(),
            Some(Err(ModelError::StreamInterrupted {
                retryable: true,
                ..
            }))
        ));
    }

    /// 空闲超时为零时看门狗完全禁用：挂起流不注册计时线程也不切断。
    #[test]
    fn watchdog_zero_timeout_disables_cut_and_timer_registration() {
        let mut stream = IdleWatchdogStream {
            inner: pending_stream(),
            idle_timeout: Duration::ZERO,
            timer: None,
            finished: false,
        };
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        for _ in 0..3 {
            assert!(matches!(
                Pin::new(&mut stream).poll_next(&mut context),
                Poll::Pending
            ));
        }
        assert!(stream.timer.is_none(), "禁用时不注册任何计时线程");
    }

    /// 同一空闲窗口内的重复轮询不重新注册：挂起期间至多一个计时线程。
    #[test]
    fn watchdog_keeps_single_timer_across_pending_polls() {
        let mut stream = IdleWatchdogStream {
            inner: pending_stream(),
            idle_timeout: Duration::from_secs(3600),
            timer: None,
            finished: false,
        };
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut context),
            Poll::Pending
        ));
        let first_flag = stream
            .timer
            .as_ref()
            .expect("首次挂起应注册计时线程")
            .cancelled
            .clone();
        assert!(matches!(
            Pin::new(&mut stream).poll_next(&mut context),
            Poll::Pending
        ));
        let timer = stream.timer.as_ref().expect("挂起期间计时线程应保持注册");
        assert!(
            first_flag.is_some() && timer.cancelled.is_some(),
            "两次挂起都应持有取消标志"
        );
        assert!(
            Arc::ptr_eq(
                first_flag.as_ref().expect("首次挂起应有取消标志"),
                timer.cancelled.as_ref().expect("重复挂起应保留同一标志")
            ),
            "重复轮询不得重新注册新的计时线程"
        );
    }

    /// 看门狗切断与真实 EOF 截断共用 `StreamInterrupted` 变体，但观测分类
    /// 必须可区分：看门狗两种切断文案（空闲到期、计时线程注册失败）归类为
    /// 超时，真实截断与其他流中断保持流中断分类。此测试钉死消息特征与
    /// [`classify_request_error`] 之间的契约。
    #[test]
    fn watchdog_cut_classifies_as_timeout_distinct_from_real_eof() {
        let interrupted = |message: &str| ModelError::StreamInterrupted {
            message: message.to_owned(),
            retryable: true,
        };
        assert_eq!(
            classify_request_error(&interrupted("模型事件流超过 90000 ms 未收到任何事件")),
            RequestErrorKind::Timeout,
            "空闲到期切断应按超时分类"
        );
        assert_eq!(
            classify_request_error(&interrupted("看门狗计时线程创建失败，按超时保守切断")),
            RequestErrorKind::Timeout,
            "计时线程注册失败的保守切断应按超时分类"
        );
        assert_eq!(
            classify_request_error(&interrupted("模型事件流在协议终态前关闭")),
            RequestErrorKind::StreamInterrupted,
            "真实 EOF 截断应保持流中断分类"
        );
        assert_eq!(
            classify_request_error(&interrupted("SSE 连接被对端重置")),
            RequestErrorKind::StreamInterrupted,
            "其他流中断不得被误判为看门狗切断"
        );
    }
}
