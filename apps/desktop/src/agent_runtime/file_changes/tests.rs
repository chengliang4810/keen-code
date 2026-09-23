//! 文件变更桌面生产装配的真实 Runtime、ACP 投影与冷恢复集成测试。

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use keencode_acp::{FILE_CHANGE_META_KEY, FileChangeSide, ReadFileChangeRequest};
use keencode_agent::{
    AgentId, AgentRunner, AgentTool, PlanGuard, RunLimits, SessionId, ToolConcurrency, ToolContext,
    ToolEffect, ToolError, ToolFuture, ToolOutput, ToolRegistry, TurnId,
};
use keencode_model::{
    ContentBlock, Message, MessageRole, ModelStreamEvent, ProviderCapabilities, ResponseMetadata,
    ScriptedProvider, ScriptedReply, StopReason, ToolDefinition,
};
use keencode_resources::{
    MessagePart, RequestId, SessionEvent, SessionEventRecord, SessionState, ToolFileChange,
};
use keencode_runtime::{CreateSessionRequest, OpenSessionResult, RuntimeSession};
use keencode_tools::{EditTool, FileMutationRecorder, ToolEnvironment, WriteTool};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::{
    AcpDelivery, AgentRuntime, AgentRuntimeError, AuthoritativeProjectionMode, DeliveryEmitter,
    map_authoritative_record, materialize_delivery,
};
use super::{RuntimeFileMutationRecorder, change_content, read_file_change_page};

/// 记录生产投递器实际接受的 ACP 载荷。
#[derive(Default)]
struct RecordingEmitter {
    /// 已按接受顺序保存的桌面事件 JSON。
    values: Mutex<Vec<Value>>,
}

impl RecordingEmitter {
    /// 返回当前已接受桌面事件的快照。
    fn snapshot(&self) -> Vec<Value> {
        self.values.lock().expect("桌面投递记录锁不应中毒").clone()
    }
}

impl DeliveryEmitter for RecordingEmitter {
    /// 将生产投递器收到的严格 ACP 联合编码为测试可检查的 JSON。
    fn emit(&self, delivery: &AcpDelivery) -> Result<(), AgentRuntimeError> {
        let value =
            serde_json::to_value(delivery).map_err(|_| AgentRuntimeError::DesktopEmitFailed)?;
        self.values
            .lock()
            .map_err(|_| AgentRuntimeError::StateUnavailable)?
            .push(value);
        Ok(())
    }
}

/// 一个真正登记到 RuntimeManager、附带桌面投递泵和临时项目目录的生产测试夹具。
struct DesktopFixture {
    /// 保存 Runtime Journal、Artifact 和 Session lease 的临时根目录。
    _storage: TempDir,
    /// 保存工具实际读写文件的临时项目目录。
    project: TempDir,
    /// 真实桌面 Agent Runtime 装配根。
    runtime: Arc<AgentRuntime>,
    /// 当前唯一 Session 的共享 Runtime 句柄。
    session: RuntimeSession,
    /// 当前桌面投递器的可观察记录。
    emitter: Arc<RecordingEmitter>,
}

impl DesktopFixture {
    /// 创建 Session、登记真实文件记录器并启动 ACP 事件投递泵。
    fn new(session_id: &str) -> Self {
        let storage = TempDir::new().expect("Runtime 存储目录应创建");
        let project = TempDir::new().expect("项目目录应创建");
        let emitter = Arc::new(RecordingEmitter::default());
        let runtime = Arc::new(
            AgentRuntime::new(
                storage.path(),
                Arc::clone(&emitter) as Arc<dyn DeliveryEmitter>,
            )
            .expect("测试 Agent Runtime 应创建"),
        );
        let session = runtime
            .runtime_manager()
            .create(CreateSessionRequest {
                session_id: session_id.to_owned(),
                title: "文件变更桌面测试".to_owned(),
                project_root: project.path().to_string_lossy().into_owned(),
            })
            .expect("测试 Session 应登记");
        runtime
            .ensure_session_delivery(session_id)
            .expect("真实 Session ACP 投递泵应启动");
        Self {
            _storage: storage,
            project,
            runtime,
            session,
            emitter,
        }
    }

