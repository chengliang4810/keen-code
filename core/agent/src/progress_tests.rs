//! 重复成功只读工具调用的确定性测试。

use super::progress::{READ_ONLY_PROGRESS_REMINDER_AFTER, ReadOnlyProgressObserver};
use super::*;
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities, ResponseMetadata,
    ScriptedProvider, ScriptedReply, StopReason, ToolCall, ToolDefinition, ToolResult,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// 创建测试所需的非空 Session 标识。
fn session_id(value: &str) -> SessionId {
    SessionId::new(value).expect("测试 Session 标识应当有效")
}

/// 创建测试所需的非空 Turn 标识。
fn turn_id(value: &str) -> TurnId {
    TurnId::new(value).expect("测试 Turn 标识应当有效")
}

/// 创建测试所需的非空 Agent 标识。
fn agent_id(value: &str) -> AgentId {
    AgentId::new(value).expect("测试 Agent 标识应当有效")
}

/// 创建最小用户 Turn 请求。
fn turn_request() -> TurnRequest {
    TurnRequest::new(
        session_id("session-read-only-progress"),
        turn_id("turn-read-only-progress"),
        agent_id("agent-read-only-progress"),
        "test-model",
        vec![Message::text(MessageRole::User, "检查文件状态")],
        PlanGuard::inactive(),
    )
}

/// 创建包含完整工具调用的模型响应。
fn tool_reply(id: &str, arguments: Value) -> ScriptedReply {
    tool_reply_named(id, "read_progress_probe", arguments)
}

/// 创建调用指定工具名称的完整模型响应。
fn tool_reply_named(id: &str, name: &str, arguments: Value) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::ToolCallStart {
            index: 0,
            id: id.to_owned(),
            name: name.to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 0,
            id: id.to_owned(),
            delta: arguments.to_string(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 0,
            id: id.to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        },
    ])
}

/// 创建普通文本模型响应。
fn text_reply(text: &str) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::TextDelta {
            index: 0,
            delta: text.to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ])
}

/// 按固定顺序返回文本的只读工具，并记录真实执行次数。
struct SequenceReadTool {
    name: String,
    effect: ToolEffect,
    results: Mutex<VecDeque<String>>,
    calls: Mutex<Vec<Value>>,
}

impl SequenceReadTool {
    /// 创建一个固定结果序列的测试工具。
    fn new(results: impl IntoIterator<Item = &'static str>) -> Self {
        Self::new_with_effect("read_progress_probe", ToolEffect::ReadOnly, results)
    }

