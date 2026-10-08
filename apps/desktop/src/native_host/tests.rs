use super::{NativeHost, projection};
use crate::{
    agent_runtime::{AgentRuntime, AgentRuntimeBuildConfig, RuntimeFileMutationRecorder},
    native_paths::NativePaths,
    native_ui::model::{
        DraftFact, MessageBlock, MessageRole, NativeEventBatch, NativeHostApi, NativeUiAction,
        NativeUiEvent, SessionStatus,
    },
};
use keencode_agent::{
    AgentRunner, PlanGuard, RunLimits, ToolRegistry, TurnCancellation, TurnRequest,
};
use keencode_model::{
    Message as ModelMessage, MessageRole as ModelMessageRole, ModelError, ModelFuture,
    ModelProvider, ModelRequest, ModelStream, ModelStreamEvent, ProviderCapabilities,
    ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason,
};
use keencode_provider::{
    ProviderConfig, ProviderModelPolicy, ProviderRegistration, ProviderRegistry, WireResponseMode,
};
use keencode_resources::{
    self as resource, ArtifactMaterialization, MessagePart, MessageRole as ResourceMessageRole,
    PersistedToolResult, ProviderProtocolSnapshot, ProviderSnapshot, SessionMessage,
    ToolCompletionStatus, ToolEffect, ToolLifecycle, ToolOutcome, ToolRequest, TurnStatus,
};
use keencode_runtime::{RuntimeSession, RuntimeTurnRequest};
use keencode_tools::{ReadTool, ToolEnvironment, WriteTool};
use std::{
    fs,
    net::TcpListener,
    path::Path,
    sync::{Arc, Condvar, Mutex, OnceLock},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::sync::Notify;

#[test]
fn draft_generation_barrier_rejects_late_edits_and_keeps_new_edits() {
    let mut state = super::DraftGenerationState::default();
    assert!(state.accepts(7));
    state.mark_cleared(7);
    assert!(!state.accepts(6));
    assert!(!state.accepts(7));
    assert!(state.accepts(8));
}

struct BlockingProvider {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl ModelProvider for BlockingProvider {
    fn capabilities(&self, _model: &str) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn stream(&self, _request: ModelRequest) -> ModelFuture<'_, Result<ModelStream, ModelError>> {
        let started = Arc::clone(&self.started);
        let release = Arc::clone(&self.release);
        Box::pin(async move {
            started.notify_one();
            let stream = futures::stream::unfold(0u8, move |state| {
                let release = Arc::clone(&release);
                async move {
                    match state {
                        0 => Some((
                            Ok(ModelStreamEvent::MessageStart {
                                metadata: ResponseMetadata::default(),
                            }),
                            1,
                        )),
                        1 => {
                            release.notified().await;
                            Some((
                                Ok(ModelStreamEvent::TextDelta {
                                    index: 0,
                                    delta: "阻塞期间未被 idle release 取消".to_owned(),
                                }),
                                2,
                            ))
                        }
                        2 => Some((
                            Ok(ModelStreamEvent::MessageEnd {
                                stop_reason: StopReason::Completed,
                            }),
                            3,
                        )),
                        _ => None,
                    }
                }
            });
            Ok(Box::pin(stream) as ModelStream)
        })
    }
}

struct Fixture {
    _storage: TempDir,
    _runtime: Arc<AgentRuntime>,
    host: NativeHost,
    session: RuntimeSession,
}

fn fixture_with_registry(
    provider_registry: ProviderRegistry,
    default_provider: Option<(String, String)>,
) -> Fixture {
    let storage = tempfile::tempdir().expect("NativeHost 测试存储目录应创建");
    let paths = Arc::new(NativePaths::from_data_root(storage.path().to_path_buf()));
    // 测试执行器由进程级夹具持有；异步测试结束时不能在 Tokio worker 中销毁 Runtime。
    static TEST_EXECUTOR: OnceLock<Arc<tokio::runtime::Runtime>> = OnceLock::new();
    let executor = TEST_EXECUTOR
        .get_or_init(|| {
            Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                    .expect("NativeHost 测试执行器应创建"),
            )
        })
        .clone();
    let runtime = AgentRuntime::build_native(AgentRuntimeBuildConfig {
        storage_root: storage.path().to_path_buf(),
        provider_registry,
        analytics: None,
        default_provider,
        memory_service: None,
        local_memories_enabled: false,
        executor_handle: executor.handle().clone(),
    })
    .expect("Native Runtime 应创建");
    let session = runtime
        .open_or_create_session(storage.path(), None, "native-host-test")
        .expect("NativeHost 测试 Session 应创建");
    let host = NativeHost::from_runtime(paths, runtime.clone(), executor)
        .expect("NativeHost 测试装配应成功");
    Fixture {
        _storage: storage,
        _runtime: runtime,
        host,
        session,
    }
}

fn fixture() -> Fixture {
    fixture_with_registry(ProviderRegistry::new(), None)
}

