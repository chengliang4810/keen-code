use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use futures::{stream, Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpRequest, HttpResponse, HttpTransport, SseEvent, SseParser};
use crate::{
    ModelError, ModelResult, ModelRuntimeConfig, ModelStream, ModelStreamEvent, RetryConfig,
    RetryObserver,
};

use super::observe::{now_ms, RequestLifecycle, RequestObservationContext, RequestObserver};
use super::retry::{retrying_stream_with_request_observer, StreamAttempt};

/// Provider decoder 使用的 crate-private SSE 事件转换器。
pub(crate) type SseDecoder =
    Arc<dyn Fn(SseEvent, Option<String>) -> ModelResult<Vec<ModelStreamEvent>> + Send + Sync>;
pub(crate) type SseCompletionDecoder =
    Arc<dyn Fn() -> ModelResult<Vec<ModelStreamEvent>> + Send + Sync>;
pub(crate) type SseDecoders = (SseDecoder, SseCompletionDecoder);
pub(crate) type SseDecoderFactory = Arc<dyn Fn() -> SseDecoders + Send + Sync>;
type DecoderFuture = Pin<Box<dyn Future<Output = ModelResult<Vec<ModelStreamEvent>>> + Send>>;
pub(crate) type AsyncSseDecoder =
    Arc<dyn Fn(SseEvent, CancellationToken) -> DecoderFuture + Send + Sync>;

/// 使用独立 child token 包装事件流，确保外部取消与 `abort()` 都能终止消费，且不会取消父 token。
pub(crate) fn cancellable_stream<S>(events: S, cancellation: CancellationToken) -> ModelStream
where
    S: Stream<Item = ModelResult<ModelStreamEvent>> + Send + 'static,
{
    ModelStream::with_cancellation(events, cancellation.child_token())
}

/// 将 crate-private HTTP seam、SSE parser、provider decoder 与通用 retry 串为一条可取消的流。
///
/// request factory 与 decoder 由后续 provider adapter 提供；本函数不认识 provider-native payload。
pub(crate) fn retrying_http_sse_stream(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
) -> ModelStream {
    retrying_http_sse_stream_with_request_observer(
        config,
        cancellation,
        observer,
        None,
        None,
        None,
        transport,
        request,
        provider,
        decoders,
    )
}

pub(crate) fn retrying_http_sse_stream_with_request_observer(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    request_observer: Option<Arc<dyn RequestObserver>>,
    request_context: Option<RequestObservationContext>,
    request_lifecycle: Option<RequestLifecycle>,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
) -> ModelStream {
    let attempt: StreamAttempt = Arc::new(move |attempt_cancellation| {
        let transport = transport.clone();
        let request = request.clone();
        let provider = provider.clone();
        let (decoder, completion_decoder) = decoders();
        Box::pin(async move {
            let request = request()?;
            let response = transport
                .send(request, attempt_cancellation.clone())
                .await?;
            response_to_sse_stream(
                response,
                attempt_cancellation,
                provider,
                decoder,
                completion_decoder,
            )
            .await
        })
    });
    retrying_stream_with_request_observer(
        config,
        cancellation,
        observer,
        request_observer,
        request_context,
        request_lifecycle,
        attempt,
    )
}

/// 使用运行时配置构造 HTTP/SSE/retry 链路，确保上层注册的安全 observer 生效。
pub(crate) fn runtime_http_sse_stream(
    runtime: &ModelRuntimeConfig,
    cancellation: CancellationToken,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
) -> ModelStream {
    retrying_http_sse_stream(
        runtime.retry().clone(),
        cancellation,
        runtime.retry_observer(),
        transport,
        request,
        provider,
        decoders,
    )
}

/// 带 logical call 上下文的 HTTP/SSE/retry 链路；provider adapter 应使用此入口。
pub(crate) fn runtime_http_sse_stream_with_lifecycle(
    runtime: &ModelRuntimeConfig,
    cancellation: CancellationToken,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
    request_context: RequestObservationContext,
    request_lifecycle: RequestLifecycle,
) -> ModelStream {
    retrying_http_sse_stream_with_request_observer(
        runtime.retry().clone(),
        cancellation,
        runtime.retry_observer(),
        runtime.request_observer(),
        Some(request_context),
        Some(request_lifecycle.clone()),
        transport,
        request,
        provider,
        decoders,
    )
    .attach_request_lifecycle(request_lifecycle)
}

/// 为测试或不需要 provider 预建 logical lifecycle 的调用方启动完整观测链路。
pub(crate) fn runtime_http_sse_stream_with_context(
    runtime: &ModelRuntimeConfig,
    cancellation: CancellationToken,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoders: SseDecoderFactory,
    request_context: RequestObservationContext,
) -> ModelStream {
    let lifecycle = RequestLifecycle::start(
        runtime.request_observer(),
        request_context.clone(),
        runtime.retry().max_attempts(),
    );
    runtime_http_sse_stream_with_lifecycle(
        runtime,
        cancellation,
        transport,
        request,
        provider,
        decoders,
        request_context,
        lifecycle,
    )
}

