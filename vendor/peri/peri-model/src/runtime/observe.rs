//! 同步、无正文的模型请求观测。
//!
//! 观测层位于 provider adapter 与通用 HTTP/SSE/retry runtime 之间：一次
//! `Model::stream`/`Model::complete` 是一个 logical call；每次进入 transport
//! 的循环都是独立的 physical attempt。observer 只收到安全元数据，调用同步执行，
//! 不使用 broadcast 或有界异步队列。

use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use url::Url;

use crate::{
    ModelError, ModelRequest, ModelRequestMode, ModelResponse, ProviderProtocol, TokenUsage,
};

/// 观测事件所属的生命周期层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestObservationScope {
    Logical,
    Attempt,
}

/// 观测事件状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestObservationState {
    Started,
    Completed,
    Failed,
    Cancelled,
}

/// 请求失败的稳定分类；不包含 provider 原始错误正文。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestErrorKind {
    Connection,
    Timeout,
    Tls,
    Transport,
    HttpStatus,
    Protocol,
    StreamInterrupted,
    Cancelled,
    RetryExhausted,
    Other,
}

/// 一次逻辑模型调用或物理 HTTP/SSE attempt 的安全观测。
///
/// 该结构明确没有 request body、response body、headers、cookie 或 API key。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestObservation {
    pub scope: RequestObservationScope,
    pub state: RequestObservationState,
    pub logical_request_id: String,
    /// 物理 attempt 从 1 开始；build_request 失败没有物理请求，使用 0。
    pub attempt: u32,
    pub max_attempts: u32,
    pub model: String,
    pub protocol: ProviderProtocol,
    pub mode: ModelRequestMode,
    /// 仅保留 scheme + host + port，不保留 path/query/fragment/userinfo。
    pub endpoint: String,
    pub at_ms: u64,
    pub duration_ms: Option<u64>,
    /// 收到 HTTP response headers 的时间；连接失败或未收到 headers 时为空。
    pub response_headers_at_ms: Option<u64>,
    pub http_status: Option<u16>,
    pub provider_request_id: Option<String>,
    pub usage: Option<TokenUsage>,
    pub error_kind: Option<RequestErrorKind>,
    pub error_summary: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub agent_id: Option<String>,
    pub purpose: Option<String>,
}

/// 同步 observer 接口。实现不得依赖 runtime 的异步广播生命周期。
pub trait RequestObserver: Send + Sync {
    fn on_request(&self, observation: RequestObservation);
}

impl<F> RequestObserver for F
where
    F: Fn(RequestObservation) + Send + Sync,
{
    fn on_request(&self, observation: RequestObservation) {
        self(observation)
    }
}

/// provider adapter 传给 HTTP/retry runtime 的只读上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequestObservationContext {
    pub(crate) logical_request_id: String,
    pub(crate) model: String,
    pub(crate) protocol: ProviderProtocol,
    pub(crate) mode: ModelRequestMode,
    pub(crate) endpoint: String,
    pub(crate) session_id: Option<String>,
    pub(crate) turn_id: Option<String>,
    pub(crate) agent_id: Option<String>,
    pub(crate) purpose: Option<String>,
}

impl RequestObservationContext {
    /// 用模型请求和未归一化 provider endpoint 构造上下文。
    pub(crate) fn from_request(
        protocol: ProviderProtocol,
        model: impl Into<String>,
        endpoint: &Url,
        request: &ModelRequest,
    ) -> Self {
        let call_context = request.call_context.as_ref();
        Self {
            logical_request_id: call_context
                .and_then(|context| context.logical_request_id.clone())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(next_logical_request_id),
            model: model.into(),
            protocol,
            mode: request.request_mode,
            endpoint: sanitize_endpoint(endpoint),
            session_id: call_context
                .and_then(|context| context.session_id.clone())
                .or_else(|| request.session_id.clone()),
            turn_id: call_context.and_then(|context| context.turn_id.clone()),
            agent_id: call_context.and_then(|context| context.agent_id.clone()),
            purpose: call_context.and_then(|context| context.purpose.clone()),
        }
    }

