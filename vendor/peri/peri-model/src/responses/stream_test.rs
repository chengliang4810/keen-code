use serde_json::json;

use std::sync::Mutex;

use crate::ModelStreamEvent;

use super::StreamState;

/// 组装一个使用纯函数解码器的状态,直接喂事件并收集事件流。
fn decode_events(events: &[serde_json::Value]) -> (Mutex<StreamState>, Vec<ModelStreamEvent>) {
    let state = Mutex::new(StreamState::default());
    let mut out = Vec::new();
    for event in events {
        let sse = crate::transport::SseEvent {
            event: None,
            data: serde_json::to_string(event).expect("serializable"),
        };
        out.extend(super::decode_event(&state, sse).expect("decode ok"));
    }
    (state, out)
}

/// 文本增量事件必须产生 TextDelta。
#[test]
fn parses_text_delta() {
    let (_state, events) = decode_events(&[json!({
        "type": "response.output_text.delta",
        "delta": "你好"
    })]);
    assert!(matches!(
        events.as_slice(),
        [ModelStreamEvent::TextDelta { text }] if text == "你好"
    ));
}

/// 空 delta 不产生事件。
#[test]
fn ignores_empty_text_delta() {
    let (_state, events) = decode_events(&[json!({
        "type": "response.output_text.delta",
        "delta": ""
    })]);
    assert!(events.is_empty());
}

/// 推理摘要增量事件必须产生 ReasoningDelta。
#[test]
fn parses_reasoning_delta() {
    let (_state, events) = decode_events(&[json!({
        "type": "response.reasoning_summary_part.delta",
        "delta": "正在分析"
    })]);
    assert!(matches!(
        events.as_slice(),
        [ModelStreamEvent::ReasoningDelta { text }] if text == "正在分析"
    ));
}

/// output_item.done 中的 function_call 必须产生完整 ToolCallDelta。
#[test]
fn parses_function_call_item_done() {
    let (_state, events) = decode_events(&[json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": "call_1",
            "name": "Read",
            "arguments": "{\"path\":\"/tmp/a\"}"
        }
    })]);
    assert!(matches!(
        events.as_slice(),
        [ModelStreamEvent::ToolCallDelta { index: 0, id: Some(id), name: Some(name), arguments_delta }]
            if id == "call_1" && name == "Read" && arguments_delta == "{\"path\":\"/tmp/a\"}"
    ));
}

/// completed 事件必须提取 usage 与 request_id。
#[test]
fn parses_completed_usage() {
    let (state, events) = decode_events(&[json!({
        "type": "response.completed",
        "response": {
            "id": "resp_1",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }
    })]);
    assert!(matches!(
        events.as_slice(),
        [
            ModelStreamEvent::Usage(usage),
            ModelStreamEvent::Completed(_),
        ]
            if usage.input_tokens == 10 && usage.output_tokens == 5
    ));
    assert_eq!(state.lock().unwrap().request_id.as_deref(), Some("resp_1"));
}

/// failed 是协议终态错误事件：decode_event 必须立即返回 Provider 错误。
#[test]
fn failed_event_errors_immediately() {
    let state = Mutex::new(StreamState::default());
    let sse = crate::transport::SseEvent {
        event: None,
        data: serde_json::to_string(&json!({
            "type": "response.failed",
            "response": {"error": {"message": "上游超时"}}
        }))
        .expect("serializable"),
    };
    let result = super::decode_event(&state, sse);
    assert!(result.is_err());
}

/// 无关事件不产生任何输出。
#[test]
fn ignores_unrelated_events() {
    let (_state, events) = decode_events(&[json!({
        "type": "response.in_progress"
    })]);
    assert!(events.is_empty());
}

/// 工具调用解析失败时完成解码器返回 Provider 错误。
#[test]
fn broken_arguments_error_on_complete() {
    let state = Mutex::new(StreamState::default());
    let sse = crate::transport::SseEvent {
        event: None,
        data: serde_json::to_string(&json!({
            "type": "response.output_item.done",
            "item": {"type": "function_call", "call_id": "c", "name": "Read", "arguments": "{bad"}
        }))
        .expect("serializable"),
    };
    super::decode_event(&state, sse).expect("decode ok");
    assert!(super::complete_stream(&state).is_err());
}

#[test]
fn blank_tool_name_errors_on_complete() {
    let state = Mutex::new(StreamState::default());
    let sse = crate::transport::SseEvent {
        event: None,
        data: serde_json::to_string(&json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "call_id": "c",
                "name": "   ",
                "arguments": "{}"
            }
        }))
        .expect("serializable"),
    };
    super::decode_event(&state, sse).expect("decode ok");
    assert!(super::complete_stream(&state).is_err());
}
