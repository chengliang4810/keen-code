//! Native GPUI 工作台的事实、动作和宿主端口。
//!
//! 这些类型是桌面 UI 与 `NativeHost` 的唯一业务边界。UI 只保存当前投影和草稿，
//! Journal、Provider、权限与文件系统的事实仍由宿主负责。事件带有 Session 内的
//! Journal sequence，宿主可以在发生丢帧时要求 UI 丢弃投影并重新分页恢复。

use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use sha2::{Digest, Sha256};

/// 有界的历史页和会话列表页大小。
pub const DEFAULT_PAGE_SIZE: usize = 50;
pub const MAX_PAGE_SIZE: usize = 200;

/// 原生聊天正文的显示预算。超出部分从最旧端卸载，`ConversationFact.history`
/// 仍保留宿主游标，因此卸载的历史可以继续从 Journal 分页读取。
pub const MAX_DISPLAY_MESSAGES: usize = 200;
pub const MAX_DISPLAY_BYTES: usize = 8 * 1024 * 1024;

/// Snapshot 会话元数据、草稿和队列预览共用的独立邮箱预算。
pub const MAX_SNAPSHOT_METADATA_BYTES: usize = 2 * 1024 * 1024;
/// Prompt 历史只保留完整条目；超出预算的条目从最新到最旧跳过。
pub const MAX_PROMPT_HISTORY_BYTES: usize = 256 * 1024;
/// 队列 Snapshot 只携带正文预览，完整正文按稳定队列项 ID 显式读取。
pub const MAX_QUEUE_PREVIEW_BYTES: usize = 256 * 1024;

/// 活动 streaming/hot 尾部的文本预算；正文总预算之外不再保留第二份无限增长的流。
pub const MAX_ACTIVE_TAIL_BYTES: usize = 2 * 1024 * 1024;

/// 宿主异步操作的统一返回类型。
pub type NativeUiTask<T> = Pin<Box<dyn Future<Output = Result<T, NativeUiError>> + Send>>;

/// 原生窗口只保留有限的流式文本，避免慢速渲染或断开连接时 UI 内存无限增长。
pub const MAX_STREAM_TEXT_BYTES: usize = MAX_ACTIVE_TAIL_BYTES;

/// Native UI 请求失败；错误文本来自宿主的已脱敏诊断。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeUiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl NativeUiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }
}

impl std::fmt::Display for NativeUiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for NativeUiError {}

/// Journal 分页游标；只按宿主确认的 sequence 追赶，不在 UI 生成第二套修订号。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JournalCursor {
    pub after_sequence: Option<u64>,
    pub before_sequence: Option<u64>,
    pub through_sequence: u64,
}

pub type PageCursor = JournalCursor;

/// 项目在侧栏中的权威投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFact {
    pub project_key: String,
    pub display_name: String,
    pub root_path: String,
    /// 项目分组的当前 CAS revision；即使所有分组暂时为空也必须保留，避免
    /// “移出最后一个分组”后 UI 用旧的 0 revision 覆盖宿主状态。
    pub group_revision: u64,
    pub session_count: usize,
    pub unread_count: usize,
    pub archived: bool,
    pub pinned: bool,
}

/// 会话状态只保留侧栏需要的元数据；正文由 `ConversationFact` 分页提供。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFact {
    pub session_id: String,
    pub project_key: String,
    pub title: String,
    pub status: SessionStatus,
    pub pinned: bool,
    pub archived: bool,
    pub unread: bool,
    pub model: Option<ModelSelection>,
    pub vision_enabled: bool,
    pub effort: Option<String>,
    pub permission: PermissionMode,
    pub plan: PlanMode,
    pub followup_mode: FollowupMode,
    pub usage: Option<UsageFact>,
    pub prompt_history: PromptHistoryFact,
    pub updated_at_unix_ms: u64,
    pub last_sequence: u64,
}

/// 宿主从 Journal 归约得到的会话状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Queued,
    Running,
    Waiting,
    Completed,
    Failed,
    Interrupted,
    Corrupt,
}

/// 侧栏项目/会话页；`next` 为空表示已读到同一快照的末尾。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePage {
    pub projects: Vec<ProjectFact>,
    pub sessions: Vec<SessionFact>,
    /// 项目侧栏分组快照；同一项目内各组共享当前 revision，提交分组成员时必须回传该值。
    pub groups: Vec<GroupFact>,
    pub next: Option<PageCursor>,
}

/// 侧栏分组事实由 NativeSidebarStore 持久化，UI 不根据 Session 列表推断分组。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupFact {
    pub group_id: String,
    pub project_key: String,
    pub root_path: String,
    pub title: String,
    pub session_ids: Vec<String>,
    pub revision: u64,
}

/// 模型选择的无凭据展示值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model: String,
}

/// Provider 目录中的模型和推理强度。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelCatalog {
    pub provider_id: String,
    pub provider_name: String,
    pub models: Vec<ModelOption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelOption {
    pub model: String,
    pub label: String,
    pub reasoning_efforts: Vec<String>,
    pub supports_images: bool,
    /// Host 当前激活的默认模型；UI 只能读取该投影，不能自行写回配置。
    pub is_default: bool,
}

/// 会话级权限选择；实际工具审批仍由宿主权限门和 Journal 决定。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PermissionMode {
    #[default]
    Build,
    Edit,
    Plan,
    Yolo,
}

/// Plan 模式状态只能由宿主确认后更新。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlanMode {
    #[default]
    Off,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FollowupMode {
    #[default]
    Queue,
    Guide,
}

/// 当前打开会话的有界投影。历史页只追加到头部，流式内容只改最后一个块。
/// 外层 `Arc` 共享消息指针向量，内层 `Arc` 让流式更新只复制正在变化的一条消息，
/// 不会因为渲染快照仍持有旧窗口而复制全部历史正文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationFact {
    pub session: SessionFact,
    pub messages: Arc<Vec<Arc<UiMessage>>>,
    pub history: Option<PageCursor>,
    pub input_queue: Vec<QueuedInputFact>,
    pub active_turn_id: Option<String>,
    pub draft: DraftFact,
    pub transcript_revision: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftFact {
    pub text: String,
    pub attachments: Vec<AttachmentFact>,
    pub mention_query: Option<MentionQuery>,
}