    /// 创建仅包含生产 Write/Edit 工具和当前 Session 记录器的冻结工具表。
    fn file_tools(&self) -> ToolRegistry {
        let recorder = Arc::new(RuntimeFileMutationRecorder::new(self.session.clone()));
        let environment = Arc::new(
            ToolEnvironment::new(self.project.path())
                .expect("文件工具环境应创建")
                .with_file_mutation_recorder(recorder),
        );
        let mut registry = ToolRegistry::new();
        registry
            .register(Arc::new(EditTool::new(Arc::clone(&environment))))
            .expect("Edit 工具应注册");
        registry
            .register(Arc::new(WriteTool::new(environment)))
            .expect("Write 工具应注册");
        registry
    }
}

/// 构造一个开启工具调用能力的脚本 Provider。
fn scripted_provider(replies: impl IntoIterator<Item = ScriptedReply>) -> Arc<ScriptedProvider> {
    Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            parallel_tool_calls: false,
            ..ProviderCapabilities::default()
        },
        replies,
    ))
}

/// 构造包含一个完整模型工具调用的脚本响应。
fn tool_reply(call_id: &str, name: &str, arguments: Value) -> ScriptedReply {
    ScriptedReply::events([
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::ToolCallStart {
            index: 0,
            id: call_id.to_owned(),
            name: name.to_owned(),
        },
        ModelStreamEvent::ToolCallArgumentsDelta {
            index: 0,
            id: call_id.to_owned(),
            delta: arguments.to_string(),
        },
        ModelStreamEvent::ToolCallEnd {
            index: 0,
            id: call_id.to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::ToolUse,
        },
    ])
}

/// 构造正常结束当前 Turn 的脚本响应。
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

/// 使用真实 RuntimeSession 绑定 AgentRunner，执行一轮脚本模型和真实文件工具。
async fn run_scripted_turn(
    session: &RuntimeSession,
    provider: Arc<ScriptedProvider>,
    tools: ToolRegistry,
    turn_id: &str,
    prompt: &str,
) {
    let input = Message::text(MessageRole::User, prompt);
    let request = keencode_agent::TurnRequest::new(
        SessionId::new(session.session_id().as_str()).expect("测试 Agent Session ID 应有效"),
        TurnId::new(turn_id).expect("测试 Turn ID 应有效"),
        AgentId::new("root").expect("测试根 Agent ID 应有效"),
        "desktop-file-change-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );
    session
        .bind_agent_runner(AgentRunner::new(provider, tools, RunLimits::default()))
        .run_turn(keencode_runtime::RuntimeTurnRequest::root(
            request,
            vec![input],
            prompt,
        ))
        .await
        .expect("真实 AgentRunner 文件工具 Turn 应完成");
}

/// 分页读取当前 Runtime 保存的快照，并重建为原始字节。
fn read_snapshot_pages(
    runtime: &AgentRuntime,
    session_id: &str,
    request_id: &RequestId,
    side: FileChangeSide,
    expected: &[u8],
) -> Vec<u8> {
    const PAGE_BYTES: u64 = 512 * 1024;
    let mut offset = 0_u64;
    let mut rebuilt = Vec::with_capacity(expected.len());
    if expected.is_empty() {
        let response = runtime
            .read_file_change(ReadFileChangeRequest::new(
                session_id,
                request_id.as_str(),
                side,
                0,
                1,
            ))
            .expect("空快照应可读取");
        response.validate().expect("空快照响应应满足 ACP 合同");
        assert!(
            response
                .decoded_data()
                .expect("空快照 Base64 应有效")
                .is_empty()
        );
        assert!(response.eof, "空快照应立即到达 EOF");
        return rebuilt;
    }
    while offset < expected.len() as u64 {
        let length = PAGE_BYTES.min(expected.len() as u64 - offset) as u32;
        let response = runtime
            .read_file_change(ReadFileChangeRequest::new(
                session_id,
                request_id.as_str(),
                side,
                offset,
                length,
            ))
            .expect("持久文件快照页应读取");
        response.validate().expect("文件快照页应满足 ACP 合同");
        assert_eq!(response.offset, offset, "响应页偏移必须回显请求偏移");
        assert_eq!(response.total_bytes, expected.len() as u64);
        let bytes = response.decoded_data().expect("文件快照页 Base64 应可解码");
        assert!(!bytes.is_empty(), "非空快照分页不能无进展");
        rebuilt.extend_from_slice(&bytes);
        offset += bytes.len() as u64;
        assert_eq!(response.eof, offset == expected.len() as u64);
    }
    assert_eq!(rebuilt, expected);
    rebuilt
}

/// 读取全部 Journal 页，避免测试依赖具体 Journal 页大小。
fn journal_records(session: &RuntimeSession) -> Vec<SessionEventRecord> {
    let mut records = Vec::new();
    let mut after = None;
    loop {
        let page = session.replay(after, 256).expect("测试 Journal 页应读取");
        records.extend(page.records);
        if !page.has_more {
            break;
        }
        let next = page.next_after.expect("有后续 Journal 时必须有游标");
        assert!(next > after.unwrap_or(0), "Journal 游标必须单调前进");
        after = Some(next);
    }
    records
}

/// 在冷恢复 Journal 中定位包含指定模型工具调用的完整 Transcript 段。
fn transcript_record_for_tool_call<'a>(
    records: &'a [SessionEventRecord],
    tool_call_id: &str,
) -> &'a SessionEventRecord {
    records
        .iter()
        .find(|record| match &record.event {
            SessionEvent::TranscriptSegmentCommitted { segment } => segment
                .messages
                .iter()
                .flat_map(|message| message.content.iter())
                .any(|part| match part {
                    MessagePart::ToolCall {
                        tool_call_id: known,
                        ..
                    }
                    | MessagePart::ToolResult {
                        tool_call_id: known,
                        ..
                    } => known == tool_call_id,
                    _ => false,
                }),
            SessionEvent::AtomicBatch { events } => events.iter().any(|event| {
                matches!(
                    event,
                    SessionEvent::TranscriptSegmentCommitted { segment }
                        if segment
                            .messages
                            .iter()
                            .flat_map(|message| message.content.iter())
                            .any(|part| match part {
                                MessagePart::ToolCall {
                                    tool_call_id: known,
                                    ..
                                }
                                | MessagePart::ToolResult {
                                    tool_call_id: known,
                                    ..
                                } => known == tool_call_id,
                                _ => false,
                            })
                )
            }),
            _ => false,
        })
        .expect("目标模型工具调用应有完整 Transcript 段")
}

