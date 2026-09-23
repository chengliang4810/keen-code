//! 三种协议 Adapter（Messages / Chat Completions / Responses）之间逐字共享的
//! 线格式辅助函数。
//!
//! 这里只收纳在多个 Adapter 中逐字重复、或仅差一个协议文案前缀的实现；带协议
//! 分支逻辑的 helper（如各自的 tool_choice 编码、Usage 解码）保留在各自文件中。
//! 错误文案通过前缀或消息参数注入，保证各协议 wire 错误字节级不变。

use keencode_model::{ImageSource, ModelError, ReasoningEffort, ResponseMetadata};
use serde_json::{Map, Value};

use crate::http::classify_in_band_provider_error;

/// 创建统一请求校验错误。
pub(super) fn invalid_request(message: impl Into<String>) -> ModelError {
    ModelError::InvalidRequest {
        message: message.into(),
    }
}

/// 创建统一协议解析错误。
pub(super) fn protocol_error(message: impl Into<String>) -> ModelError {
    ModelError::Protocol {
        message: message.into(),
    }
}

/// 从对象读取必需的字符串字段；`protocol` 注入错误文案的协议名前缀。
pub(super) fn required_str_from_map<'a>(
    value: &'a Map<String, Value>,
    field: &str,
    protocol: &str,
) -> Result<&'a str, ModelError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error(format!("{protocol} 字段 {field} 必须是字符串")))
}

/// 从顶层 JSON 值读取可转换为 u32 的必需整数；`protocol` 注入错误文案前缀。
pub(super) fn required_u32(value: &Value, field: &str, protocol: &str) -> Result<u32, ModelError> {
    let number = value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| protocol_error(format!("{protocol} 字段 {field} 必须是非负整数")))?;
    u32::try_from(number)
        .map_err(|_| protocol_error(format!("{protocol} 字段 {field} 超过 u32 范围")))
}

/// 提取 Provider 错误对象中的安全文本摘要：`error.message` → `message` → fallback。
pub(super) fn provider_error_message(value: &Value, fallback: &str) -> String {
    provider_error_message_with_nested(value, None, fallback)
}

/// 在 `error.message` 与顶层 `message` 之间插入一个嵌套回退摘要
/// （Responses 网关的 `response.error.message`），查找顺序与其余协议一致。
pub(super) fn provider_error_message_with_nested(
    value: &Value,
    nested_message: Option<&str>,
    fallback: &str,
) -> String {
    value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .or(nested_message)
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or(fallback)
        .to_owned()
}

/// 只把具有明确结构或上下文超限证据的 Provider 错误归一为稳定错误类型；
/// `fallback` 注入未说明错误文案的协议前缀。
pub(super) fn classify_provider_error(value: &Value, fallback: &str) -> ModelError {
    let message = provider_error_message(value, fallback);
    let code = value
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("code").or_else(|| error.get("type")))
        .and_then(Value::as_str)
        .or_else(|| value.get("code").and_then(Value::as_str));
    classify_in_band_provider_error(&message, code)
}

/// 映射 Provider 中立推理强度到 Chat Completions 与 Responses 共用的字段取值。
pub(super) fn reasoning_effort(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::ExtraHigh => "xhigh",
        ReasoningEffort::Maximum => "max",
    }
}

/// 从 Chat Completions / Responses JSON 对象提取响应元数据。
pub(super) fn response_metadata(
    response: &Map<String, Value>,
) -> Result<ResponseMetadata, ModelError> {
    let metadata = ResponseMetadata {
        decode_duration_ms: None,
        response_id: response
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        model: response
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    };
    metadata.validate()?;
    Ok(metadata)
}

/// 将图片来源转换为 Chat `image_url.url` 与 Responses `input_image.image_url`
/// 共用的 data URL 或远程地址字符串。
pub(super) fn image_url(source: &ImageSource) -> String {
    match source {
        ImageSource::Url { url } => url.clone(),
        ImageSource::Base64 { media_type, data } => {
            format!("data:{media_type};base64,{data}")
        }
    }
}

/// 要求 SSE 已经收到响应开始事件；`message` 注入各协议的开始事件名称。
pub(super) fn require_started(started: bool, message: &str) -> Result<(), ModelError> {
    if started {
        Ok(())
    } else {
        Err(protocol_error(message))
    }
}
