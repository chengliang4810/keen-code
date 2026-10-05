//! ZCode V4 会话协议的轻量解析与常量。
//!
//! 这里不复制 TypeScript schema；字段校验只覆盖 Rust 网关能够裁决的边界，
//! 业务事实仍由 Runtime/Journal 投影产生。未知命令必须返回错误，不能伪造 ACK。

use serde_json::Value;

pub const WIRE_PROTOCOL_VERSION: u64 = 3;
pub const SNAPSHOT_PROTOCOL_VERSION: u64 = 1;
pub const MAX_ROWS_RANGE: usize = 200;
pub const DEFAULT_ROWS_WINDOW: usize = 60;

pub fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(name))
}

pub fn string_field(value: &Value, name: &str) -> Result<String, String> {
    field(value, name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty() && value.trim() == value)
        .ok_or_else(|| format!("{name} 必须是非空字符串"))
}

pub fn optional_string_field(value: &Value, name: &str) -> Option<String> {
    field(value, name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty() && value.trim() == value)
}

pub fn topic_parts(topic: &str) -> Result<(&str, &str), String> {
    for kind in ["conversation", "sessions-index", "workspace-config"] {
        let prefix = format!("{kind}/");
        if let Some(id) = topic.strip_prefix(&prefix) {
            // workspaceKey 可以是绝对路径或带 authority 的 URI，剩余部分允许
            // 继续包含 `/`；只禁止空标识，不能把路径误判为无效 topic。
            if id.is_empty() {
                return Err("V4 topic 标识无效".to_owned());
            }
            return Ok((kind, id));
        }
    }
    Err("V4 topic 必须包含受支持的 kind/id".to_owned())
}

pub fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn topic_parser_accepts_only_known_topics() {
        assert_eq!(
            topic_parts("conversation/session-1").unwrap(),
            ("conversation", "session-1")
        );
        assert_eq!(
            topic_parts("sessions-index/ssh:host:/workspace/repo").unwrap(),
            ("sessions-index", "ssh:host:/workspace/repo")
        );
        assert!(topic_parts("conversation/").is_err());
        assert!(topic_parts("unknown/value").is_err());
    }

    #[test]
    fn string_field_does_not_trim_user_input() {
        let value = json!({"sessionId": " session-1"});
        assert!(string_field(&value, "sessionId").is_err());
    }
}