/// 撤销测试的恢复路径需要可解析 Provider 快照；Turn 执行器仍使用脚本 Provider，不发网络请求。
fn rewind_fixture() -> (Fixture, ProviderSnapshot) {
    let provider_id = "native-rewind-provider";
    let model = "native-rewind-model";
    let provider_registry = ProviderRegistry::new();
    let provider_config = ProviderConfig::new_unauthenticated(
        provider_id,
        keencode_model::ProviderProtocol::Responses,
        "https://native-rewind.test/v1",
    )
    .expect("撤销测试 Provider 配置应有效");
    provider_registry
        .replace_all([ProviderRegistration::new(
            provider_config,
            "Native Rewind 测试 Provider",
            "native-rewind-test-revision",
            ProviderModelPolicy::Enumerated {
                models: vec![model.to_owned()],
            },
        )
        .expect("撤销测试 Provider 注册项应有效")])
        .expect("撤销测试 Provider 注册表应替换");
    let resolved = provider_registry
        .resolve(provider_id, model)
        .expect("撤销测试 Provider 应可解析");
    let provider_snapshot = ProviderSnapshot {
        provider_id: provider_id.to_owned(),
        model: model.to_owned(),
        context_window: None,
        protocol: ProviderProtocolSnapshot::OpenAiResponses,
        config_fingerprint: resolved.config_identity().to_owned(),
        reasoning_effort: None,
    };
    (
        fixture_with_registry(
            provider_registry,
            Some((provider_id.to_owned(), model.to_owned())),
        ),
        provider_snapshot,
    )
}

/// 为 Stop 测试提供一个真实 HTTP Provider；它在收到请求后保持连接打开，
/// 让 Runtime 的取消路径而不是测试 fixture 的直连 Runner 决定 Turn 终态。
struct BlockingResponsesServer {
    started: Arc<Notify>,
    release: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl BlockingResponsesServer {
    fn release(&self) {
        let (released, signal) = &*self.release;
        if let Ok(mut released) = released.lock() {
            *released = true;
            signal.notify_all();
        }
    }
}

impl Drop for BlockingResponsesServer {
    fn drop(&mut self) {
        self.release();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_blocking_responses_server() -> (String, BlockingResponsesServer) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("本地阻塞 Provider 端口应绑定");
    listener
        .set_nonblocking(true)
        .expect("本地阻塞 Provider 监听器应设为非阻塞");
    let address = listener.local_addr().expect("本地阻塞 Provider 地址应读取");
    let started = Arc::new(Notify::new());
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let server_started = Arc::clone(&started);
    let server_release = Arc::clone(&release);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let Ok((_stream, _peer)) = (|| {
            loop {
                match listener.accept() {
                    Ok(connection) => break Ok(connection),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err(());
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return Err(()),
                }
            }
        })() else {
            return;
        };
        server_started.notify_one();
        let (released, signal) = &*server_release;
        let Ok(mut released) = released.lock() else {
            return;
        };
        while !*released {
            let Ok(next) = signal.wait(released) else {
                return;
            };
            released = next;
        }
    });
    (
        format!("http://{address}/v1"),
        BlockingResponsesServer {
            started,
            release,
            handle: Some(handle),
        },
    )
}

fn scripted_events(text: &str) -> impl IntoIterator<Item = ModelStreamEvent> {
    [
        ModelStreamEvent::MessageStart {
            metadata: ResponseMetadata::default(),
        },
        ModelStreamEvent::ReasoningDelta {
            index: 0,
            delta: "先检查上下文。".to_owned(),
        },
        ModelStreamEvent::TextDelta {
            index: 1,
            delta: text.to_owned(),
        },
        ModelStreamEvent::MessageEnd {
            stop_reason: StopReason::Completed,
        },
    ]
}

fn tool_reply(call_id: &str, name: &str, arguments: serde_json::Value) -> ScriptedReply {
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

async fn run_file_write_turn(
    session: &RuntimeSession,
    project_root: &Path,
    turn_id: &str,
    provider_snapshot: ProviderSnapshot,
) {
    let recorder = Arc::new(RuntimeFileMutationRecorder::new(session.clone()));
    let environment = Arc::new(
        ToolEnvironment::new(project_root)
            .expect("文件工具环境应创建")
            .with_file_mutation_recorder(recorder),
    );
    let mut tools = ToolRegistry::new();
    tools
        .register(Arc::new(ReadTool::new(Arc::clone(&environment))))
        .expect("Read 工具应注册");
    tools
        .register(Arc::new(WriteTool::new(environment)))
        .expect("Write 工具应注册");
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            parallel_tool_calls: false,
            ..ProviderCapabilities::default()
        },
        [
            tool_reply(
                "call-native-rewind-read",
                "Read",
                serde_json::json!({
                    "file_path": "rewind.txt",
                }),
            ),
            tool_reply(
                "call-native-rewind-write",
                "Write",
                serde_json::json!({
                    "file_path": "rewind.txt",
                    "content": "after-rewind",
                }),
            ),
            text_reply("文件写入完成"),
        ],
    ));
    let input = ModelMessage::text(ModelMessageRole::User, "写入用于撤销的文件");
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("测试 Session ID 应有效"),
        keencode_agent::TurnId::new(turn_id).expect("测试 Turn ID 应有效"),
        keencode_agent::AgentId::new(resource::ROOT_AGENT_ID).expect("根 Agent ID 应有效"),
        "native-rewind-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );
    session
        .bind_agent_runner(AgentRunner::new(provider, tools, RunLimits::default()))
        .run_turn(
            RuntimeTurnRequest::root(request, vec![input], "写入用于撤销的文件")
                .with_provider_snapshot(provider_snapshot),
        )
        .await
        .expect("真实 Write Turn 应完成");
}