    fn observation(
        &self,
        event: RequestObservationEvent<'_>,
        max_attempts: u32,
        started_at_ms: u64,
        at_ms: u64,
    ) -> RequestObservation {
        let RequestObservationEvent {
            scope,
            state,
            attempt,
            response_headers_at_ms,
            http_status,
            provider_request_id,
            usage,
            error,
        } = event;
        let (error_kind, error_summary) = error
            .map(|(error, retry_exhausted)| error_projection(error, retry_exhausted))
            .unwrap_or((None, None));
        RequestObservation {
            scope,
            state,
            logical_request_id: self.logical_request_id.clone(),
            attempt,
            max_attempts,
            model: self.model.clone(),
            protocol: self.protocol.clone(),
            mode: self.mode,
            endpoint: self.endpoint.clone(),
            at_ms,
            duration_ms: (state != RequestObservationState::Started)
                .then(|| at_ms.saturating_sub(started_at_ms)),
            response_headers_at_ms,
            http_status,
            provider_request_id: provider_request_id.and_then(sanitize_provider_request_id),
            usage,
            error_kind,
            error_summary,
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            agent_id: self.agent_id.clone(),
            purpose: self.purpose.clone(),
        }
    }
}

/// 一条请求观测事件相对于逻辑上下文的动态字段。
///
/// 逻辑请求标识、模型和 endpoint 等稳定字段保留在 [`RequestObservationContext`]；
/// 本结构只携带生命周期状态、attempt 以及安全的结果元数据。
struct RequestObservationEvent<'a> {
    /// 事件属于逻辑调用还是物理 attempt。
    scope: RequestObservationScope,
    /// 当前生命周期状态。
    state: RequestObservationState,
    /// 物理 attempt 编号，逻辑事件使用当前已知编号。
    attempt: u32,
    /// 收到 HTTP response headers 的时间。
    response_headers_at_ms: Option<u64>,
    /// HTTP 响应状态码。
    http_status: Option<u16>,
    /// 已经脱敏的 provider request id。
    provider_request_id: Option<String>,
    /// provider 报告的 token 使用量。
    usage: Option<TokenUsage>,
    /// 供稳定分类使用的模型错误及是否为 retry exhausted。
    error: Option<(&'a ModelError, bool)>,
}

/// 绑定一条 logical call 的 finish-once 生命周期。
///
/// runtime 在每个 physical attempt 更新 `last_attempt`；ModelStream 在完成、错误
/// 或 Drop 时调用 finish，AtomicBool 保证取消/Drop/错误竞态只产生一条 logical 结束事件。
#[derive(Clone)]
pub(crate) struct RequestLifecycle {
    inner: Arc<RequestLifecycleInner>,
}

struct RequestLifecycleInner {
    observer: Option<Arc<dyn RequestObserver>>,
    context: RequestObservationContext,
    max_attempts: u32,
    started_at_ms: u64,
    last_attempt: AtomicU32,
    attempt_started_at_ms: AtomicU64,
    /// 当前已收到 terminal event 的最大 attempt；attempt 按 retry 顺序递增，
    /// 因此可用单个原子值保证同一 attempt 的 Completed/Cancelled/Failed 只落一次。
    terminal_attempt: AtomicU32,
    /// 首个 attempt 收到 response headers 的时间；0 表示尚未收到。
    response_headers_at_ms: AtomicU64,
    finished: AtomicBool,
}

impl RequestLifecycle {
    pub(crate) fn start(
        observer: Option<Arc<dyn RequestObserver>>,
        context: RequestObservationContext,
        max_attempts: u32,
    ) -> Self {
        let started_at_ms = now_ms();
        let lifecycle = Self {
            inner: Arc::new(RequestLifecycleInner {
                observer,
                context,
                max_attempts,
                started_at_ms,
                last_attempt: AtomicU32::new(0),
                attempt_started_at_ms: AtomicU64::new(0),
                terminal_attempt: AtomicU32::new(0),
                response_headers_at_ms: AtomicU64::new(0),
                finished: AtomicBool::new(false),
            }),
        };
        lifecycle.emit(RequestObservationEvent {
            scope: RequestObservationScope::Logical,
            state: RequestObservationState::Started,
            attempt: 0,
            response_headers_at_ms: None,
            http_status: None,
            provider_request_id: None,
            usage: None,
            error: None,
        });
        lifecycle
    }

