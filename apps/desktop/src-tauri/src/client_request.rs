//! 桌面 Runtime 的标准 ACP Client Request 共享投递与响应路由。

use crate::agent_runtime::{AgentRuntime, SessionDeliverySender};
use keencode_acp::{AcpClientRequestFrame, AcpResponseDecoder, ConnectionId};
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

/// 在每次发送时解析 Session 当前活跃世代的 Client Request 出口。
///
/// 工具表在 Turn 装配时冻结并跨整个 Turn 复用，而 Session 重载会替换投递世代并关闭旧世代。
/// 因此一次性交互工具（例如 AskUser）不能缓存装配时的世代句柄，否则会向已关闭的 FIFO 投递。
/// 这里与实时事件泵保持同一契约：只持有装配根弱引用，发送瞬间再解析当时的世代。
#[derive(Clone)]
pub(crate) struct SessionDeliverySink {
    /// 持有投递世代注册表的装配根；Runtime 已回收时视为不可投递。
    runtime: Weak<AgentRuntime>,
    /// 该出口唯一允许投递的 Session。
    session_id: String,
    /// Client Request 唯一允许送达的 ACP 连接。
    connection_id: ConnectionId,
}

impl SessionDeliverySink {
    /// 使用装配根弱引用和固定 Session 创建出口。
    #[cfg(test)]
    pub(crate) fn new(runtime: Weak<AgentRuntime>, session_id: String) -> Self {
        Self::for_connection(
            runtime,
            session_id,
            ConnectionId::new("embedded-desktop").expect("固定桌面连接标识应合法"),
        )
    }

    /// 使用装配根、Session 和固定目标连接创建出口。
    pub(crate) fn for_connection(
        runtime: Weak<AgentRuntime>,
        session_id: String,
        connection_id: ConnectionId,
    ) -> Self {
        Self {
            runtime,
            session_id,
            connection_id,
        }
    }
}

impl ClientRequestSink for SessionDeliverySink {
    /// 发送前解析当时活跃的投递世代，避免写入已经被替换的旧 FIFO。
    fn send_client_request(&self, request: AcpClientRequestFrame) -> ClientRequestFuture<'_> {
        let runtime = self.runtime.clone();
        let session_id = self.session_id.clone();
        let connection_id = self.connection_id.clone();
        Box::pin(async move {
            let runtime = runtime
                .upgrade()
                .ok_or(ClientRequestBridgeError::DeliveryUnavailable)?;
            let delivery = runtime
                .session_delivery(&session_id)
                .map_err(|_| ClientRequestBridgeError::DeliveryUnavailable)?;
            SessionDeliverySender::send_client_request_to(&delivery, connection_id, request)
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

/// 将响应连同 transport 连接身份交给 Runtime，防止跨连接抢答。
pub(crate) fn route_client_response_from_connection(
    runtime: &AgentRuntime,
    connection_id: &ConnectionId,
    response_json: &str,
) -> Result<(), String> {
    let request_id = route_response_request_id(&AcpResponseDecoder::new(), response_json)
        .map_err(|error| error.to_string())?;
    runtime
        .route_client_response_from_connection(connection_id, &request_id, response_json)
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