async fn run_scripted_turn(
    session: &RuntimeSession,
    turn_id: &str,
    prompt: &str,
    events: impl IntoIterator<Item = ModelStreamEvent>,
) -> keencode_agent::TurnResult {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events(events)],
    ));
    let input = ModelMessage::text(ModelMessageRole::User, prompt);
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("测试 Session ID 应有效"),
        keencode_agent::TurnId::new(turn_id).expect("测试 Turn ID 应有效"),
        keencode_agent::AgentId::new(resource::ROOT_AGENT_ID).expect("根 Agent ID 应有效"),
        "scripted-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );
    session
        .bind_agent_runner(AgentRunner::new(
            provider,
            ToolRegistry::new(),
            RunLimits::default(),
        ))
        .run_turn(RuntimeTurnRequest::root(request, vec![input], prompt))
        .await
        .expect("脚本化 Turn 应形成权威终态")
}

async fn run_failed_turn(
    session: &RuntimeSession,
    turn_id: &str,
    prompt: &str,
) -> keencode_agent::TurnResult {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![Err(
            ModelError::ProviderUnavailable {
                message: "测试 Provider 失败".to_owned(),
                status_code: None,
                retryable: false,
            },
        )])],
    ));
    let input = ModelMessage::text(ModelMessageRole::User, prompt);
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("测试 Session ID 应有效"),
        keencode_agent::TurnId::new(turn_id).expect("测试 Turn ID 应有效"),
        keencode_agent::AgentId::new(resource::ROOT_AGENT_ID).expect("根 Agent ID 应有效"),
        "scripted-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );
    session
        .bind_agent_runner(AgentRunner::new(
            provider,
            ToolRegistry::new(),
            RunLimits::default(),
        ))
        .run_turn(RuntimeTurnRequest::root(request, vec![input], prompt))
        .await
        .expect("Provider 失败也应持久化 TurnStopped")
}

async fn run_cancelled_turn(
    session: &RuntimeSession,
    turn_id: &str,
    prompt: &str,
) -> keencode_agent::TurnResult {
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events(scripted_events("不会显示"))],
    ));
    let input = ModelMessage::text(ModelMessageRole::User, prompt);
    let cancellation = TurnCancellation::new();
    cancellation.cancel();
    let mut request = TurnRequest::new(
        keencode_agent::SessionId::new(session.session_id().as_str())
            .expect("测试 Session ID 应有效"),
        keencode_agent::TurnId::new(turn_id).expect("测试 Turn ID 应有效"),
        keencode_agent::AgentId::new(resource::ROOT_AGENT_ID).expect("根 Agent ID 应有效"),
        "scripted-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );
    request.set_cancellation(cancellation);
    session
        .bind_agent_runner(AgentRunner::new(
            provider,
            ToolRegistry::new(),
            RunLimits::default(),
        ))
        .run_turn(RuntimeTurnRequest::root(request, vec![input], prompt))
        .await
        .expect("预取消 Turn 应持久化 TurnStopped")
}

async fn wait_for_batch<F>(
    batches: &Arc<Mutex<Vec<NativeEventBatch>>>,
    changed: &Arc<Notify>,
    predicate: F,
) where
    F: Fn(&[NativeEventBatch]) -> bool,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if predicate(&batches.lock().expect("事件集合锁应可用")) {
                return;
            }
            changed.notified().await;
        }
    })
    .await
    .expect("NativeHost 事件应在超时前到达");
}

