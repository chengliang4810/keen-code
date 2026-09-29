use std::collections::VecDeque;

use keencode_model::{ModelStreamEvent, ResponseMetadata};
use serde_json::{Value, json};

use super::MessagesAdapter;
use crate::sse::SseFrame;

fn frame(data: Value) -> SseFrame {
    SseFrame {
        event: None,
        data: data.to_string(),
    }
}

#[test]
fn empty_message_start_synthesizes_message_start_for_gateway_streams() {
    let mut adapter = MessagesAdapter::new();
    let mut output = VecDeque::new();
    for data in [
        // Bedrock 形态：message_start 不带 message 体（或全空）。
        json!({"type": "message_start"}),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "你好"}
        }),
        json!({"type": "content_block_stop", "index": 0}),
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"input_tokens": 3, "output_tokens": 5}
        }),
        json!({"type": "message_stop"}),
    ] {
        adapter
            .consume_sse(frame(data), &mut output)
            .expect("空 message_start 流的帧应可解码");
    }
    adapter.finish_stream().expect("流应以 message_stop 结束");
    let events: Vec<_> = output.into_iter().collect();
    // 中立层要求内容事件之前先有 MessageStart：空 message_start 必须合成
    // 默认元数据的开始事件，而不是只标记内部状态让整条流被判协议错误。
    assert!(matches!(
        events.first(),
        Some(ModelStreamEvent::MessageStart { metadata })
            if *metadata == ResponseMetadata::default()
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        ModelStreamEvent::TextDelta { delta, .. } if delta == "你好"
    )));
    assert!(matches!(
        events.last(),
        Some(ModelStreamEvent::MessageEnd { .. })
    ));
}
