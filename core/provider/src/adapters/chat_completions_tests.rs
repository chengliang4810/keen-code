use std::collections::VecDeque;

use keencode_model::{
    ContentBlock, Message, MessageRole, ModelRequest, ModelStreamEvent, OpaqueReasoningState,
    ReasoningContent, ToolCall, ToolDefinition, ToolResult,
};
use serde_json::{Value, json};

use super::{CHAT_REASONING_STATE_KIND, ChatCompletionsAdapter};
use crate::sse::SseFrame;

fn tool_definition() -> ToolDefinition {
    ToolDefinition::new(
        "lookup",
        "查询城市信息",
        json!({
            "type": "object",
            "properties": { "city": { "type": "string" } },
            "required": ["city"],
            "additionalProperties": false
        }),
    )
}

fn next_request(continuation: OpaqueReasoningState) -> ModelRequest {
    let mut request = ModelRequest::new(
        "deepseek-test",
        vec![
            Message::text(MessageRole::User, "查询天气"),
            Message::new(
                MessageRole::Assistant,
                vec![
                    ContentBlock::Reasoning {
                        reasoning: ReasoningContent {
                            text: "先分析".to_owned(),
                            summary: None,
                            continuation: Some(continuation),
                        },
                    },
                    ContentBlock::ToolCall {
                        tool_call: ToolCall::new("call-1", "lookup", json!({ "city": "杭州" })),
                    },
                ],
            ),
            Message::new(
                MessageRole::Tool,
                vec![ContentBlock::ToolResult {
                    tool_result: ToolResult::text("call-1", "晴", false),
                }],
            ),
        ],
    );
    request.tools.push(tool_definition());
    request
}

fn continuation_from(events: &[ModelStreamEvent]) -> OpaqueReasoningState {
    events
        .iter()
        .find_map(|event| match event {
            ModelStreamEvent::ReasoningContinuation { continuation, .. } => {
                Some(continuation.clone())
            }
            _ => None,
        })
        .expect("完整 Chat 响应应生成推理续传状态")
}

fn encoded_assistant(request: &ModelRequest) -> Value {
    let body = ChatCompletionsAdapter::new()
        .encode_request(request, false)
        .expect("Chat 历史应可编码");
    body["messages"][1].clone()
}

fn has_continuation(events: &[ModelStreamEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, ModelStreamEvent::ReasoningContinuation { .. }))
}

#[test]
fn buffered_reasoning_content_and_tool_call_are_replayed() {
    let mut adapter = ChatCompletionsAdapter::new();
    let events = adapter
        .decode_json(json!({
            "id": "response-1",
            "model": "deepseek-test",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "先分析",
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": "{\"city\":\"杭州\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("完整 Chat JSON 应可解码");

    assert!(events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::ToolCallStart { id, name, .. }
            if id == "call-1" && name == "lookup"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::ToolCallArgumentsDelta { id, delta, .. }
            if id == "call-1" && delta == "{\"city\":\"杭州\"}"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::ToolCallEnd { id, .. } if id == "call-1"
    )));

    let continuation = continuation_from(&events);
    assert_eq!(continuation.kind, CHAT_REASONING_STATE_KIND);
    assert_eq!(
        continuation.data["reasoning_content"],
        Value::String("先分析".to_owned())
    );
    let assistant = encoded_assistant(&next_request(continuation));
    assert_eq!(assistant["reasoning_content"], "先分析");
    assert_eq!(assistant["tool_calls"][0]["id"], "call-1");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "lookup");
}

#[test]
fn buffered_reasoning_field_is_replayed_without_guessing_the_field_name() {
    for field in ["reasoning_content", "reasoning"] {
        let mut message = json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call-1",
                "type": "function",
                "function": {
                    "name": "lookup",
                    "arguments": "{\"city\":\"杭州\"}"
                }
            }]
        });
        message
            .as_object_mut()
            .expect("测试消息必须是对象")
            .insert(field.to_owned(), Value::String("字段推理".to_owned()));
        let mut adapter = ChatCompletionsAdapter::new();
        let events = adapter
            .decode_json(json!({
                "id": "response-field",
                "model": "deepseek-test",
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": "tool_calls"
                }]
            }))
            .expect("Chat 原生推理字段应可解码");
        let continuation = continuation_from(&events);
        assert_eq!(continuation.data[field], "字段推理");
        let assistant = encoded_assistant(&next_request(continuation));
        assert_eq!(assistant[field], "字段推理");
    }
}

#[test]
fn streaming_reasoning_content_and_tool_call_are_replayed_after_done() {
    let mut adapter = ChatCompletionsAdapter::new();
    let mut output = VecDeque::new();
    for data in [
        json!({
            "id": "stream-1",
            "model": "deepseek-test",
            "choices": [{
                "index": 0,
                "delta": { "role": "assistant", "reasoning_content": "先" },
                "finish_reason": null
            }]
        })
        .to_string(),
        json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call-1",
                        "function": {
                            "name": "lookup",
                            "arguments": "{\"city\":\"杭州\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        })
        .to_string(),
        json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]
        })
        .to_string(),
    ] {
        adapter
            .consume_sse(SseFrame { event: None, data }, &mut output)
            .expect("完整 Chat SSE 帧应可解码");
    }
    adapter
        .consume_sse(
            SseFrame {
                event: None,
                data: "[DONE]".to_owned(),
            },
            &mut output,
        )
        .expect("Chat SSE 应接受 DONE");
    let events: Vec<_> = output.into_iter().collect();
    let continuation = continuation_from(&events);
    assert_eq!(continuation.data["reasoning_content"], "先");
    let assistant = encoded_assistant(&next_request(continuation));
    assert_eq!(assistant["reasoning_content"], "先");
    assert_eq!(assistant["tool_calls"][0]["id"], "call-1");
}