#[test]
fn initialize_hot_keeps_the_highest_observed_watermark() {
    let fixture = fixture();
    let session_id = fixture.session.session_id().as_str();

    fixture.host.initialize_hot(session_id, 41);
    fixture.host.initialize_hot(session_id, 7);
    assert_eq!(fixture.host.hot_snapshot(session_id).0, 41);

    fixture.host.initialize_hot(session_id, 44);
    assert_eq!(fixture.host.hot_snapshot(session_id).0, 44);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_projects_reasoning_text_and_replay_pages() {
    let fixture = fixture();
    let result = run_scripted_turn(
        &fixture.session,
        "turn-native-projection",
        "分页和投影",
        scripted_events("最终正文"),
    )
    .await;
    assert!(result.is_success());

    let conversation =
        projection::conversation(&fixture.host, &fixture.session, None).expect("会话投影应成功");
    assert_eq!(
        conversation.session.last_sequence,
        fixture.session.snapshot().unwrap().state.last_sequence
    );
    let assistant = conversation
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Assistant)
        .expect("应投影 Assistant 消息");
    assert!(assistant.blocks.iter().any(|block| matches!(
        block,
        MessageBlock::Reasoning { source, streaming: false, .. } if source == "先检查上下文。"
    )));
    assert!(assistant.blocks.iter().any(|block| matches!(
        block,
        MessageBlock::Markdown { source, streaming: false, .. } if source == "最终正文"
    )));

    let first = fixture
        .session
        .replay(None, 1)
        .expect("第一页 Journal 应可读取");
    assert_eq!(first.records.len(), 1);
    let second = fixture
        .session
        .replay(first.next_after, 1)
        .expect("第二页 Journal 应可读取");
    assert_eq!(second.records.len(), 1);
    assert!(second.records[0].sequence > first.records[0].sequence);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conversation_history_cursor_pages_are_exclusive_and_repeatable() {
    let fixture = fixture();
    for index in 0..80 {
        let text = format!("分页正文-{index}");
        run_scripted_turn(
            &fixture.session,
            &format!("turn-native-page-{index}"),
            &format!("分页问题-{index}"),
            scripted_events(&text),
        )
        .await;
    }

    let mut page =
        projection::conversation(&fixture.host, &fixture.session, None).expect("最新会话页应读取");
    let mut message_ids = page
        .messages
        .iter()
        .map(|message| message.message_id.clone())
        .collect::<Vec<_>>();
    let mut page_count = 0;
    while let Some(cursor) = page.history.clone() {
        let next = projection::conversation(&fixture.host, &fixture.session, Some(cursor))
            .expect("历史页应按排他游标读取");
        assert!(
            next.messages
                .iter()
                .all(|message| !message_ids.iter().any(|id| id == &message.message_id)),
            "历史页不应重复已经读取的消息"
        );
        message_ids.extend(
            next.messages
                .iter()
                .map(|message| message.message_id.clone()),
        );
        page = next;
        page_count += 1;
        assert!(page_count <= 8, "历史游标不应在有限 Journal 中循环");
    }
    assert!(page_count > 0, "测试数据应跨越至少一个历史页");
    assert!(message_ids.len() >= 80, "所有脚本化轮次都应可从历史页恢复");
}

/// 真实 Write 的 Journal 事实、Native 投影和重复撤销保护必须贯通。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_write_projection_rewind_and_duplicate_are_guarded() {
    let (fixture, provider_snapshot) = rewind_fixture();
    let path = fixture._storage.path().join("rewind.txt");
    let before = b"before-rewind";
    fs::write(&path, before).expect("撤销测试的原文件应创建");
    run_file_write_turn(
        &fixture.session,
        fixture._storage.path(),
        "turn-native-rewind",
        provider_snapshot,
    )
    .await;
    assert_eq!(
        fs::read(&path).expect("真实 Write 文件应存在"),
        b"after-rewind"
    );

    let snapshot = fixture
        .session
        .snapshot()
        .expect("Write Session 状态应读取");
    let tool = snapshot
        .state
        .tools
        .values()
        .find(|tool| tool.request.model_tool_call_id == "call-native-rewind-write")
        .expect("真实 Write 应进入 SessionState.tools");
    let projected =
        projection::tool_message_fact(&fixture.session, tool, &std::collections::BTreeSet::new());
    let initial_change = projected
        .blocks
        .iter()
        .find_map(|block| match block {
            MessageBlock::FileChange(change) => Some(change),
            _ => None,
        })
        .expect("已应用文件变更应投影 FileChangeFact");
    assert!(initial_change.path.ends_with("rewind.txt"));
    assert!(initial_change.applied);
    assert!(initial_change.rewindable);

    let session_id = fixture.session.session_id().as_str().to_owned();
    // 关闭后恢复会重新取得 Session lease；旧视图必须先释放其共享句柄。
    drop(fixture.session);
    let receipt = crate::native_rewind::apply(
        Arc::clone(fixture.host.runtime()),
        fixture.host.native_paths().as_ref(),
        &session_id,
        "turn-native-rewind",
        "rewind-operation-1",
    )
    .await
    .expect("首次文件撤销应完成");
    assert_eq!(receipt.turn_id, "turn-native-rewind");
    assert_eq!(fs::read(&path).expect("撤销后文件应恢复"), before);

    let reopened = fixture
        .host
        .runtime()
        .runtime_manager()
        .get(session_id.clone())
        .expect("撤销后 Session 应恢复打开");
    let rewound_turns = projection::completed_rewind_turn_ids(&fixture.host, &session_id)
        .expect("完成事务应可投影");
    assert!(rewound_turns.contains("turn-native-rewind"));
    let restored_snapshot = reopened.snapshot().expect("撤销后状态应读取");
    let restored_tool = restored_snapshot
        .state
        .tools
        .values()
        .find(|tool| tool.request.model_tool_call_id == "call-native-rewind-write")
        .expect("撤销后 Write 事实仍应保留");
    let restored_projection =
        projection::tool_message_fact(&reopened, restored_tool, &rewound_turns);
    let restored_change = restored_projection
        .blocks
        .iter()
        .find_map(|block| match block {
            MessageBlock::FileChange(change) => Some(change),
            _ => None,
        })
        .expect("撤销后仍应投影 FileChangeFact");
    assert!(restored_change.applied);
    assert!(!restored_change.rewindable);

    let duplicate = crate::native_rewind::apply(
        Arc::clone(fixture.host.runtime()),
        fixture.host.native_paths().as_ref(),
        &session_id,
        "turn-native-rewind",
        "rewind-operation-2",
    )
    .await
    .expect_err("不同 operationId 不得再次撤销同一 Turn");
    assert_eq!(duplicate.code(), "native-rewind-already-completed");
    assert_eq!(duplicate.to_string(), "目标 Turn 已完成文件撤销");
    assert_eq!(fs::read(&path).expect("重复撤销不得改变文件"), before);
    drop(reopened);
    fixture
        .host
        .runtime()
        .shutdown()
        .await
        .expect("撤销测试 Runtime 应关闭");
}