    pub(crate) fn set_attempt(&self, attempt: u32, started_at_ms: u64) {
        self.inner.last_attempt.store(attempt, Ordering::Release);
        self.inner
            .attempt_started_at_ms
            .store(started_at_ms, Ordering::Release);
    }

    pub(crate) fn set_response_headers(&self, at_ms: u64) {
        if at_ms != 0 {
            self.inner
                .response_headers_at_ms
                .compare_exchange(0, at_ms, Ordering::AcqRel, Ordering::Acquire)
                .ok();
        }
    }

    pub(crate) fn finish_ok(&self, response: &ModelResponse) {
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        self.emit(RequestObservationEvent {
            scope: RequestObservationScope::Logical,
            state: RequestObservationState::Completed,
            attempt: self.inner.last_attempt.load(Ordering::Acquire),
            response_headers_at_ms: nonzero_timestamp(
                self.inner.response_headers_at_ms.load(Ordering::Acquire),
            ),
            http_status: None,
            provider_request_id: response.request_id().map(str::to_owned),
            usage: response.usage().cloned(),
            error: None,
        });
    }

    pub(crate) fn finish_error(&self, error: &ModelError) {
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        self.finish_last_attempt_error(error);
        let state = if error.is_cancelled() {
            RequestObservationState::Cancelled
        } else {
            RequestObservationState::Failed
        };
        self.emit(RequestObservationEvent {
            scope: RequestObservationScope::Logical,
            state,
            attempt: self.inner.last_attempt.load(Ordering::Acquire),
            response_headers_at_ms: nonzero_timestamp(
                self.inner.response_headers_at_ms.load(Ordering::Acquire),
            ),
            http_status: error.http_status_code(),
            provider_request_id: error.request_id().map(str::to_owned),
            usage: None,
            error: Some((error, error.retry_error_kind().is_some())),
        });
    }

    pub(crate) fn finish_cancelled(&self) {
        if self.inner.finished.swap(true, Ordering::AcqRel) {
            return;
        }
        let error = ModelError::cancelled();
        self.finish_last_attempt_error(&error);
        self.emit(RequestObservationEvent {
            scope: RequestObservationScope::Logical,
            state: RequestObservationState::Cancelled,
            attempt: self.inner.last_attempt.load(Ordering::Acquire),
            response_headers_at_ms: nonzero_timestamp(
                self.inner.response_headers_at_ms.load(Ordering::Acquire),
            ),
            http_status: error.http_status_code(),
            provider_request_id: None,
            usage: None,
            error: Some((&error, false)),
        });
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.inner.finished.load(Ordering::Acquire)
    }

    fn emit(&self, event: RequestObservationEvent<'_>) {
        let Some(observer) = self.inner.observer.as_ref() else {
            return;
        };
        let at_ms = now_ms();
        observer.on_request(self.inner.context.observation(
            event,
            self.inner.max_attempts,
            self.inner.started_at_ms,
            at_ms,
        ));
    }

    pub(crate) fn max_attempts(&self) -> u32 {
        self.inner.max_attempts
    }

    fn finish_last_attempt_error(&self, error: &ModelError) {
        let attempt = self.inner.last_attempt.load(Ordering::Acquire);
        if attempt == 0 {
            return;
        }
        let started_at_ms = match self.inner.attempt_started_at_ms.load(Ordering::Acquire) {
            0 => self.inner.started_at_ms,
            value => value,
        };
        let mut attempt_context = RequestAttemptContext::new(
            self.inner.observer.as_ref(),
            Some(&self.inner.context),
            Some(self),
            attempt,
            started_at_ms,
        );
        attempt_context.set_response_metadata(
            nonzero_timestamp(self.inner.response_headers_at_ms.load(Ordering::Acquire)),
            error.http_status_code(),
            error.request_id(),
        );
        emit_attempt_error(&attempt_context, error, None);
    }

