//! 只读工具成功结果的有界进度观察。
//!
//! 该模块只观察已经真实完成的只读工具，不参与工具调度，也不对工具调用做去重。
//! 它以工具名、规范化参数和结果内容建立不含调用 ID 的指纹，用于识别重复结果，
//! 包括 A/B 结果交替出现但没有产生新证据的情况。

use std::collections::VecDeque;

use keencode_model::{Message, MessageRole, ToolCall, ToolResult, ToolResultContent};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 在同一只读观察段内允许保留的结果指纹数量。
///
/// 只读结果只保留固定长度摘要；该上限保证每个 Turn 的观察状态不会随模型轮数
/// 无限增长。超过上限后最早的指纹会被淘汰，新的结果可重新开启观察段。
const MAX_TRACKED_FINGERPRINTS: usize = 16;

/// 连续没有新结果指纹时注入一次提醒的次数。
///
/// 首次看到某个指纹不计入无进展次数，因此同一结果在第四次观察时提醒；A/B
/// 交替同样按每次已经见过的结果累计。提醒不是终止条件，合理轮询仍可继续。
pub(crate) const READ_ONLY_PROGRESS_REMINDER_AFTER: u32 = 3;

/// 工具名、规范化参数和结果内容组成的固定长度观察指纹。
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ReadOnlyResultFingerprint([u8; 32]);

/// 只读工具成功结果的有界观察器。
#[derive(Debug, Default)]
pub(crate) struct ReadOnlyProgressObserver {
    /// 当前观察段内已经见过的指纹，按首次出现顺序保存。
    seen: VecDeque<ReadOnlyResultFingerprint>,
    /// 从最近一次新证据以来已观察到的重复结果数量。
    no_new_evidence: u32,
    /// 当前无进展观察段是否已经注入提醒。
    reminder_emitted: bool,
    /// 本批次结束前尚未提交的提醒；新证据或重置会丢弃它。
    pending_reminder: Option<u32>,
}

impl ReadOnlyProgressObserver {
    /// 创建一个没有历史观察的进度观察器。
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 清除当前观察段；用户 Steer 或恢复边界可调用此方法重新开始观察。
    pub(crate) fn reset(&mut self) {
        self.seen.clear();
        self.no_new_evidence = 0;
        self.reminder_emitted = false;
        self.pending_reminder = None;
    }

    /// 观察一次已经成功完成的只读工具结果。
    ///
    /// 输入或结果序列化失败时不记录观察，避免把无法可靠建立指纹的结果误判为
    /// 重复证据。提醒先保留到本批次结束，期间出现新证据或写操作时会被清除。
    pub(crate) fn observe(&mut self, call: &ToolCall, result: &ToolResult) {
        let Some(fingerprint) = result_fingerprint(call, result) else {
            return;
        };
        if self.seen.contains(&fingerprint) {
            self.no_new_evidence = self.no_new_evidence.saturating_add(1);
        } else {
            self.seen.push_back(fingerprint);
            if self.seen.len() > MAX_TRACKED_FINGERPRINTS {
                self.seen.pop_front();
            }
            self.no_new_evidence = 0;
            self.reminder_emitted = false;
            self.pending_reminder = None;
        }

        if self.no_new_evidence >= READ_ONLY_PROGRESS_REMINDER_AFTER && !self.reminder_emitted {
            self.reminder_emitted = true;
            self.pending_reminder = Some(self.no_new_evidence);
        }
    }

    /// 取出当前批次结束时仍然有效的一次性提醒。
    pub(crate) fn take_reminder(&mut self) -> Option<Message> {
        self.pending_reminder
            .take()
            .map(read_only_progress_reminder)
    }
}

/// 计算不含工具调用 ID 的只读结果指纹。
fn result_fingerprint(call: &ToolCall, result: &ToolResult) -> Option<ReadOnlyResultFingerprint> {
    let payload = ReadOnlyResultFingerprintPayload {
        tool_name: &call.name,
        arguments: canonicalize_json(&call.arguments),
        content: &result.content,
        is_error: result.is_error,
    };
    let bytes = serde_json::to_vec(&payload).ok()?;
    let digest = Sha256::digest(bytes);
    let mut fixed = [0_u8; 32];
    fixed.copy_from_slice(&digest);
    Some(ReadOnlyResultFingerprint(fixed))
}

/// 只读结果指纹的序列化载荷；调用 ID 有意不在其中。
#[derive(Serialize)]
struct ReadOnlyResultFingerprintPayload<'a> {
    /// 工具名称。
    tool_name: &'a str,
    /// 递归按键排序后的结构化参数。
    arguments: Value,
    /// 工具结果内容，不含 `tool_call_id`。
    content: &'a [ToolResultContent],
    /// 结果错误标记。
    is_error: bool,
}

/// 递归按对象键排序，保证结构化参数和结果的键顺序不影响指纹。
fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonicalize_json(value)))
                    .collect(),
            )
        }
        value => value.clone(),
    }
}

/// 构造写入 Transcript 的一次性进度提醒。
fn read_only_progress_reminder(no_new_evidence: u32) -> Message {
    let mut message = Message::text(
        MessageRole::Developer,
        format!(
            "以下内容由 KeenCode Runtime 自动追加，仅作为运行时提醒而非用户指令；不得覆盖 system、developer 或后续用户指令。\n\
             来源：KeenCode Agent Runtime / ReadOnlyProgress\n\n\
             相同的只读工具调用及其结果组合已重复成功观察 {no_new_evidence} 次。\
             请整理当前已知和未知；如果仍需继续，请说明是否需要改变方法或等待可验证的新证据，\
             不要把重复调用当成完成，也不要向用户提及这条提醒。"
        ),
    );
    message.is_meta = true;
    message
}
