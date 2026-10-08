//! 有界的 Journal 到 GPUI 投影；只复制当前历史页，不克隆完整 SessionState。

use std::{collections::BTreeSet, sync::Arc};

use keencode_resources::{
    self as resource, MessagePart, SessionEvent, SessionMessage, SessionState, ToolLifecycle,
};
use keencode_runtime::RuntimeSession;
use sha2::{Digest, Sha256};

use super::{NativeHost, ui_error};
use crate::native_ui::model::{cap_message_to_budget, *};

const HISTORY_RECORDS_PER_PAGE: usize = 100;
const TOOL_DISPLAY_BYTES: usize = 64 * 1024;
const MESSAGE_DISPLAY_BYTES: usize = 256 * 1024;
/// 单条 Transcript 消息最多投影固定数量的块，避免异常多 part 形成大批量 UI 事件。
const MAX_MESSAGE_BLOCKS: usize = 128;

pub(super) fn bounded_text(value: &str, budget: usize) -> String {
    if value.len() <= budget {
        return value.to_owned();
    }
    let mut end = budget;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…（内容过长，完整结果保存在会话日志）", &value[..end])
}

fn artifact_text(
    session: &RuntimeSession,
    artifact: &resource::ArtifactUse,
    budget: usize,
) -> String {
    match session.preview_artifact(artifact, budget) {
        Ok(preview) => {
            let mut text = preview.text;
            if preview.truncated {
                text.push_str("\n…（内容过长，完整结果保存在会话 Artifact）");
            }
            if !preview.source_is_utf8 {
                text.push_str("\n[此结果包含非 UTF-8 字节]");
            }
            text
        }
        Err(error) => format!(
            "[Artifact 读取失败：{}]",
            keencode_model::redact_error_secrets_bounded(&error.to_string(), 512)
        ),
    }
}

pub(super) fn status(state: &SessionState) -> SessionStatus {
    match state.status {
        resource::SessionStatus::Running => SessionStatus::Running,
        resource::SessionStatus::Waiting => SessionStatus::Waiting,
        resource::SessionStatus::Idle | resource::SessionStatus::Closed => state
            .turns
            .values()
            .filter(|turn| turn.source_agent_id.as_str() == resource::ROOT_AGENT_ID)
            .max_by_key(|turn| turn.started_at_unix_ms)
            .map_or(SessionStatus::Idle, |turn| match turn.status {
                resource::TurnStatus::Running => SessionStatus::Running,
                resource::TurnStatus::Completed => SessionStatus::Completed,
                resource::TurnStatus::Failed => SessionStatus::Failed,
                resource::TurnStatus::Cancelled => SessionStatus::Interrupted,
            }),
    }
}

