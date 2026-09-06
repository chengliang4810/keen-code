use std::collections::VecDeque;

use futures_util::stream;
use keencode_model::{ModelError, ModelStream, ModelStreamEvent};
#[cfg(feature = "live-test-trace")]
use keencode_model::{ModelResponse, ProviderProtocol, collect_model_stream};
use reqwest::Response;
use reqwest::header::RETRY_AFTER;
use serde_json::Value;

use crate::adapters::Adapter;
use crate::config::ApiKey;
use crate::sse::SseDecoder;
#[cfg(feature = "live-test-trace")]
use crate::trace::WireTraceSink;

/// 把成功 HTTP 响应按媒体类型转换为真实增量或缓冲事件流。
pub(crate) async fn decode_success_response(
    response: Response,
    adapter: Adapter,
    max_event_bytes: usize,
    max_response_bytes: usize,
    #[cfg(feature = "live-test-trace")] trace: Option<WireTraceSink>,
) -> Result<ModelStream, ModelError> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type.contains("text/event-stream") {
        return Ok(stream_sse(
            response,
            adapter,
            max_event_bytes,
            max_response_bytes,
            #[cfg(feature = "live-test-trace")]
            trace,
        ));
    }

    let body = read_limited(
        response,
        max_response_bytes,
        #[cfg(feature = "live-test-trace")]
        trace.as_ref(),
    )
    .await?;
    if looks_like_sse(&body) {
        let events = decode_buffered_sse(&body, adapter, max_event_bytes)?;
        return Ok(Box::pin(stream::iter(events.into_iter().map(Ok))));
    }
    let value: Value = serde_json::from_slice(&body).map_err(|error| ModelError::Protocol {
        message: format!("模型 HTTP 响应不是有效 JSON：{error}"),
    })?;
    let mut adapter = adapter;
    let events = adapter.decode_json(value)?;
    Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
}

/// 读取非成功 HTTP 响应并按状态、错误码和文本归一化。
pub(crate) async fn decode_error_response(
    response: Response,
    api_key: Option<&ApiKey>,
    max_bytes: usize,
    #[cfg(feature = "live-test-trace")] trace: Option<WireTraceSink>,
) -> ModelError {
    let status = response.status().as_u16();
    let retry_after_ms = response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .and_then(|seconds| seconds.checked_mul(1000));
    let body = match read_limited(
        response,
        max_bytes,
        #[cfg(feature = "live-test-trace")]
        trace.as_ref(),
    )
    .await
    {
        Ok(body) => body,
        Err(error) => return error,
    };
    let (message, code) = provider_error_fields(&body);
    let message = safe_error_message(api_key, &message);
    classify_http_error(status, retry_after_ms, message, code.as_deref())
}

/// 按状态、公开错误码和脱敏文本构造 Provider 中立错误。
pub(crate) fn classify_http_error(
    status: u16,
    retry_after_ms: Option<u64>,
    message: String,
    code: Option<&str>,
) -> ModelError {
    let classifier = format!("{} {}", code.unwrap_or_default(), message).to_ascii_lowercase();

    if classifier.contains("context_length")
        || classifier.contains("context length")
        || classifier.contains("context window")
        || classifier.contains("maximum context")
        || classifier.contains("max context")
        || classifier.contains("prompt is too long")
        || classifier.contains("input is too long")
        || classifier.contains("too many tokens")
        || classifier.contains("input")
            && classifier.contains("token")
            && (classifier.contains("exceed") || classifier.contains("maximum"))
        || classifier.contains("input")
            && classifier.contains("context")
            && (classifier.contains("exceed")
                || classifier.contains("maximum")
                || classifier.contains("too_long")
                || classifier.contains("too long"))
        || classifier.contains("上下文") && classifier.contains("超")
    {
        return ModelError::ContextLengthExceeded { message };
    }
    if classifier.contains("quota_exhausted")
        || classifier.contains("insufficient_balance")
        || classifier.contains("insufficient_quota")
        || classifier.contains("余额不足")
        || classifier.contains("套餐次数已用尽")
        || status == 402
    {
        return ModelError::QuotaExceeded {
            message,
            status_code: Some(status),
        };
    }
    if classifier.contains("invalid_api_key")
        || classifier.contains("authentication_error")
        || classifier.contains("unauthorized")
        || classifier.contains("authentication failed")
        || classifier.contains("认证失败")
    {
        return ModelError::Authentication {
            message,
            status_code: Some(status),
        };
    }
    if classifier.contains("permission_denied")
        || classifier.contains("forbidden")
        || classifier.contains("not authorized")
        || classifier.contains("authorization failed")
        || classifier.contains("无权")
        || classifier.contains("未授权")
    {
        return ModelError::Authorization {
            message,
            status_code: Some(status),
        };
    }
    if classifier.contains("rate_limit")
        || classifier.contains("rate limited")
        || classifier.contains("too many requests")
        || classifier.contains("throttled")
        || classifier.contains("请求过于频繁")
    {
        return ModelError::RateLimited {
            message,
            retry_after_ms,
            status_code: Some(status),
        };
    }
    if classifier.contains("model_not_found")
        || classifier.contains("unsupported_model")
        || classifier.contains("model") && classifier.contains("not supported")
        || classifier.contains("模型") && classifier.contains("不支持")
    {
        return ModelError::ModelNotFound {
            message,
            status_code: Some(status),
        };
    }
    if classifier.contains("service_unavailable")
        || classifier.contains("server_error")
        || classifier.contains("temporarily unavailable")
        || classifier.contains("服务不可用")
    {
        return ModelError::ProviderUnavailable {
            message,
            status_code: Some(status),
            retryable: true,
        };
    }

    match status {
        401 => ModelError::Authentication {
            message,
            status_code: Some(status),
        },
        403 => ModelError::Authorization {
            message,
            status_code: Some(status),
        },
        404 | 405 => ModelError::ProtocolUnsupported {
            message,
            status_code: Some(status),
        },
        425 | 429 => ModelError::RateLimited {
            message,
            retry_after_ms,
            status_code: Some(status),
        },
        408 => ModelError::ProviderUnavailable {
            message,
            status_code: Some(status),
            retryable: true,
        },
        400 | 409 | 422 => ModelError::InvalidRequest { message },
        500 | 502 | 503 | 504 => ModelError::ProviderUnavailable {
            message,
            status_code: Some(status),
            retryable: true,
        },
        _ => ModelError::ProviderUnavailable {
            message,
            status_code: Some(status),
            retryable: false,
        },
    }
}