#[test]
fn tool_and_message_projection_reads_real_artifact_preview() {
    let fixture = fixture();
    let artifact = fixture
        .session
        .put_artifact(b"artifact tool output", Some("text/plain".to_owned()))
        .expect("测试 Artifact 应写入真实 Session 存储");
    let artifact_use = artifact.as_event_use();
    let turn_id = resource::TurnId::new("turn-artifact").expect("Turn ID 应有效");
    let agent_id = resource::AgentId::new(resource::ROOT_AGENT_ID).expect("Agent ID 应有效");
    let request_id = resource::RequestId::new("a".repeat(64)).expect("Request ID 应有效");
    let tool = ToolLifecycle {
        request: ToolRequest {
            request_id: request_id.clone(),
            turn_id: turn_id.clone(),
            agent_id: agent_id.clone(),
            model_round: 0,
            request_index: 0,
            model_tool_call_id: "call-artifact".to_owned(),
            tool_name: "read".to_owned(),
            arguments: serde_json::json!({"path":"facts.txt"}),
            effect: ToolEffect::ReadOnly,
        },
        requested_at_unix_ms: 1,
        execution_started: true,
        execution_started_at_unix_ms: Some(2),
        outcome: Some(ToolOutcome {
            status: ToolCompletionStatus::Succeeded,
            result: PersistedToolResult {
                tool_call_id: "call-artifact".to_owned(),
                content: vec![resource::ToolResultPart::Artifact {
                    artifact: artifact_use.clone(),
                    materialization: ArtifactMaterialization::Utf8Text,
                }],
                is_error: false,
            },
        }),
        completed_at_unix_ms: Some(3),
        file_change: None,
        transcript_segment: None,
    };
    let mut state = fixture
        .session
        .snapshot()
        .expect("Session 快照应读取")
        .state;
    state.tools.insert(request_id, tool.clone());

    let fact = projection::tool_fact(&fixture.session, &tool);
    assert_eq!(
        fact.output_markdown.as_deref(),
        Some("artifact tool output")
    );
    assert_eq!(fact.status, crate::native_ui::model::ToolStatus::Succeeded);

    let message = SessionMessage {
        is_meta: false,
        references: Vec::new(),
        message_id: "assistant-artifact".to_owned(),
        turn_id: Some(turn_id),
        agent_id: Some(agent_id),
        role: ResourceMessageRole::Assistant,
        content: vec![MessagePart::Artifact {
            artifact: artifact_use,
            materialization: ArtifactMaterialization::Utf8Text,
        }],
    };
    let projected = projection::message_fact(
        &fixture.session,
        &state,
        &message,
        &std::collections::BTreeSet::new(),
    )
    .expect("Artifact Assistant 消息应投影");
    assert!(projected.blocks.iter().any(|block| matches!(
        block,
        MessageBlock::Markdown { source, streaming: false, .. } if source == "artifact tool output"
    )));
}

