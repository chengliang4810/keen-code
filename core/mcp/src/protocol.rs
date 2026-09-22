//! JSON-RPC 2.0 编解码与严格消息分类。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::McpError;

/// JSON-RPC 请求或响应标识。
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// 有符号整数标识；不接受浮点数。
    Number(i64),
    /// 字符串标识。
    String(String),
}

impl std::fmt::Debug for RequestId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => formatter.debug_tuple("Number").field(value).finish(),
            Self::String(_) => formatter
                .debug_tuple("String")
                .field(&"<redacted>")
                .finish(),
        }
    }
}

/// MCP 服务端返回的 JSON-RPC 错误对象。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// JSON-RPC 或服务端定义的错误码。
    pub code: i64,
    /// 人类可读的错误说明。
    pub message: String,
    /// 可选结构化错误数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// MCP 服务端发出的异步 JSON-RPC 通知。
#[derive(Debug, Clone, PartialEq)]
pub struct McpNotification {
    /// 通知方法名。
    pub method: String,
    /// 通知参数；服务端未发送 params 时为空。
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JsonRpcRequest {
    jsonrpc: &'static str,
    pub(crate) id: RequestId,
    pub(crate) method: String,
    params: Value,
}

impl JsonRpcRequest {
    pub(crate) fn new(id: RequestId, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

impl JsonRpcNotification {
    pub(crate) fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug)]
pub(crate) enum IncomingMessage {
    Response(JsonRpcResponse),
    Notification(McpNotification),
    ServerRequest { id: RequestId, method: String },
}

#[derive(Debug)]
pub(crate) struct JsonRpcResponse {
    pub(crate) id: RequestId,
    outcome: Result<Value, JsonRpcError>,
}

impl JsonRpcResponse {
    pub(crate) fn into_result(self, expected_id: &RequestId) -> Result<Value, McpError> {
        if &self.id != expected_id {
            return Err(McpError::Protocol(format!(
                "JSON-RPC 响应 ID 不匹配：期望 {expected_id:?}，实际 {:?}",
                self.id
            )));
        }
        self.outcome.map_err(|error| McpError::Rpc {
            code: error.code,
            message: error.message,
            data: error.data,
        })
    }
}

pub(crate) fn parse_incoming(bytes: &[u8]) -> Result<IncomingMessage, McpError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| McpError::Protocol(format!("JSON 解析失败：{error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| McpError::Protocol("JSON-RPC 消息必须是对象".to_owned()))?;
    match object.get("jsonrpc") {
        Some(Value::String(version)) if version == "2.0" => {}
        Some(_) => {
            return Err(McpError::Protocol(
                "JSON-RPC jsonrpc 字段必须严格等于 2.0".to_owned(),
            ));
        }
        None => {
            return Err(McpError::Protocol(
                "JSON-RPC 消息缺少 jsonrpc 字段".to_owned(),
            ));
        }
    }

    if let Some(method) = object.get("method") {
        let method = method
            .as_str()
            .filter(|method| !method.is_empty())
            .ok_or_else(|| McpError::Protocol("JSON-RPC method 必须是非空字符串".to_owned()))?
            .to_owned();
        let params = object.get("params").cloned();
        validate_params(params.as_ref())?;
        return if let Some(id) = object.get("id") {
            Ok(IncomingMessage::ServerRequest {
                id: parse_id(id)?,
                method,
            })
        } else {
            Ok(IncomingMessage::Notification(McpNotification {
                method,
                params,
            }))
        };
    }

    let id = object
        .get("id")
        .ok_or_else(|| McpError::Protocol("JSON-RPC 响应缺少 id".to_owned()))
        .and_then(parse_id)?;
    let result = object.get("result");
    let error = object.get("error");
    let outcome = match (result, error) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(parse_error(error)?),
        (Some(_), Some(_)) => {
            return Err(McpError::Protocol(
                "JSON-RPC 响应不能同时包含 result 和 error".to_owned(),
            ));
        }
        (None, None) => {
            return Err(McpError::Protocol(
                "JSON-RPC 响应必须包含 result 或 error".to_owned(),
            ));
        }
    };
    Ok(IncomingMessage::Response(JsonRpcResponse { id, outcome }))
}

/// 为客户端实际支持的服务端请求构造 JSON-RPC 响应。
pub(crate) fn server_request_response(id: RequestId, method: &str) -> Value {
    if method == "ping" {
        serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": "客户端未实现该服务端请求"
            }
        })
    }
}

fn parse_id(value: &Value) -> Result<RequestId, McpError> {
    match value {
        Value::String(value) => Ok(RequestId::String(value.clone())),
        Value::Number(value) => value.as_i64().map(RequestId::Number).ok_or_else(|| {
            McpError::Protocol("JSON-RPC 数字 ID 必须是 i64 范围内的整数".to_owned())
        }),
        _ => Err(McpError::Protocol(
            "JSON-RPC ID 必须是字符串或整数".to_owned(),
        )),
    }
}

fn parse_error(value: &Value) -> Result<JsonRpcError, McpError> {
    serde_json::from_value(value.clone())
        .map_err(|error| McpError::Protocol(format!("JSON-RPC error 对象无效：{error}")))
}

fn validate_params(params: Option<&Value>) -> Result<(), McpError> {
    if params.is_some_and(|value| !value.is_object() && !value.is_array()) {
        return Err(McpError::Protocol(
            "JSON-RPC params 必须是对象或数组".to_owned(),
        ));
    }
    Ok(())
}
