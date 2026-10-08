//! 当前模型响应的有界热尾部，供窗口切换和订阅重建；完成后立即释放。

use super::NativeHost;
use crate::native_ui::model::*;
use keencode_agent::AgentStreamEventKind;
use keencode_model::ModelStreamEvent;
use keencode_resources::{ROOT_AGENT_ID, SessionEvent};
use keencode_runtime::{RuntimeEventDelivery, RuntimeEventPayload};

const MAX_HOT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_HOT_MESSAGES: usize = 64;
const MAX_HOT_BLOCKS_PER_MESSAGE: usize = 128;

#[derive(Default)]
pub(super) struct HotState {
    through: u64,
    messages: Vec<UiMessage>,
    text_bytes: usize,
}

impl NativeHost {
    pub(super) fn initialize_hot(&self, session_id: &str, through: u64) {
        self.inner
            .hot
            .lock()
            .expect("原生热流锁已损坏")
            .entry(session_id.to_owned())
            .and_modify(|hot| hot.through = hot.through.max(through))
            .or_insert(HotState {
                through,
                ..Default::default()
            });
        self.inner.hot_changed.notify_waiters();
    }

    pub(super) fn observe_hot_event(&self, delivery: &RuntimeEventDelivery) {
        let mut cache = self.inner.hot.lock().expect("原生热流锁已损坏");
        let state = cache.entry(delivery.session_id().to_owned()).or_default();
        if delivery.delivery_sequence <= state.through {
            return;
        }
        match &delivery.payload {
            RuntimeEventPayload::Transient(event)
                if event.source_agent_id().as_str() == ROOT_AGENT_ID =>
            {
                if let AgentStreamEventKind::ModelEvent { event: model } = event.kind() {
                    let delta = match model {
                        ModelStreamEvent::TextDelta { index, delta } => {
                            Some((*index, delta, false))
                        }
                        ModelStreamEvent::ReasoningDelta { index, delta }
                        | ModelStreamEvent::ReasoningSummaryDelta { index, delta } => {
                            Some((*index, delta, true))
                        }
                        _ => None,
                    };
                    if let Some((index, delta, reasoning)) = delta {
                        let message_id = format!(
                            "stream:{}:{}:{}",
                            event.turn_id(),
                            event.source_agent_id(),
                            event.model_round()
                        );
                        append_hot_delta(
                            state,
                            message_id,
                            event.turn_id().as_str(),
                            index,
                            delta,
                            reasoning,
                        );
                    }
                }
            }
            RuntimeEventPayload::Authoritative(record) => remove_committed(&record.event, state),
            RuntimeEventPayload::Control(_) => {
                state.messages.clear();
                state.text_bytes = 0;
            }
            _ => {}
        }
        state.through = delivery.delivery_sequence;
        drop(cache);
        self.inner.hot_changed.notify_waiters();
    }

    pub(super) async fn wait_hot_through(&self, session_id: &str, sequence: u64) {
        loop {
            let notification = self.inner.hot_changed.notified();
            if self
                .inner
                .hot
                .lock()
                .expect("原生热流锁已损坏")
                .get(session_id)
                .is_some_and(|hot| hot.through >= sequence)
            {
                return;
            }
            notification.await;
        }
    }

    pub(super) fn hot_messages(&self, session_id: &str) -> Vec<UiMessage> {
        self.hot_snapshot(session_id).1
    }

    pub(super) fn hot_snapshot(&self, session_id: &str) -> (u64, Vec<UiMessage>) {
        self.inner
            .hot
            .lock()
            .expect("原生热流锁已损坏")
            .get(session_id)
            .map(|hot| (hot.through, hot.messages.clone()))
            .unwrap_or_default()
    }
}