#[test]
fn incomplete_stream_does_not_emit_reasoning_continuation() {
    let mut adapter = ChatCompletionsAdapter::new();
    let mut output = VecDeque::new();
    adapter
        .consume_sse(
            SseFrame {
                event: None,
                data: json!({
                    "id": "stream-incomplete",
                    "choices": [{
                        "index": 0,
                        "delta": { "reasoning_content": "未完成" },
                        "finish_reason": null
                    }]
                })
                .to_string(),
            },
            &mut output,
        )
        .expect("中断前的 SSE 帧应可解码");
    assert!(adapter.finish_stream(&mut output).is_err());
    assert!(
        !output
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::ReasoningContinuation { .. }))
    );
}

#[test]
fn generic_reasoning_without_chat_state_is_not_replayed() {
    let request = ModelRequest::new(
        "deepseek-test",
        vec![
            Message::text(MessageRole::User, "继续"),
            Message::new(
                MessageRole::Assistant,
                vec![ContentBlock::Reasoning {
                    reasoning: ReasoningContent::new("仅供展示的推理"),
                }],
            ),
        ],
    );
    let assistant = encoded_assistant(&request);
    assert!(assistant.get("reasoning_content").is_none());
    assert!(assistant.get("reasoning").is_none());
}

#[test]
fn opaque_state_from_another_protocol_is_not_sent_to_chat() {
    let request = next_request(OpaqueReasoningState::new(
        "responses-reasoning-state-v1",
        json!({ "type": "reasoning", "id": "rs_1" }),
    ));
    let assistant = encoded_assistant(&request);
    assert!(assistant.get("reasoning_content").is_none());
    assert!(assistant.get("reasoning").is_none());
}

#[test]
fn reasoning_details_remains_display_only_without_fake_continuation() {
    let mut adapter = ChatCompletionsAdapter::new();
    let events = adapter
        .decode_json(json!({
            "id": "details-1",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "完成",
                    "reasoning_details": [{
                        "type": "reasoning.text",
                        "text": "可展示推理"
                    }]
                },
                "finish_reason": "stop"
            }]
        }))
        .expect("reasoning_details 应保留显示语义");
    assert!(events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::ReasoningDelta { delta, .. } if delta == "可展示推理"
    )));
    assert!(!has_continuation(&events));
}

#[test]
fn streaming_fields_preserve_repeated_fragments_and_original_names() {
    let mut adapter = ChatCompletionsAdapter::new();
    let mut output = VecDeque::new();
    for (index, delta) in [
        json!({"reasoning_content": "same", "reasoning": "甲"}),
        json!({"reasoning_content": "same", "reasoning": "乙"}),
        json!({}),
    ]
    .into_iter()
    .enumerate()
    {
        adapter
            .consume_sse(
                SseFrame {
                    event: None,
                    data: json!({
                        "id": "multi-fragment",
                        "choices": [{"index": 0, "delta": delta,
                            "finish_reason": if index == 2 { json!("stop") } else { Value::Null }}]
                    })
                    .to_string(),
                },
                &mut output,
            )
            .unwrap();
    }
    adapter.finish_stream(&mut output).unwrap();
    let events = output.into_iter().collect::<Vec<_>>();
    let continuation = continuation_from(&events);
    assert_eq!(continuation.data["reasoning_content"], "samesame");
    assert_eq!(continuation.data["reasoning"], "甲乙");
    let assistant = encoded_assistant(&next_request(continuation));
    assert_eq!(assistant["reasoning_content"], "samesame");
    assert_eq!(assistant["reasoning"], "甲乙");
}

#[test]
fn reasoning_limit_is_shared_across_fields_and_degrades_past_limit() {
    let mut adapter = ChatCompletionsAdapter::new();
    let at_limit = "x".repeat(super::MAX_CHAT_REASONING_STATE_BYTES);
    adapter
        .remember_reasoning_text("reasoning_content", &at_limit)
        .unwrap();
    // 超限后优雅降级：不报错，仅停止回放续传。
    adapter.remember_reasoning_text("reasoning", "y").unwrap();
    assert!(!adapter.reasoning_state_replayable);
    assert!(adapter.chat_reasoning_content.is_none());
    assert!(adapter.chat_reasoning.is_none());
    adapter
        .remember_reasoning_text("reasoning_content", "later")
        .unwrap();
    assert!(adapter.chat_reasoning_content.is_none());
    assert!(
        adapter
            .take_reasoning_continuation_event()
            .unwrap()
            .is_none()
    );
}

#[test]
fn malformed_chat_state_does_not_become_wire_reasoning() {
    for data in [json!([]), json!({"reasoning_content": 1, "reasoning": {}})] {
        let request = next_request(OpaqueReasoningState::new(CHAT_REASONING_STATE_KIND, data));
        let assistant = encoded_assistant(&request);
        assert!(assistant.get("reasoning_content").is_none());
        assert!(assistant.get("reasoning").is_none());
    }
}
