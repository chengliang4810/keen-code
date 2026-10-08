use rcode_model::ModelError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 已验证且有界的模型思考参数，仅能覆盖协议的推理字段。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(try_from = "Value", into = "Value")]
pub struct ReasoningBody(Value);

impl TryFrom<Value> for ReasoningBody {
    type Error = String;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let fields = value.as_object().ok_or("思考参数必须是 JSON 对象")?;
        if serde_json::to_vec(&value).map_err(|e| e.to_string())?.len() > 8192 {
            return Err("思考参数超过大小上限".into());
        }
        for (key, item) in fields {
            let valid = match key.as_str() {
                "reasoning_effort" => valid_text(item),
                "enable_thinking" => item.is_boolean(),
                "thinking_budget" => valid_budget(item),
                "reasoning" => valid_object(
                    item,
                    &["effort", "summary", "max_tokens", "enabled", "exclude"],
                ),
                "thinking" => valid_object(item, &["type", "budget_tokens"]),
                "output_config" => valid_object(item, &["effort"]),
                _ => false,
            };
            if !valid {
                return Err("思考映射只能包含有效的推理参数".into());
            }
        }
        Ok(Self(value))
    }
}

impl From<ReasoningBody> for Value {
    fn from(value: ReasoningBody) -> Self {
        value.0
    }
}

impl ReasoningBody {
    pub(crate) fn apply(&self, body: &mut Value) -> Result<(), ModelError> {
        let object = body
            .as_object_mut()
            .ok_or_else(|| ModelError::InvalidRequest {
                message: "模型请求必须是 JSON 对象".into(),
            })?;
        for (key, value) in self.0.as_object().expect("validated reasoning body") {
            object.insert(key.clone(), value.clone());
        }
        Ok(())
    }
}

fn valid_text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        !text.is_empty() && text.len() <= 64 && !text.chars().any(char::is_control)
    })
}

fn valid_budget(value: &Value) -> bool {
    value.as_u64().is_some_and(|budget| budget <= 1_000_000)
}

fn valid_object(value: &Value, allowed: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.iter().all(|(key, value)| {
            allowed.contains(&key.as_str())
                && match key.as_str() {
                    "enabled" | "exclude" => value.is_boolean(),
                    "budget_tokens" | "max_tokens" => valid_budget(value),
                    _ => valid_text(value),
                }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mapping_cannot_replace_model_messages_tools_or_transport_fields() {
        for value in [
            json!({"model":"other"}),
            json!({"messages":[]}),
            json!({"tools":[]}),
            json!({"stream":false}),
            json!({"thinking":{"type":"enabled", "headers":{}}}),
            json!({"reasoning_effort":false}),
            json!({"enable_thinking":"yes"}),
            json!({"thinking":{"budget_tokens":-1}}),
        ] {
            assert!(ReasoningBody::try_from(value).is_err());
        }
    }

    #[test]
    fn three_protocol_mappings_preserve_agent_payload_and_empty_mapping() {
        for mapping in [
            json!({"reasoning_effort":"xhigh"}),
            json!({"reasoning":{"effort":"none"}}),
            json!({"thinking":{"type":"adaptive"},"output_config":{"effort":"max"}}),
            json!({}),
        ] {
            let mut body = json!({"model":"model", "messages":[], "tools":[], "stream":true});
            let patch = ReasoningBody::try_from(mapping.clone()).unwrap();
            patch.apply(&mut body).unwrap();
            assert_eq!(body["model"], "model");
            assert_eq!(body["messages"], json!([]));
            assert_eq!(body["tools"], json!([]));
            assert_eq!(body["stream"], true);
            for (key, value) in mapping.as_object().unwrap() {
                assert_eq!(&body[key], value);
            }
        }
    }
}