/// 归一化 HTTP 200 正文中携带的 Provider 错误，并仅对已知错误类别升级分类。
///
/// 未知或普通 `invalid_request` 错误仍保留协议错误，避免在没有 HTTP 状态码时臆测
/// 认证、额度或瞬时故障；明确的错误码则沿用同一套 HTTP 分类规则但不伪造状态码。
pub(crate) fn classify_in_band_provider_error(message: &str, code: Option<&str>) -> ModelError {
    let classified = classify_http_error(400, None, message.to_owned(), code);
    match classified {
        ModelError::ContextLengthExceeded { .. }
        | ModelError::Authentication { .. }
        | ModelError::Authorization { .. }
        | ModelError::QuotaExceeded { .. }
        | ModelError::ModelNotFound { .. }
        | ModelError::ProtocolUnsupported { .. }
        | ModelError::RateLimited { .. }
        | ModelError::ProviderUnavailable { .. } => without_in_band_status(classified),
        ModelError::InvalidRequest { message } | ModelError::Protocol { message } => {
            ModelError::Protocol { message }
        }
        other => other,
    }
}

/// 去掉 HTTP 200 错误中虚构的状态码，保留错误类别和重试属性。
fn without_in_band_status(error: ModelError) -> ModelError {
    match error {
        ModelError::Authentication { message, .. } => ModelError::Authentication {
            message,
            status_code: None,
        },
        ModelError::Authorization { message, .. } => ModelError::Authorization {
            message,
            status_code: None,
        },
        ModelError::QuotaExceeded { message, .. } => ModelError::QuotaExceeded {
            message,
            status_code: None,
        },
        ModelError::ModelNotFound { message, .. } => ModelError::ModelNotFound {
            message,
            status_code: None,
        },
        ModelError::ProtocolUnsupported { message, .. } => ModelError::ProtocolUnsupported {
            message,
            status_code: None,
        },
        ModelError::RateLimited { message, .. } => ModelError::RateLimited {
            message,
            retry_after_ms: None,
            status_code: None,
        },
        ModelError::ProviderUnavailable {
            message, retryable, ..
        } => ModelError::ProviderUnavailable {
            message,
            status_code: None,
            retryable,
        },
        other => other,
    }
}

/// 用已经脱敏并持久化的错误正文重新执行当前 HTTP 错误分类器。
#[cfg(feature = "live-test-trace")]
pub(crate) fn replay_wire_error_response(status: u16, body: &[u8]) -> ModelError {
    let (message, code) = provider_error_fields(body);
    classify_http_error(status, None, message, code.as_deref())
}

/// 把 reqwest 错误转换为不含认证信息的传输错误。
pub(crate) fn transport_error(error: reqwest::Error, api_key: Option<&ApiKey>) -> ModelError {
    let retryable = error.is_timeout() || error.is_connect() || error.is_body();
    let error = error.without_url();
    ModelError::Transport {
        message: safe_error_message(api_key, &error.to_string()),
        retryable,
    }
}

