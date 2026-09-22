use std::sync::{Arc, Mutex, MutexGuard};

use keencode_model::{ModelError, ModelRequest, ModelResponse, ProviderProtocol};
use serde_json::Value;

/// 单次线上交换最多保留的响应正文证据字节数。
const MAX_CAPTURED_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// 一次不包含认证 Header 的 Provider 线级请求与响应证据。
#[derive(Clone)]
pub struct WireExchange {
    /// Adapter 编码前收到的完整 Provider 中立请求快照。
    pub model_request: ModelRequest,
    /// 本次响应解析实际采用的单事件字节上限。
    pub max_event_bytes: usize,
    /// Adapter 实际编码并发送的 JSON 请求体。
    pub request_body: Value,
    /// 收到响应头时记录的 HTTP 状态。
    pub response_status: Option<u16>,
    /// 收到响应头时记录的媒体类型。
    pub response_content_type: Option<String>,
    /// 在安全上限内捕获的原始 JSON 或 SSE 正文字节。
    pub response_body: Vec<u8>,
    /// 正文是否超过证据上限而被截断。
    pub response_body_truncated: bool,
    /// 是否由 HTTP 传输层明确观察到远端响应正文结束；解析成功不能替代 EOF 事实。
    pub response_body_eof_observed: bool,
    /// 调用在返回完整响应前形成的统一终态错误；本地丢弃仍在途调用时为空。
    pub terminal_error: Option<ModelError>,
}

impl std::fmt::Debug for WireExchange {
    /// 调试输出只展示固定元数据，绝不展示请求、响应正文或不可信媒体类型。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WireExchange")
            .field("model_request", &"<redacted>")
            .field("max_event_bytes", &self.max_event_bytes)
            .field("request_body", &"<redacted>")
            .field("response_status", &self.response_status)
            .field("response_content_type", &"<redacted>")
            .field("response_body_bytes", &self.response_body.len())
            .field("response_body_truncated", &self.response_body_truncated)
            .field(
                "response_body_eof_observed",
                &self.response_body_eof_observed,
            )
            .field(
                "terminal_error",
                &self.terminal_error.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// 仅在显式启用时收集无认证 Header 的 Provider 线级证据。
#[derive(Clone, Default)]
pub struct WireTraceCollector {
    /// 可被异步响应流安全追加的交换列表。
    exchanges: Arc<Mutex<Vec<WireExchange>>>,
}

impl std::fmt::Debug for WireTraceCollector {
    /// 调试输出仅展示交换数量，避免通过 ProviderClient 的派生输出泄露正文。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WireTraceCollector")
            .field("exchange_count", &self.lock().len())
            .finish()
    }
}

impl WireTraceCollector {
    /// 返回当前已经收集完成或仍在追加的交换快照。
    pub fn exchanges(&self) -> Vec<WireExchange> {
        self.lock().clone()
    }

    /// 为统一请求及其已经编码的线级正文创建响应捕获槽位。
    pub(crate) fn begin(
        &self,
        model_request: ModelRequest,
        max_event_bytes: usize,
        request_body: Value,
    ) -> WireTraceSink {
        let mut exchanges = self.lock();
        let index = exchanges.len();
        exchanges.push(WireExchange {
            model_request,
            max_event_bytes,
            request_body,
            response_status: None,
            response_content_type: None,
            response_body: Vec::new(),
            response_body_truncated: false,
            response_body_eof_observed: false,
            terminal_error: None,
        });
        WireTraceSink {
            collector: self.clone(),
            index,
        }
    }

    /// 即使前一次测试 panic 造成锁中毒，也只恢复证据容器而不终止运行。
    fn lock(&self) -> MutexGuard<'_, Vec<WireExchange>> {
        self.exchanges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// 一个正在接收响应头和正文的线级捕获槽位。
#[derive(Clone, Debug)]
pub(crate) struct WireTraceSink {
    /// 捕获槽位所属的共享收集器。
    collector: WireTraceCollector,
    /// 当前交换在收集器中的稳定序号。
    index: usize,
}

impl WireTraceSink {
    /// 记录不包含 Header 值的响应状态与媒体类型。
    pub(crate) fn record_response_head(&self, status: u16, content_type: Option<String>) {
        if let Some(exchange) = self.collector.lock().get_mut(self.index) {
            exchange.response_status = Some(status);
            exchange.response_content_type = content_type;
        }
    }