pub(super) fn session_fact(host: &NativeHost, state: &SessionState) -> SessionFact {
    let last_round = state
        .model_rounds
        .iter()
        .rev()
        .find(|round| round.source_agent_id.as_str() == resource::ROOT_AGENT_ID);
    SessionFact {
        session_id: state.session_id.as_str().to_owned(),
        // Journal 保存真实 OS 路径，侧栏身份统一去掉 Windows verbatim 前缀和分隔符差异。
        project_key: crate::path_utils::path_text_to_frontend(&state.project_root),
        title: state.title.clone(),
        status: status(state),
        pinned: state.pinned,
        archived: state.archived,
        unread: !host
            .inner
            .native_session_subscribers
            .lock()
            .expect("原生订阅锁已损坏")
            .contains_key(state.session_id.as_str())
            && host
                .inner
                .attention
                .is_unread(state.session_id.as_str(), state.last_sequence)
                .unwrap_or(true),
        model: state.provider.as_ref().map(|provider| ModelSelection {
            provider_id: provider.provider_id.clone(),
            model: provider.model.clone(),
        }),
        vision_enabled: state.vision_enabled,
        effort: state
            .provider
            .as_ref()
            .and_then(|provider| provider.reasoning_effort)
            .map(|effort| {
                match effort {
                    resource::ReasoningEffortSnapshot::Minimal => "minimal",
                    resource::ReasoningEffortSnapshot::Low => "low",
                    resource::ReasoningEffortSnapshot::Medium => "medium",
                    resource::ReasoningEffortSnapshot::High => "high",
                    resource::ReasoningEffortSnapshot::ExtraHigh => "xhigh",
                    resource::ReasoningEffortSnapshot::Maximum => "max",
                }
                .to_owned()
            }),
        permission: match state.permission_mode {
            resource::SessionPermissionMode::Build => PermissionMode::Build,
            resource::SessionPermissionMode::Edit => PermissionMode::Edit,
            resource::SessionPermissionMode::Plan => PermissionMode::Plan,
            resource::SessionPermissionMode::Yolo => PermissionMode::Yolo,
        },
        plan: if state.plan.enabled {
            PlanMode::ReadOnly
        } else {
            PlanMode::Off
        },
        followup_mode: match state.followup_mode {
            resource::FollowupMode::Queue => FollowupMode::Queue,
            resource::FollowupMode::Guide => FollowupMode::Guide,
        },
        usage: last_round.map(|round| UsageFact {
            input_tokens: round.usage.input_tokens,
            output_tokens: round.usage.output_tokens,
            reasoning_tokens: round.usage.reasoning_tokens,
            context_window: state
                .provider
                .as_ref()
                .and_then(|provider| provider.context_window),
            // 解码耗时来自 Provider 中立响应元数据；不以 Unix 时间差估算模型速度。
            duration_ms: round.metadata.decode_duration_ms,
            tokens_per_second: round.metadata.decode_duration_ms.and_then(|duration| {
                round
                    .usage
                    .output_tokens?
                    .checked_mul(1_000)?
                    .checked_div(duration)
            }),
        }),
        prompt_history: PromptHistoryFact::default(),
        updated_at_unix_ms: state.updated_at_unix_ms,
        last_sequence: state.last_sequence,
    }
}

pub(super) fn tool_fact(session: &RuntimeSession, tool: &ToolLifecycle) -> ToolFact {
    ToolFact {
        request_id: tool.request.request_id.as_str().to_owned(),
        name: tool.request.tool_name.clone(),
        arguments_json: bounded_text(&tool.request.arguments.to_string(), TOOL_DISPLAY_BYTES),
        output_markdown: tool.outcome.as_ref().map(|outcome| {
            let mut text = String::new();
            for part in &outcome.result.content {
                match part {
                    resource::ToolResultPart::Text { text: body } => text.push_str(body),
                    resource::ToolResultPart::Artifact { artifact, .. } => {
                        text.push_str(&artifact_text(
                            session,
                            artifact,
                            TOOL_DISPLAY_BYTES.saturating_sub(text.len()),
                        ))
                    }
                    resource::ToolResultPart::Image { .. } => text.push_str("\n[工具图片结果]\n"),
                }
                if text.len() > TOOL_DISPLAY_BYTES {
                    break;
                }
            }
            bounded_text(&text, TOOL_DISPLAY_BYTES)
        }),
        status: tool.outcome.as_ref().map_or(
            if tool.execution_started {
                ToolStatus::Running
            } else {
                ToolStatus::Requested
            },
            |outcome| match outcome.status {
                resource::ToolCompletionStatus::Succeeded => ToolStatus::Succeeded,
                resource::ToolCompletionStatus::Failed => ToolStatus::Failed,
                resource::ToolCompletionStatus::Cancelled => ToolStatus::Cancelled,
                resource::ToolCompletionStatus::SideEffectUnknown => ToolStatus::SideEffectUnknown,
            },
        ),
        effect: match tool.request.effect {
            resource::ToolEffect::ReadOnly => ToolEffect::ReadOnly,
            resource::ToolEffect::ChangesState => ToolEffect::ChangesState,
        },
        started_at_unix_ms: tool.execution_started_at_unix_ms,
        completed_at_unix_ms: tool.completed_at_unix_ms,
    }
}

pub(super) fn completed_rewind_turn_ids(
    host: &NativeHost,
    session_id: &str,
) -> Result<BTreeSet<String>, NativeUiError> {
    crate::native_rewind::completed_rewind_turn_ids(&host.inner.paths.data_root, session_id)
        .map_err(ui_error)
}