    /// 创建指定工具名和副作用分类的固定结果工具。
    fn new_with_effect(
        name: &str,
        effect: ToolEffect,
        results: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        Self {
            name: name.to_owned(),
            effect,
            results: Mutex::new(results.into_iter().map(str::to_owned).collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// 返回真实执行次数。
    fn call_count(&self) -> usize {
        self.calls.lock().expect("工具调用锁不应损坏").len()
    }
}

impl AgentTool for SequenceReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            self.name.clone(),
            "返回确定性只读结果的测试工具",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    fn execute(&self, _context: ToolContext, input: Value) -> ToolFuture<'_> {
        self.calls.lock().expect("工具调用锁不应损坏").push(input);
        let result = self
            .results
            .lock()
            .expect("工具结果锁不应损坏")
            .pop_front()
            .expect("测试工具结果序列不应提前耗尽");
        Box::pin(async move { Ok(ToolOutput::text(result)) })
    }
}

/// 只读取工具结果的调用 ID 不参与指纹，结构化参数键顺序也不影响判定。
#[test]
fn read_only_progress_fingerprint_excludes_call_id_and_canonicalizes_input() {
    let mut observer = ReadOnlyProgressObserver::new();
    let first = ToolCall::new(
        "call-a",
        "read",
        json!({"path": "a.rs", "options": {"line": 3, "hidden": false}}),
    );
    let second = ToolCall::new(
        "call-b",
        "read",
        json!({"options": {"hidden": false, "line": 3}, "path": "a.rs"}),
    );
    let first_result = ToolResult::text("result-a", "same content", false);
    let second_result = ToolResult::text("result-b", "same content", false);

    observer.observe(&first, &first_result);
    observer.observe(&second, &second_result);
}

/// 同一成功结果持续出现时只在确定轮次提醒一次，随后不每轮刷屏。
#[test]
fn repeated_success_reminds_once_at_bounded_threshold() {
    let mut observer = ReadOnlyProgressObserver::new();
    let call = ToolCall::new("call", "read", json!({"path": "a.rs"}));
    let result = ToolResult::text("result", "unchanged", false);

    for _ in 0..READ_ONLY_PROGRESS_REMINDER_AFTER {
        observer.observe(&call, &result);
    }
    observer.observe(&call, &result);
    let reminder = observer.take_reminder().expect("达到无新证据阈值时应提醒");
    assert!(reminder.is_meta);
    assert_eq!(reminder.role, MessageRole::Developer);
    let text = match &reminder.content[0] {
        ContentBlock::Text { text } => text,
        block => panic!("提醒应为文本块，实际为 {block:?}"),
    };
    assert!(text.contains("ReadOnlyProgress"));
    assert!(text.contains("已知和未知"));
    observer.observe(&call, &result);
    assert!(observer.take_reminder().is_none());
}

/// A/B 结果交替并不会因为相邻结果不同而逃过无新证据观察。
#[test]
fn alternating_success_results_are_recognized_as_repeated_observations() {
    let mut observer = ReadOnlyProgressObserver::new();
    let call = ToolCall::new("call", "read", json!({"path": "a.rs"}));
    let results = ["A", "B", "A", "B", "A", "B"];
    let mut reminders = 0;

    for (index, text) in results.into_iter().enumerate() {
        let result = ToolResult::text(format!("result-{index}"), text, false);
        observer.observe(&call, &result);
        if observer.take_reminder().is_some() {
            reminders += 1;
        }
    }

    assert_eq!(reminders, 1);
}

/// 新结果证据和用户 steer 都能清除当前无进展段，避免变化结果误报。
#[test]
fn new_evidence_and_reset_start_a_fresh_progress_segment() {
    let mut observer = ReadOnlyProgressObserver::new();
    let call = ToolCall::new("call", "read", json!({"path": "a.rs"}));
    let first = ToolResult::text("result-a", "A", false);
    let changed = ToolResult::text("result-b", "B", false);

    for _ in 0..=READ_ONLY_PROGRESS_REMINDER_AFTER {
        observer.observe(&call, &first);
    }
    observer.observe(&call, &changed);
    assert!(observer.take_reminder().is_none());
    observer.observe(&call, &changed);
    assert!(observer.take_reminder().is_none());

    observer.reset();
    observer.observe(&call, &first);
    assert!(observer.take_reminder().is_none());
}

/// Runner 继续执行成功重复读取并持久化一次提醒，工具调用与结果历史保持完整。
#[tokio::test]
async fn runner_warns_without_skipping_tools_or_breaking_tool_pairs() {
    let tool = Arc::new(SequenceReadTool::new([
        "unchanged",
        "unchanged",
        "unchanged",
        "unchanged",
        "unchanged",
    ]));
    let replies = [
        tool_reply("call-1", json!({"path": "a.rs"})),
        tool_reply("call-2", json!({"path": "a.rs"})),
        tool_reply("call-3", json!({"path": "a.rs"})),
        tool_reply("call-4", json!({"path": "a.rs"})),
        tool_reply("call-5", json!({"path": "a.rs"})),
        text_reply("已完成检查"),
    ];
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        replies,
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(tool.clone())
        .expect("测试只读工具应成功注册");

    let result = AgentRunner::new(provider.clone(), registry, RunLimits::default())
        .run_turn(turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(tool.call_count(), 5, "重复成功调用不能被去重或跳过");
    let requests = provider.requests().expect("模型请求快照应可读取");
    assert_eq!(requests.len(), 6);
    let reminder_counts = requests
        .iter()
        .map(|request| {
            request
                .messages
                .iter()
                .filter(|message| {
                    message.content.iter().any(|block| {
                        matches!(block, ContentBlock::Text { text } if text.contains("ReadOnlyProgress"))
                    })
                })
                .count()
        })
        .collect::<Vec<_>>();
    assert_eq!(reminder_counts, vec![0, 0, 0, 0, 1, 1]);

    let mut calls = Vec::new();
    let mut results = Vec::new();
    let mut reminders = 0;
    for message in result.messages.iter() {
        for block in &message.content {
            match block {
                ContentBlock::ToolCall { tool_call } => calls.push(tool_call.id.clone()),
                ContentBlock::ToolResult { tool_result } => {
                    results.push(tool_result.tool_call_id.clone())
                }
                ContentBlock::Text { text } if text.contains("ReadOnlyProgress") => reminders += 1,
                _ => {}
            }
        }
    }
    assert_eq!(calls, results, "工具调用与结果必须保持一一配对");
    assert_eq!(
        calls,
        vec!["call-1", "call-2", "call-3", "call-4", "call-5"]
    );
    assert_eq!(reminders, 1, "提醒应可持久化且不能每轮重复追加");
}

/// 真实变化结果会重置无进展观察，不应产生重复成功提醒。
#[tokio::test]
async fn runner_does_not_warn_when_read_result_changes() {
    let tool = Arc::new(SequenceReadTool::new(["A", "A", "B", "B"]));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply("change-1", json!({"path": "a.rs"})),
            tool_reply("change-2", json!({"path": "a.rs"})),
            tool_reply("change-3", json!({"path": "a.rs"})),
            tool_reply("change-4", json!({"path": "a.rs"})),
            text_reply("结果发生变化"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(tool.clone())
        .expect("测试只读工具应成功注册");

    let result = AgentRunner::new(provider, registry, RunLimits::default())
        .run_turn(turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(tool.call_count(), 4);
    assert!(result.messages.iter().all(|message| {
        !message.content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text } if text.contains("ReadOnlyProgress"))
        })
    }));
}