    /// 尝试将物理 attempt 标记为已结束；重复终态（例如 Completed 已交付后
    /// 下游取消导致的补发 Cancelled）会被抑制。
    pub(crate) fn finish_attempt_once(&self, attempt: u32) -> bool {
        if attempt == 0 {
            return false;
        }
        let mut current = self.inner.terminal_attempt.load(Ordering::Acquire);
        loop {
            if current >= attempt {
                return false;
            }
            match self.inner.terminal_attempt.compare_exchange(
                current,
                attempt,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(next) => current = next,
            }
        }
    }
}

/// 将可选的请求 observer、逻辑上下文和生命周期绑定到一次 retry 流。
///
/// 三者要么共同启用，要么共同为空；集中保存可以避免 HTTP/SSE 与 retry
/// 入口在每次调用时重复传递一组容易错配的参数。
#[derive(Clone, Default)]
pub(crate) struct RequestObservationBinding {
    /// 同步接收安全请求观测的 observer。
    observer: Option<Arc<dyn RequestObserver>>,
    /// 当前 logical call 的稳定请求上下文。
    context: Option<RequestObservationContext>,
    /// 当前 logical call 的 finish-once 生命周期。
    lifecycle: Option<RequestLifecycle>,
}

impl RequestObservationBinding {
    /// 组装一组请求观测绑定。
    pub(crate) fn new(
        observer: Option<Arc<dyn RequestObserver>>,
        context: Option<RequestObservationContext>,
        lifecycle: Option<RequestLifecycle>,
    ) -> Self {
        Self {
            observer,
            context,
            lifecycle,
        }
    }

    /// 为 retry 循环创建一个物理 attempt 的观测上下文。
    pub(crate) fn attempt_context(
        &self,
        attempt: u32,
        started_at_ms: u64,
    ) -> RequestAttemptContext<'_> {
        RequestAttemptContext::new(
            self.observer.as_ref(),
            self.context.as_ref(),
            self.lifecycle.as_ref(),
            attempt,
            started_at_ms,
        )
    }
}

/// 一次物理 attempt 的可变请求元数据。
///
/// 该结构只借用 retry 运行期间的观测绑定，并保存当前 response metadata，
/// 供取消、失败和完成分支共享同一套发射逻辑。
pub(crate) struct RequestAttemptContext<'a> {
    /// 可选的同步请求 observer。
    observer: Option<&'a Arc<dyn RequestObserver>>,
    /// 可选的逻辑请求上下文。
    context: Option<&'a RequestObservationContext>,
    /// 可选的逻辑生命周期。
    lifecycle: Option<&'a RequestLifecycle>,
    /// 从 1 开始的物理 attempt 编号。
    attempt: u32,
    /// attempt 开始时间。
    started_at_ms: u64,
    /// 收到 response headers 的时间。
    response_headers_at_ms: Option<u64>,
    /// response HTTP 状态码。
    http_status: Option<u16>,
    /// provider 返回的 request id。
    provider_request_id: Option<String>,
}

impl<'a> RequestAttemptContext<'a> {
    /// 创建尚未收到 response metadata 的物理 attempt 上下文。
    fn new(
        observer: Option<&'a Arc<dyn RequestObserver>>,
        context: Option<&'a RequestObservationContext>,
        lifecycle: Option<&'a RequestLifecycle>,
        attempt: u32,
        started_at_ms: u64,
    ) -> Self {
        Self {
            observer,
            context,
            lifecycle,
            attempt,
            started_at_ms,
            response_headers_at_ms: None,
            http_status: None,
            provider_request_id: None,
        }
    }

    /// 记录 response metadata，并同步更新逻辑生命周期的首个 headers 时间。
    pub(crate) fn set_response_metadata(
        &mut self,
        response_headers_at_ms: Option<u64>,
        http_status: Option<u16>,
        provider_request_id: Option<&str>,
    ) {
        if let (Some(lifecycle), Some(at_ms)) = (self.lifecycle, response_headers_at_ms) {
            lifecycle.set_response_headers(at_ms);
        }
        self.response_headers_at_ms = response_headers_at_ms;
        self.http_status = http_status;
        // 先完成 headers 时间写入，再复制 request id；这样大 request id 的分配不会
        // 扩大 logical cancellation 观察不到 headers 时间的窗口。
        self.provider_request_id = provider_request_id.map(str::to_owned);
    }