fn file_change_fact(
    tool: &ToolLifecycle,
    completed_rewind_turn_ids: &BTreeSet<String>,
) -> Option<FileChangeFact> {
    let change = tool.file_change.as_ref()?;
    Some(FileChangeFact {
        path: change.path.clone(),
        operation: if change.before.is_none() {
            FileChangeOperation::Create
        } else {
            FileChangeOperation::Modify
        },
        applied: change.applied,
        // 撤销后 Journal 的 `applied` 仍是原始事实，sidecar 只负责禁止再次撤销，
        // 避免在 SessionState 中引入第二份可变状态。
        rewindable: change.applied
            && !completed_rewind_turn_ids.contains(tool.request.turn_id.as_str()),
    })
}

pub(super) fn tool_message_fact(
    session: &RuntimeSession,
    tool: &ToolLifecycle,
    completed_rewind_turn_ids: &BTreeSet<String>,
) -> UiMessage {
    let mut blocks = vec![MessageBlock::Tool(tool_fact(session, tool))];
    if let Some(change) = file_change_fact(tool, completed_rewind_turn_ids) {
        blocks.push(MessageBlock::FileChange(change));
    }
    UiMessage {
        message_id: format!("tool:{}", tool.request.request_id.as_str()),
        turn_id: Some(tool.request.turn_id.as_str().to_owned()),
        role: MessageRole::Assistant,
        blocks,
        feedback: None,
        created_at_unix_ms: None,
    }
}

pub(super) fn messages_for_tool(
    session: &RuntimeSession,
    state: &SessionState,
    request_id: &resource::RequestId,
    completed_rewind_turn_ids: &BTreeSet<String>,
) -> Vec<UiMessage> {
    let Some(tool) = state.tools.get(request_id) else {
        return Vec::new();
    };
    let model_tool_call_id = tool.request.model_tool_call_id.as_str();
    state
        .raw_transcript_messages()
        .into_iter()
        .filter(|message| {
            message.turn_id.as_ref() == Some(&tool.request.turn_id)
                && message.content.iter().any(|part| {
                    matches!(
                        part,
                        MessagePart::ToolCall { tool_call_id, .. }
                            if tool_call_id.as_str() == model_tool_call_id
                    )
                })
        })
        .filter_map(|message| message_fact(session, state, message, completed_rewind_turn_ids))
        .collect()
}