/// 将 HTTP seam、SSE parser、可取消的 provider decoder 与 retry 串为一条通用内部链路。
pub(crate) fn retrying_http_sse_stream_async(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoder: AsyncSseDecoder,
) -> ModelStream {
    retrying_http_sse_stream_async_with_request_observer(
        config,
        cancellation,
        observer,
        None,
        None,
        None,
        transport,
        request,
        provider,
        decoder,
    )
}

pub(crate) fn retrying_http_sse_stream_async_with_request_observer(
    config: RetryConfig,
    cancellation: CancellationToken,
    observer: Option<Arc<dyn RetryObserver>>,
    request_observer: Option<Arc<dyn RequestObserver>>,
    request_context: Option<RequestObservationContext>,
    request_lifecycle: Option<RequestLifecycle>,
    transport: Arc<dyn HttpTransport>,
    request: Arc<dyn Fn() -> ModelResult<HttpRequest> + Send + Sync>,
    provider: Arc<str>,
    decoder: AsyncSseDecoder,
) -> ModelStream {
    let attempt: StreamAttempt = Arc::new(move |attempt_cancellation| {
        let transport = transport.clone();
        let request = request.clone();
        let provider = provider.clone();
        let decoder = decoder.clone();
        Box::pin(async move {
            let request = request()?;
            let response = transport
                .send(request, attempt_cancellation.clone())
                .await?;
            response_to_async_sse_stream(response, attempt_cancellation, provider, decoder).await
        })
    });
    retrying_stream_with_request_observer(
        config,
        cancellation,
        observer,
        request_observer,
        request_context,
        request_lifecycle,
        attempt,
    )
}

async fn response_to_async_sse_stream(
    mut response: HttpResponse,
    cancellation: CancellationToken,
    provider: Arc<str>,
    decoder: AsyncSseDecoder,
) -> ModelResult<ModelStream> {
    let response_headers_at_ms = now_ms();
    let response_status = response.status;
    let response_request_id = response.request_id.clone();
    if !(200..=299).contains(&response.status) {
        let message = read_provider_error_message(&mut response.body, &cancellation).await;
        let error = ModelError::http_status_with_message(
            response.status,
            provider.as_ref(),
            response_request_id.as_deref(),
            message.as_deref(),
        );
        return Ok(ModelStream::with_cancellation(
            stream::once(async move { Err(error) }),
            cancellation.child_token(),
        )
        .with_response_metadata(
            response_headers_at_ms,
            response_status,
            response_request_id,
        ));
    }

    let stream_provider = provider.clone();
    let events = futures::stream::unfold(
        AsyncSseReadState {
            body: response.body,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            pending_provider_events: VecDeque::new(),
            cancellation: cancellation.clone(),
            decoder,
            done: false,
            first_provider_event_emitted: false,
        },
        move |mut state| {
            let provider = stream_provider.clone();
            async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        return Some((Ok(event), state));
                    }
                    // 每次只解码一个 provider frame。这样同一网络 chunk 内的后续
                    // reasoning frame 不会在首个增量交付前被批量解码/缓冲。
                    if let Some(event) = state.pending_provider_events.pop_front() {
                        let decoded = tokio::select! {
                            biased;
                            _ = state.cancellation.cancelled() => return Some((Err(ModelError::cancelled()), state)),
                            decoded = (state.decoder)(event, state.cancellation.clone()) => decoded,
                        };
                        match decoded {
                            Ok(events) => state.pending.extend(events),
                            Err(error) => return Some((Err(error), state)),
                        }
                        continue;
                    }
                    if state.done {
                        return None;
                    }
                    let chunk = tokio::select! {
                        biased;
                        _ = state.cancellation.cancelled() => return Some((Err(ModelError::cancelled()), state)),
                        chunk = state.body.next() => chunk,
                    };
                    match chunk {
                        Some(Ok(bytes)) => {
                            let parsed = match state.parser.push(&bytes) {
                                Ok(events) => events,
                                Err(error) => return Some((Err(error), state)),
                            };
                            state.pending_provider_events.extend(parsed);
                            let provider_event_observed = state.parser.take_event_observed();
                            state.done = state.parser.is_done();
                            if provider_event_observed && !state.first_provider_event_emitted {
                                state.first_provider_event_emitted = true;
                                return Some((Ok(provider_event_now()), state));
                            }
                        }
                        Some(Err(error)) => return Some((Err(error), state)),
                        None => {
                            return Some((
                                Err(ModelError::transport(
                                    crate::TransportErrorKind::Other,
                                    Some(provider.as_ref()),
                                )),
                                state,
                            ));
                        }
                    }
                }
            }
        },
    );
    Ok(
        ModelStream::with_cancellation(events, cancellation.child_token()).with_response_metadata(
            response_headers_at_ms,
            response_status,
            response_request_id,
        ),
    )
}

