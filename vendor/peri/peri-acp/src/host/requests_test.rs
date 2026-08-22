use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
};

use crate::provider::{PeriConfig, ProviderConfig, ProviderModels};
use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use async_trait::async_trait;
use peri_acp_types::thread::ThreadMeta;
use peri_agent::thread::FilesystemThreadStore;
use peri_middlewares::hitl::shared_mode::{PermissionMode, SharedPermissionMode};
use serde_json::{json, Value};

use super::*;
use crate::provider::LlmProvider;

// ── Mock AcpTransport ─────────────────────────────────────────────────────────

/// 丢弃所有发送操作的 mock transport
struct MockTransport;

#[async_trait]
impl crate::transport::AcpTransport for MockTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }
    async fn send_notification(&self, _method: &str, _params: Value) -> Result<(), AcpError> {
        Ok(())
    }
    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }
    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

/// 记录 Host 发出的通知，供重放与恢复 wire 契约断言。
struct RecordingTransport {
    /// 按发送顺序保存 `(method, params)`。
    notifications: Arc<StdMutex<Vec<(String, Value)>>>,
}

#[async_trait]
impl crate::transport::AcpTransport for RecordingTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

/// 只提供一个入站请求并记录出站顺序，供 Host 主循环的响应/通知时序测试使用。
struct OrderedTransport {
    incoming: StdMutex<Option<IncomingMessage>>,
    outgoing: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl crate::transport::AcpTransport for OrderedTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }

    async fn send_notification(&self, method: &str, _params: Value) -> Result<(), AcpError> {
        self.outgoing.lock().unwrap().push(method.to_string());
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        self.incoming.lock().unwrap().take()
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        self.outgoing.lock().unwrap().push("response".to_string());
        Ok(())
    }
}

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn make_provider_config(
    id: &str,
    provider_type: &str,
    api_key: &str,
    model: &str,
) -> ProviderConfig {
    ProviderConfig {
        id: id.to_string(),
        provider_type: provider_type.to_string(),
        api_key: api_key.to_string(),
        // 仅保留具体模型元数据；模型选择通过 provider_id::model 完成。
        models: ProviderModels {
            models: [(model.to_string(), Value::Null)].into_iter().collect(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 构造含单个 provider 的 PeriConfig，供 `LlmProvider::from_config` 使用。
fn make_peri_config_with_provider(provider: ProviderConfig) -> PeriConfig {
    let mut peri_config = PeriConfig::default();
    peri_config.config.providers = vec![provider];
    peri_config
}

/// 构造 SessionState 测试夹具使用的固定模型供应商。
fn make_test_provider(model: &str) -> LlmProvider {
    LlmProvider::OpenAi {
        api_key: "test-key".to_string(),
        base_url: "https://models.example/v1".to_string(),
        model: model.to_string(),
        effort: Some("high".to_string()),
        max_tokens: 32_000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    }
}

fn make_server_config(
    peri_config: PeriConfig,
    provider: LlmProvider,
    tmp: &tempfile::TempDir,
) -> AcpServerConfig {
    let thread_store = FilesystemThreadStore::new(tmp.path().join("threads"));
    let arc_thread_store: Arc<dyn peri_acp_types::store::ThreadStore> = Arc::new(thread_store);
    let session_manager = crate::session::SessionManager::new(
        arc_thread_store.clone(),
        provider.clone(),
        Arc::new(peri_config.clone()),
        SharedPermissionMode::new(PermissionMode::Bypass),
        None,
        None,
        // 注入真实 TaskManager 工厂：cancel-bg-task 回归测试依赖 registry 簿记
        Some(Arc::new(|| {
            Arc::new(peri_agent::agent::async_tasks::TaskManager::new())
                as Arc<dyn peri_acp_types::tasks::TaskManager>
        })),
        Arc::new(peri_middlewares::host_ports::SkillsProvider),
    );
    AcpServerConfig {
        provider: Arc::new(parking_lot::RwLock::new(provider)),
        request_observer: None,
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode: SharedPermissionMode::new(PermissionMode::Bypass),
        cron_scheduler: None,
        mcp_pool: None,
        oauth_event_tx: None,
        oauth_event_rx: None,
        channel_state: None,
        plugin_skill_roots: Arc::new(parking_lot::RwLock::new(Vec::new())),
        plugin_agent_dirs: Vec::new(),
        plugin_hooks: Vec::new(),
        plugin_hooks_only: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        plugin_lsp_servers: Vec::new(),
        skills: Arc::new(peri_middlewares::host_ports::SkillsProvider),
        plugin_manager: Arc::new(peri_middlewares::host_ports::PluginManager),
        settings_hooks: Arc::new(peri_middlewares::host_ports::SettingsHooksLoader),
        thread_store: arc_thread_store.clone(),
        controller: Arc::new(peri_controller::Controller::new(arc_thread_store)),
        langfuse_session: None,
        config_path: tmp.path().join("test_config.json"),
        session_manager,
    }
}

/// session/new 必须先写出 response，再发送首个 AvailableCommandsUpdate；否则
/// Mpsc 客户端在尚未建立 sessionId 路由时会先收到无法归属的通知。
#[tokio::test]
async fn test_session_new_response_precedes_available_commands() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "ordering-provider",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&peri_config).expect("测试 provider 应可构造");
    let cfg = make_server_config(peri_config, provider, &tmp);
    let outgoing = Arc::new(StdMutex::new(Vec::new()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(OrderedTransport {
        incoming: StdMutex::new(Some(IncomingMessage::Request {
            id: RequestId::Number(1),
            method: "session/new".to_string(),
            params: json!({ "cwd": tmp.path().to_string_lossy() }),
        })),
        outgoing: Arc::clone(&outgoing),
    });

    crate::host::run_acp_server(transport, cfg).await;

    assert_eq!(
        *outgoing.lock().unwrap(),
        vec!["response".to_string(), "session/update".to_string()]
    );
}

/// 使用真实 SQLite Resources 构造增量重放测试环境。
async fn make_replay_server_config(
    tmp: &tempfile::TempDir,
) -> (
    AcpServerConfig,
    Arc<dyn crate::transport::AcpTransport>,
    Arc<StdMutex<Vec<(String, Value)>>>,
) {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "replay-provider",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&peri_config).expect("测试 provider 应可构造");
    let mut cfg = make_server_config(peri_config, provider, tmp);
    let store: Arc<dyn peri_acp_types::store::ThreadStore> = Arc::new(
        peri_resources::sessions::SqliteThreadStore::new(tmp.path().join("replay.db"))
            .await
            .expect("SQLite 重放存储应可创建"),
    );
    cfg.thread_store = Arc::clone(&store);
    cfg.controller = Arc::new(peri_controller::Controller::new(store));

    let notifications = Arc::new(StdMutex::new(Vec::new()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(RecordingTransport {
        notifications: Arc::clone(&notifications),
    });
    (cfg, transport, notifications)
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

/// session/replay 必须按页发射标准 session/update，并稳定推进同一纪元的游标。
#[tokio::test]
async fn test_session_replay_paginates_with_stable_epoch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport, notifications) = make_replay_server_config(&tmp).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_string_lossy() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 应成功");
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    let store = cfg.controller.sessions();
    for index in 0..3 {
        store
            .append_message(
                &session_id,
                peri_acp_types::messages::BaseMessage::human(format!("消息 {index}")),
            )
            .await
            .unwrap();
    }
    // 排除 session/new 发送的 AvailableCommandsUpdate，只统计重放通知。
    notifications.lock().unwrap().clear();

    let first_page = handle_request(
        "session/replay",
        &json!({ "sessionId": session_id, "limit": 2 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("第一页重放应成功");
    assert_eq!(first_page["status"], "ok");
    assert_eq!(first_page["replayed_events"], 2);
    assert_eq!(first_page["truncated"], true);
    assert_eq!(first_page["next"]["sequence"], 2);
    let epoch = first_page["next"]["epoch"].as_str().unwrap().to_string();

    let first_notifications = notifications.lock().unwrap().clone();
    assert_eq!(
        first_notifications
            .iter()
            .filter(|(method, _)| method == "session/update")
            .count(),
        2
    );
    let recovery = first_notifications
        .iter()
        .find(|(method, _)| method == "session/recovery")
        .expect("每页完成后必须发送 recovery 通知");
    assert_eq!(recovery.1["status"], "not_required");
    assert_eq!(recovery.1["cursor"]["epoch"], epoch);
    assert_eq!(recovery.1["cursor"]["sequence"], 2);
    assert_eq!(recovery.1["pending_tools"], json!([]));

    notifications.lock().unwrap().clear();
    let second_page = handle_request(
        "session/replay",
        &json!({
            "sessionId": session_id,
            "after": { "epoch": epoch, "sequence": 2 },
            "limit": 2,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("第二页重放应成功");
    assert_eq!(second_page["replayed_events"], 1);
    assert_eq!(second_page["truncated"], false);
    assert_eq!(second_page["from"]["sequence"], 2);
    assert_eq!(second_page["next"]["sequence"], 3);
    assert_eq!(second_page["next"]["epoch"], epoch);
}

/// 过期纪元必须回退全量重放，同时在响应中保留旧游标供客户端诊断。
#[tokio::test]
async fn test_session_replay_stale_epoch_falls_back_to_snapshot() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport, _notifications) = make_replay_server_config(&tmp).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_string_lossy() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    cfg.controller
        .sessions()
        .append_message(
            &session_id,
            peri_acp_types::messages::BaseMessage::human("需要全量恢复"),
        )
        .await
        .unwrap();

    let page = handle_request(
        "session/replay",
        &json!({
            "sessionId": session_id,
            "after": { "epoch": "stale-epoch", "sequence": 99 },
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(page["replayed_events"], 1);
    assert_eq!(page["from"]["epoch"], "stale-epoch");
    assert_eq!(page["from"]["sequence"], 99);
    assert_ne!(page["next"]["epoch"], "stale-epoch");
    assert_eq!(page["next"]["sequence"], 1);
}

/// recovery 通知必须使用桌面契约字段，不得把数据库字段名直接透出。
#[tokio::test]
async fn test_session_recovery_pending_tool_wire_shape() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport, notifications) = make_replay_server_config(&tmp).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_string_lossy() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    cfg.controller
        .sessions()
        .record_pending_tool(
            &session_id,
            "call-crashed",
            "Bash",
            Some(r#"{"cmd":"cargo test"}"#.to_string()),
        )
        .await
        .unwrap();

    handle_request(
        "session/replay",
        &json!({ "sessionId": session_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    let recorded = notifications.lock().unwrap();
    let recovery = recorded
        .iter()
        .find(|(method, _)| method == "session/recovery")
        .expect("必须发送 recovery 通知");
    assert_eq!(recovery.1["session_id"], session_id);
    assert_eq!(recovery.1["status"], "restoring");
    let pending = &recovery.1["pending_tools"][0];
    assert_eq!(pending["call_id"], "call-crashed");
    assert_eq!(pending["name"], "Bash");
    assert_eq!(pending["status"], "unknown_outcome");
    assert_eq!(pending["detail"], r#"{"cmd":"cargo test"}"#);
    assert!(pending["started_at_unix_ms"].as_i64().is_some());
    assert!(pending.get("tool_call_id").is_none());
    assert!(pending.get("input_json").is_none());
    assert!(pending.get("started_at").is_none());
}

/// session/replay 缺少必填 Session 标识时必须返回协议参数错误。
#[tokio::test]
async fn test_session_replay_missing_session_id_returns_invalid_params() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport, _notifications) = make_replay_server_config(&tmp).await;
    let mut sessions = HashMap::new();
    let error = handle_request(
        "session/replay",
        &json!({ "limit": 10 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "missing sessionId");
}

/// 验证宿主持有的 Skill 根热更新后，请求侧读取的是同一份共享事实源。
#[test]
fn test_plugin_skill_roots_share_hot_reload_source() {
    let tmp = tempfile::tempdir().expect("创建临时目录");
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "provider",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&peri_config).expect("构造测试供应商");
    let config = make_server_config(peri_config, provider, &tmp);
    let shared_roots = config.plugin_skill_roots.clone();

    shared_roots
        .write()
        .push(peri_acp_types::skills::SkillRoot {
            path: tmp.path().join("plugin-skills"),
            source: peri_acp_types::skills::SkillSource::Plugin,
            plugin_name: Some("test-plugin".to_owned()),
        });

    assert_eq!(config.plugin_skill_roots.read().len(), 1);
    assert_eq!(
        config.plugin_skill_roots.read()[0].plugin_name.as_deref(),
        Some("test-plugin")
    );
}

/// `mcp/list` 必须直接返回注入连接池的当前快照，且查询不触发初始化。
#[tokio::test]
async fn test_mcp_list_returns_pending_pool_snapshot() {
    let tmp = tempfile::tempdir().expect("创建临时目录");
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "provider",
        "openai",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&peri_config).expect("构造测试供应商");
    let mut config = make_server_config(peri_config, provider, &tmp);
    let pool = Arc::new(peri_middlewares::mcp::McpClientPool::new_pending());
    let pool_port: Arc<dyn peri_acp_types::ports::McpPoolPort> = pool;
    config.mcp_pool = Some(pool_port);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

    let result = handle_request("mcp/list", &json!({}), &config, &mut sessions, &transport)
        .await
        .expect("mcp/list 应返回快照");

    assert_eq!(result["initPhase"], "pending");
    assert_eq!(result["servers"], json!([]));
}

/// 验证 session/update_config 使用新配置中的第一个显式 provider/model 更新默认 Provider。
#[tokio::test]
async fn test_update_config_切换provider后cfg_provider更新() {
    // Arrange: 构造两个 provider（a=openai, b=anthropic）。
    let tmp = tempfile::TempDir::new().unwrap();
    let provider_a = make_provider_config("a", "openai", "sk-openai-test", "gpt-4o");
    let provider_b = make_provider_config("b", "anthropic", "sk-ant-test", "model-a");

    let peri_config = make_peri_config_with_provider(provider_a.clone());

    let initial_provider = LlmProvider::from_config(&peri_config).unwrap();
    assert!(
        matches!(initial_provider, LlmProvider::OpenAi { .. }),
        "初始 provider 应为 OpenAI"
    );

    let cfg = make_server_config(peri_config.clone(), initial_provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

    // 构造 update_config 参数：将 b 放在新配置的首位，显式模型元数据随 provider 提供。
    let mut updated_config = peri_config.clone();
    updated_config.config.providers = vec![provider_b.clone(), provider_a.clone()];

    let params = json!({
        "sessionId": "test-session",
        "config": updated_config,
    });

    // Act: 调用 handle_request
    let result = handle_request(
        "session/update_config",
        &params,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();

    // Assert: cfg.provider 应切换到 anthropic
    let provider = cfg.provider.read();
    assert!(
        matches!(&*provider, LlmProvider::Anthropic { model, .. } if model == "model-a"),
        "切换后 provider 应为 Anthropic model-a，实际: display={} model={}",
        provider.display_name(),
        provider.model_name(),
    );
    assert_eq!(
        provider.display_name(),
        "Anthropic",
        "display_name 应为 Anthropic"
    );

    // 验证返回值包含 configOptions
    assert!(
        result.get("configOptions").is_some(),
        "响应应包含 configOptions"
    );
}

/// 验证 session/update_config 空 providers 时返回错误
#[tokio::test]
async fn test_update_config_空providers返回错误() {
    let tmp = tempfile::TempDir::new().unwrap();
    let provider_a = make_provider_config("a", "openai", "sk-openai-test", "gpt-4o");

    let peri_config = make_peri_config_with_provider(provider_a);

    let initial_provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config.clone(), initial_provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

    // 空 providers
    let mut bad_config = PeriConfig::default();
    bad_config.config.providers = vec![];

    let params = json!({
        "sessionId": "test-session",
        "config": bad_config,
    });

    let result = handle_request(
        "session/update_config",
        &params,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "空 providers 应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("providers cannot be empty"),
        "错误消息应提及 providers 为空，实际: {}",
        err.message,
    );
}

/// 验证没有显式模型元数据时，session/update_config 不会伪造默认模型。
#[tokio::test]
async fn test_update_config_缺少模型元数据不替换provider() {
    let tmp = tempfile::TempDir::new().unwrap();
    let provider_a = make_provider_config("a", "openai", "sk-openai-test", "gpt-4o");

    let peri_config = make_peri_config_with_provider(provider_a);

    let initial_provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config.clone(), initial_provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

    // 新配置只有 provider 元数据，没有模型选择；运行时应保留原 provider。
    let mut bad_config = peri_config.clone();
    bad_config.config.providers[0].models = ProviderModels::default();

    let params = json!({
        "sessionId": "test-session",
        "config": bad_config,
    });

    let result = handle_request(
        "session/update_config",
        &params,
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_ok(), "缺少模型元数据不应让配置请求失败");
    assert_eq!(cfg.provider.read().model_name(), "gpt-4o");
}

// ── 会话级 Provider 隔离 ────────────────────────────────────────────────────

/// 构造两个供应商：p1 是新会话默认值，p2 用于单会话模型切换。
fn make_peri_config_two_providers() -> PeriConfig {
    let p1 = make_provider_config("p1", "anthropic", "key-1", "m1-default");
    let p2 = make_provider_config("p2", "openai", "key-2", "m2");
    PeriConfig {
        config: crate::provider::AppConfig {
            providers: vec![p1, p2],
            ..Default::default()
        },
        ..Default::default()
    }
}

/// 构造启用双供应商配置的 ACP Server 测试夹具。
fn make_two_provider_server_config(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let peri_config = make_peri_config_two_providers();
    let provider = LlmProvider::from_config(&peri_config).expect("默认供应商应可构造");
    make_server_config(peri_config, provider, tmp)
}

/// 从 ConfigOptionUpdate 序列化值中读取指定选项的 currentValue。
fn config_option_current_value(update: &Value, config_id: &str) -> String {
    update["configOptions"]
        .as_array()
        .and_then(|options| {
            options
                .iter()
                .find(|option| option["id"].as_str() == Some(config_id))
        })
        .and_then(|option| option["currentValue"].as_str())
        .unwrap_or("")
        .to_string()
}

/// 会话级模型切换只更新目标 Session，其他 Session 与全局默认值保持不变。
#[tokio::test]
async fn test_set_config_option_model_会话隔离() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_two_provider_server_config(&tmp);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let mut sessions = HashMap::new();
    let created_a = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new a 应成功");
    let session_a = created_a["sessionId"].as_str().unwrap().to_string();
    let created_b = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new b 应成功");
    let session_b = created_b["sessionId"].as_str().unwrap().to_string();
    let response = handle_request(
        "session/set_config_option",
        &json!({
            "sessionId": session_a,
            "configId": "model",
            "value": "p2::m2",
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("set_config_option 应成功");
    assert_eq!(sessions[&session_a].provider.read().model_name(), "m2");
    assert_eq!(
        sessions[&session_b].provider.read().model_name(),
        "m1-default",
        "其他会话的模型不应改变"
    );
    assert_eq!(
        cfg.provider.read().model_name(),
        "m1-default",
        "全局默认模型不应改变"
    );
    assert_eq!(
        cfg.peri_config.read().config.providers.len(),
        2,
        "会话模型切换不应改写全局 provider 配置"
    );
    assert_eq!(
        config_option_current_value(&response, "model"),
        "m2",
        "响应应反映目标会话模型"
    );
}

/// 会话级推理强度切换保留当前供应商和模型，且不影响其他 Session。
#[tokio::test]
async fn test_set_config_option_thinking_effort_会话隔离() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_two_provider_server_config(&tmp);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let mut sessions = HashMap::new();
    let created_a = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new a 应成功");
    let session_a = created_a["sessionId"].as_str().unwrap().to_string();
    let created_b = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new b 应成功");
    let session_b = created_b["sessionId"].as_str().unwrap().to_string();
    handle_request(
        "session/set_config_option",
        &json!({ "sessionId": session_a, "configId": "model", "value": "p2::m2" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("目标会话模型切换应成功");
    let response = handle_request(
        "session/set_config_option",
        &json!({
            "sessionId": session_a,
            "configId": "thinking_effort",
            "value": "max",
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("推理强度切换应成功");
    let provider_a = sessions[&session_a].provider.read();
    assert_eq!(provider_a.model_name(), "m2", "推理强度切换应保留当前模型");
    assert_eq!(provider_a.effort(), Some("max"));
    drop(provider_a);
    assert_eq!(sessions[&session_b].provider.read().effort(), Some("high"));
    assert_eq!(cfg.provider.read().effort(), Some("high"));
    assert_eq!(
        config_option_current_value(&response, "thinking_effort"),
        "max",
        "响应应反映目标会话推理强度"
    );
}

/// 标题 Provider 解析必须按目标 Session 选择，不能回退全局默认值或串用其他会话。
#[tokio::test]
async fn test_session_title_provider_选择目标会话冻结配置() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_two_provider_server_config(&tmp);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let mut sessions = HashMap::new();
    let created_a = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new a 应成功");
    let session_a = created_a["sessionId"].as_str().unwrap().to_string();
    let created_b = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new b 应成功");
    let session_b = created_b["sessionId"].as_str().unwrap().to_string();

    handle_request(
        "session/set_config_option",
        &json!({ "sessionId": session_a, "configId": "model", "value": "p2::m2" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("目标会话模型切换应成功");
    handle_request(
        "session/set_config_option",
        &json!({ "sessionId": session_a, "configId": "thinking_effort", "value": "max" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("目标会话推理强度切换应成功");

    let selected_a = session_title_provider(&sessions, &session_a).unwrap();
    assert_eq!(selected_a.display_name(), "OpenAI");
    assert_eq!(selected_a.model_name(), "m2");
    assert_eq!(selected_a.effort(), Some("max"));

    let selected_b = session_title_provider(&sessions, &session_b).unwrap();
    assert_eq!(selected_b.display_name(), "Anthropic");
    assert_eq!(selected_b.model_name(), "m1-default");
    assert_eq!(selected_b.effort(), Some("high"));
    assert_eq!(cfg.provider.read().display_name(), "Anthropic");
}

/// new/load/resume 冻结当时默认 Provider，fork 则继承源 Session 的完整模型配置。
#[tokio::test]
async fn test_session_title_provider_覆盖全部会话入口() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_two_provider_server_config(&tmp);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let mut sessions = HashMap::new();
    let cwd = tmp.path().to_str().unwrap();

    let created_default = handle_request(
        "session/new",
        &json!({ "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 应成功");
    let new_session_id = created_default["sessionId"].as_str().unwrap().to_string();

    let load_session_id = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd))
        .await
        .expect("应创建 load 测试线程");
    handle_request(
        "session/load",
        &json!({ "sessionId": load_session_id, "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/load 应成功");

    let resume_session_id = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd))
        .await
        .expect("应创建 resume 测试线程");
    handle_request(
        "session/resume",
        &json!({ "sessionId": resume_session_id, "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/resume 应成功");

    let created_source = handle_request(
        "session/new",
        &json!({ "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("fork 源 session/new 应成功");
    let source_session_id = created_source["sessionId"].as_str().unwrap().to_string();
    handle_request(
        "session/set_config_option",
        &json!({ "sessionId": source_session_id, "configId": "model", "value": "p2::m2" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("fork 源模型切换应成功");
    handle_request(
        "session/set_config_option",
        &json!({ "sessionId": source_session_id, "configId": "thinking_effort", "value": "max" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("fork 源推理强度切换应成功");
    let forked = handle_request(
        "session/fork",
        &json!({ "sessionId": source_session_id, "cwd": cwd }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/fork 应成功");
    let fork_session_id = forked["sessionId"].as_str().unwrap().to_string();

    handle_request(
        "session/set_config_option",
        &json!({ "sessionId": source_session_id, "configId": "thinking_effort", "value": "low" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("fork 后源会话应可独立修改");
    let replacement_default = {
        let peri_config = cfg.peri_config.read();
        LlmProvider::from_provider_config(
            &peri_config,
            "p2",
            "m2",
            Some("low".to_string()),
            32_000,
            false,
            None,
        )
        .expect("替换默认 Provider 应可构造")
    };
    *cfg.provider.write() = replacement_default;

    for session_id in [&new_session_id, &load_session_id, &resume_session_id] {
        let provider = session_title_provider(&sessions, session_id).unwrap();
        assert_eq!(provider.display_name(), "Anthropic");
        assert_eq!(provider.model_name(), "m1-default");
        assert_eq!(provider.effort(), Some("high"));
    }
    let fork_provider = session_title_provider(&sessions, &fork_session_id).unwrap();
    assert_eq!(fork_provider.display_name(), "OpenAI");
    assert_eq!(fork_provider.model_name(), "m2");
    assert_eq!(fork_provider.effort(), Some("max"));
}

/// 未知 Session 的标题请求必须在模型调用前返回显式参数错误。
#[tokio::test]
async fn test_session_title_未知会话显式报错() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_two_provider_server_config(&tmp);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let mut sessions = HashMap::new();
    let error = handle_request(
        "peri/session-title",
        &json!({
            "sessionId": "missing-session",
            "userMessage": "请生成标题",
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect_err("未知 Session 必须被拒绝");
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "unknown sessionId");
}

/// 无效的 provider/model 编码被忽略，目标会话保持原模型。
#[tokio::test]
async fn test_set_config_option_model_无效值被忽略() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_two_provider_server_config(&tmp);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 应成功");
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    for bad_value in ["ghost::m", "p1::", "缺少分隔符"] {
        handle_request(
            "session/set_config_option",
            &json!({
                "sessionId": session_id,
                "configId": "model",
                "value": bad_value,
            }),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .expect("无效值应被安全忽略");
        assert_eq!(
            sessions[&session_id].provider.read().model_name(),
            "m1-default",
            "无效模型编码 {bad_value} 不应改变会话"
        );
    }
}

/// 捕获通知内容，用于验证 ConfigOptionUpdate 的会话作用域。
struct CaptureTransport {
    notifications: std::sync::Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl crate::transport::AcpTransport for CaptureTransport {
    async fn send_request(&self, _method: &str, _params: Value) -> Result<Value, AcpError> {
        Ok(json!({}))
    }

    async fn send_notification(&self, method: &str, params: Value) -> Result<(), AcpError> {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_string(), params));
        Ok(())
    }

    async fn recv(&self) -> Option<IncomingMessage> {
        None
    }

    async fn send_response(
        &self,
        _id: RequestId,
        _result: Result<Value, AcpError>,
    ) -> Result<(), AcpError> {
        Ok(())
    }
}

/// ConfigOptionUpdate 通知使用目标 Session 的模型，而不是全局默认模型。
#[tokio::test]
async fn test_config_option_update_按会话provider广播() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_two_provider_server_config(&tmp);
    let capture = Arc::new(CaptureTransport {
        notifications: std::sync::Mutex::new(Vec::new()),
    });
    let transport: Arc<dyn crate::transport::AcpTransport> = capture.clone();
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 应成功");
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    handle_request(
        "session/set_config_option",
        &json!({
            "sessionId": session_id,
            "configId": "model",
            "value": "p2::m2",
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("set_config_option 应成功");
    let (_method, params) = capture
        .notifications
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(method, _)| method == "session/update")
        .cloned()
        .expect("应发送 session/update 通知");
    assert_eq!(params["sessionId"].as_str(), Some(session_id.as_str()));
    assert_eq!(
        config_option_current_value(&params["update"], "model"),
        "m2",
        "通知应反映目标会话模型"
    );
}

// ── Rewind RPC 路由测试 ─────────────────────────────────────────────────────

/// 注册一个含 user/ai 消息的 SessionState（字段以 mod.rs 定义为准）。
fn register_session_with_history(
    sessions: &mut HashMap<String, SessionState>,
    cwd: &str,
) -> String {
    let history = vec![
        peri_acp_types::messages::BaseMessage::human("第一轮用户问题"),
        peri_acp_types::messages::BaseMessage::ai("第一轮回答"),
        peri_acp_types::messages::BaseMessage::human("第二轮用户问题"),
    ];
    let sid = "rewind-test-session".to_string();
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: "thread-1".to_string(),
            cwd: cwd.to_string(),
            history,
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            provider: Arc::new(parking_lot::RwLock::new(make_test_provider("gpt-4o"))),
            tool_registry: crate::host::SessionToolRegistry::new(),
            lsp_pool: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );
    sid
}

/// 构造没有历史的活跃 SessionState，用于删除等路由测试。
fn register_session(sessions: &mut HashMap<String, SessionState>, sid: &str, cwd: &str) {
    sessions.insert(
        sid.to_string(),
        SessionState {
            session_id: sid.to_string(),
            thread_id: sid.to_string(),
            cwd: cwd.to_string(),
            history: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            provider: Arc::new(parking_lot::RwLock::new(make_test_provider("gpt-4o"))),
            tool_registry: crate::host::SessionToolRegistry::new(),
            lsp_pool: None,
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );
}

/// session/rewind-candidates 路由到 dispatch：返回 user-only 候选。
#[tokio::test]
async fn test_rewind_candidates_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());

    let result = handle_request(
        "session/rewind-candidates",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    let value = result.unwrap();
    let messages = value["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2, "只返回 user 消息");
}

/// session/rewind-preview 路由到 dispatch：返回 file_changes 数组（无工具调用 → 空）。
/// 目标取 history[2]（Human 消息）——与生产口径一致：rewind-candidates 只返回
/// user 消息，AI 消息永远不可能成为回滚目标。
#[tokio::test]
async fn test_rewind_preview_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    let target_id = sessions.get(&sid).unwrap().history[2]
        .id()
        .as_uuid()
        .to_string();

    let result = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    let value = result.unwrap();
    let changes = value["file_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 0, "历史无工具调用 → 空预算");
}

/// session/rewind-preview：目标消息不存在时返回 not found 错误（生产 rewind_preview
/// 按 id 定位，history 之外的 id 一律拒绝）。「仅 AI 消息」场景由候选层保证不可达
/// （rewind-candidates 只返回 user 消息），UI 不可能选中 AI 消息作为目标。
#[tokio::test]
async fn test_rewind_preview_missing_target_returns_not_found() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());

    let result = handle_request(
        "session/rewind-preview",
        &json!({ "sessionId": sid, "target_message_id": "00000000-0000-0000-0000-000000000000" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "目标不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("未找到目标消息"),
        "错误消息应提及未找到目标，实际: {}",
        err.message,
    );
}

/// session/rewind 路由到 dispatch：执行回退（无 Write/Edit 时仅截断）。
#[tokio::test]
async fn test_rewind_routes_to_dispatch() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = register_session_with_history(&mut sessions, tmp.path().to_str().unwrap());
    let target_id = sessions.get(&sid).unwrap().history[0]
        .id()
        .as_uuid()
        .to_string();

    let result = handle_request(
        "session/rewind",
        &json!({ "sessionId": sid, "target_message_id": target_id }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert_eq!(result.unwrap()["status"], "executed");

    // P1：rewind 后 SessionState.history 必须截断——它是后续候选/预算查询的
    // 数据源，不写回会导致第二次回退 not found。
    let s = sessions.get(&sid).unwrap();
    assert_eq!(s.history.len(), 0, "回退到第一条后 history 应为空");
}

// ── session/cancel-bg-task 路由测试（issue 2026-08-05）───────────────────

/// [回归测试] cancel-bg-task 会话不存在时必须如实报错（issue 2026-08-05）。
/// 历史 bug：静默返回 success，掩盖"取消未生效"。
#[tokio::test]
async fn test_cancel_bg_task_session_not_found_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": "no-such-session", "taskId": "wf-run-1" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "会话不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("session not found"),
        "错误消息应提及 session not found，实际: {}",
        err.message
    );
}

/// [回归测试] cancel-bg-task 任务不存在时必须如实报错（issue 2026-08-05）。
/// 与 session_not_found 区分（错误消息不同），客户端可据此判断重试策略。
#[tokio::test]
async fn test_cancel_bg_task_task_not_found_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let sid = "cancel-bg-session".to_string();
    cfg.session_manager
        .new_session_with_id(&sid, tmp.path().to_str().unwrap())
        .await
        .unwrap();

    let result = handle_request(
        "session/cancel-bg-task",
        &json!({ "sessionId": sid, "taskId": "no-such-task" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await;

    assert!(result.is_err(), "任务不存在应返回错误");
    let err = result.unwrap_err();
    assert!(
        err.message.contains("not found"),
        "错误消息应提及 not found，实际: {}",
        err.message
    );
}

// ── session/delete（标准 ACP，agentclientprotocol.com/protocol/v1/session-delete）──

/// 删除后：响应为空对象、线程从 store 移除（load_meta 报错）、活跃会话从
/// sessions 表清理。
#[tokio::test]
async fn test_delete_removes_thread_and_active_session() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let cwd = tmp.path().to_str().unwrap();

    // 真实创建线程（id 即 session id）
    let thread_id = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd))
        .await
        .unwrap();
    let sid = thread_id.clone();

    // 活跃会话登记（与 session/new 后的内存态一致）
    register_session(&mut sessions, &sid, cwd);

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/delete 应成功");

    // 标准响应为空对象
    assert_eq!(
        resp,
        serde_json::json!({}),
        "标准 session/delete 响应为 {{}}"
    );

    // 活跃会话已清理
    assert!(
        !sessions.contains_key(&sid),
        "删除后活跃会话应从 sessions 表移除"
    );

    // 线程已从 store 持久化删除（元数据不存在 + 列表不再包含）
    assert!(
        cfg.thread_store.load_meta(&sid).await.is_err(),
        "删除后线程元数据不应存在"
    );
    let remaining = cfg.thread_store.list_threads().await.unwrap();
    assert!(
        !remaining.iter().any(|m| m.id == sid),
        "删除后 session/list 不应再包含该线程"
    );
}

/// 删除不存在的线程：幂等成功（存储层不报错，历史不存在视为已删除）。
#[tokio::test]
async fn test_delete_unknown_session_is_idempotent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": "never-existed" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("删除不存在的会话应幂等成功");
    assert_eq!(resp, serde_json::json!({}));
}

/// 缺失 sessionId → -32602 Invalid params。
#[tokio::test]
async fn test_delete_missing_session_id_returns_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);

    let err = handle_request(
        "session/delete",
        &json!({}),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert!(
        err.message.contains("missing sessionId"),
        "缺失 sessionId 应报 -32602，实际: {}",
        err.message
    );
}

// ── M2 回归：进程内 session/delete 必须 shutdown LSP pool ────────────────────

/// 记录 shutdown 调用的 mock LSP pool。
struct MockLspPool {
    shutdown_calls: Arc<std::sync::atomic::AtomicU32>,
}

#[async_trait::async_trait]
impl peri_acp_types::ports::LspPoolPort for MockLspPool {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    async fn shutdown(&self) {
        self.shutdown_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 删除活跃会话（带 lsp_pool）时必须在锁外 shutdown pool，与 stdio 路径一致，
/// 避免 LSP 服务器子进程/read task 残留（M2；此前进程内路径直接丢弃 pool）。
#[tokio::test]
async fn test_delete_active_session_shuts_down_lsp_pool() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "a",
        "openai",
        "sk-openai-test",
        "gpt-4o",
    ));
    let provider = LlmProvider::from_config(&peri_config).unwrap();
    let cfg = make_server_config(peri_config, provider, &tmp);
    let mut sessions = HashMap::new();
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let cwd = tmp.path().to_str().unwrap();

    // 真实创建线程（id 即 session id），与 delete 分支的 thread_store 删除对应
    let sid = cfg
        .thread_store
        .create_thread(ThreadMeta::new(cwd))
        .await
        .unwrap();

    let shutdown_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let pool: Arc<dyn peri_acp_types::ports::LspPoolPort> = Arc::new(MockLspPool {
        shutdown_calls: Arc::clone(&shutdown_calls),
    });

    // 构造带 lsp_pool 的活跃会话。
    sessions.insert(
        sid.clone(),
        SessionState {
            session_id: sid.clone(),
            thread_id: sid.clone(),
            cwd: cwd.to_string(),
            history: Vec::new(),
            cancel_token: None,
            frozen: None,
            recall_items: Vec::new(),
            agent_pool: crate::session::agent_pool::AgentPool::new(),
            provider: Arc::new(parking_lot::RwLock::new(make_test_provider("gpt-4o"))),
            tool_registry: crate::host::SessionToolRegistry::new(),
            lsp_pool: Some(pool),
            title: None,
            tags: Vec::new(),
            continuation_armed: false,
            continuation_epoch: 0,
            continuation_in_flight: false,
            lease: crate::host::lease::WriterLease::acquired("default"),
        },
    );

    let resp = handle_request(
        "session/delete",
        &json!({ "sessionId": sid }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/delete 应成功");
    assert_eq!(resp, serde_json::json!({}));
    assert!(
        !sessions.contains_key(&sid),
        "删除后活跃会话应从 sessions 表移除"
    );
    assert_eq!(
        shutdown_calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "删除活跃会话必须 shutdown LSP pool（M2）"
    );
}

// ── KeenCode 运行中引导 ACP 方法 wire 测试 ──────────────────────────────────

/// 运行中引导只允许写入活跃回合，并保留 UserSteering 来源。
#[tokio::test]
async fn test_session_steer_仅在运行中注入用户prompt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "steer-provider",
        "anthropic",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&peri_config).expect("测试配置应可构造");
    let cfg = make_server_config(peri_config, provider, &tmp);
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport);
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/new",
        &json!({ "cwd": tmp.path().to_str().unwrap() }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 应成功");
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    let idle_error = handle_request(
        "session/steer",
        &json!({ "sessionId": session_id, "text": "只改前端" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect_err("空闲会话不得接受引导");
    assert_eq!(idle_error.code, -32000);
    sessions.get_mut(&session_id).unwrap().cancel_token =
        Some(tokio_util::sync::CancellationToken::new());
    let accepted = handle_request(
        "session/steer",
        &json!({ "sessionId": session_id, "text": "只改前端" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("运行中会话应接受引导");
    assert_eq!(accepted["accepted"], true);
    let queue = cfg.session_manager.v2_queue_for(&session_id).unwrap();
    let drained = queue.drain_all();
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].kind,
        peri_acp_types::session::MessageKind::Prompt
    );
    assert_eq!(
        drained[0].source,
        peri_acp_types::session::MessageSource::UserSteering
    );
    assert_eq!(drained[0].message.content().to_string(), "只改前端");
}

// ── KeenCode Goal ACP 方法 wire 测试 ────────────────────────────────────────

/// 创建带 Goal 支持的会话，返回 Host 配置与丢弃通知的 transport。
async fn make_goal_session(
    tmp: &tempfile::TempDir,
) -> (
    AcpServerConfig,
    std::sync::Arc<dyn crate::transport::AcpTransport>,
) {
    let peri_config = make_peri_config_with_provider(make_provider_config(
        "goal-provider",
        "anthropic",
        "test-key",
        "test-model",
    ));
    let provider = LlmProvider::from_config(&peri_config).expect("测试配置应可构造");
    let cfg = make_server_config(peri_config, provider, tmp);
    cfg.session_manager
        .ensure_session("goal-sess", tmp.path().to_str().unwrap());
    let transport: std::sync::Arc<dyn crate::transport::AcpTransport> =
        std::sync::Arc::new(MockTransport);
    (cfg, transport)
}

/// 空会话查询应返回空 Goal 列表。
#[tokio::test]
async fn test_goal_get_空session返回空列表() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport) = make_goal_session(&tmp).await;
    let mut sessions = HashMap::new();
    let result = handle_request(
        "session/goal-get",
        &json!({ "sessionId": "goal-sess" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("goal-get 应成功");
    assert_eq!(result["revision"], 0);
    assert_eq!(result["goals"].as_array().map(|items| items.len()), Some(0));
    assert!(result["activeGoalId"].is_null());
}

/// 创建 Goal 应返回桌面契约需要的 revision 与 deduplicated 字段。
#[tokio::test]
async fn test_goal_upsert_创建返回revision与deduplicated标志() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport) = make_goal_session(&tmp).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/goal-upsert",
        &json!({
            "sessionId": "goal-sess",
            "goal": { "id": "goal-1", "title": "Ship v2" },
            "requestNonce": "n-1",
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("goal-upsert 创建应成功");
    assert_eq!(created["revision"], 0);
    assert!(created["goal"]["id"]
        .as_str()
        .is_some_and(|id| !id.is_empty()));
    assert_eq!(created["goal"]["status"], "active");
    assert_eq!(created["deduplicated"], false);
}

/// 更新现有 Goal 应沿用新版无 revision 冲突语义。
#[tokio::test]
async fn test_goal_upsert_更新无需expected_revision() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport) = make_goal_session(&tmp).await;
    let mut sessions = HashMap::new();
    handle_request(
        "session/goal-upsert",
        &json!({ "sessionId": "goal-sess", "goal": { "title": "初始" } }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let updated = handle_request(
        "session/goal-upsert",
        &json!({ "sessionId": "goal-sess", "goal": { "title": "更新" } }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("更新应成功（新版 GoalState 无 revision 冲突）");
    assert_eq!(updated["revision"], 0);
    assert_eq!(updated["goal"]["title"], "更新");
}

/// 状态迁移应拒绝非法值并映射 completed 到内部 Complete 状态。
#[tokio::test]
async fn test_goal_transition_completed与非法值() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport) = make_goal_session(&tmp).await;
    let mut sessions = HashMap::new();
    let created = handle_request(
        "session/goal-upsert",
        &json!({ "sessionId": "goal-sess", "goal": { "title": "目标" } }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let goal_id = created["goal"]["id"].as_str().unwrap().to_string();
    let error = handle_request(
        "session/goal-transition",
        &json!({
            "sessionId": "goal-sess",
            "goalId": goal_id,
            "status": "archived",
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, -32602);
    let completed = handle_request(
        "session/goal-transition",
        &json!({
            "sessionId": "goal-sess",
            "goalId": goal_id,
            "status": "completed",
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(completed["goal"]["status"], "completed");
    assert_eq!(completed["revision"], 0);
}

/// 清除 Goal 后再次查询应恢复为空列表。
#[tokio::test]
async fn test_goal_clear_清除后查询为空() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport) = make_goal_session(&tmp).await;
    let mut sessions = HashMap::new();
    handle_request(
        "session/goal-upsert",
        &json!({ "sessionId": "goal-sess", "goal": { "title": "临时目标" } }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let cleared = handle_request(
        "session/goal-clear",
        &json!({ "sessionId": "goal-sess" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("goal-clear 应成功");
    assert_eq!(cleared["cleared"], true);
    let result = handle_request(
        "session/goal-get",
        &json!({ "sessionId": "goal-sess" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(result["goals"].as_array().map(|items| items.len()), Some(0));
}