pub(super) fn message_fact(
    session: &RuntimeSession,
    state: &SessionState,
    message: &SessionMessage,
    completed_rewind_turn_ids: &BTreeSet<String>,
) -> Option<UiMessage> {
    // 内部背景和子 Agent 正文只在 Agent 面板展示，不能污染根对话时间线。
    if message.is_meta
        || message
            .agent_id
            .as_ref()
            .is_some_and(|id| id.as_str() != resource::ROOT_AGENT_ID)
        || matches!(
            message.role,
            resource::MessageRole::Tool | resource::MessageRole::Developer
        )
    {
        return None;
    }
    let mut blocks = Vec::new();
    for (index, part) in message.content.iter().enumerate() {
        if blocks.len() >= MAX_MESSAGE_BLOCKS {
            break;
        }
        let block_id = format!("{}:{index}", message.message_id);
        match part {
            MessagePart::Text { text } => blocks.push(MessageBlock::Markdown {
                block_id,
                source: bounded_text(text, MESSAGE_DISPLAY_BYTES),
                streaming: false,
            }),
            MessagePart::Reasoning { text, summary, .. } => blocks.push(MessageBlock::Reasoning {
                block_id,
                source: if text.is_empty() {
                    bounded_text(
                        summary.as_deref().unwrap_or_default(),
                        MESSAGE_DISPLAY_BYTES,
                    )
                } else {
                    bounded_text(text, MESSAGE_DISPLAY_BYTES)
                },
                streaming: false,
            }),
            MessagePart::ToolCall {
                tool_call_id,
                tool_name,
                arguments,
            } => {
                let tool = state.tools.values().find(|tool| {
                    tool.request.model_tool_call_id == *tool_call_id
                        && message.turn_id.as_ref() == Some(&tool.request.turn_id)
                });
                blocks.push(MessageBlock::Tool(tool.map_or_else(
                    || ToolFact {
                        request_id: tool_call_id.clone(),
                        name: tool_name.clone(),
                        arguments_json: bounded_text(&arguments.to_string(), TOOL_DISPLAY_BYTES),
                        output_markdown: None,
                        status: ToolStatus::Requested,
                        effect: ToolEffect::ReadOnly,
                        started_at_unix_ms: None,
                        completed_at_unix_ms: None,
                    },
                    |tool| tool_fact(session, tool),
                )));
                if let Some(tool) = tool
                    && let Some(change) = file_change_fact(tool, completed_rewind_turn_ids)
                {
                    blocks.push(MessageBlock::FileChange(change));
                }
            }
            MessagePart::Image { source } => blocks.push(MessageBlock::Attachment(match source {
                resource::MessageImageSource::Url { url } => AttachmentFact {
                    attachment_id: block_id,
                    path: bounded_text(url, TOOL_DISPLAY_BYTES),
                    file_name: "图片".into(),
                    media_type: "image/*".into(),
                    bytes: 0,
                    image: true,
                },
                resource::MessageImageSource::Artifact { artifact } => AttachmentFact {
                    attachment_id: artifact.artifact_id.as_str().to_owned(),
                    path: String::new(),
                    file_name: "图片附件".into(),
                    media_type: artifact.media_type.clone().unwrap_or_default(),
                    bytes: artifact.size_bytes,
                    image: true,
                },
            })),
            MessagePart::Artifact { artifact, .. } => blocks.push(MessageBlock::Markdown {
                block_id,
                source: artifact_text(session, artifact, MESSAGE_DISPLAY_BYTES),
                streaming: false,
            }),
            MessagePart::ToolResult { .. } => return None,
        }
    }
    let feedback = state
        .assistant_feedback
        .iter()
        .find(|record| record.entity_id == message.message_id)
        .map(|record| match record.feedback {
            resource::AssistantFeedback::Like => AssistantFeedback::Like,
            resource::AssistantFeedback::Dislike => AssistantFeedback::Dislike,
        });
    let mut projected = UiMessage {
        message_id: message.message_id.clone(),
        turn_id: message.turn_id.as_ref().map(|id| id.as_str().to_owned()),
        role: match message.role {
            resource::MessageRole::User => MessageRole::User,
            resource::MessageRole::Assistant => MessageRole::Assistant,
            _ => MessageRole::System,
        },
        blocks,
        feedback,
        created_at_unix_ms: message
            .turn_id
            .as_ref()
            .and_then(|id| state.turns.get(id))
            .map(|turn| turn.started_at_unix_ms),
    };
    // 逐块 bounded_text 只限制单块；这里再执行消息级总量裁剪，确保单个
    // Transcript 消息不会绕过 256 KiB 文本预算堆积 128 个大块。
    cap_message_to_budget(&mut projected, MESSAGE_DISPLAY_BYTES, MAX_MESSAGE_BLOCKS);
    Some(projected)
}

pub(super) fn event_messages<'a>(event: &'a SessionEvent, output: &mut Vec<&'a SessionMessage>) {
    match event {
        SessionEvent::MessageAdded { message } => output.push(message),
        SessionEvent::TranscriptSegmentCommitted { segment } => output.extend(&segment.messages),
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                event_messages(event, output);
            }
        }
        _ => {}
    }
}