/// 写操作会清除旧只读观察段，写后相同读取不能沿用写前计数。
#[tokio::test]
async fn runner_resets_read_only_progress_after_side_effect() {
    let reader = Arc::new(SequenceReadTool::new(["A", "A", "A", "A"]));
    let writer = Arc::new(SequenceReadTool::new_with_effect(
        "write_progress_probe",
        ToolEffect::ChangesState,
        ["written"],
    ));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply("before-1", json!({"path": "a.rs"})),
            tool_reply("before-2", json!({"path": "a.rs"})),
            tool_reply("before-3", json!({"path": "a.rs"})),
            tool_reply_named("write-1", "write_progress_probe", json!({"path": "a.rs"})),
            tool_reply("after-1", json!({"path": "a.rs"})),
            text_reply("写入后重新检查"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(reader.clone())
        .expect("测试读取工具应成功注册");
    registry
        .register(writer.clone())
        .expect("测试写入工具应成功注册");

    let result = AgentRunner::new(provider, registry, RunLimits::default())
        .run_turn(turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(reader.call_count(), 4);
    assert_eq!(writer.call_count(), 1);
    assert!(result.messages.iter().all(|message| {
        !message.content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text } if text.contains("ReadOnlyProgress"))
        })
    }));
}

/// 失败工具结果会清除旧只读观察段，失败前的成功次数不能跨错误累计。
#[tokio::test]
async fn runner_resets_read_only_progress_after_tool_failure() {
    let reader = Arc::new(SequenceReadTool::new(["A", "A", "A", "A"]));
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [
            tool_reply("before-failure-1", json!({"path": "a.rs"})),
            tool_reply("before-failure-2", json!({"path": "a.rs"})),
            tool_reply("before-failure-3", json!({"path": "a.rs"})),
            tool_reply_named(
                "failure-1",
                "missing_progress_probe",
                json!({"path": "a.rs"}),
            ),
            tool_reply("after-failure-1", json!({"path": "a.rs"})),
            text_reply("失败后重新检查"),
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .register(reader.clone())
        .expect("测试读取工具应成功注册");

    let result = AgentRunner::new(provider, registry, RunLimits::default())
        .run_turn(turn_request())
        .await;

    assert!(result.is_success(), "{:?}", result.error);
    assert_eq!(reader.call_count(), 4);
    assert!(result.messages.iter().all(|message| {
        !message.content.iter().any(|block| {
            matches!(block, ContentBlock::Text { text } if text.contains("ReadOnlyProgress"))
        })
    }));
}