/// 对任意归一化错误再次执行当前 Provider 凭据脱敏。
pub(crate) fn redact_model_error(error: ModelError, api_key: Option<&ApiKey>) -> ModelError {
    match error {
        ModelError::Authentication {
            message,
            status_code,
        } => ModelError::Authentication {
            message: safe_error_message(api_key, &message),
            status_code,
        },
        ModelError::Authorization {
            message,
            status_code,
        } => ModelError::Authorization {
            message: safe_error_message(api_key, &message),
            status_code,
        },
        ModelError::QuotaExceeded {
            message,
            status_code,
        } => ModelError::QuotaExceeded {
            message: safe_error_message(api_key, &message),
            status_code,
        },
        ModelError::ModelNotFound {
            message,
            status_code,
        } => ModelError::ModelNotFound {
            message: safe_error_message(api_key, &message),
            status_code,
        },
        ModelError::ProtocolUnsupported {
            message,
            status_code,
        } => ModelError::ProtocolUnsupported {
            message: safe_error_message(api_key, &message),
            status_code,
        },
        ModelError::RateLimited {
            message,
            retry_after_ms,
            status_code,
        } => ModelError::RateLimited {
            message: safe_error_message(api_key, &message),
            retry_after_ms,
            status_code,
        },
        ModelError::ContextLengthExceeded { message } => ModelError::ContextLengthExceeded {
            message: safe_error_message(api_key, &message),
        },
        ModelError::InvalidRequest { message } => ModelError::InvalidRequest {
            message: safe_error_message(api_key, &message),
        },
        ModelError::UnsupportedCapability {
            capability,
            message,
        } => ModelError::UnsupportedCapability {
            capability,
            message: safe_error_message(api_key, &message),
        },
        ModelError::StructuredOutput {
            enforcement,
            failure,
            message,
        } => ModelError::StructuredOutput {
            enforcement,
            failure,
            message: safe_error_message(api_key, &message),
        },
        ModelError::ProviderUnavailable {
            message,
            status_code,
            retryable,
        } => ModelError::ProviderUnavailable {
            message: safe_error_message(api_key, &message),
            status_code,
            retryable,
        },
        ModelError::Transport { message, retryable } => ModelError::Transport {
            message: safe_error_message(api_key, &message),
            retryable,
        },
        ModelError::StreamInterrupted { message, retryable } => ModelError::StreamInterrupted {
            message: safe_error_message(api_key, &message),
            retryable,
        },
        ModelError::Protocol { message } => ModelError::Protocol {
            message: safe_error_message(api_key, &message),
        },
        ModelError::Cancelled { message } => ModelError::Cancelled {
            message: safe_error_message(api_key, &message),
        },
    }
}

/// SSE 增量读取器的完整可恢复状态。
struct SseStreamState {
    response: Response,
    decoder: SseDecoder,
    adapter: Adapter,
    pending: VecDeque<ModelStreamEvent>,
    deferred_error: Option<ModelError>,
    wire_bytes: usize,
    max_response_bytes: usize,
    /// 仅真实兼容性测试启用的线级响应证据捕获槽位。
    #[cfg(feature = "live-test-trace")]
    trace: Option<WireTraceSink>,
    eof: bool,
}

/// 创建随着 HTTP 字节到达而产出模型事件的 SSE 流。
fn stream_sse(
    response: Response,
    adapter: Adapter,
    max_event_bytes: usize,
    max_response_bytes: usize,
    #[cfg(feature = "live-test-trace")] trace: Option<WireTraceSink>,
) -> ModelStream {
    let state = SseStreamState {
        response,
        decoder: SseDecoder::new(max_event_bytes),
        adapter,
        pending: VecDeque::new(),
        deferred_error: None,
        wire_bytes: 0,
        max_response_bytes,
        #[cfg(feature = "live-test-trace")]
        trace,
        eof: false,
    };
    Box::pin(stream::try_unfold(state, |mut state| async move {
        loop {
            if let Some(event) = state.pending.pop_front() {
                return Ok(Some((event, state)));
            }
            if let Some(error) = state.deferred_error.take() {
                return Err(error);
            }
            if state.eof {
                return Ok(None);
            }
            match state.response.chunk().await {
                Ok(Some(chunk)) => {
                    state.wire_bytes =
                        state.wire_bytes.checked_add(chunk.len()).ok_or_else(|| {
                            ModelError::Protocol {
                                message: "模型流式响应累计长度溢出".to_owned(),
                            }
                        })?;
                    if state.wire_bytes > state.max_response_bytes {
                        return Err(ModelError::Protocol {
                            message: format!(
                                "模型流式响应超过 {} 字节安全上限",
                                state.max_response_bytes
                            ),
                        });
                    }
                    #[cfg(feature = "live-test-trace")]
                    if let Some(trace) = &state.trace {
                        trace.append_response_body(&chunk);
                    }
                    let frames = state.decoder.push(&chunk)?;
                    for frame in frames {
                        state.adapter.consume_sse(frame, &mut state.pending)?;
                    }
                }
                Ok(None) => {
                    #[cfg(feature = "live-test-trace")]
                    if let Some(trace) = &state.trace {
                        trace.record_response_body_eof();
                    }
                    let frames = state.decoder.finish()?;
                    for frame in frames {
                        state.adapter.consume_sse(frame, &mut state.pending)?;
                    }
                    if let Err(error) = state.adapter.finish_stream(&mut state.pending) {
                        state.deferred_error = Some(error);
                    }
                    state.eof = true;
                }
                Err(error) => {
                    return Err(ModelError::Transport {
                        message: error.to_string(),
                        retryable: true,
                    });
                }
            }
        }
    }))
}