pub(super) fn conversation(
    host: &NativeHost,
    session: &RuntimeSession,
    cursor: Option<PageCursor>,
) -> Result<ConversationFact, NativeUiError> {
    let draft = host.draft(session.session_id().as_str())?;
    // 一次快照读取覆盖当前分页中的全部消息，避免每条消息重复扫描撤销 sidecar。
    let rewound_turn_ids = completed_rewind_turn_ids(host, session.session_id().as_str())?;
    let last_sequence = session
        .read_state(|state| state.last_sequence)
        .map_err(ui_error)?;
    let end = cursor
        .as_ref()
        .and_then(|cursor| cursor.before_sequence)
        .unwrap_or_else(|| last_sequence.saturating_add(1))
        .min(last_sequence.saturating_add(1));
    let mut page_end = end;
    let mut start;
    let mut pages = Vec::new();
    loop {
        start = page_end
            .saturating_sub(HISTORY_RECORDS_PER_PAGE as u64)
            .max(1);
        let count =
            usize::try_from(page_end.saturating_sub(start)).unwrap_or(HISTORY_RECORDS_PER_PAGE);
        if count == 0 {
            break;
        }
        let page = session
            .replay((start > 1).then_some(start - 1), count)
            .map_err(ui_error)?;
        let has_messages = page.records.iter().any(|record| {
            let mut messages = Vec::new();
            event_messages(&record.event, &mut messages);
            messages.iter().any(|message| {
                !message.is_meta
                    && matches!(
                        message.role,
                        resource::MessageRole::User | resource::MessageRole::Assistant
                    )
            })
        });
        pages.push(page.records);
        if has_messages || start == 1 {
            break;
        }
        // 纯控制事件页只定位历史，不保留它们，避免无正文长尾堆积。
        pages.clear();
        page_end = start;
    }
    session
        .read_state(|state| {
            let mut messages = Vec::new();
            let mut prompt_messages = Vec::new();
            for records in pages.iter().rev() {
                for record in records {
                    let mut source = Vec::new();
                    event_messages(&record.event, &mut source);
                    for message in source {
                        // 历史菜单必须从原始 Transcript 取正文；UI 消息投影会为显示
                        // 预算裁剪大块，不能把那份预览当作可复用的用户输入。
                        if !message.is_meta
                            && message
                                .agent_id
                                .as_ref()
                                .is_none_or(|id| id.as_str() == resource::ROOT_AGENT_ID)
                            && message.role == resource::MessageRole::User
                        {
                            prompt_messages.push(message);
                        }
                        if let Some(message) =
                            message_fact(session, state, message, &rewound_turn_ids)
                        {
                            messages.push(message);
                        }
                    }
                }
            }
            // 单页事件可能包含多个 Transcript 消息；在离开 Host 前再次执行总量预算，
            // 避免大批量历史先完整复制到 GPUI，再由 UI 才开始裁剪。内层 Arc 让 UI
            // 后续的流式更新只复制活动消息，不复制整页历史。
            let mut messages = messages.into_iter().map(Arc::new).collect::<Vec<_>>();
            trim_message_window(&mut messages);
            let mut fact = session_fact(host, state);
            // 事实读取可能已前进；事件订阅只能跳过本页实际读取到的 Journal 水位。
            fact.last_sequence = last_sequence;
            fact.prompt_history = prompt_history_fact(state, &prompt_messages);
            ConversationFact {
                session: fact,
                messages: Arc::new(messages),
                history: (start > 1).then_some(JournalCursor {
                    after_sequence: None,
                    before_sequence: Some(start),
                    through_sequence: last_sequence,
                }),
                input_queue: input_queue_facts(&state.input_queue),
                active_turn_id: state
                    .turns
                    .values()
                    .find(|turn| {
                        turn.source_agent_id.as_str() == resource::ROOT_AGENT_ID
                            && turn.status == resource::TurnStatus::Running
                    })
                    .map(|turn| turn.turn_id.as_str().to_owned()),
                draft,
                transcript_revision: state.transcript_revision,
            }
        })
        .map_err(ui_error)
}