#[test]
fn workspace_page_keeps_project_identity_and_group_membership_across_roots() {
    let fixture = fixture();
    let first_root = tempfile::tempdir().expect("第一个测试项目目录应创建");
    let second_root = tempfile::tempdir().expect("第二个测试项目目录应创建");
    let first_project = crate::workspace::project_create(
        fixture.host.native_paths().as_ref(),
        Some(first_root.path().to_string_lossy().into_owned()),
        "Native identity first project".to_owned(),
        false,
    )
    .expect("第一个测试项目应登记");
    let second_project = crate::workspace::project_create(
        fixture.host.native_paths().as_ref(),
        Some(second_root.path().to_string_lossy().into_owned()),
        "Native identity second project".to_owned(),
        false,
    )
    .expect("第二个测试项目应登记");
    let normalized_first_path = crate::path_utils::path_to_frontend(
        &std::fs::canonicalize(first_root.path()).expect("第一个测试项目路径应规范化"),
    );
    let normalized_second_path = crate::path_utils::path_to_frontend(
        &std::fs::canonicalize(second_root.path()).expect("第二个测试项目路径应规范化"),
    );
    assert_eq!(first_project.path, normalized_first_path);
    assert_eq!(second_project.path, normalized_second_path);

    let first_session = fixture
        .host
        .runtime()
        .open_or_create_session(first_root.path(), None, "native-project-identity-first")
        .expect("第一个项目 Session 应创建");
    let first_session_id = first_session.session_id().as_str().to_owned();
    let second_session = fixture
        .host
        .runtime()
        .open_or_create_session(second_root.path(), None, "native-project-identity-second")
        .expect("第二个项目 Session 应创建");
    let second_session_id = second_session.session_id().as_str().to_owned();
    let page = fixture
        .host
        .workspace_page("", None, false)
        .expect("工作区页应读取");
    let first_project_fact = page
        .projects
        .iter()
        .find(|fact| fact.project_key == normalized_first_path)
        .expect("工作区页应包含第一个登记项目");
    let second_project_fact = page
        .projects
        .iter()
        .find(|fact| fact.project_key == normalized_second_path)
        .expect("工作区页应包含第二个登记项目");
    assert_eq!(first_project_fact.root_path, normalized_first_path);
    assert_eq!(first_project_fact.session_count, 1);
    assert_eq!(second_project_fact.root_path, normalized_second_path);
    assert_eq!(second_project_fact.session_count, 1);

    let first_session_fact = page
        .sessions
        .iter()
        .find(|fact| fact.session_id == first_session_id)
        .expect("工作区页应包含第一个项目 Session");
    let second_session_fact = page
        .sessions
        .iter()
        .find(|fact| fact.session_id == second_session_id)
        .expect("工作区页应包含第二个项目 Session");
    assert_eq!(first_session_fact.project_key, normalized_first_path);
    assert_eq!(
        first_session_fact.project_key,
        first_project_fact.project_key
    );
    assert_eq!(second_session_fact.project_key, normalized_second_path);
    assert_eq!(
        second_session_fact.project_key,
        second_project_fact.project_key
    );

    fixture
        .host
        .group_sessions(
            &first_project.path,
            "native-group",
            std::slice::from_ref(&first_session_id),
            first_project_fact.group_revision,
            "native-project-group",
        )
        .expect("第一个项目的 Session 应允许加入分组");
    let grouped_page = fixture
        .host
        .workspace_page("", None, false)
        .expect("分组后的工作区页应读取");
    let group = grouped_page
        .groups
        .iter()
        .find(|group| group.group_id == "native-group")
        .expect("工作区页应包含持久化分组");
    assert_eq!(group.project_key, normalized_first_path);
    assert_eq!(group.root_path, normalized_first_path);
    assert_eq!(group.session_ids, vec![first_session_id.clone()]);

    assert!(
        fixture
            .host
            .group_sessions(
                &first_project.path,
                "native-group",
                std::slice::from_ref(&second_session_id),
                group.revision,
                "native-cross-project-group",
            )
            .is_err(),
        "分组不得接收其他项目的 Session"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_draft_text_update_preserves_newly_added_attachment() {
    let fixture = fixture();
    let session_id = fixture.session.session_id().as_str().to_owned();
    crate::workspace::project_create(
        fixture.host.native_paths().as_ref(),
        Some(fixture._storage.path().to_string_lossy().into_owned()),
        "Native draft race project".to_owned(),
        false,
    )
    .expect("草稿竞态测试项目应登记");

    let attachment_path = fixture._storage.path().join("draft-race.txt");
    std::fs::write(&attachment_path, "attachment survives stale draft")
        .expect("草稿竞态测试附件应写入");
    let attachment_path =
        std::fs::canonicalize(&attachment_path).expect("草稿竞态测试附件路径应规范化");

    fixture
        .host
        .dispatch(NativeUiAction::AddAttachment {
            session_id: session_id.clone(),
            path: attachment_path.to_string_lossy().into_owned(),
            operation_id: "draft-race-attachment".to_owned(),
        })
        .await
        .expect("真实 AddAttachment 应成功");

    // 模拟附件确认后仍在队列中的旧文本草稿；它不能覆盖较新的附件事实。
    fixture
        .host
        .dispatch(NativeUiAction::SetDraft {
            session_id: session_id.clone(),
            draft: DraftFact {
                text: "latest draft text".to_owned(),
                attachments: Vec::new(),
                mention_query: None,
            },
            edit_generation: 1,
        })
        .await
        .expect("旧草稿文本更新应成功");

    let conversation = fixture
        .host
        .load_conversation(session_id, None)
        .await
        .expect("草稿冷读取应成功");
    assert_eq!(conversation.draft.text, "latest draft text");
    assert_eq!(conversation.draft.attachments.len(), 1);
    assert_eq!(
        conversation.draft.attachments[0].path,
        attachment_path.to_string_lossy()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_draft_after_send_clear_does_not_restore_submitted_text() {
    let fixture = fixture();
    let session_id = fixture.session.session_id().as_str().to_owned();

    fixture
        .host
        .dispatch(NativeUiAction::SetDraft {
            session_id: session_id.clone(),
            draft: DraftFact {
                text: "submitted text".to_owned(),
                ..DraftFact::default()
            },
            edit_generation: 7,
        })
        .await
        .expect("发送前草稿应可保存");

    // 复用 Send 的清理边界，模拟 Runtime 已接受正文后的清理回执。
    fixture
        .host
        .clear_draft_after_send(&session_id, 7)
        .expect("发送成功后的草稿清理应成功");

    fixture
        .host
        .dispatch(NativeUiAction::SetDraft {
            session_id: session_id.clone(),
            draft: DraftFact {
                text: "submitted text".to_owned(),
                ..DraftFact::default()
            },
            edit_generation: 7,
        })
        .await
        .expect("迟到草稿回调应被幂等消费");

    let conversation = fixture
        .host
        .load_conversation(session_id.clone(), None)
        .await
        .expect("草稿冷读取应成功");
    assert!(
        conversation.draft.text.is_empty(),
        "发送清理后的同代迟到 SetDraft 不得恢复已发送正文"
    );

    fixture
        .host
        .dispatch(NativeUiAction::SetDraft {
            session_id: session_id.clone(),
            draft: DraftFact {
                text: "new draft".to_owned(),
                ..DraftFact::default()
            },
            edit_generation: 8,
        })
        .await
        .expect("发送后的新编辑应可保存");
    let conversation = fixture
        .host
        .load_conversation(session_id, None)
        .await
        .expect("新草稿冷读取应成功");
    assert_eq!(conversation.draft.text, "new draft");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_removes_committed_stream_and_cleans_idle_registration() {
    let fixture = fixture();
    let session_id = fixture.session.session_id().as_str().to_owned();
    let batches = Arc::new(Mutex::new(Vec::<NativeEventBatch>::new()));
    let changed = Arc::new(Notify::new());
    let sink_batches = Arc::clone(&batches);
    let sink_changed = Arc::clone(&changed);
    let sink = Arc::new(move |batch: NativeEventBatch| {
        sink_batches.lock().expect("事件集合锁应可用").push(batch);
        sink_changed.notify_waiters();
    });
    let mut subscription = fixture
        .host
        .subscribe_native(session_id.clone(), sink)
        .await
        .expect("原生订阅应建立");
    assert_eq!(
        fixture
            .host
            .inner
            .native_session_subscribers
            .lock()
            .unwrap()
            .get(&session_id),
        Some(&1)
    );

    run_scripted_turn(
        &fixture.session,
        "turn-stream-cleanup",
        "验证 provisional 清理",
        scripted_events("已提交正文"),
    )
    .await;
    // 模型回合从 1 开始，投影身份必须与真实 Runtime transcript 的轮次一致。
    let stream_id = "stream:turn-stream-cleanup:root:1".to_owned();
    wait_for_batch(&batches, &changed, |batches| {
        batches.iter().any(|batch| {
            batch.events.iter().any(|event| {
                matches!(event, NativeUiEvent::RemoveMessage { message_id } if message_id == &stream_id)
            })
        })
    })
    .await;
    assert!(batches.lock().unwrap().iter().any(|batch| {
        batch.events.iter().any(|event| {
            matches!(
                event,
                NativeUiEvent::MessageUpserted(message)
                    if message.role == MessageRole::Assistant
                        && message.blocks.iter().any(|block| matches!(
                            block,
                            MessageBlock::Markdown { source, streaming: false, .. }
                                if source == "已提交正文"
                        ))
            )
        })
    }));

    subscription.cancel();
    drop(subscription);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let no_registration = fixture
                .host
                .inner
                .native_session_subscribers
                .lock()
                .unwrap()
                .get(&session_id)
                .is_none();
            let no_hot = fixture.host.hot_snapshot(&session_id).0 == 0
                && fixture.host.hot_snapshot(&session_id).1.is_empty();
            if no_registration && no_hot {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("空闲订阅清理应在超时前完成");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_projects_provider_failure_and_cancellation_statuses() {
    let failed = fixture();
    let failed_id = failed.session.session_id().as_str().to_owned();
    let failed_batches = Arc::new(Mutex::new(Vec::<NativeEventBatch>::new()));
    let failed_changed = Arc::new(Notify::new());
    let sink_batches = Arc::clone(&failed_batches);
    let sink_changed = Arc::clone(&failed_changed);
    let failed_sink = Arc::new(move |batch: NativeEventBatch| {
        sink_batches.lock().unwrap().push(batch);
        sink_changed.notify_waiters();
    });
    let mut failed_subscription = failed
        .host
        .subscribe_native(failed_id, failed_sink)
        .await
        .expect("失败测试订阅应建立");
    let failed_result = run_failed_turn(&failed.session, "turn-provider-failure", "触发失败").await;
    assert_eq!(
        failed_result.state.terminal_reason(),
        Some(keencode_agent::TerminalReason::Failed)
    );
    wait_for_batch(&failed_batches, &failed_changed, |batches| {
        batches.iter().any(|batch| {
            batch.events.iter().any(|event| {
                matches!(
                    event,
                    NativeUiEvent::TurnChanged {
                        status: SessionStatus::Failed,
                        ..
                    }
                )
            })
        })
    })
    .await;
    assert!(failed_batches.lock().unwrap().iter().any(|batch| {
        batch.events.iter().any(|event| matches!(
            event,
            NativeUiEvent::MessageUpserted(message)
                if message.message_id == "stopped:turn-provider-failure"
                    && message.blocks.iter().any(|block| matches!(block, MessageBlock::Error { .. }))
        ))
    }));
    failed_subscription.cancel();

    let cancelled = fixture();
    let cancelled_id = cancelled.session.session_id().as_str().to_owned();
    let cancelled_batches = Arc::new(Mutex::new(Vec::<NativeEventBatch>::new()));
    let cancelled_changed = Arc::new(Notify::new());
    let sink_batches = Arc::clone(&cancelled_batches);
    let sink_changed = Arc::clone(&cancelled_changed);
    let cancelled_sink = Arc::new(move |batch: NativeEventBatch| {
        sink_batches.lock().unwrap().push(batch);
        sink_changed.notify_waiters();
    });
    let mut cancelled_subscription = cancelled
        .host
        .subscribe_native(cancelled_id, cancelled_sink)
        .await
        .expect("取消测试订阅应建立");
    let cancelled_result =
        run_cancelled_turn(&cancelled.session, "turn-pre-cancelled", "触发取消").await;
    assert_eq!(
        cancelled_result.state.terminal_reason(),
        Some(keencode_agent::TerminalReason::Cancelled)
    );
    wait_for_batch(&cancelled_batches, &cancelled_changed, |batches| {
        batches.iter().any(|batch| {
            batch.events.iter().any(|event| {
                matches!(
                    event,
                    NativeUiEvent::TurnChanged {
                        status: SessionStatus::Interrupted,
                        ..
                    }
                )
            })
        })
    })
    .await;
    cancelled_subscription.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_release_and_reopen_do_not_cancel_active_turn() {
    let fixture = fixture();
    let session_id = fixture.session.session_id().as_str().to_owned();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let input = ModelMessage::text(ModelMessageRole::User, "保持活动并重新打开");
    let request = TurnRequest::new(
        keencode_agent::SessionId::new(fixture.session.session_id().as_str())
            .expect("测试 Session ID 应有效"),
        keencode_agent::TurnId::new("turn-idle-release-reopen").expect("Turn ID 应有效"),
        keencode_agent::AgentId::new(resource::ROOT_AGENT_ID).expect("根 Agent ID 应有效"),
        "blocking-model",
        vec![input.clone()],
        PlanGuard::inactive(),
    );
    let runner = fixture.session.bind_agent_runner(AgentRunner::new(
        Arc::new(BlockingProvider {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
        ToolRegistry::new(),
        RunLimits::default(),
    ));
    let turn = tokio::spawn(async move {
        runner
            .run_turn(RuntimeTurnRequest::root(
                request,
                vec![input],
                "保持活动并重新打开",
            ))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("阻塞 Provider 应已开始");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture
                .session
                .has_active_work()
                .expect("活动 Turn 状态应可读取")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("阻塞 Turn 应在 idle release 前变为活动");

    fixture
        .host
        .schedule_idle_native_release(session_id.clone());
    let reopened_sink = Arc::new(|_batch: NativeEventBatch| {});
    let mut reopened = fixture
        .host
        .subscribe_session(session_id.clone(), reopened_sink)
        .await
        .expect("活动 Turn 期间应允许重新订阅");
    assert!(
        fixture
            .session
            .has_active_work()
            .expect("重新订阅后活动 Turn 状态应可读取")
    );
    assert!(
        fixture
            .host
            .runtime()
            .runtime_manager()
            .get(session_id.clone())
            .is_ok(),
        "idle release 不得关闭被重新打开的活动 Session"
    );

    release.notify_one();
    let result = tokio::time::timeout(Duration::from_secs(5), turn)
        .await
        .expect("阻塞 Turn 应在释放后完成")
        .expect("阻塞 Turn 任务不应 panic")
        .expect("阻塞 Turn 应正常完成");
    assert!(result.is_success());
    reopened.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_dispatch_bypasses_native_operation_gate_for_active_turn() {
    let (base_url, server) = spawn_blocking_responses_server();
    let provider_registry = ProviderRegistry::new();
    let mut provider_config = ProviderConfig::new_unauthenticated(
        "native-stop-test",
        keencode_model::ProviderProtocol::Responses,
        &base_url,
    )
    .expect("Stop 测试 Provider 配置应有效");
    provider_config.response_mode = WireResponseMode::Buffered;
    provider_registry
        .replace_all([ProviderRegistration::new(
            provider_config,
            "Native Stop 测试 Provider",
            "native-stop-test-revision",
            ProviderModelPolicy::Enumerated {
                models: vec!["blocking-model".to_owned()],
            },
        )
        .expect("Stop 测试 Provider 注册项应有效")])
        .expect("Stop 测试 Provider 注册表应替换");
    let fixture = fixture_with_registry(
        provider_registry,
        Some(("native-stop-test".to_owned(), "blocking-model".to_owned())),
    );
    let session_id = fixture.session.session_id().as_str().to_owned();
    fixture
        .host
        .runtime()
        .start_root_turn(
            &session_id,
            "turn-stop-native-gate",
            "持有 Native gate 时停止",
            crate::agent_runtime::RootTurnOptions::default(),
        )
        .await
        .expect("真实 Runtime 根 Turn 应启动");
    tokio::time::timeout(Duration::from_secs(5), server.started.notified())
        .await
        .expect("阻塞 Provider 应已收到真实 Runtime 请求");

    // 模拟设置/扩展等长操作占有 admission；Stop 仍必须直接交给 Runtime。
    let _operation = fixture.host.inner.native_operations.lock().await;
    let receipt = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.host.dispatch(NativeUiAction::Stop {
            session_id: session_id.clone(),
            operation_id: "stop-native-gate".to_owned(),
        }),
    )
    .await
    .expect("Stop 不应等待 native operation gate")
    .expect("Stop 应由 Runtime 接受");
    assert_eq!(receipt.operation_id, "stop-native-gate");
    assert_eq!(receipt.session_id.as_deref(), Some(session_id.as_str()));

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !fixture
                .host
                .runtime()
                .session_has_active_work(&session_id)
                .expect("Stop 后活动状态应可读取")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Stop 后阻塞 Turn 应进入终态");
    server.release();
    let snapshot = fixture
        .host
        .runtime()
        .session_snapshot(&session_id)
        .expect("Stop 后 Session 快照应可读取");
    assert_eq!(
        snapshot
            .state
            .turns
            .values()
            .find(|turn| turn.turn_id.as_str() == "turn-stop-native-gate")
            .map(|turn| &turn.status),
        Some(&TurnStatus::Cancelled)
    );
}