async fn response_to_sse_stream(
    mut response: HttpResponse,
    cancellation: CancellationToken,
    provider: Arc<str>,
    decoder: SseDecoder,
    completion_decoder: SseCompletionDecoder,
) -> ModelResult<ModelStream> {
    let response_headers_at_ms = now_ms();
    let response_status = response.status;
    let response_request_id = response.request_id.clone();
    if !(200..=299).contains(&response.status) {
        let message = read_provider_error_message(&mut response.body, &cancellation).await;
        let error = ModelError::http_status_with_message(
            response.status,
            provider.as_ref(),
            response_request_id.as_deref(),
            message.as_deref(),
        );
        return Ok(ModelStream::with_cancellation(
            stream::once(async move { Err(error) }),
            cancellation.child_token(),
        )
        .with_response_metadata(
            response_headers_at_ms,
            response_status,
            response_request_id,
        ));
    }

    let stream_provider = provider.clone();
    let events = futures::stream::unfold(
        SseReadState {
            body: response.body,
            parser: SseParser::new(),
            pending: VecDeque::new(),
            pending_provider_events: VecDeque::new(),
            cancellation: cancellation.clone(),
            decoder,
            completion_decoder,
            request_id: response.request_id,
            done: false,
            completion_decoded: false,
            first_provider_event_emitted: false,
        },
        move |mut state| {
            let provider = stream_provider.clone();
            async move {
                loop {
                    if let Some(event) = state.pending.pop_front() {
                        return Some((Ok(event), state));
                    }
                    if let Some(event) = state.pending_provider_events.pop_front() {
                        match (state.decoder)(event, state.request_id.clone()) {
                            Ok(events) => state.pending.extend(events),
                            Err(error) => return Some((Err(error), state)),
                        }
                        continue;
                    }
                    if state.done {
                        if state.completion_decoded {
                            return None;
                        }
                        state.completion_decoded = true;
                        match (state.completion_decoder)() {
                            Ok(events) => state.pending.extend(events),
                            Err(error) => return Some((Err(error), state)),
                        }
                        continue;
                    }
                    let chunk = tokio::select! {
                        biased;
                        _ = state.cancellation.cancelled() => return Some((Err(ModelError::cancelled()), state)),
                        chunk = state.body.next() => chunk,
                    };
                    match chunk {
                        Some(Ok(bytes)) => {
                            let parsed = match state.parser.push(&bytes) {
                                Ok(events) => events,
                                Err(error) => return Some((Err(error), state)),
                            };
                            state.pending_provider_events.extend(parsed);
                            let provider_event_observed = state.parser.take_event_observed();
                            if state.parser.is_done() {
                                state.done = true;
                            }
                            if provider_event_observed && !state.first_provider_event_emitted {
                                state.first_provider_event_emitted = true;
                                return Some((Ok(provider_event_now()), state));
                            }
                        }
                        Some(Err(error)) => return Some((Err(error), state)),
                        None => {
                            return Some((
                                Err(ModelError::transport(
                                    crate::TransportErrorKind::Other,
                                    Some(provider.as_ref()),
                                )),
                                state,
                            ));
                        }
                    }
                }
            }
        },
    );
    Ok(
        ModelStream::with_cancellation(events, cancellation.child_token()).with_response_metadata(
            response_headers_at_ms,
            response_status,
            response_request_id,
        ),
    )
}

/// 在公共 parser 完成首个 provider frame 的同一调度点取时间，避免上层用
/// 内容分片或前端收取时间伪装首 SSE 边界。
fn provider_event_now() -> ModelStreamEvent {
    let at_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    ModelStreamEvent::ProviderEvent { at_ms }
}

/// 非成功响应允许读取的最大正文大小，避免为错误页分配无界内存。
const MAX_PROVIDER_ERROR_BODY_BYTES: usize = 64 * 1024;

/// 从非成功响应中提取供应商的短错误说明，不保留原始正文。
async fn read_provider_error_message(
    body: &mut crate::transport::HttpBody,
    cancellation: &CancellationToken,
) -> Option<String> {
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return None,
            chunk = body.next() => chunk,
        };
        match chunk {
            Some(Ok(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > MAX_PROVIDER_ERROR_BODY_BYTES {
                    return None;
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(Err(_)) => return None,
            None => break,
        }
    }

    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let message = [
        value.pointer("/error/message"),
        value.pointer("/response/error/message"),
        value.get("message"),
        value.get("detail"),
    ]
    .into_iter()
    .flatten()
    .find_map(serde_json::Value::as_str)
    .map(str::to_owned);
    message
}

struct AsyncSseReadState {
    body: crate::transport::HttpBody,
    parser: SseParser,
    pending: VecDeque<ModelStreamEvent>,
    pending_provider_events: VecDeque<SseEvent>,
    cancellation: CancellationToken,
    decoder: AsyncSseDecoder,
    done: bool,
    first_provider_event_emitted: bool,
}

struct SseReadState {
    body: crate::transport::HttpBody,
    parser: SseParser,
    pending: VecDeque<ModelStreamEvent>,
    pending_provider_events: VecDeque<SseEvent>,
    cancellation: CancellationToken,
    decoder: SseDecoder,
    completion_decoder: SseCompletionDecoder,
    request_id: Option<String>,
    done: bool,
    completion_decoded: bool,
    first_provider_event_emitted: bool,
}

#[cfg(test)]
#[path = "stream_test.rs"]
mod stream_test;