    /// 返回当前 attempt 保存的 provider request id，供流结束错误构造使用。
    pub(crate) fn provider_request_id(&self) -> Option<&str> {
        self.provider_request_id.as_deref()
    }

    /// 发射当前 attempt 的一个终态观测。
    fn emit(
        &self,
        state: RequestObservationState,
        response_headers_at_ms: Option<u64>,
        http_status: Option<u16>,
        provider_request_id: Option<String>,
        usage: Option<TokenUsage>,
        error: Option<(&ModelError, bool)>,
    ) {
        let (Some(observer), Some(context), Some(lifecycle)) =
            (self.observer, self.context, self.lifecycle)
        else {
            return;
        };
        let http_status = http_status
            .or(self.http_status)
            .or_else(|| error.and_then(|(value, _)| value.http_status_code()));
        observer.on_request(context.observation(
            RequestObservationEvent {
                scope: RequestObservationScope::Attempt,
                state,
                attempt: self.attempt,
                response_headers_at_ms: response_headers_at_ms.or(self.response_headers_at_ms),
                http_status,
                provider_request_id,
                usage,
                error,
            },
            lifecycle.max_attempts(),
            self.started_at_ms,
            now_ms(),
        ));
    }
}

fn nonzero_timestamp(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

/// 为 provider 入口启动 logical call。
pub(crate) fn start_logical_request(
    runtime: &crate::ModelRuntimeConfig,
    context: RequestObservationContext,
) -> RequestLifecycle {
    RequestLifecycle::start(
        runtime.request_observer(),
        context,
        runtime.retry().max_attempts(),
    )
}

/// 发出一次物理模型请求开始观测，并同步更新请求生命周期状态。
pub(crate) fn emit_attempt_started(attempt_context: &mut RequestAttemptContext<'_>) {
    let at_ms = now_ms();
    let Some(lifecycle) = attempt_context.lifecycle else {
        return;
    };
    lifecycle.set_attempt(attempt_context.attempt, at_ms);
    let Some(observer) = attempt_context.observer else {
        return;
    };
    let Some(context) = attempt_context.context else {
        return;
    };
    observer.on_request(context.observation(
        RequestObservationEvent {
            scope: RequestObservationScope::Attempt,
            state: RequestObservationState::Started,
            attempt: attempt_context.attempt,
            response_headers_at_ms: None,
            http_status: None,
            provider_request_id: None,
            usage: None,
            error: None,
        },
        lifecycle.max_attempts(),
        at_ms,
        at_ms,
    ));
}

/// 发出一次物理模型请求成功观测，合并响应与流式兜底用量。
pub(crate) fn emit_attempt_ok(
    attempt_context: &RequestAttemptContext<'_>,
    response_headers_at_ms: u64,
    http_status: u16,
    response: &ModelResponse,
    fallback_usage: Option<TokenUsage>,
) {
    let Some(lifecycle) = attempt_context.lifecycle else {
        return;
    };
    if !lifecycle.finish_attempt_once(attempt_context.attempt) {
        return;
    }
    attempt_context.emit(
        RequestObservationState::Completed,
        Some(response_headers_at_ms),
        Some(http_status),
        response.request_id().map(str::to_owned),
        response.usage().cloned().or(fallback_usage),
        None,
    );
}

/// 发出一次物理模型请求失败观测，并保证同一 attempt 只产生一个终态。
pub(crate) fn emit_attempt_error(
    attempt_context: &RequestAttemptContext<'_>,
    error: &ModelError,
    usage: Option<TokenUsage>,
) {
    let Some(lifecycle) = attempt_context.lifecycle else {
        return;
    };
    if !lifecycle.finish_attempt_once(attempt_context.attempt) {
        return;
    }
    let state = if error.is_cancelled() {
        RequestObservationState::Cancelled
    } else {
        RequestObservationState::Failed
    };
    attempt_context.emit(
        state,
        None,
        None,
        attempt_context
            .provider_request_id
            .clone()
            .or_else(|| error.request_id().map(str::to_owned)),
        usage,
        // 物理 attempt 记录原始失败原因。只有 logical 终态收到 retry-exhausted
        // 错误时，才把最后一个 attempt 通过 logical correction 标成耗尽。
        Some((error, false)),
    );
}

pub(crate) fn error_projection(
    error: &ModelError,
    retry_exhausted: bool,
) -> (Option<RequestErrorKind>, Option<String>) {
    if error.is_cancelled() {
        return (
            Some(RequestErrorKind::Cancelled),
            Some("request cancelled".to_owned()),
        );
    }
    if retry_exhausted || error.retry_error_kind().is_some() {
        return (
            Some(RequestErrorKind::RetryExhausted),
            Some("retry exhausted".to_owned()),
        );
    }
    if let Some(kind) = error.transport_kind() {
        let (kind, summary) = match kind {
            crate::TransportErrorKind::Connection => {
                (RequestErrorKind::Connection, "connection error")
            }
            crate::TransportErrorKind::Timeout => (RequestErrorKind::Timeout, "timeout"),
            crate::TransportErrorKind::Tls => (RequestErrorKind::Tls, "TLS error"),
            crate::TransportErrorKind::Other => (RequestErrorKind::Transport, "transport error"),
        };
        return (Some(kind), Some(summary.to_owned()));
    }
    if let Some(status) = error.http_status_code() {
        return (
            Some(RequestErrorKind::HttpStatus),
            Some(format!("HTTP {status}")),
        );
    }
    if error.is_stream_interrupted() {
        return (
            Some(RequestErrorKind::StreamInterrupted),
            Some("stream interrupted".to_owned()),
        );
    }
    if error.protocol_error().is_some() {
        return (
            Some(RequestErrorKind::Protocol),
            Some("protocol error".to_owned()),
        );
    }
    (
        Some(RequestErrorKind::Other),
        Some("provider request failed".to_owned()),
    )
}

pub(crate) fn sanitize_endpoint(endpoint: &Url) -> String {
    let Some(host) = endpoint.host_str() else {
        return format!("{}://[invalid]", endpoint.scheme());
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = endpoint
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    format!("{}://{host}{port}", endpoint.scheme())
}

/// 只保留常见 provider request id 的安全字符，避免不可信响应 header 或 SSE id
/// 被当作可持久化元数据原样传播。
fn sanitize_provider_request_id(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return None;
    }
    Some(value.to_owned())
}

fn next_logical_request_id() -> String {
    use std::sync::atomic::AtomicU64;
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    format!(
        "model-{}-{}",
        now_ms(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{
        emit_attempt_error, emit_attempt_ok, emit_attempt_started, sanitize_endpoint,
        sanitize_provider_request_id, RequestLifecycle, RequestObservation,
        RequestObservationBinding, RequestObservationContext, RequestObservationScope,
        RequestObservationState, RequestObserver,
    };
    use crate::{
        ModelError, ModelMessage, ModelRequest, ModelResponse, ProviderProtocol, StopReason,
    };
    use url::Url;

    #[test]
    fn endpoint_projection_keeps_only_origin() {
        let url =
            Url::parse("https://user:secret@example.test:8443/v1/messages?api_key=x#fragment")
                .unwrap();
        assert_eq!(sanitize_endpoint(&url), "https://example.test:8443");
    }

    #[test]
    fn provider_request_id_projection_rejects_untrusted_values() {
        assert_eq!(
            sanitize_provider_request_id("chatcmpl-123".into()).as_deref(),
            Some("chatcmpl-123")
        );
        assert_eq!(sanitize_provider_request_id("api_key=secret".into()), None);
        assert_eq!(sanitize_provider_request_id("x".repeat(129)), None);
    }

    #[test]
    fn attempt_terminal_is_emitted_once_after_completed_delivery() {
        let observed = Arc::new(Mutex::new(Vec::<RequestObservation>::new()));
        let observer: Arc<dyn RequestObserver> = {
            let observed = Arc::clone(&observed);
            Arc::new(move |observation: RequestObservation| {
                observed.lock().expect("observation lock").push(observation);
            })
        };
        let request = ModelRequest::new(vec![ModelMessage::user_text("go")]);
        let context = RequestObservationContext::from_request(
            ProviderProtocol::OpenAiCompatible,
            "gpt-test",
            &Url::parse("https://user:password@example.test/v1?api_key=secret").unwrap(),
            &request,
        );
        let lifecycle = RequestLifecycle::start(Some(observer.clone()), context.clone(), 1);
        let binding = RequestObservationBinding::new(
            Some(observer.clone()),
            Some(context.clone()),
            Some(lifecycle.clone()),
        );
        let mut attempt_context = binding.attempt_context(1, 100);
        emit_attempt_started(&mut attempt_context);
        let response = ModelResponse::new(
            ModelMessage::assistant_text("ok"),
            StopReason::EndTurn,
            None,
            Some("request-1".to_owned()),
        )
        .expect("assistant response");
        attempt_context.set_response_metadata(Some(110), Some(200), Some("request-1"));
        emit_attempt_ok(&attempt_context, 110, 200, &response, None);
        // 下游在 Completed 已发出后取消时，retry runtime 可能走到补发错误分支；
        // 同一物理 attempt 不得再生成第二条 terminal 观测。
        emit_attempt_error(&attempt_context, &ModelError::cancelled(), None);
        let observed = observed.lock().expect("observation lock");
        assert_eq!(
            observed
                .iter()
                .map(|event| (event.scope, event.state, event.attempt))
                .collect::<Vec<_>>(),
            vec![
                (
                    RequestObservationScope::Logical,
                    RequestObservationState::Started,
                    0,
                ),
                (
                    RequestObservationScope::Attempt,
                    RequestObservationState::Started,
                    1,
                ),
                (
                    RequestObservationScope::Attempt,
                    RequestObservationState::Completed,
                    1,
                ),
            ]
        );
        assert_eq!(observed[2].response_headers_at_ms, Some(110));
        assert_eq!(observed[2].http_status, Some(200));
        assert_eq!(
            observed[2].provider_request_id.as_deref(),
            Some("request-1")
        );
    }

    #[test]
    fn logical_cancellation_closes_an_in_flight_attempt_before_logical_terminal() {
        let observed = Arc::new(Mutex::new(Vec::<RequestObservation>::new()));
        let observer: Arc<dyn RequestObserver> = {
            let observed = Arc::clone(&observed);
            Arc::new(move |observation: RequestObservation| {
                observed.lock().expect("observation lock").push(observation);
            })
        };
        let request = ModelRequest::new(vec![ModelMessage::user_text("go")]);
        let context = RequestObservationContext::from_request(
            ProviderProtocol::OpenAiCompatible,
            "gpt-test",
            &Url::parse("https://example.test/v1").unwrap(),
            &request,
        );
        let lifecycle = RequestLifecycle::start(Some(observer.clone()), context.clone(), 1);
        let binding = RequestObservationBinding::new(
            Some(observer.clone()),
            Some(context.clone()),
            Some(lifecycle.clone()),
        );
        let mut attempt_context = binding.attempt_context(1, 100);
        emit_attempt_started(&mut attempt_context);
        lifecycle.finish_cancelled();
        // A late retry-task cancellation must be idempotent.
        emit_attempt_error(&attempt_context, &ModelError::cancelled(), None);
        let observed = observed.lock().expect("observation lock");
        assert_eq!(
            observed
                .iter()
                .map(|event| (event.scope, event.state, event.attempt))
                .collect::<Vec<_>>(),
            vec![
                (
                    RequestObservationScope::Logical,
                    RequestObservationState::Started,
                    0,
                ),
                (
                    RequestObservationScope::Attempt,
                    RequestObservationState::Started,
                    1,
                ),
                (
                    RequestObservationScope::Attempt,
                    RequestObservationState::Cancelled,
                    1,
                ),
                (
                    RequestObservationScope::Logical,
                    RequestObservationState::Cancelled,
                    1,
                ),
            ]
        );
    }
}