/// 把缓冲的 SSE 正文转换为事件序列，用于错误媒体类型的兼容服务。
fn decode_buffered_sse(
    body: &[u8],
    mut adapter: Adapter,
    max_event_bytes: usize,
) -> Result<Vec<ModelStreamEvent>, ModelError> {
    let mut decoder = SseDecoder::new(max_event_bytes);
    let mut pending = VecDeque::new();
    for frame in decoder.push(body)? {
        adapter.consume_sse(frame, &mut pending)?;
    }
    for frame in decoder.finish()? {
        adapter.consume_sse(frame, &mut pending)?;
    }
    adapter.finish_stream(&mut pending)?;
    Ok(pending.into_iter().collect())
}

/// 使用捕获的 UTF-8 JSON 或 SSE 正文离线重放目标协议 Adapter。
#[cfg(feature = "live-test-trace")]
pub(crate) async fn replay_wire_response(
    protocol: ProviderProtocol,
    content_type: &str,
    body: &[u8],
    max_event_bytes: usize,
) -> Result<ModelResponse, ModelError> {
    let adapter = Adapter::new(protocol);
    let events = if content_type
        .to_ascii_lowercase()
        .contains("text/event-stream")
        || looks_like_sse(body)
    {
        decode_buffered_sse(body, adapter, max_event_bytes)?
    } else {
        let value: Value = serde_json::from_slice(body).map_err(|error| ModelError::Protocol {
            message: format!("离线 Fixture 响应不是有效 JSON：{error}"),
        })?;
        let mut adapter = adapter;
        adapter.decode_json(value)?
    };
    let stream: ModelStream = Box::pin(stream::iter(events.into_iter().map(Ok)));
    collect_model_stream(stream).await
}

/// 在内存上限内读取完整 HTTP 正文。
async fn read_limited(
    mut response: Response,
    max_bytes: usize,
    #[cfg(feature = "live-test-trace")] trace: Option<&WireTraceSink>,
) -> Result<Vec<u8>, ModelError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ModelError::Transport {
            message: error.to_string(),
            retryable: true,
        })?
    {
        let next_len = body
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| ModelError::Protocol {
                message: "模型 HTTP 响应长度溢出".to_owned(),
            })?;
        if next_len > max_bytes {
            return Err(ModelError::Protocol {
                message: format!("模型 HTTP 响应超过 {max_bytes} 字节安全上限"),
            });
        }
        #[cfg(feature = "live-test-trace")]
        if let Some(trace) = trace {
            trace.append_response_body(&chunk);
        }
        body.extend_from_slice(&chunk);
    }
    #[cfg(feature = "live-test-trace")]
    if let Some(trace) = trace {
        trace.record_response_body_eof();
    }
    Ok(body)
}

/// 判断缓冲正文是否呈现 SSE 字段边界。
fn looks_like_sse(body: &[u8]) -> bool {
    let body = body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(body);
    body.starts_with(b"data:") || body.starts_with(b"event:") || body.starts_with(b":")
}

/// 从常见错误 JSON 或纯文本正文提取 message 与 code。
fn provider_error_fields(body: &[u8]) -> (String, Option<String>) {
    if let Ok(value) = serde_json::from_slice::<Value>(body) {
        let error = value.get("error").unwrap_or(&value);
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
            .unwrap_or("模型服务返回未说明错误")
            .to_owned();
        let code = error.get("code").and_then(|code| {
            code.as_str()
                .map(ToOwned::to_owned)
                .or_else(|| Some(code.to_string()))
        });
        return (message, code);
    }
    (String::from_utf8_lossy(body).into_owned(), None)
}

/// 移除凭据、控制字符并限制错误文本长度。
fn safe_error_message(api_key: Option<&ApiKey>, message: &str) -> String {
    let redacted = api_key.map_or_else(|| message.to_owned(), |api_key| api_key.redact(message));
    let mut safe = redacted
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(1000)
        .collect::<String>();
    if safe.trim().is_empty() {
        safe = "模型服务返回空错误".to_owned();
    }
    safe
}