    /// 在固定上限内追加响应原始字节并标记截断状态。
    pub(crate) fn append_response_body(&self, bytes: &[u8]) {
        let mut exchanges = self.collector.lock();
        let Some(exchange) = exchanges.get_mut(self.index) else {
            return;
        };
        let remaining = MAX_CAPTURED_RESPONSE_BYTES.saturating_sub(exchange.response_body.len());
        let copied = remaining.min(bytes.len());
        exchange.response_body.extend_from_slice(&bytes[..copied]);
        if copied < bytes.len() {
            exchange.response_body_truncated = true;
        }
    }

    /// 仅在 HTTP 读取器明确返回正文结束时记录远端 EOF。
    pub(crate) fn record_response_body_eof(&self) {
        if let Some(exchange) = self.collector.lock().get_mut(self.index) {
            exchange.response_body_eof_observed = true;
        }
    }

    /// 保存本次交换在线实际返回的统一错误，供测试器逐交换绑定失败终态。
    pub(crate) fn record_terminal_error(&self, error: &ModelError) {
        if let Some(exchange) = self.collector.lock().get_mut(self.index) {
            exchange.terminal_error = Some(error.clone());
        }
    }
}

/// 在无网络、无凭据条件下用目标 Adapter 重放一份 JSON 或 SSE 响应正文。
pub async fn replay_wire_response(
    protocol: ProviderProtocol,
    content_type: &str,
    body: &[u8],
    max_event_bytes: usize,
) -> Result<ModelResponse, ModelError> {
    crate::http::replay_wire_response(protocol, content_type, body, max_event_bytes).await
}

/// 在无网络、无凭据条件下重新执行非成功 HTTP 响应分类。
pub fn replay_wire_error_response(status: u16, body: &[u8]) -> ModelError {
    crate::http::replay_wire_error_response(status, body)
}

/// 在无网络、无凭据条件下用目标 Adapter 编码一份统一请求。
pub fn encode_wire_request(
    protocol: ProviderProtocol,
    request: &ModelRequest,
    streaming: bool,
) -> Result<Value, ModelError> {
    crate::adapters::Adapter::new(protocol).encode_request(request, streaming)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证线级证据及其收集器的调试输出不会包含请求、响应或媒体类型正文。
    #[test]
    fn debug_不泄露线级正文() {
        let collector = WireTraceCollector::default();
        let request = ModelRequest::new(
            "synthetic-private-model",
            vec![keencode_model::Message::text(
                keencode_model::MessageRole::User,
                "KC_PRIVATE_PROMPT",
            )],
        );
        let sink = collector.begin(
            request,
            1024,
            serde_json::json!({
                "input": "KC_PRIVATE_PROMPT",
                "authorization": "synthetic-private-token"
            }),
        );
        sink.record_response_head(
            200,
            Some("text/event-stream; secret=synthetic-media-secret".to_owned()),
        );
        sink.append_response_body(b"synthetic-response-secret");
        sink.record_response_body_eof();
        let exchange_debug = format!("{:?}", collector.exchanges()[0]);
        let collector_debug = format!("{collector:?}");
        for forbidden in [
            "KC_PRIVATE_PROMPT",
            "synthetic-private-model",
            "synthetic-private-token",
            "synthetic-media-secret",
            "synthetic-response-secret",
        ] {
            assert!(!exchange_debug.contains(forbidden));
            assert!(!collector_debug.contains(forbidden));
        }
        assert!(exchange_debug.contains("response_body_bytes: 25"));
        assert!(exchange_debug.contains("response_body_eof_observed: true"));
        assert!(collector_debug.contains("exchange_count: 1"));
    }
}
