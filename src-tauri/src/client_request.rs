//! 桌面 Runtime 的标准 ACP Client Request 共享投递与响应路由。

use crate::agent_runtime::{AgentRuntime, SessionDeliverySender};
use keencode_acp::{AcpClientRequestFrame, AcpResponseDecoder};
use parking_lot::Mutex;
use serde_json::Value;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 向当前 Session 的唯一 ACP 投递泵发送 Client Request 的异步结果。
pub type ClientRequestFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), ClientRequestBridgeError>> + Send + 'a>>;

/// 标准 Client Request 协调器对 Session 串行投递泵的最小依赖边界。
pub trait ClientRequestSink: Send + Sync {
    /// 将一个完整标准 ACP Client Request 放入当前 Session 的串行投递队列。
    fn send_client_request(&self, request: AcpClientRequestFrame) -> ClientRequestFuture<'_>;
}

impl ClientRequestSink for SessionDeliverySender {
    /// 把请求交给与 Session 更新共享顺序的唯一桌面投递泵。
    fn send_client_request(&self, request: AcpClientRequestFrame) -> ClientRequestFuture<'_> {
        Box::pin(async move {
            SessionDeliverySender::send_client_request(self, request)
                .await
                .map_err(|_| ClientRequestBridgeError::DeliveryUnavailable)
        })
    }
}

/// 每个 Session 只允许一个可见 Client Request 的串行门。
pub(crate) struct ClientRequestDisplayGate {
    /// 仅保存弱引用，使没有待决请求的 Session 不会永久占用 Runtime 内存。
    sessions: Mutex<HashMap<String, Weak<Semaphore>>>,
}

impl ClientRequestDisplayGate {
    /// 创建尚未登记任何 Session 的串行门。
    pub(crate) fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// 按 Tokio FIFO 顺序取得指定 Session 的唯一可见请求许可。
    pub(crate) async fn acquire(&self, session_id: &str) -> Option<ClientRequestDisplayPermit> {
        let semaphore = {
            let mut sessions = self.sessions.lock();
            sessions.retain(|_, semaphore| semaphore.strong_count() > 0);
            match sessions.get(session_id).and_then(Weak::upgrade) {
                Some(semaphore) => semaphore,
                None => {
                    let semaphore = Arc::new(Semaphore::new(1));
                    sessions.insert(session_id.to_owned(), Arc::downgrade(&semaphore));
                    semaphore
                }
            }
        };
        semaphore
            .acquire_owned()
            .await
            .map(|permit| ClientRequestDisplayPermit { _permit: permit })
            .ok()
    }
}

impl Default for ClientRequestDisplayGate {
    /// 创建默认的空串行门。
    fn default() -> Self {
        Self::new()
    }
}

/// 一个待决请求在完成响应或取消前持有的 Session 独占展示许可。
pub(crate) struct ClientRequestDisplayPermit {
    /// 字段仅通过析构释放信号量许可，不暴露手动解锁能力。
    _permit: OwnedSemaphorePermit,
}

/// Client Request 共享投递或宽松路由视图违反边界时的稳定错误。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientRequestBridgeError {
    /// 当前 Session 的 Client Request 无法送达桌面。
    DeliveryUnavailable,
    /// Client 返回的 JSON-RPC 外层不是受支持的响应形状。
    InvalidResponse,
    /// 响应没有可路由的字符串请求标识。
    UnknownRequest,
}

impl fmt::Display for ClientRequestBridgeError {
    /// 输出不包含请求正文或响应载荷的稳定说明。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeliveryUnavailable => formatter.write_str("Client Request 无法送达桌面"),
            Self::InvalidResponse => formatter.write_str("ACP Client Response 无效"),
            Self::UnknownRequest => formatter.write_str("ACP 待决请求不存在或已经结束"),
        }
    }
}

impl Error for ClientRequestBridgeError {}

/// 将一个完整 JSON-RPC Client Response 交给现有 Runtime 响应路由。
///
/// `acp_dispatch` 与保留的内部命令共用该入口，确保响应不会再次进入
/// `AcpRequestDecoder`，并且响应成功、失败信封都保持同一严格路由边界。
pub(crate) fn route_client_response(
    runtime: &AgentRuntime,
    response_json: &str,
) -> Result<(), String> {
    let request_id = route_response_request_id(&AcpResponseDecoder::new(), response_json)
        .map_err(|error| error.to_string())?;
    runtime
        .route_client_response(&request_id, response_json)
        .map_err(|error| error.to_string())
}

/// 从有界宽松路由视图提取字符串 ID，选中路由后仍由对应模块严格解析完整 DTO。
fn route_response_request_id(
    decoder: &AcpResponseDecoder,
    response_json: &str,
) -> Result<String, ClientRequestBridgeError> {
    if response_json.len() > decoder.limits().max_payload_bytes() {
        return Err(ClientRequestBridgeError::InvalidResponse);
    }
    let value = serde_json::from_str::<Value>(response_json)
        .map_err(|_| ClientRequestBridgeError::InvalidResponse)?;
    let object = value
        .as_object()
        .ok_or(ClientRequestBridgeError::InvalidResponse)?;
    let response_id = object
        .get("id")
        .ok_or(ClientRequestBridgeError::InvalidResponse)?;
    let Some(request_id) = response_id.as_str() else {
        return if response_id.is_null() || response_id.is_number() {
            Err(ClientRequestBridgeError::UnknownRequest)
        } else {
            Err(ClientRequestBridgeError::InvalidResponse)
        };
    };
    Ok(request_id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路由视图只接受有界 JSON 对象中的字符串标识。
    #[test]
    fn response_route_requires_string_request_id() {
        let decoder = AcpResponseDecoder::new();
        assert_eq!(
            route_response_request_id(&decoder, r#"{"id":"request-1","result":{}}"#),
            Ok("request-1".to_owned())
        );
        assert_eq!(
            route_response_request_id(&decoder, r#"{"id":1,"result":{}}"#),
            Err(ClientRequestBridgeError::UnknownRequest)
        );
        assert_eq!(
            route_response_request_id(&decoder, r#"{"result":{}}"#),
            Err(ClientRequestBridgeError::InvalidResponse)
        );
    }
}
