use rcode_model::{ContentBlock, ImageContent, Message, MessageRole, ToolCall, ToolResult};
use serde_json::Value;

// 原生提交的数据块保存签名推理和工具配对；界面文本只是展示投影。
pub(super) fn model_history(ui_messages: &[Value]) -> Result<Vec<Message>, String> {
    let mut history = Vec::new();
    for message in ui_messages {
        let parts = message
            .get("parts")
            .and_then(Value::as_array)
            .ok_or("消息缺少 parts")?;
        let native: Vec<_> = parts
            .iter()
            .filter(|p| p["type"] == "data-rcode-messages")
            .collect();
        if !native.is_empty() {
            for part in native {
                let messages: Vec<Message> =
                    serde_json::from_value(part["data"]["messages"].clone())
                        .map_err(|_| "原生消息记录无效")?;
                for message in &messages {
                    message.validate().map_err(|e| e.to_string())?;
                }
                history.extend(messages);
            }
            continue;
        }
        let role = match message["role"].as_str() {
            Some("user") => MessageRole::User,
            Some("assistant") => MessageRole::Assistant,
            Some("system") => MessageRole::System,
            _ => return Err("消息角色无效".into()),
        };
        let mut content = Vec::new();
        let mut results = Vec::new();
        for part in parts {
            match part["type"].as_str().unwrap_or_default() {
                "text" => {
                    if let Some(text) = part["text"].as_str().filter(|text| !text.is_empty()) {
                        content.push(ContentBlock::text(text));
                    }
                }
                "file" => {
                    let media = part["mediaType"].as_str().unwrap_or_default();
                    let url = part["url"].as_str().unwrap_or_default();
                    if !media.starts_with("image/") {
                        return Err("该协议只支持文本和图片附件".into());
                    }
                    let image =
                        if let Some(data) = url.strip_prefix(&format!("data:{media};base64,")) {
                            ImageContent::from_base64(media, data)
                        } else if url.starts_with("https://") || url.starts_with("http://") {
                            ImageContent::from_url(url)
                        } else {
                            return Err("图片来源无效".into());
                        };
                    content.push(ContentBlock::Image { image });
                }
                kind if role == MessageRole::Assistant
                    && (kind.starts_with("tool-") || kind == "dynamic-tool") =>
                {
                    let state = part["state"].as_str().unwrap_or_default();
                    if !matches!(state, "output-available" | "output-error" | "output-denied") {
                        continue;
                    }
                    let id = part["toolCallId"].as_str().ok_or("工具消息缺少调用 ID")?;
                    let name = part["toolName"]
                        .as_str()
                        .or_else(|| kind.strip_prefix("tool-"))
                        .ok_or("工具消息缺少名称")?;
                    content.push(ContentBlock::ToolCall {
                        tool_call: ToolCall {
                            id: id.into(),
                            name: name.into(),
                            arguments: part["input"].clone(),
                        },
                    });
                    let text = if state == "output-available" {
                        part["output"].to_string()
                    } else {
                        part["errorText"]
                            .as_str()
                            .unwrap_or("工具未执行")
                            .to_owned()
                    };
                    results.push(ContentBlock::ToolResult {
                        tool_result: ToolResult::text(id, text, state != "output-available"),
                    });
                }
                _ => {}
            }
        }
        if !content.is_empty() {
            history.push(Message::new(role, content));
        }
        if !results.is_empty() {
            history.push(Message::new(MessageRole::Tool, results));
        }
    }
    if history.is_empty() {
        return Err("消息不能为空".into());
    }
    for message in &history {
        message.validate().map_err(|e| e.to_string())?;
    }
    Ok(history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn native_transcript_is_replayed_once_instead_of_display_projection() {
        let messages = vec![Message::text(MessageRole::Assistant, "authoritative")];
        let ui = vec![json!({"role":"assistant","parts":[
            {"type":"text","text":"display"},
            {"type":"data-rcode-messages","data":{"messages":messages}}
        ]})];
        assert_eq!(model_history(&ui).unwrap(), messages);
    }

    #[test]
    fn legacy_completed_tools_retain_request_result_pairs() {
        let ui = vec![json!({"role":"assistant","parts":[{"type":"tool-read_file",
            "state":"output-available","toolCallId":"call-1","input":{"path":"src/a.ts"},"output":"ok"}]})];
        let result = model_history(&ui).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].role, MessageRole::Tool);
    }
}