pub(super) fn input_queue_facts(queue: &resource::SessionInputQueueState) -> Vec<QueuedInputFact> {
    // 所有队列项 ID 必须保留，先从预算中预留 ID，再把剩余空间分配给正文预览；
    // 否则第一个大正文可能耗尽预算，后续 ID 会让总快照再次超过上限。
    let identity_bytes = queue
        .items
        .iter()
        .map(|item| item.queue_item_id.len())
        .fold(0usize, usize::saturating_add);
    let mut remaining = MAX_QUEUE_PREVIEW_BYTES.saturating_sub(identity_bytes);
    queue
        .items
        .iter()
        .map(|item| {
            let queue_item_id = item.queue_item_id.clone();
            let (text, text_complete) = bounded_queue_preview(&item.text, remaining);
            remaining = remaining.saturating_sub(text.len());
            QueuedInputFact {
                queue_item_id,
                text,
                text_complete,
                text_fingerprint: text_fingerprint(&item.text),
                state: queue_item_state(item),
            }
        })
        .collect()
}

/// 为单个队列项建立完整事实，供用户明确请求编辑时通过 ID 读取；正文不经过预览裁剪。
pub(super) fn full_input_queue_fact(item: &resource::SessionInputQueueItem) -> QueuedInputFact {
    QueuedInputFact {
        queue_item_id: item.queue_item_id.clone(),
        text: item.text.clone(),
        text_complete: true,
        text_fingerprint: text_fingerprint(&item.text),
        state: queue_item_state(item),
    }
}

fn queue_item_state(item: &resource::SessionInputQueueItem) -> QueuedInputState {
    match item.dispatch {
        resource::SessionInputDispatch::Queued => QueuedInputState::Waiting,
        resource::SessionInputDispatch::Reserved => QueuedInputState::Reserved,
        resource::SessionInputDispatch::Promoting => QueuedInputState::Dispatching,
    }
}

fn text_fingerprint(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 至少包含八字节"))
}

fn bounded_queue_preview(value: &str, budget: usize) -> (String, bool) {
    if value.len() <= budget {
        return (value.to_owned(), true);
    }
    const SUFFIX: &str = "\n…（正文较长，点击加载完整内容）";
    if budget <= SUFFIX.len() {
        return (String::new(), false);
    }
    let mut end = budget - SUFFIX.len();
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = value[..end].to_owned();
    preview.push_str(SUFFIX);
    (preview, false)
}