/// 除正文外也限制空块和索引数量，异常流不能用零字节增量绕过内存预算。
fn append_hot_delta(
    hot: &mut HotState,
    message_id: String,
    turn_id: &str,
    index: u32,
    delta: &str,
    reasoning: bool,
) {
    if hot.text_bytes.saturating_add(delta.len()) > MAX_HOT_TEXT_BYTES {
        return;
    }
    let message_index = hot
        .messages
        .iter()
        .position(|message| message.message_id == message_id);
    let message_index = match message_index {
        Some(index) => index,
        None if hot.messages.len() < MAX_HOT_MESSAGES => {
            hot.messages.push(UiMessage {
                message_id: message_id.clone(),
                turn_id: Some(turn_id.to_owned()),
                role: MessageRole::Assistant,
                blocks: Vec::new(),
                feedback: None,
                created_at_unix_ms: None,
            });
            hot.messages.len() - 1
        }
        None => return,
    };
    let message = &mut hot.messages[message_index];
    let block_id = format!(
        "{message_id}:{index}:{}",
        if reasoning { "reasoning" } else { "text" }
    );
    let block_index = message.blocks.iter().position(|block| matches!(block, MessageBlock::Markdown { block_id: id, .. } | MessageBlock::Reasoning { block_id: id, .. } if id == &block_id));
    let block_index = match block_index {
        Some(index) => index,
        None if message.blocks.len() < MAX_HOT_BLOCKS_PER_MESSAGE => {
            message.blocks.push(if reasoning {
                MessageBlock::Reasoning {
                    block_id,
                    source: String::new(),
                    streaming: true,
                }
            } else {
                MessageBlock::Markdown {
                    block_id,
                    source: String::new(),
                    streaming: true,
                }
            });
            message.blocks.len() - 1
        }
        None => return,
    };
    if let MessageBlock::Markdown { source, .. } | MessageBlock::Reasoning { source, .. } =
        &mut message.blocks[block_index]
    {
        source.push_str(delta);
        hot.text_bytes += delta.len();
    }
}

fn remove_committed(event: &SessionEvent, hot: &mut HotState) {
    match event {
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                remove_committed(event, hot);
            }
        }
        SessionEvent::TranscriptSegmentCommitted { segment } => {
            let id = format!(
                "stream:{}:{}:{}",
                segment.turn_id, segment.source_agent_id, segment.model_round
            );
            hot.messages.retain(|message| message.message_id != id);
        }
        SessionEvent::TurnCompleted { turn_id } | SessionEvent::TurnStopped { turn_id, .. } => {
            hot.messages
                .retain(|message| message.turn_id.as_deref() != Some(turn_id.as_str()));
        }
        _ => return,
    }
    hot.text_bytes = hot
        .messages
        .iter()
        .flat_map(|message| &message.blocks)
        .map(|block| match block {
            MessageBlock::Markdown { source, .. } | MessageBlock::Reasoning { source, .. } => {
                source.len()
            }
            _ => 0,
        })
        .sum();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stream_indices_cannot_bypass_hot_memory_bounds() {
        let mut hot = HotState::default();
        for message in 0..MAX_HOT_MESSAGES + 4 {
            for block in 0..MAX_HOT_BLOCKS_PER_MESSAGE + 4 {
                append_hot_delta(
                    &mut hot,
                    format!("stream:{message}"),
                    "turn",
                    block as u32,
                    "",
                    false,
                );
            }
        }
        assert_eq!(hot.messages.len(), MAX_HOT_MESSAGES);
        assert!(
            hot.messages
                .iter()
                .all(|message| message.blocks.len() == MAX_HOT_BLOCKS_PER_MESSAGE)
        );
        append_hot_delta(
            &mut hot,
            "stream:0".into(),
            "turn",
            0,
            "仍可更新已存在的块",
            false,
        );
        assert!(
            matches!(&hot.messages[0].blocks[0], MessageBlock::Markdown { source, .. } if source == "仍可更新已存在的块")
        );
        let before = hot.text_bytes;
        append_hot_delta(
            &mut hot,
            "stream:0".into(),
            "turn",
            0,
            &"x".repeat(MAX_HOT_TEXT_BYTES),
            false,
        );
        assert_eq!(hot.text_bytes, before);
    }
}