/// 在普通事件或 AtomicBatch 中查找目标文件生命周期阶段。
fn phase_for_event(event: &SessionEvent, request_id: &RequestId) -> Option<&'static str> {
    match event {
        SessionEvent::AtomicBatch { events } => events
            .iter()
            .find_map(|nested| phase_for_event(nested, request_id)),
        SessionEvent::ToolFileChangePrepared {
            request_id: known, ..
        } if known == request_id => Some("prepared"),
        SessionEvent::ToolFileChangeApplied { request_id: known } if known == request_id => {
            Some("applied")
        }
        SessionEvent::ToolCompleted {
            request_id: known, ..
        } if known == request_id => Some("completed"),
        _ => None,
    }
}

/// 断言一个真实工具请求恰好拥有 Prepared、Applied、Completed 三条有序证据。
fn assert_file_lifecycle_order(records: &[SessionEventRecord], request_id: &RequestId) {
    let phases = records
        .iter()
        .filter_map(|record| {
            phase_for_event(&record.event, request_id).map(|phase| (phase, record.sequence))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        phases
            .iter()
            .filter(|(phase, _)| *phase == "prepared")
            .count(),
        1,
        "每个请求必须只有一个 Prepared 事件"
    );
    assert_eq!(
        phases
            .iter()
            .filter(|(phase, _)| *phase == "applied")
            .count(),
        1,
        "每个请求必须只有一个 Applied 事件"
    );
    assert_eq!(
        phases
            .iter()
            .filter(|(phase, _)| *phase == "completed")
            .count(),
        1,
        "每个请求必须只有一个 Completed 事件"
    );
    let prepared = phases
        .iter()
        .find(|(phase, _)| *phase == "prepared")
        .map(|(_, sequence)| *sequence)
        .expect("Prepared 序号应存在");
    let applied = phases
        .iter()
        .find(|(phase, _)| *phase == "applied")
        .map(|(_, sequence)| *sequence)
        .expect("Applied 序号应存在");
    let completed = phases
        .iter()
        .find(|(phase, _)| *phase == "completed")
        .map(|(_, sequence)| *sequence)
        .expect("Completed 序号应存在");
    assert!(
        prepared < applied,
        "Journal 必须先确认 Prepared 再确认 Applied"
    );
    assert!(
        applied < completed,
        "Journal 必须先确认 Applied 再确认 Completed"
    );
}

/// 从最终 Session 状态取出某个模型工具调用的持久文件证据。
fn file_change_for_call(
    state: &SessionState,
    model_tool_call_id: &str,
) -> (RequestId, ToolFileChange) {
    state
        .tools
        .values()
        .find(|tool| tool.request.model_tool_call_id == model_tool_call_id)
        .map(|tool| (tool.request.request_id.clone(), tool.file_change.clone()))
        .and_then(|(request_id, change)| change.map(|change| (request_id, change)))
        .expect("模型工具调用应有文件变更证据")
}

/// 递归收集 ACP JSON 中指定 `type` 的全部节点。
fn typed_nodes(value: &Value, kind: &str) -> Vec<Value> {
    let mut nodes = Vec::new();
    match value {
        Value::Array(items) => {
            for item in items {
                nodes.extend(typed_nodes(item, kind));
            }
        }
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some(kind) {
                nodes.push(value.clone());
            }
            for child in object.values() {
                nodes.extend(typed_nodes(child, kind));
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    nodes
}

/// 将一批 ACP 投影草稿编码为可检查的标准桌面载荷。
fn materialize_drafts(session_id: &str, drafts: Vec<super::super::DeliveryDraft>) -> Vec<Value> {
    drafts
        .into_iter()
        .enumerate()
        .map(|(index, draft)| {
            serde_json::to_value(
                materialize_delivery(session_id, (index + 1) as u64, draft)
                    .expect("重放草稿应可物化为 ACP 载荷"),
            )
            .expect("ACP 载荷应可编码")
        })
        .collect()
}

/// 断言 ACP 内容中存在指定的标准 Diff。
fn assert_has_diff(values: &[Value], old_text: Option<&str>, new_text: &str) {
    let diffs = values
        .iter()
        .flat_map(|value| typed_nodes(value, "diff"))
        .collect::<Vec<_>>();
    if !diffs.iter().any(|diff| {
        diff.get("newText").and_then(Value::as_str) == Some(new_text)
            && diff.get("oldText").and_then(Value::as_str) == old_text
    }) {
        panic!("ACP 载荷应包含精确的标准 Diff：{new_text}");
    }
}

/// 断言 ACP 内容使用 ResourceLink，并核对命名空间元数据中的快照状态。
fn assert_has_file_change_link(values: &[Value], applied: bool) {
    let links = values
        .iter()
        .flat_map(|value| typed_nodes(value, "resource_link"))
        .collect::<Vec<_>>();
    assert!(
        !links.is_empty(),
        "大文件、二进制或 Prepared 证据必须使用 ResourceLink"
    );
    assert!(links.iter().any(|link| {
        link.get("_meta")
            .and_then(|meta| meta.get(FILE_CHANGE_META_KEY))
            .and_then(|reference| reference.get("applied"))
            .and_then(Value::as_bool)
            == Some(applied)
    }));
}

/// 等待异步桌面投递泵真正处理到一个匹配的 Diff。
async fn wait_for_diff(emitter: &RecordingEmitter, new_text: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let values = emitter.snapshot();
        let diffs = values
            .iter()
            .flat_map(|value| typed_nodes(value, "diff"))
            .collect::<Vec<_>>();
        if diffs
            .iter()
            .any(|diff| diff.get("newText").and_then(Value::as_str) == Some(new_text))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("桌面投递泵未在有限时间内发送目标 Diff");
}

/// 真实 Write/Edit 应保存 BOM、CRLF 与缺失文件语义，并在关闭重开后保持标准 ACP Diff。
#[tokio::test]
async fn real_write_edit_lifecycle_and_replay_diff() {
    let fixture = DesktopFixture::new("desktop-file-change-replay");
    let path = fixture.project.path().join("notes.txt");
    let before_text = "\u{feff}BEFORE_SNAPSHOT_ONLY\r\n";
    let after_text = "\u{feff}AFTER_SNAPSHOT_ONLY\r\n";
    let provider = scripted_provider([
        tool_reply(
            "call-write",
            "Write",
            json!({
                "file_path": "notes.txt",
                "content": before_text,
            }),
        ),
        tool_reply(
            "call-edit",
            "Edit",
            json!({
                "file_path": "notes.txt",
                "old_string": "BEFORE_SNAPSHOT_ONLY",
                "new_string": "AFTER_SNAPSHOT_ONLY",
            }),
        ),
        text_reply("文件变更已完成"),
    ]);
    run_scripted_turn(
        &fixture.session,
        provider.clone(),
        fixture.file_tools(),
        "turn-write-edit",
        "写入并编辑 notes.txt",
    )
    .await;

    let final_bytes = fs::read(&path).expect("真实 Edit 文件应存在");
    assert_eq!(final_bytes, after_text.as_bytes());
    let snapshot = fixture.session.snapshot().expect("最终 Session 状态应读取");
    let (write_id, write_change) = file_change_for_call(&snapshot.state, "call-write");
    let (edit_id, edit_change) = file_change_for_call(&snapshot.state, "call-edit");
    assert!(
        write_change.before.is_none(),
        "首次 Write 必须保存缺失文件语义"
    );
    assert_eq!(
        fixture
            .session
            .read_file_snapshot(&write_change.after)
            .expect("Write after 快照应读取"),
        before_text.as_bytes()
    );
    assert_eq!(
        fixture
            .session
            .read_file_snapshot(edit_change.before.as_ref().expect("Edit before 应存在"))
            .expect("Edit before 快照应读取"),
        before_text.as_bytes()
    );
    assert_eq!(
        fixture
            .session
            .read_file_snapshot(&edit_change.after)
            .expect("Edit after 快照应读取"),
        after_text.as_bytes()
    );
    let records = journal_records(&fixture.session);
    assert_file_lifecycle_order(&records, &write_id);
    assert_file_lifecycle_order(&records, &edit_id);

    // 模型看到的 ToolResult 只能是工具自身短结果，不能把 recorder 的原始快照带回模型。
    let model_transcript = fixture
        .session
        .model_transcript()
        .expect("模型 Transcript 应物化");
    for message in model_transcript {
        for block in message.content {
            if let ContentBlock::ToolResult { tool_result } = block {
                let result = serde_json::to_string(&tool_result).expect("模型 ToolResult 应可编码");
                assert!(!result.contains("BEFORE_SNAPSHOT_ONLY"));
                assert!(!result.contains("AFTER_SNAPSHOT_ONLY"));
            }
        }
    }

    // 外部修改当前文件后，读取仍只返回 Journal/Artifact 中的历史 after 快照。
    fs::write(&path, b"EXTERNAL_CURRENT_FILE").expect("应模拟外部修改当前文件");
    let persisted_after = fixture
        .runtime
        .read_file_change(ReadFileChangeRequest::new(
            fixture.session.session_id().as_str(),
            edit_id.as_str(),
            FileChangeSide::After,
            0,
            512,
        ))
        .expect("历史 after 快照应可读取");
    assert_eq!(
        persisted_after
            .decoded_data()
            .expect("历史 after 页应可解码"),
        after_text.as_bytes()
    );

    let expected_records = records.clone();
    wait_for_diff(&fixture.emitter, after_text).await;
    let DesktopFixture {
        _storage: storage,
        project,
        runtime,
        session,
        emitter,
    } = fixture;
    runtime
        .shutdown()
        .await
        .expect("关闭 RuntimeManager 应成功");
    let live_values = emitter.snapshot();
    assert_has_diff(&live_values, Some(before_text), after_text);
    drop(session);
    drop(runtime);

    let replay_emitter = Arc::new(RecordingEmitter::default());
    let replay_runtime = Arc::new(
        AgentRuntime::new(
            storage.path(),
            Arc::clone(&replay_emitter) as Arc<dyn DeliveryEmitter>,
        )
        .expect("冷恢复 Agent Runtime 应创建"),
    );
    let reopened = match replay_runtime
        .runtime_manager()
        .open("desktop-file-change-replay")
        .expect("同一 Session 应重新打开")
    {
        OpenSessionResult::Ready(session) => session,
        OpenSessionResult::Corrupt(report) => {
            panic!("文件变更 Journal 不应损坏：{:?}", report.issues)
        }
    };
    let reopened_records = journal_records(&reopened);
    assert_eq!(
        reopened_records.len(),
        expected_records.len(),
        "冷恢复不得重复追加工具生命周期事件"
    );
    assert_file_lifecycle_order(&reopened_records, &write_id);
    assert_file_lifecycle_order(&reopened_records, &edit_id);
    let reopened_state = reopened.snapshot().expect("冷恢复状态应读取").state;
    let transcript_record = transcript_record_for_tool_call(&reopened_records, "call-edit");
    let replay_drafts = map_authoritative_record(
        &reopened,
        &reopened_state,
        transcript_record,
        AuthoritativeProjectionMode::Replay,
    )
    .expect("冷恢复工具 Transcript 应可映射");
    let replay_values = materialize_drafts(reopened.session_id().as_str(), replay_drafts);
    assert_has_diff(&replay_values, Some(before_text), after_text);
    assert_eq!(
        read_snapshot_pages(
            &replay_runtime,
            reopened.session_id().as_str(),
            &edit_id,
            FileChangeSide::After,
            after_text.as_bytes(),
        ),
        after_text.as_bytes()
    );
    replay_runtime
        .shutdown()
        .await
        .expect("冷恢复 RuntimeManager 应关闭");
    drop(reopened);
    drop(replay_runtime);
    drop(project);
}

/// 大文件分页、二进制 before、NUL after 与空文件必须走无损快照引用边界。
#[tokio::test]
async fn large_binary_null_and_empty_changes_are_lossless() {
    let fixture = DesktopFixture::new("desktop-file-change-pages");
    let large_path = fixture.project.path().join("large.txt");
    let large_before = format!("{}NEEDLE{}", "x".repeat(350_000), "y".repeat(350_000));
    fs::write(&large_path, large_before.as_bytes()).expect("大文件应创建");
    let large_after = large_before.replacen("NEEDLE", "CHANGED", 1);
    run_scripted_turn(
        &fixture.session,
        scripted_provider([
            tool_reply(
                "call-large-edit",
                "Edit",
                json!({
                    "file_path": "large.txt",
                    "old_string": "NEEDLE",
                    "new_string": "CHANGED",
                }),
            ),
            text_reply("大文件已编辑"),
        ]),
        fixture.file_tools(),
        "turn-large-edit",
        "编辑大文件",
    )
    .await;
    let state = fixture.session.snapshot().expect("大文件状态应读取").state;
    let (large_id, large_change) = file_change_for_call(&state, "call-large-edit");
    let large_content =
        change_content(&fixture.session, &large_id, &large_change).expect("大文件 ACP 内容应构造");
    let large_value = serde_json::to_value(large_content).expect("大文件 ACP 内容应编码");
    assert_has_file_change_link(std::slice::from_ref(&large_value), true);
    assert!(
        !serde_json::to_string(&large_value)
            .expect("大文件引用应编码")
            .contains("xxxxxxxx")
    );
    assert_eq!(
        read_snapshot_pages(
            &fixture.runtime,
            fixture.session.session_id().as_str(),
            &large_id,
            FileChangeSide::Before,
            large_before.as_bytes(),
        ),
        large_before.as_bytes()
    );
    assert_eq!(
        read_snapshot_pages(
            &fixture.runtime,
            fixture.session.session_id().as_str(),
            &large_id,
            FileChangeSide::After,
            large_after.as_bytes(),
        ),
        large_after.as_bytes()
    );
    let over_page = ReadFileChangeRequest::new(
        fixture.session.session_id().as_str(),
        large_id.as_str(),
        FileChangeSide::After,
        0,
        512 * 1024 + 1,
    );
    assert!(
        over_page.validate().is_err(),
        "超过 512 KiB 的页必须在参数边界拒绝"
    );

    let binary_path = fixture.project.path().join("binary.bin");
    let binary_before = vec![0_u8, 0xff, 0x80, b'B', b'I', b'N'];
    fs::write(&binary_path, &binary_before).expect("二进制 before 应写入");
    run_scripted_turn(
        &fixture.session,
        scripted_provider([
            tool_reply(
                "call-binary-write",
                "Write",
                json!({"file_path": "binary.bin", "content": "binary-after"}),
            ),
            text_reply("二进制文件已替换"),
        ]),
        fixture.file_tools(),
        "turn-binary-write",
        "替换二进制文件",
    )
    .await;
    let state = fixture.session.snapshot().expect("二进制状态应读取").state;
    let (binary_id, binary_change) = file_change_for_call(&state, "call-binary-write");
    let binary_content = change_content(&fixture.session, &binary_id, &binary_change)
        .expect("二进制 ACP 内容应构造");
    let binary_value = serde_json::to_value(binary_content).expect("二进制 ACP 内容应编码");
    assert_has_file_change_link(std::slice::from_ref(&binary_value), true);
    assert!(
        !typed_nodes(&binary_value, "diff").iter().any(|diff| {
            diff.get("oldText")
                .and_then(Value::as_str)
                .is_some_and(|old| old.contains('\u{fffd}'))
        }),
        "二进制 before 不得通过 lossyDiff 伪造文本"
    );
    assert_eq!(
        read_snapshot_pages(
            &fixture.runtime,
            fixture.session.session_id().as_str(),
            &binary_id,
            FileChangeSide::Before,
            &binary_before,
        ),
        binary_before
    );

    run_scripted_turn(
        &fixture.session,
        scripted_provider([
            tool_reply(
                "call-empty-write",
                "Write",
                json!({"file_path": "empty.txt", "content": ""}),
            ),
            text_reply("空文件已创建"),
        ]),
        fixture.file_tools(),
        "turn-empty-write",
        "创建空文件",
    )
    .await;
    let state = fixture.session.snapshot().expect("空文件状态应读取").state;
    let (empty_id, empty_change) = file_change_for_call(&state, "call-empty-write");
    assert!(empty_change.before.is_none());
    assert!(
        typed_nodes(
            &serde_json::to_value(
                change_content(&fixture.session, &empty_id, &empty_change)
                    .expect("空文件 Diff 应构造")
            )
            .expect("空文件 ACP 内容应编码"),
            "diff"
        )
        .iter()
        .any(|diff| diff.get("newText").and_then(Value::as_str) == Some("")),
        "缺失文件到空文件必须保留标准 Diff"
    );
    assert_eq!(
        read_snapshot_pages(
            &fixture.runtime,
            fixture.session.session_id().as_str(),
            &empty_id,
            FileChangeSide::After,
            &[],
        ),
        Vec::<u8>::new()
    );

    let nul_path = fixture.project.path().join("nul.txt");
    run_scripted_turn(
        &fixture.session,
        scripted_provider([
            tool_reply(
                "call-nul-write",
                "Write",
                json!({"file_path": "nul.txt", "content": "visible\0binary"}),
            ),
            text_reply("NUL 文件已创建"),
        ]),
        fixture.file_tools(),
        "turn-nul-write",
        "创建带 NUL 的文件",
    )
    .await;
    assert_eq!(
        fs::read(&nul_path).expect("NUL 文件应存在"),
        b"visible\0binary",
        "NUL after 应按原始字节写入工作区"
    );
    let state = fixture.session.snapshot().expect("NUL 状态应读取").state;
    let (nul_id, nul_change) = file_change_for_call(&state, "call-nul-write");
    let nul_value = serde_json::to_value(
        change_content(&fixture.session, &nul_id, &nul_change).expect("NUL ACP 内容应构造"),
    )
    .expect("NUL ACP 内容应编码");
    assert_has_file_change_link(std::slice::from_ref(&nul_value), true);
    assert!(
        typed_nodes(&nul_value, "diff").is_empty(),
        "NUL after 不得伪造文本 Diff"
    );
    fs::write(&large_path, b"EXTERNAL_LARGE_FILE").expect("应模拟大文件外部修改");
    assert_eq!(
        read_snapshot_pages(
            &fixture.runtime,
            fixture.session.session_id().as_str(),
            &large_id,
            FileChangeSide::After,
            large_after.as_bytes(),
        ),
        large_after.as_bytes(),
        "外部修改当前文件不得改变历史快照"
    );
    fixture
        .runtime
        .shutdown()
        .await
        .expect("大文件 RuntimeManager 应关闭");
}

/// Prepared 未应用时只能暴露可读取引用；跨 Session、未知请求、缺失 before 和超页请求必须拒绝。
#[tokio::test]
async fn prepared_change_and_read_boundaries_are_fail_closed() {
    let fixture = DesktopFixture::new("desktop-file-change-guards");
    let recorder = Arc::new(RuntimeFileMutationRecorder::new(fixture.session.clone()));
    let prepared_path = fixture.project.path().join("prepared-only.txt");
    let prepared_after = b"prepared-only-after".to_vec();
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(PreparedOnlyWriteTool {
            recorder,
            path: prepared_path.clone(),
            before: None,
            after: prepared_after.clone(),
        }))
        .expect("Prepared-only Write 工具应注册");
    run_scripted_turn(
        &fixture.session,
        scripted_provider([
            tool_reply(
                "call-prepared-only",
                "Write",
                json!({"file_path": "prepared-only.txt", "content": "ignored-by-fixture"}),
            ),
            text_reply("Prepared 证据已保存"),
        ]),
        tools,
        "turn-prepared-only",
        "只准备文件变更但不应用",
    )
    .await;
    let state = fixture
        .session
        .snapshot()
        .expect("Prepared 状态应读取")
        .state;
    let (request_id, change) = file_change_for_call(&state, "call-prepared-only");
    assert!(
        !change.applied,
        "未调用 mark_applied 的证据必须保持 Prepared"
    );
    let content = serde_json::to_value(
        change_content(&fixture.session, &request_id, &change).expect("Prepared ACP 内容应构造"),
    )
    .expect("Prepared ACP 内容应编码");
    assert_has_file_change_link(std::slice::from_ref(&content), false);
    assert!(
        typed_nodes(&content, "diff").is_empty(),
        "Prepared 不得被当作完成 Diff"
    );
    assert!(
        !prepared_path.exists(),
        "Prepared-only 测试工具不得伪造工作区写入"
    );

    let after_request = ReadFileChangeRequest::new(
        fixture.session.session_id().as_str(),
        request_id.as_str(),
        FileChangeSide::After,
        0,
        prepared_after.len() as u32,
    );
    assert!(
        read_file_change_page(&fixture.session, after_request)
            .expect("Prepared after 快照应可读取")
            .decoded_data()
            .expect("Prepared after 页应可解码")
            == prepared_after
    );
    assert!(
        matches!(
            read_file_change_page(
                &fixture.session,
                ReadFileChangeRequest::new(
                    fixture.session.session_id().as_str(),
                    request_id.as_str(),
                    FileChangeSide::Before,
                    0,
                    1,
                ),
            ),
            Err(AgentRuntimeError::SessionUnavailable)
        ),
        "不存在 before 快照必须拒绝"
    );
    assert!(
        matches!(
            read_file_change_page(
                &fixture.session,
                ReadFileChangeRequest::new(
                    fixture.session.session_id().as_str(),
                    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                    FileChangeSide::After,
                    0,
                    1,
                ),
            ),
            Err(AgentRuntimeError::SessionUnavailable)
        ),
        "未知 request_id 必须拒绝"
    );
    assert!(
        matches!(
            read_file_change_page(
                &fixture.session,
                ReadFileChangeRequest::new(
                    "other-session",
                    request_id.as_str(),
                    FileChangeSide::After,
                    0,
                    1,
                ),
            ),
            Err(AgentRuntimeError::SessionUnavailable)
        ),
        "跨 Session 读取必须拒绝"
    );
    let too_large = ReadFileChangeRequest::new(
        fixture.session.session_id().as_str(),
        request_id.as_str(),
        FileChangeSide::After,
        0,
        512 * 1024 + 1,
    );
    assert!(
        too_large.validate().is_err(),
        "超过单页预算必须在协议参数层拒绝"
    );

    fixture
        .runtime
        .shutdown()
        .await
        .expect("边界测试 RuntimeManager 应关闭");
}

/// 仅提交 Prepared 事件而不触碰工作区的工具，用于验证未应用状态的桌面投影边界。
#[derive(Debug)]
struct PreparedOnlyWriteTool {
    /// 当前 Session 的真实文件变更记录器。
    recorder: Arc<RuntimeFileMutationRecorder>,
    /// 传给记录器的绝对文件路径。
    path: PathBuf,
    /// 明确表示原文件不存在的 before 快照。
    before: Option<Vec<u8>>,
    /// 要登记但不落盘的 after 快照。
    after: Vec<u8>,
}

impl AgentTool for PreparedOnlyWriteTool {
    /// 返回与真实 Write 相同的 Provider 中立输入契约。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "Write",
            "测试只准备文件证据而不调用实际文件写入。",
            json!({
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "minLength": 1},
                    "content": {"type": "string"}
                },
                "required": ["file_path", "content"],
                "additionalProperties": false
            }),
        )
    }

    /// 测试工具仍声明与真实 Write 相同的状态变更效果。
    fn effect(&self, _input: &Value) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::ChangesState)
    }

    /// 状态变更工具必须作为独占副作用屏障执行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Exclusive
    }

    /// 调用生产 recorder 的 prepare，但有意丢弃应用句柄，不写入工作区。
    fn execute(&self, context: ToolContext, _input: Value) -> ToolFuture<'_> {
        let recorder = Arc::clone(&self.recorder);
        let path = self.path.clone();
        let before = self.before.clone();
        let after = self.after.clone();
        Box::pin(async move {
            recorder.prepare(&context, &path, before.as_deref(), &after)?;
            Ok(ToolOutput::text("Prepared 证据已登记，工作区尚未应用"))
        })
    }
}