fn prompt_history_fact(state: &SessionState, messages: &[&SessionMessage]) -> PromptHistoryFact {
    let mut remaining = MAX_PROMPT_HISTORY_BYTES;
    let mut entries = Vec::new();
    let mut omitted = false;
    for (considered, message) in messages
        .iter()
        .rev()
        .filter(|message| {
            !message.is_meta
                && message
                    .agent_id
                    .as_ref()
                    .is_none_or(|id| id.as_str() == resource::ROOT_AGENT_ID)
                && message.role == resource::MessageRole::User
        })
        .enumerate()
    {
        if considered >= 50 {
            omitted = true;
            break;
        }
        let text = message
            .content
            .iter()
            .filter_map(|part| {
                if let MessagePart::Text { text } = part {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<String>();
        let entry_bytes = message.message_id.len().saturating_add(text.len());
        if entry_bytes > remaining {
            // 历史条目只能完整进入菜单；SelectPromptHistory 会按 entry_id 从
            // Runtime 重新读取正文，因此跳过条目不会损坏可复用的原文。
            omitted = true;
            continue;
        }
        remaining = remaining.saturating_sub(entry_bytes);
        entries.push(PromptHistoryEntry {
            entry_id: message.message_id.clone(),
            text,
            created_at_unix_ms: message
                .turn_id
                .as_ref()
                .and_then(|id| state.turns.get(id))
                .map(|turn| turn.started_at_unix_ms)
                .unwrap_or(0),
        });
    }
    PromptHistoryFact {
        entries,
        selected: None,
        omitted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_item(sequence: u64, text: String) -> resource::SessionInputQueueItem {
        resource::SessionInputQueueItem {
            queue_item_id: format!("queue-{sequence}"),
            source_command_id: format!("command-{sequence}"),
            client_id: None,
            kind: resource::SessionInputKind::SendText,
            text,
            attachments: Vec::new(),
            model_selection: None,
            mode: None,
            plan_enabled: false,
            requested_delivery: resource::SessionInputDelivery::Queue,
            admitted_delivery: resource::SessionInputDelivery::Queue,
            admission_seq: sequence,
            reserve_attempt: 0,
            dispatch: resource::SessionInputDispatch::Queued,
            promoted_turn_id: None,
            admitted_at_unix_ms: 0,
        }
    }

    #[test]
    fn large_queue_projection_is_bounded_and_full_loader_preserves_text() {
        let full_text = "x".repeat(1024 * 1024);
        let queue = resource::SessionInputQueueState {
            items: (1..=256)
                .map(|sequence| queue_item(sequence, full_text.clone()))
                .collect(),
            completions: Vec::new(),
            auto_drain: true,
            pause_reason: None,
            next_admission_seq: 257,
        };

        let projected = input_queue_facts(&queue);
        let projected_bytes = projected
            .iter()
            .map(|item| item.queue_item_id.len().saturating_add(item.text.len()))
            .fold(0usize, usize::saturating_add);
        assert_eq!(projected.len(), 256);
        assert!(projected_bytes <= MAX_QUEUE_PREVIEW_BYTES);
        assert!(projected.iter().any(|item| !item.text_complete));

        let loaded = full_input_queue_fact(&queue.items[0]);
        assert!(loaded.text_complete);
        assert_eq!(loaded.text, full_text);
        assert_eq!(loaded.text_fingerprint, projected[0].text_fingerprint);
    }

    fn user_message(message_id: &str, text: String) -> SessionMessage {
        SessionMessage {
            is_meta: false,
            references: Vec::new(),
            message_id: message_id.to_owned(),
            turn_id: None,
            agent_id: None,
            role: resource::MessageRole::User,
            content: vec![MessagePart::Text { text }],
        }
    }

    #[test]
    fn prompt_history_keeps_complete_latest_entries_with_total_budget() {
        let messages = (0..50)
            .map(|index| user_message(&format!("message-{index}"), "用户输入".repeat(8_000)))
            .collect::<Vec<_>>();
        let references = messages.iter().collect::<Vec<_>>();
        let state = SessionState::empty(resource::SessionId::new("history-test").unwrap());

        let history = prompt_history_fact(&state, &references);
        let history_bytes = history
            .entries
            .iter()
            .map(|entry| entry.entry_id.len().saturating_add(entry.text.len()))
            .fold(0usize, usize::saturating_add);
        assert!(history.omitted);
        assert!(history_bytes <= MAX_PROMPT_HISTORY_BYTES);
        assert!(
            history
                .entries
                .iter()
                .all(|entry| entry.text == "用户输入".repeat(8_000))
        );
        assert_eq!(
            history.entries.first().map(|entry| entry.entry_id.as_str()),
            Some("message-49")
        );
    }

    #[test]
    fn prompt_history_skips_oversized_and_non_root_messages() {
        let mut messages = vec![
            user_message("old", "旧输入".to_owned()),
            user_message(
                "too-large",
                "x".repeat(MAX_PROMPT_HISTORY_BYTES.saturating_add(1)),
            ),
            user_message("latest", "最新输入\n完整正文".to_owned()),
        ];
        let mut meta = user_message("meta", "内部输入".to_owned());
        meta.is_meta = true;
        messages.push(meta);
        let mut child = user_message("child", "子 Agent 输入".to_owned());
        child.agent_id = Some(resource::AgentId::new("child").unwrap());
        messages.push(child);

        let references = messages.iter().collect::<Vec<_>>();
        let state = SessionState::empty(resource::SessionId::new("history-filter-test").unwrap());
        let history = prompt_history_fact(&state, &references);

        assert!(history.omitted);
        assert_eq!(
            history
                .entries
                .iter()
                .map(|entry| entry.entry_id.as_str())
                .collect::<Vec<_>>(),
            vec!["latest", "old"]
        );
        assert_eq!(history.entries[0].text, "最新输入\n完整正文");
        assert_eq!(history.entries[1].text, "旧输入");
        let history_bytes = history
            .entries
            .iter()
            .map(|entry| entry.entry_id.len().saturating_add(entry.text.len()))
            .fold(0usize, usize::saturating_add);
        assert!(history_bytes <= MAX_PROMPT_HISTORY_BYTES);
    }
}