/// 会话级模型统计；未知值保持 `None`，不能把缺失数据显示为零。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageFact {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub context_window: Option<u64>,
    pub duration_ms: Option<u64>,
    pub tokens_per_second: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptHistoryFact {
    pub entries: Vec<PromptHistoryEntry>,
    pub selected: Option<usize>,
    /// 存在因条目上限或总字节预算未展示的历史；点击已展示条目仍按 ID 读取完整正文。
    pub omitted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptHistoryEntry {
    pub entry_id: String,
    pub text: String,
    pub created_at_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionQuery {
    pub trigger: char,
    pub query: String,
    pub candidates: Vec<MentionCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MentionCandidate {
    pub id: String,
    pub label: String,
    pub kind: MentionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MentionKind {
    File,
    Session,
    Agent,
    Symbol,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedInputFact {
    pub queue_item_id: String,
    pub text: String,
    /// false 表示当前仅为 Snapshot 预览，编辑前必须按 queue_item_id 读取完整正文。
    pub text_complete: bool,
    /// 用于在权威文本变化或预览/完整正文切换时重建输入控件，不携带正文语义。
    pub text_fingerprint: u64,
    pub state: QueuedInputState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedInputState {
    Waiting,
    Reserved,
    Dispatching,
}

/// 一条展示消息；稳定的 `message_id` 使 GPUI 只重绘变化的尾部。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiMessage {
    pub message_id: String,
    pub turn_id: Option<String>,
    pub role: MessageRole,
    pub blocks: Vec<MessageBlock>,
    pub feedback: Option<AssistantFeedback>,
    pub created_at_unix_ms: Option<u64>,
}

/// 计算一条消息在 UI 投影中占用的可见文本字节数。
pub fn message_display_bytes(message: &UiMessage) -> usize {
    let mut bytes = message
        .message_id
        .len()
        .saturating_add(message.turn_id.as_deref().map_or(0, str::len))
        .saturating_add(message.feedback.map_or(0, |_| 1));
    for block in &message.blocks {
        bytes = bytes.saturating_add(message_block_display_bytes(block));
    }
    bytes
}

fn message_block_display_bytes(block: &MessageBlock) -> usize {
    match block {
        MessageBlock::Markdown {
            block_id, source, ..
        }
        | MessageBlock::Reasoning {
            block_id, source, ..
        } => block_id.len().saturating_add(source.len()),
        MessageBlock::Tool(tool) => tool
            .request_id
            .len()
            .saturating_add(tool.name.len())
            .saturating_add(tool.arguments_json.len())
            .saturating_add(tool.output_markdown.as_deref().map_or(0, str::len)),
        MessageBlock::Approval(approval) => approval
            .request_id
            .len()
            .saturating_add(approval.name.len())
            .saturating_add(approval.arguments_json.len()),
        MessageBlock::Question(question) => {
            let choices = question.choices.iter().fold(0usize, |total, choice| {
                total
                    .saturating_add(choice.id.len())
                    .saturating_add(choice.label.len())
                    .saturating_add(choice.description.as_deref().map_or(0, str::len))
            });
            question
                .request_id
                .len()
                .saturating_add(question.prompt_markdown.len())
                .saturating_add(choices)
                .saturating_add(question.answer.as_deref().map_or(0, str::len))
        }
        MessageBlock::FileChange(change) => change.path.len(),
        MessageBlock::Attachment(attachment) => attachment
            .attachment_id
            .len()
            .saturating_add(attachment.path.len())
            .saturating_add(attachment.file_name.len())
            .saturating_add(attachment.media_type.len()),
        MessageBlock::Error {
            block_id,
            code,
            message,
        } => block_id
            .len()
            .saturating_add(code.len())
            .saturating_add(message.len()),
    }
}

fn take_text(value: &mut String, remaining: &mut usize) {
    if value.len() <= *remaining {
        *remaining -= value.len();
        // 即使正文没有被截断，也不能让历史流式扩容留下超出字节预算的容量。
        value.shrink_to_fit();
        return;
    }
    let mut end = *remaining;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    // 仅超预算时释放原大缓冲；保留容量会让文本长度上限无法约束实际驻留内存。
    value.shrink_to_fit();
    *remaining = 0;
}

/// 路由 ID、工具名和文件路径不能随展示预算截断；无法完整保留时丢弃整个块。
fn block_identity_bytes(block: &MessageBlock) -> usize {
    match block {
        MessageBlock::Markdown { block_id, .. } | MessageBlock::Reasoning { block_id, .. } => {
            block_id.len()
        }
        MessageBlock::Tool(tool) => tool.request_id.len().saturating_add(tool.name.len()),
        MessageBlock::Approval(approval) => approval
            .request_id
            .len()
            .saturating_add(approval.name.len()),
        MessageBlock::Question(question) => question
            .choices
            .iter()
            .fold(question.request_id.len(), |bytes, choice| {
                bytes.saturating_add(choice.id.len())
            }),
        MessageBlock::FileChange(_) | MessageBlock::Attachment(_) => {
            message_block_display_bytes(block)
        }
        MessageBlock::Error { block_id, code, .. } => block_id.len().saturating_add(code.len()),
    }
}

fn cap_block_for_display(block: &mut MessageBlock, remaining: &mut usize) -> bool {
    let identity_bytes = block_identity_bytes(block);
    if identity_bytes > *remaining {
        return false;
    }
    *remaining -= identity_bytes;
    match block {
        MessageBlock::Markdown { source, .. } | MessageBlock::Reasoning { source, .. } => {
            take_text(source, remaining);
        }
        MessageBlock::Tool(tool) => {
            take_text(&mut tool.arguments_json, remaining);
            if let Some(output) = tool.output_markdown.as_mut() {
                take_text(output, remaining);
            }
        }
        MessageBlock::Approval(approval) => {
            take_text(&mut approval.arguments_json, remaining);
        }
        MessageBlock::Question(question) => {
            take_text(&mut question.prompt_markdown, remaining);
            for choice in &mut question.choices {
                take_text(&mut choice.label, remaining);
                if let Some(description) = choice.description.as_mut() {
                    take_text(description, remaining);
                }
            }
            if let Some(answer) = question.answer.as_mut() {
                take_text(answer, remaining);
            }
        }
        MessageBlock::FileChange(_) | MessageBlock::Attachment(_) => {}
        MessageBlock::Error { message, .. } => {
            take_text(message, remaining);
        }
    }
    true
}

pub(crate) fn cap_message_to_budget(message: &mut UiMessage, budget: usize, max_blocks: usize) {
    let fixed = message
        .message_id
        .len()
        .saturating_add(message.turn_id.as_deref().map_or(0, str::len))
        .saturating_add(message.feedback.map_or(0, |_| 1));
    let mut remaining = budget.saturating_sub(fixed);
    let mut retained = 0;
    for block in &mut message.blocks {
        if retained >= max_blocks || remaining == 0 || !cap_block_for_display(block, &mut remaining)
        {
            break;
        }
        retained += 1;
    }
    // 预算耗尽后必须释放剩余块；仅停止遍历会留下未裁剪的正文，绕过内存上限。
    message.blocks.truncate(retained);
}

fn cap_message_for_display(message: &mut UiMessage) {
    cap_message_to_budget(message, MAX_DISPLAY_BYTES, usize::MAX);
}

/// 将当前投影限制在固定消息数和文本字节预算内，并保留最新活动尾部。
pub fn trim_message_window(messages: &mut Vec<Arc<UiMessage>>) -> bool {
    let mut bytes = messages
        .iter()
        .map(|message| message_display_bytes(message))
        .fold(0usize, usize::saturating_add);
    let mut trimmed = false;
    while (messages.len() > MAX_DISPLAY_MESSAGES || bytes > MAX_DISPLAY_BYTES)
        && !messages.is_empty()
    {
        if messages.len() == 1 {
            cap_message_for_display(Arc::make_mut(&mut messages[0]));
            if message_display_bytes(&messages[0]) > MAX_DISPLAY_BYTES {
                messages.clear();
            }
            trimmed = true;
            break;
        }
        let removed = messages.remove(0);
        bytes = bytes.saturating_sub(message_display_bytes(removed.as_ref()));
        trimmed = true;
    }
    trimmed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// Markdown、reasoning、工具和交互请求均来自同一份 Journal 投影。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageBlock {
    Markdown {
        block_id: String,
        source: String,
        streaming: bool,
    },
    Reasoning {
        block_id: String,
        source: String,
        streaming: bool,
    },
    Tool(ToolFact),
    Approval(ToolApprovalFact),
    Question(QuestionFact),
    FileChange(FileChangeFact),
    Attachment(AttachmentFact),
    Error {
        block_id: String,
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolFact {
    pub request_id: String,
    pub name: String,
    pub arguments_json: String,
    pub output_markdown: Option<String>,
    pub status: ToolStatus,
    pub effect: ToolEffect,
    pub started_at_unix_ms: Option<u64>,
    pub completed_at_unix_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Requested,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    SideEffectUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolEffect {
    ReadOnly,
    ChangesState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolApprovalFact {
    pub request_id: String,
    pub name: String,
    pub arguments_json: String,
    pub effect: ToolEffect,
    pub can_approve: bool,
    pub can_deny: bool,
    pub expires_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionFact {
    pub request_id: String,
    pub prompt_markdown: String,
    pub choices: Vec<QuestionChoice>,
    /// AskUser 的结构化多选标志；多选回答必须以 JSON 数组提交。
    pub multi_select: bool,
    pub allow_freeform: bool,
    pub answered: bool,
    /// 宿主确认后的原始答案，用于重绘后显示已回答内容。
    pub answer: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChangeFact {
    pub path: String,
    pub operation: FileChangeOperation,
    pub applied: bool,
    pub rewindable: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChangeOperation {
    Create,
    Modify,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentFact {
    pub attachment_id: String,
    pub path: String,
    pub file_name: String,
    pub media_type: String,
    pub bytes: u64,
    pub image: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistantFeedback {
    Like,
    Dislike,
}

/// 一次 Journal/Runtime 批次。`delivery_sequence` 发现缺口时必须 resync。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeEventBatch {
    pub session_id: String,
    pub delivery_sequence: u64,
    pub journal_sequence: u64,
    pub events: Vec<NativeUiEvent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeUiEvent {
    Snapshot(Box<ConversationFact>),
    /// 按稳定 `message_id` 写入一条消息；已存在的 transient/历史消息会被替换。
    MessageUpserted(UiMessage),
    /// 移除 transient 消息或宿主确认已删除的消息。
    RemoveMessage {
        message_id: String,
    },
    /// 为已存在的消息追加一个首次出现的 block；后续内容由对应 Delta 原地追加。
    MessageBlockAppended {
        message_id: String,
        block: MessageBlock,
    },
    MarkdownDelta {
        message_id: String,
        block_id: String,
        append: String,
        completed: bool,
    },
    ReasoningDelta {
        message_id: String,
        block_id: String,
        append: String,
        completed: bool,
    },
    ToolChanged(ToolFact),
    ApprovalChanged(ToolApprovalFact),
    QuestionChanged(QuestionFact),
    /// 宿主确认输入队列持久化后的最新投影。
    InputQueueChanged(Vec<QueuedInputFact>),
    /// 按稳定队列项 ID 显式读取的完整正文；发送动作不依赖该 UI 事件。
    QueuedInputLoaded(QueuedInputFact),
    /// 宿主确认草稿持久化后的最新投影。
    DraftChanged(DraftFact),
    TurnChanged {
        turn_id: String,
        status: SessionStatus,
    },
    SessionChanged(SessionFact),
    HistoryInvalidated {
        transcript_revision: u64,
    },
    /// 订阅器被宿主明确关闭；UI 应取消自身任务并释放句柄。
    Closed,
    /// 不提供伪造的连续事件，要求 UI 重新读取 Snapshot/Journal。
    ResyncRequired {
        first_missing_delivery_sequence: u64,
        last_missing_delivery_sequence: u64,
    },
}

impl NativeEventBatch {
    /// 返回事件批次占用的文本字节数，供 Native UI 邮箱执行进程内总量限流。
    ///
    /// 这不是序列化大小，而是 UI 可能保留的字符串总量估算。所有字段都使用
    /// `saturating_add`，异常大输入也只能触发丢批重同步，不能绕过预算或溢出。
    pub(crate) fn mailbox_text_bytes(&self) -> usize {
        self.session_id.len().saturating_add(
            self.events
                .iter()
                .map(native_event_text_bytes)
                .fold(0usize, usize::saturating_add),
        )
    }

    /// Snapshot 正文达到显示上限时，仍为会话元数据、草稿和输入队列保留独立的
    /// 有界预算；其余事件不享受该预留，避免把普通增量批次的 8 MiB 上限放大。
    pub(crate) fn mailbox_snapshot_metadata_bytes(&self) -> usize {
        if !self
            .events
            .iter()
            .any(|event| matches!(event, NativeUiEvent::Snapshot(_)))
        {
            return 0;
        }
        self.session_id.len().saturating_add(
            self.events
                .iter()
                .map(native_event_snapshot_metadata_bytes)
                .fold(0usize, usize::saturating_add),
        )
    }
}

fn session_text_bytes(session: &SessionFact) -> usize {
    session
        .session_id
        .len()
        .saturating_add(session.project_key.len())
        .saturating_add(session.title.len())
        .saturating_add(session.model.as_ref().map_or(0, |model| {
            model.provider_id.len().saturating_add(model.model.len())
        }))
        .saturating_add(session.effort.as_deref().map_or(0, str::len))
        .saturating_add(
            session
                .prompt_history
                .entries
                .iter()
                .map(|entry| entry.entry_id.len().saturating_add(entry.text.len()))
                .fold(0usize, usize::saturating_add),
        )
}

fn draft_text_bytes(draft: &DraftFact) -> usize {
    draft
        .text
        .len()
        .saturating_add(
            draft
                .attachments
                .iter()
                .map(|attachment| {
                    attachment
                        .attachment_id
                        .len()
                        .saturating_add(attachment.path.len())
                        .saturating_add(attachment.file_name.len())
                        .saturating_add(attachment.media_type.len())
                })
                .fold(0usize, usize::saturating_add),
        )
        .saturating_add(draft.mention_query.as_ref().map_or(0, |query| {
            query.query.len().saturating_add(
                query
                    .candidates
                    .iter()
                    .map(|candidate| candidate.id.len().saturating_add(candidate.label.len()))
                    .fold(0usize, usize::saturating_add),
            )
        }))
}

fn input_queue_text_bytes(queue: &[QueuedInputFact]) -> usize {
    queue
        .iter()
        .map(|item| item.queue_item_id.len().saturating_add(item.text.len()))
        .fold(0usize, usize::saturating_add)
}

fn queue_text_fingerprint(text: &str) -> u64 {
    let digest = Sha256::digest(text.as_bytes());
    u64::from_le_bytes(digest[..8].try_into().expect("SHA-256 至少包含八字节"))
}

fn bounded_queue_preview(value: &str, budget: usize) -> String {
    const SUFFIX: &str = "\n…（正文较长，点击加载完整内容）";
    if value.len() <= budget {
        return value.to_owned();
    }
    if budget <= SUFFIX.len() {
        return String::new();
    }
    let mut end = budget - SUFFIX.len();
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = value[..end].to_owned();
    preview.push_str(SUFFIX);
    preview
}

/// 读取另一个完整队列项时，只回收已经和权威 fingerprint 对齐的旧正文。
/// fingerprint 不匹配说明 UI 可能仍持有未确认编辑，此时宁可暂留正文，也不能静默覆盖。
fn bound_loaded_queue_items(queue: &mut [QueuedInputFact], keep_item_id: &str) {
    let identity_bytes = queue
        .iter()
        .map(|item| item.queue_item_id.len())
        .fold(0usize, usize::saturating_add);
    let mut remaining = MAX_QUEUE_PREVIEW_BYTES.saturating_sub(identity_bytes);
    for item in queue {
        if item.queue_item_id == keep_item_id && item.text_complete {
            continue;
        }
        let was_complete = item.text_complete;
        item.text = bounded_queue_preview(&item.text, remaining);
        remaining = remaining.saturating_sub(item.text.len());
        if was_complete {
            item.text_complete = false;
        }
    }
}

fn question_text_bytes(question: &QuestionFact) -> usize {
    question
        .request_id
        .len()
        .saturating_add(question.prompt_markdown.len())
        .saturating_add(question.answer.as_deref().map_or(0, str::len))
        .saturating_add(
            question
                .choices
                .iter()
                .map(|choice| {
                    choice
                        .id
                        .len()
                        .saturating_add(choice.label.len())
                        .saturating_add(choice.description.as_deref().map_or(0, str::len))
                })
                .fold(0usize, usize::saturating_add),
        )
}

fn tool_text_bytes(tool: &ToolFact) -> usize {
    tool.request_id
        .len()
        .saturating_add(tool.name.len())
        .saturating_add(tool.arguments_json.len())
        .saturating_add(tool.output_markdown.as_deref().map_or(0, str::len))
}

fn approval_text_bytes(approval: &ToolApprovalFact) -> usize {
    approval
        .request_id
        .len()
        .saturating_add(approval.name.len())
        .saturating_add(approval.arguments_json.len())
}

fn native_event_text_bytes(event: &NativeUiEvent) -> usize {
    match event {
        NativeUiEvent::Snapshot(snapshot) => session_text_bytes(&snapshot.session)
            .saturating_add(
                snapshot
                    .messages
                    .iter()
                    .map(|message| message_display_bytes(message))
                    .fold(0usize, usize::saturating_add),
            )
            .saturating_add(input_queue_text_bytes(&snapshot.input_queue))
            .saturating_add(draft_text_bytes(&snapshot.draft))
            .saturating_add(snapshot.active_turn_id.as_deref().map_or(0, str::len)),
        NativeUiEvent::MessageUpserted(message) => message_display_bytes(message),
        NativeUiEvent::RemoveMessage { message_id } => message_id.len(),
        NativeUiEvent::MessageBlockAppended { message_id, block } => message_id
            .len()
            .saturating_add(message_block_display_bytes(block)),
        NativeUiEvent::MarkdownDelta {
            message_id,
            block_id,
            append,
            ..
        }
        | NativeUiEvent::ReasoningDelta {
            message_id,
            block_id,
            append,
            ..
        } => message_id
            .len()
            .saturating_add(block_id.len())
            .saturating_add(append.len()),
        NativeUiEvent::ToolChanged(tool) => tool_text_bytes(tool),
        NativeUiEvent::ApprovalChanged(approval) => approval_text_bytes(approval),
        NativeUiEvent::QuestionChanged(question) => question_text_bytes(question),
        NativeUiEvent::InputQueueChanged(queue) => input_queue_text_bytes(queue),
        NativeUiEvent::QueuedInputLoaded(item) => {
            item.queue_item_id.len().saturating_add(item.text.len())
        }
        NativeUiEvent::DraftChanged(draft) => draft_text_bytes(draft),
        NativeUiEvent::TurnChanged { turn_id, .. } => turn_id.len(),
        NativeUiEvent::SessionChanged(session) => session_text_bytes(session),
        NativeUiEvent::HistoryInvalidated { .. }
        | NativeUiEvent::Closed
        | NativeUiEvent::ResyncRequired { .. } => 0,
    }
}

fn native_event_snapshot_metadata_bytes(event: &NativeUiEvent) -> usize {
    match event {
        NativeUiEvent::Snapshot(snapshot) => session_text_bytes(&snapshot.session)
            .saturating_add(input_queue_text_bytes(&snapshot.input_queue))
            .saturating_add(draft_text_bytes(&snapshot.draft))
            .saturating_add(snapshot.active_turn_id.as_deref().map_or(0, str::len)),
        _ => 0,
    }
}

/// UI 提交给 NativeHost 的业务动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeUiAction {
    /// 选择已登记的项目根目录，并刷新侧栏投影。
    OpenProject { project_root: String },
    LoadWorkspace {
        root_path: String,
        cursor: Option<PageCursor>,
        include_archived: bool,
    },
    /// 目录为空时使用宿主默认项目目录；提供 path 时登记已有目录或创建目标目录。
    CreateProject {
        path: Option<String>,
        name: String,
        create_workspace_root_if_missing: bool,
        operation_id: String,
    },
    RenameProject {
        project_root: String,
        name: String,
        operation_id: String,
    },
    /// Forget 只移除项目登记，保留项目目录和既有会话文件。
    ForgetProject {
        project_root: String,
        operation_id: String,
    },
    ReorderProjects {
        project_roots: Vec<String>,
        operation_id: String,
    },
    CreateSession {
        project_root: String,
        operation_id: String,
    },
    OpenSession {
        session_id: String,
        project_root: String,
    },
    RenameSession {
        session_id: String,
        title: String,
        operation_id: String,
    },
    DeleteSession {
        session_id: String,
        operation_id: String,
    },
    SetSessionArchived {
        session_id: String,
        archived: bool,
        operation_id: String,
    },
    SetSessionPinned {
        session_id: String,
        pinned: bool,
        operation_id: String,
    },
    GroupSessions {
        project_key: String,
        group_id: String,
        session_ids: Vec<String>,
        expected_revision: u64,
        operation_id: String,
    },
    SetDraft {
        session_id: String,
        draft: DraftFact,
        /// 单调递增的本地编辑代次；Host 用它丢弃发送前排队的迟到写入。
        edit_generation: u64,
    },
    Send {
        session_id: String,
        text: String,
        attachments: Vec<AttachmentFact>,
        model: Option<ModelSelection>,
        effort: Option<String>,
        permission: PermissionMode,
        plan: PlanMode,
        /// Send 接纳时看到的编辑代次，清理草稿后同代及更旧的 SetDraft 均失效。
        draft_edit_generation: u64,
        operation_id: String,
    },
    SetModel {
        session_id: String,
        selection: ModelSelection,
        operation_id: String,
    },
    SetEffort {
        session_id: String,
        effort: String,
        operation_id: String,
    },
    SetVision {
        session_id: String,
        enabled: bool,
        operation_id: String,
    },
    SetPermission {
        session_id: String,
        mode: PermissionMode,
        operation_id: String,
    },
    SetPlan {
        session_id: String,
        mode: PlanMode,
        operation_id: String,
    },
    SetFollowupMode {
        session_id: String,
        mode: FollowupMode,
        operation_id: String,
    },
    Stop {
        session_id: String,
        operation_id: String,
    },
    LoadHistory {
        session_id: String,
        cursor: PageCursor,
    },
    /// 丢弃已加载的旧页，重新读取会话尾部，供用户快速回到最新消息。
    LoadLatest { session_id: String },
    ColdRestore {
        session_id: String,
        operation_id: String,
    },
    Rewind {
        session_id: String,
        target_turn_id: String,
        operation_id: String,
    },
    Branch {
        session_id: String,
        through_turn_id: Option<String>,
        operation_id: String,
    },
    Feedback {
        session_id: String,
        message_id: String,
        feedback: Option<AssistantFeedback>,
        expected_transcript_revision: u64,
        operation_id: String,
    },
    ApproveTool {
        session_id: String,
        request_id: String,
        approved: bool,
        operation_id: String,
    },
    AnswerQuestion {
        session_id: String,
        request_id: String,
        answer: String,
        operation_id: String,
    },
    AddAttachment {
        session_id: String,
        path: String,
        operation_id: String,
    },
    RemoveAttachment {
        session_id: String,
        attachment_id: String,
    },
    /// 按队列项稳定 ID 读取完整正文；只用于编辑预览，不参与发送语义。
    LoadQueuedInput {
        session_id: String,
        queue_item_id: String,
    },
    EditQueuedInput {
        session_id: String,
        queue_item_id: String,
        text: String,
        operation_id: String,
    },
    ReorderQueuedInput {
        session_id: String,
        queue_item_ids: Vec<String>,
        operation_id: String,
    },
    SendQueuedInput {
        session_id: String,
        queue_item_id: String,
        operation_id: String,
    },
    DeleteQueuedInput {
        session_id: String,
        queue_item_id: String,
        operation_id: String,
    },
    SelectPromptHistory {
        session_id: String,
        entry_id: String,
    },
}

/// 统一的操作回执；只有宿主确认 Journal/文件边界后 UI 才更新事实。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeActionReceipt {
    pub operation_id: String,
    pub session_id: Option<String>,
    pub accepted_sequence: Option<u64>,
}

/// 当前会话订阅的句柄；丢弃或显式取消都会停止宿主事件任务。
pub trait NativeUiSubscription: Send {
    fn cancel(&mut self);
}

/// NativeHost 必须实现的 typed API。所有业务动作都有明确的异步回执，
/// 不允许 UI 仅修改本地状态制造“成功”。
pub trait NativeHostApi: Send + Sync {
    fn load_workspace(
        &self,
        root_path: String,
        cursor: Option<PageCursor>,
        include_archived: bool,
    ) -> NativeUiTask<WorkspacePage>;
    fn model_catalog(&self) -> NativeUiTask<Vec<ModelCatalog>>;
    fn load_conversation(
        &self,
        session_id: String,
        cursor: Option<PageCursor>,
    ) -> NativeUiTask<ConversationFact>;
    fn dispatch(&self, action: NativeUiAction) -> NativeUiTask<NativeActionReceipt>;
    fn subscribe_session(
        &self,
        session_id: String,
        sink: Arc<dyn Fn(NativeEventBatch) + Send + Sync>,
    ) -> NativeUiTask<Box<dyn NativeUiSubscription>>;
}

/// UI 可丢弃的当前投影；不缓存项目全量正文，只缓存当前页面和会话尾部。
#[derive(Clone, Debug, Default)]
pub struct NativeUiState {
    pub workspace_root: Option<String>,
    pub workspace: Option<WorkspacePage>,
    pub conversation: Option<ConversationFact>,
    /// 模型目录在同一帧的多个输入组件之间共享；目录只在宿主刷新时替换。
    pub model_catalog: Arc<Vec<ModelCatalog>>,
    pub pending_operations: BTreeMap<String, NativeUiAction>,
    /// 最近一次加载完整队列正文被本地未确认编辑阻止的原因；工作台消费后显示，
    /// 不把错误状态伪装成队列事实，也不会覆盖本地仍需保存的正文。
    queue_load_rejection: Option<String>,
}

impl NativeUiState {
    pub(crate) fn take_queue_load_rejection(&mut self) -> Option<String> {
        self.queue_load_rejection.take()
    }

    /// 仅更新 streaming tail，保持已完成消息的稳定分配和历史页边界。
    pub fn apply_event(&mut self, event: NativeUiEvent) -> Option<String> {
        self.queue_load_rejection = None;
        let Some(conversation) = self.conversation.as_mut() else {
            if let NativeUiEvent::Snapshot(mut snapshot) = event {
                trim_message_window(Arc::make_mut(&mut snapshot.messages));
                self.conversation = Some(*snapshot);
            }
            return None;
        };
        let mut messages_changed = false;
        let dirty_message_id = match event {
            NativeUiEvent::Snapshot(mut snapshot) => {
                messages_changed = true;
                trim_message_window(Arc::make_mut(&mut snapshot.messages));
                *conversation = *snapshot;
                None
            }
            NativeUiEvent::MessageUpserted(message) => {
                messages_changed = true;
                Some(upsert_message(conversation, message))
            }
            NativeUiEvent::RemoveMessage { message_id } => {
                messages_changed = true;
                Arc::make_mut(&mut conversation.messages)
                    .retain(|message| message.message_id != message_id);
                Some(message_id)
            }
            NativeUiEvent::MessageBlockAppended { message_id, block } => {
                messages_changed = true;
                append_message_block(conversation, &message_id, block)
            }
            NativeUiEvent::MarkdownDelta {
                message_id,
                block_id,
                append,
                completed,
            } => {
                messages_changed = true;
                append_markdown_delta(conversation, &message_id, &block_id, &append, completed)
            }
            NativeUiEvent::ReasoningDelta {
                message_id,
                block_id,
                append,
                completed,
            } => {
                messages_changed = true;
                append_reasoning_delta(conversation, &message_id, &block_id, &append, completed)
            }
            NativeUiEvent::ToolChanged(tool) => {
                messages_changed = true;
                replace_tool(conversation, tool)
            }
            NativeUiEvent::ApprovalChanged(approval) => {
                messages_changed = true;
                replace_approval(conversation, approval)
            }
            NativeUiEvent::QuestionChanged(question) => {
                messages_changed = true;
                replace_question(conversation, question)
            }
            NativeUiEvent::InputQueueChanged(queue) => {
                conversation.input_queue = queue;
                None
            }
            NativeUiEvent::QueuedInputLoaded(item) => {
                let loaded_item_id = item.queue_item_id.clone();
                // 状态模型看不到 TextInput 的临时值，只能用 fingerprint 不一致来识别
                // 已进入投影但尚未由 Host 确认的完整正文。此时拒绝新加载，避免
                // 为满足单项完整正文约束而静默回收用户仍可能编辑的文本。
                if conversation.input_queue.iter().any(|current| {
                    current.text_complete
                        && queue_text_fingerprint(&current.text) != current.text_fingerprint
                }) {
                    self.queue_load_rejection =
                        Some("请先保存当前排队输入，宿主确认后再加载另一项完整正文".to_owned());
                    return None;
                }
                let loaded = if let Some(current) = conversation
                    .input_queue
                    .iter_mut()
                    .find(|current| current.queue_item_id == loaded_item_id)
                {
                    *current = item;
                    true
                } else {
                    false
                };
                if loaded {
                    bound_loaded_queue_items(&mut conversation.input_queue, &loaded_item_id);
                }
                None
            }
            NativeUiEvent::DraftChanged(draft) => {
                conversation.draft = draft;
                None
            }
            NativeUiEvent::TurnChanged { turn_id, status } => {
                conversation.active_turn_id = match status {
                    SessionStatus::Running | SessionStatus::Queued | SessionStatus::Waiting => {
                        Some(turn_id)
                    }
                    _ => None,
                };
                conversation.session.status = status;
                None
            }
            NativeUiEvent::SessionChanged(session) => {
                conversation.session = session;
                None
            }
            NativeUiEvent::HistoryInvalidated {
                transcript_revision,
            } => {
                conversation.transcript_revision = transcript_revision;
                conversation.history = None;
                None
            }
            NativeUiEvent::Closed | NativeUiEvent::ResyncRequired { .. } => None,
        };
        if messages_changed {
            trim_message_window(Arc::make_mut(&mut conversation.messages));
        }
        dirty_message_id
    }
}

fn upsert_message(conversation: &mut ConversationFact, message: UiMessage) -> String {
    let message_id = message.message_id.clone();
    let messages = Arc::make_mut(&mut conversation.messages);
    if let Some(current) = messages
        .iter_mut()
        .find(|current| current.message_id == message_id)
    {
        // 替换整条消息只更新一个内层 Arc；历史消息的正文仍与渲染快照共享。
        *current = Arc::new(message);
    } else {
        messages.push(Arc::new(message));
    }
    message_id
}

fn append_message_block(
    conversation: &mut ConversationFact,
    message_id: &str,
    block: MessageBlock,
) -> Option<String> {
    let messages = Arc::make_mut(&mut conversation.messages);
    let index = messages
        .iter()
        .position(|message| message.message_id == message_id)?;
    let message = Arc::make_mut(&mut messages[index]);
    let block_id = message_block_id(&block);
    if message
        .blocks
        .iter()
        .all(|current| message_block_id(current) != block_id)
    {
        message.blocks.push(block);
    }
    Some(message_id.to_owned())
}

fn message_block_id(block: &MessageBlock) -> &str {
    match block {
        MessageBlock::Markdown { block_id, .. }
        | MessageBlock::Reasoning { block_id, .. }
        | MessageBlock::Error { block_id, .. } => block_id,
        MessageBlock::Tool(tool) => &tool.request_id,
        MessageBlock::Approval(approval) => &approval.request_id,
        MessageBlock::Question(question) => &question.request_id,
        MessageBlock::FileChange(change) => &change.path,
        MessageBlock::Attachment(attachment) => &attachment.attachment_id,
    }
}

fn append_markdown_delta(
    conversation: &mut ConversationFact,
    message_id: &str,
    block_id: &str,
    append: &str,
    completed: bool,
) -> Option<String> {
    let used = streaming_text_bytes(conversation);
    let remaining = MAX_STREAM_TEXT_BYTES.saturating_sub(used);
    let messages = Arc::make_mut(&mut conversation.messages);
    let message_index = messages
        .iter()
        .position(|message| message.message_id == message_id)?;
    let message = Arc::make_mut(&mut messages[message_index]);
    if let Some(MessageBlock::Markdown {
        source, streaming, ..
    }) = message.blocks.iter_mut().find(
        |block| matches!(block, MessageBlock::Markdown { block_id: id, .. } if id == block_id),
    ) {
        source.push_str(utf8_prefix(append, remaining));
        *streaming = !completed;
        Some(message_id.to_owned())
    } else {
        None
    }
}

fn append_reasoning_delta(
    conversation: &mut ConversationFact,
    message_id: &str,
    block_id: &str,
    append: &str,
    completed: bool,
) -> Option<String> {
    let used = streaming_text_bytes(conversation);
    let remaining = MAX_STREAM_TEXT_BYTES.saturating_sub(used);
    let messages = Arc::make_mut(&mut conversation.messages);
    let message_index = messages
        .iter()
        .position(|message| message.message_id == message_id)?;
    let message = Arc::make_mut(&mut messages[message_index]);
    if let Some(MessageBlock::Reasoning {
        source, streaming, ..
    }) = message.blocks.iter_mut().find(
        |block| matches!(block, MessageBlock::Reasoning { block_id: id, .. } if id == block_id),
    ) {
        source.push_str(utf8_prefix(append, remaining));
        *streaming = !completed;
        Some(message_id.to_owned())
    } else {
        None
    }
}

fn streaming_text_bytes(conversation: &ConversationFact) -> usize {
    conversation
        .messages
        .iter()
        .flat_map(|message| message.blocks.iter())
        .map(|block| match block {
            MessageBlock::Markdown {
                source, streaming, ..
            }
            | MessageBlock::Reasoning {
                source, streaming, ..
            } if *streaming => source.len(),
            _ => 0,
        })
        .fold(0usize, usize::saturating_add)
}

fn utf8_prefix(value: &str, budget: usize) -> &str {
    if value.len() <= budget {
        return value;
    }
    let mut end = budget;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn replace_tool(conversation: &mut ConversationFact, tool: ToolFact) -> Option<String> {
    let messages = Arc::make_mut(&mut conversation.messages);
    let (message_index, block_index) =
        messages.iter().enumerate().find_map(|(index, message)| {
            message.blocks.iter().position(|block| {
            matches!(block, MessageBlock::Tool(current) if current.request_id == tool.request_id)
        }).map(|block_index| (index, block_index))
        })?;
    let message = Arc::make_mut(&mut messages[message_index]);
    if let MessageBlock::Tool(current) = &mut message.blocks[block_index] {
        *current = tool;
    }
    Some(message.message_id.clone())
}

fn replace_approval(
    conversation: &mut ConversationFact,
    approval: ToolApprovalFact,
) -> Option<String> {
    let messages = Arc::make_mut(&mut conversation.messages);
    let (message_index, block_index) =
        messages.iter().enumerate().find_map(|(index, message)| {
            message
                .blocks
                .iter()
                .position(|block| {
                    matches!(
                        block,
                        MessageBlock::Approval(current) if current.request_id == approval.request_id
                    )
                })
                .map(|block_index| (index, block_index))
        })?;
    let message = Arc::make_mut(&mut messages[message_index]);
    if let MessageBlock::Approval(current) = &mut message.blocks[block_index] {
        *current = approval;
    }
    Some(message.message_id.clone())
}

fn replace_question(conversation: &mut ConversationFact, question: QuestionFact) -> Option<String> {
    let messages = Arc::make_mut(&mut conversation.messages);
    let (message_index, block_index) =
        messages.iter().enumerate().find_map(|(index, message)| {
            message
                .blocks
                .iter()
                .position(|block| {
                    matches!(
                        block,
                        MessageBlock::Question(current) if current.request_id == question.request_id
                    )
                })
                .map(|block_index| (index, block_index))
        })?;
    let message = Arc::make_mut(&mut messages[message_index]);
    if let MessageBlock::Question(current) = &mut message.blocks[block_index] {
        *current = question;
    }
    Some(message.message_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation() -> ConversationFact {
        ConversationFact {
            session: SessionFact {
                session_id: "s".into(),
                project_key: "p".into(),
                title: "会话".into(),
                status: SessionStatus::Running,
                pinned: false,
                archived: false,
                unread: false,
                model: None,
                vision_enabled: false,
                effort: None,
                permission: PermissionMode::Build,
                plan: PlanMode::Off,
                followup_mode: FollowupMode::Queue,
                usage: None,
                prompt_history: PromptHistoryFact::default(),
                updated_at_unix_ms: 0,
                last_sequence: 1,
            },
            messages: Arc::new(vec![Arc::new(UiMessage {
                message_id: "m".into(),
                turn_id: Some("t".into()),
                role: MessageRole::Assistant,
                blocks: vec![MessageBlock::Markdown {
                    block_id: "b".into(),
                    source: "old".into(),
                    streaming: true,
                }],
                feedback: None,
                created_at_unix_ms: None,
            })]),
            history: None,
            input_queue: vec![],
            active_turn_id: Some("t".into()),
            draft: DraftFact::default(),
            transcript_revision: 1,
        }
    }

    #[test]
    fn streaming_delta_changes_only_matching_tail() {
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::MarkdownDelta {
            message_id: "m".into(),
            block_id: "b".into(),
            append: " text".into(),
            completed: true,
        });
        let block = &state.conversation.unwrap().messages[0].blocks[0];
        assert_eq!(
            block,
            &MessageBlock::Markdown {
                block_id: "b".into(),
                source: "old text".into(),
                streaming: false,
            }
        );
    }

    #[test]
    fn snapshot_is_required_before_incremental_events() {
        let mut state = NativeUiState::default();
        state.apply_event(NativeUiEvent::MessageUpserted(UiMessage {
            message_id: "ignored".into(),
            turn_id: None,
            role: MessageRole::User,
            blocks: vec![],
            feedback: None,
            created_at_unix_ms: None,
        }));
        assert!(state.conversation.is_none());
    }

    #[test]
    fn message_upsert_replaces_transient_and_remove_deletes_by_stable_id() {
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::MessageUpserted(UiMessage {
            message_id: "m".into(),
            turn_id: Some("t".into()),
            role: MessageRole::Assistant,
            blocks: vec![MessageBlock::Markdown {
                block_id: "b-final".into(),
                source: "final".into(),
                streaming: false,
            }],
            feedback: None,
            created_at_unix_ms: None,
        }));
        assert_eq!(state.conversation.as_ref().unwrap().messages.len(), 1);
        assert_eq!(
            state.conversation.as_ref().unwrap().messages[0].blocks,
            vec![MessageBlock::Markdown {
                block_id: "b-final".into(),
                source: "final".into(),
                streaming: false,
            }]
        );

        state.apply_event(NativeUiEvent::RemoveMessage {
            message_id: "m".into(),
        });
        assert!(state.conversation.as_ref().unwrap().messages.is_empty());
    }

    #[test]
    fn message_block_append_is_idempotent_by_block_id() {
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        let block = MessageBlock::Reasoning {
            block_id: "reasoning-1".into(),
            source: "思考".into(),
            streaming: true,
        };
        state.apply_event(NativeUiEvent::MessageBlockAppended {
            message_id: "m".into(),
            block: block.clone(),
        });
        state.apply_event(NativeUiEvent::MessageBlockAppended {
            message_id: "m".into(),
            block,
        });
        let blocks = &state.conversation.unwrap().messages[0].blocks;
        assert_eq!(blocks.len(), 2);
        assert!(matches!(blocks[1], MessageBlock::Reasoning { .. }));
    }

    #[test]
    fn persisted_draft_and_queue_events_replace_projection() {
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::InputQueueChanged(vec![QueuedInputFact {
            queue_item_id: "q-1".into(),
            text: "排队任务".into(),
            text_complete: true,
            text_fingerprint: 1,
            state: QueuedInputState::Waiting,
        }]));
        state.apply_event(NativeUiEvent::DraftChanged(DraftFact {
            text: "当前草稿".into(),
            ..DraftFact::default()
        }));
        let conversation = state.conversation.unwrap();
        assert_eq!(conversation.input_queue[0].queue_item_id, "q-1");
        assert_eq!(conversation.draft.text, "当前草稿");
    }

    #[test]
    fn loaded_queue_item_replaces_preview_without_changing_identity() {
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::InputQueueChanged(vec![QueuedInputFact {
            queue_item_id: "q-1".into(),
            text: "预览".into(),
            text_complete: false,
            text_fingerprint: 7,
            state: QueuedInputState::Waiting,
        }]));
        state.apply_event(NativeUiEvent::QueuedInputLoaded(QueuedInputFact {
            queue_item_id: "q-1".into(),
            text: "完整排队正文".into(),
            text_complete: true,
            text_fingerprint: 8,
            state: QueuedInputState::Waiting,
        }));
        let item = &state.conversation.unwrap().input_queue[0];
        assert_eq!(item.queue_item_id, "q-1");
        assert_eq!(item.text, "完整排队正文");
        assert!(item.text_complete);
    }

    #[test]
    fn loading_another_queue_item_demotes_previous_full_text_to_preview() {
        let previous_text = "x".repeat(MAX_QUEUE_PREVIEW_BYTES + 1024);
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::InputQueueChanged(vec![
            QueuedInputFact {
                queue_item_id: "q-1".into(),
                text: previous_text.clone(),
                text_complete: true,
                text_fingerprint: queue_text_fingerprint(&previous_text),
                state: QueuedInputState::Waiting,
            },
            QueuedInputFact {
                queue_item_id: "q-2".into(),
                text: "预览".into(),
                text_complete: false,
                text_fingerprint: queue_text_fingerprint("第二项"),
                state: QueuedInputState::Waiting,
            },
        ]));
        state.apply_event(NativeUiEvent::QueuedInputLoaded(QueuedInputFact {
            queue_item_id: "q-2".into(),
            text: "第二项".into(),
            text_complete: true,
            text_fingerprint: queue_text_fingerprint("第二项"),
            state: QueuedInputState::Waiting,
        }));

        let queue = &state.conversation.as_ref().unwrap().input_queue;
        let previous = &queue[0];
        assert!(!previous.text_complete);
        assert!(previous.text.len() <= MAX_QUEUE_PREVIEW_BYTES);
        assert!(queue[1].text_complete);
    }

    #[test]
    fn loading_preserves_full_item_when_its_fingerprint_is_not_authoritative() {
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::InputQueueChanged(vec![
            QueuedInputFact {
                queue_item_id: "q-1".into(),
                text: "可能未保存的编辑".into(),
                text_complete: true,
                text_fingerprint: 7,
                state: QueuedInputState::Waiting,
            },
            QueuedInputFact {
                queue_item_id: "q-2".into(),
                text: "预览".into(),
                text_complete: false,
                text_fingerprint: queue_text_fingerprint("第二项"),
                state: QueuedInputState::Waiting,
            },
        ]));
        state.apply_event(NativeUiEvent::QueuedInputLoaded(QueuedInputFact {
            queue_item_id: "q-2".into(),
            text: "第二项".into(),
            text_complete: true,
            text_fingerprint: queue_text_fingerprint("第二项"),
            state: QueuedInputState::Waiting,
        }));

        let queue = &state.conversation.as_ref().unwrap().input_queue;
        assert!(queue[0].text_complete);
        assert_eq!(queue[0].text, "可能未保存的编辑");
        assert!(!queue[1].text_complete);
        assert_eq!(
            state.take_queue_load_rejection().as_deref(),
            Some("请先保存当前排队输入，宿主确认后再加载另一项完整正文")
        );
    }

    #[test]
    fn repeated_loaded_events_keep_one_full_item_and_bound_previews() {
        let mut state = NativeUiState {
            conversation: Some(conversation()),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::InputQueueChanged(
            (1..=3)
                .map(|index| QueuedInputFact {
                    queue_item_id: format!("q-{index}"),
                    text: "预览".to_owned(),
                    text_complete: false,
                    text_fingerprint: queue_text_fingerprint(&format!("正文-{index}")),
                    state: QueuedInputState::Waiting,
                })
                .collect(),
        ));

        for index in 1..=3 {
            let text = format!("正文-{index}").repeat(MAX_QUEUE_PREVIEW_BYTES / 32 + 1);
            state.apply_event(NativeUiEvent::QueuedInputLoaded(QueuedInputFact {
                queue_item_id: format!("q-{index}"),
                text: text.clone(),
                text_complete: true,
                text_fingerprint: queue_text_fingerprint(&text),
                state: QueuedInputState::Waiting,
            }));
            let queue = &state.conversation.as_ref().unwrap().input_queue;
            assert_eq!(
                queue.iter().filter(|item| item.text_complete).count(),
                1,
                "一次只能保留一个完整队列项"
            );
            let preview_bytes = queue
                .iter()
                .filter(|item| !item.text_complete)
                .map(|item| item.queue_item_id.len().saturating_add(item.text.len()))
                .fold(0usize, usize::saturating_add);
            let identity_bytes = queue
                .iter()
                .map(|item| item.queue_item_id.len())
                .fold(0usize, usize::saturating_add);
            assert!(
                preview_bytes <= MAX_QUEUE_PREVIEW_BYTES,
                "预览正文和 ID 必须服从统一预算"
            );
            assert_eq!(
                queue
                    .iter()
                    .find(|item| item.queue_item_id == format!("q-{index}"))
                    .unwrap()
                    .text,
                text
            );
            assert!(identity_bytes <= MAX_QUEUE_PREVIEW_BYTES);
        }
    }

    #[test]
    fn display_window_keeps_latest_messages_with_bounded_count() {
        let mut messages = (0..MAX_DISPLAY_MESSAGES + 7)
            .map(|index| {
                Arc::new(UiMessage {
                    message_id: format!("m-{index}"),
                    turn_id: None,
                    role: MessageRole::User,
                    blocks: Vec::new(),
                    feedback: None,
                    created_at_unix_ms: None,
                })
            })
            .collect::<Vec<_>>();

        assert!(trim_message_window(&mut messages));
        assert_eq!(messages.len(), MAX_DISPLAY_MESSAGES);
        assert_eq!(messages.first().unwrap().message_id, "m-7");
        assert_eq!(messages.last().unwrap().message_id, "m-206");
        assert!(
            messages
                .iter()
                .map(|message| message_display_bytes(message))
                .sum::<usize>()
                <= MAX_DISPLAY_BYTES
        );
    }

    #[test]
    fn oversized_single_message_releases_later_blocks_and_keeps_route_ids() {
        let mut messages = vec![Arc::new(UiMessage {
            message_id: "message-route".into(),
            turn_id: Some("turn-route".into()),
            role: MessageRole::Assistant,
            blocks: vec![
                MessageBlock::Tool(ToolFact {
                    request_id: "tool-route".into(),
                    name: "Write".into(),
                    arguments_json: "界".repeat(MAX_DISPLAY_BYTES / 3 + 1),
                    output_markdown: Some("later-output".repeat(1024)),
                    status: ToolStatus::Succeeded,
                    effect: ToolEffect::ChangesState,
                    started_at_unix_ms: None,
                    completed_at_unix_ms: None,
                }),
                MessageBlock::Markdown {
                    block_id: "later-route".into(),
                    source: "later-block".repeat(1024),
                    streaming: false,
                },
            ],
            feedback: None,
            created_at_unix_ms: None,
        })];
        assert!(trim_message_window(&mut messages));
        assert!(message_display_bytes(&messages[0]) <= MAX_DISPLAY_BYTES);
        assert_eq!(messages[0].blocks.len(), 1);
        assert_eq!(messages[0].message_id, "message-route");
        let MessageBlock::Tool(tool) = &messages[0].blocks[0] else {
            panic!("应保留完整工具身份")
        };
        assert_eq!(tool.request_id, "tool-route");
        assert_eq!(tool.name, "Write");
        assert_eq!(tool.output_markdown.as_deref(), Some(""));
    }

    #[test]
    fn file_route_is_dropped_as_a_whole_when_display_budget_is_exhausted() {
        let mut remaining = 3;
        let path = "src/important.rs";
        let mut block = MessageBlock::FileChange(FileChangeFact {
            path: path.into(),
            operation: FileChangeOperation::Modify,
            applied: true,
            rewindable: true,
        });
        assert!(!cap_block_for_display(&mut block, &mut remaining));
        assert_eq!(remaining, 3);
        assert!(matches!(block, MessageBlock::FileChange(change) if change.path == path));
    }

    #[test]
    fn display_window_keeps_history_cursor_after_eviction() {
        let mut fact = conversation();
        fact.history = Some(JournalCursor {
            after_sequence: None,
            before_sequence: Some(10),
            through_sequence: 20,
        });
        fact.messages = Arc::new(
            (0..MAX_DISPLAY_MESSAGES + 1)
                .map(|index| {
                    Arc::new(UiMessage {
                        message_id: format!("m-{index}"),
                        turn_id: None,
                        role: MessageRole::Assistant,
                        blocks: Vec::new(),
                        feedback: None,
                        created_at_unix_ms: None,
                    })
                })
                .collect(),
        );
        let mut state = NativeUiState::default();
        state.apply_event(NativeUiEvent::Snapshot(Box::new(fact)));

        let current = state.conversation.unwrap();
        assert_eq!(current.messages.len(), MAX_DISPLAY_MESSAGES);
        assert_eq!(current.messages.first().unwrap().message_id, "m-1");
        assert!(current.history.is_some());
    }

    #[test]
    fn streaming_update_shares_unchanged_message_arcs() {
        let mut fact = conversation();
        let unchanged = Arc::new(UiMessage {
            message_id: "unchanged".into(),
            turn_id: None,
            role: MessageRole::User,
            blocks: Vec::new(),
            feedback: None,
            created_at_unix_ms: None,
        });
        let streaming = fact.messages[0].clone();
        fact.messages = Arc::new(vec![unchanged.clone(), streaming.clone()]);
        let rendered = fact.clone();
        let mut state = NativeUiState {
            conversation: Some(fact),
            ..NativeUiState::default()
        };

        state.apply_event(NativeUiEvent::MarkdownDelta {
            message_id: "m".into(),
            block_id: "b".into(),
            append: " token".into(),
            completed: false,
        });

        let current = state.conversation.unwrap();
        assert!(Arc::ptr_eq(&rendered.messages[0], &current.messages[0]));
        assert!(!Arc::ptr_eq(&rendered.messages[1], &current.messages[1]));
        assert_eq!(
            rendered.messages[1].blocks[0],
            MessageBlock::Markdown {
                block_id: "b".into(),
                source: "old".into(),
                streaming: true,
            }
        );
    }

    #[test]
    fn active_streaming_budget_stops_at_a_utf8_boundary() {
        let mut fact = conversation();
        let messages = Arc::make_mut(&mut fact.messages);
        let message = Arc::make_mut(&mut messages[0]);
        if let MessageBlock::Markdown { source, .. } = &mut message.blocks[0] {
            source.clear();
        }
        let mut state = NativeUiState {
            conversation: Some(fact),
            ..NativeUiState::default()
        };
        state.apply_event(NativeUiEvent::MarkdownDelta {
            message_id: "m".into(),
            block_id: "b".into(),
            append: "界".repeat(MAX_ACTIVE_TAIL_BYTES / "界".len() + 2),
            completed: false,
        });

        let MessageBlock::Markdown {
            source, streaming, ..
        } = &state.conversation.unwrap().messages[0].blocks[0]
        else {
            panic!("应保留 Markdown 流式块");
        };
        assert!(*streaming);
        assert!(source.len() <= MAX_ACTIVE_TAIL_BYTES);
        assert!(source.is_char_boundary(source.len()));
    }

    #[test]
    fn completed_streaming_block_is_removed_from_active_budget() {
        let mut fact = conversation();
        let messages = Arc::make_mut(&mut fact.messages);
        let blocks = &mut Arc::make_mut(&mut messages[0]).blocks;
        blocks[0] = MessageBlock::Markdown {
            block_id: "markdown".into(),
            source: "x".repeat(MAX_ACTIVE_TAIL_BYTES),
            streaming: true,
        };
        blocks.push(MessageBlock::Reasoning {
            block_id: "reasoning".into(),
            source: String::new(),
            streaming: true,
        });
        let mut state = NativeUiState {
            conversation: Some(fact),
            ..NativeUiState::default()
        };

        state.apply_event(NativeUiEvent::MarkdownDelta {
            message_id: "m".into(),
            block_id: "markdown".into(),
            append: "ignored-after-budget".into(),
            completed: true,
        });
        state.apply_event(NativeUiEvent::ReasoningDelta {
            message_id: "m".into(),
            block_id: "reasoning".into(),
            append: "x".repeat(MAX_ACTIVE_TAIL_BYTES),
            completed: false,
        });

        let message = &state.conversation.unwrap().messages[0];
        assert!(matches!(
            &message.blocks[0],
            MessageBlock::Markdown { source, streaming: false, .. }
                if source.len() == MAX_ACTIVE_TAIL_BYTES
        ));
        assert!(matches!(
            &message.blocks[1],
            MessageBlock::Reasoning { source, streaming: true, .. }
                if source.len() == MAX_ACTIVE_TAIL_BYTES
        ));
    }
}
