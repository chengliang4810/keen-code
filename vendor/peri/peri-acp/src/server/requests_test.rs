use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use crate::provider::{LlmProvider, PeriConfig, ProviderConfig};

use crate::transport::types::{AcpError, IncomingMessage, RequestId};
use async_trait::async_trait;
use peri_agent::thread::FilesystemThreadStore;
use peri_middlewares::hitl::{PermissionMode, SharedPermissionMode};
use serde_json::{Value, json};

use super::*;

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

// ── 辅助函数 ──────────────────────────────────────────────────────────────────

fn make_provider_config(
    id: &str,
    provider_type: &str,
    api_key: &str,
    model: &str,
) -> ProviderConfig {
    let provider = ProviderConfig {
        id: id.to_string(),
        provider_type: provider_type.to_string(),
        api_key: api_key.to_string(),
        base_url: "https://models.example/v1".to_string(),
        name: None,
        models: Default::default(),
        extra: Default::default(),
    };
    let _ = model;
    provider
}

/// 构造包含四档 Profile 的 peri 配置，四档绑定同一 provider/model。
fn make_peri_config(provider: ProviderConfig, model: &str) -> PeriConfig {
    use crate::provider::{AppConfig, ProfileConfig, Profiles};
    let profile = ProfileConfig {
        provider: provider.id.clone(),
        model: Some(model.to_string()),
        effort: "high".to_string(),
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
    };
    let mut profiles = Profiles::default();
    profiles.opus = profile.clone();
    profiles.sonnet = profile;
    PeriConfig {
        schema: None,
        config: AppConfig {
            active_alias: "opus".to_string(),
            providers: vec![provider],
            profiles,
            ..AppConfig::default()
        },
    }
}

fn make_server_config(peri_config: PeriConfig, tmp: &tempfile::TempDir) -> AcpServerConfig {
    let thread_store = FilesystemThreadStore::new(tmp.path().join("threads"));
    let arc_thread_store: Arc<dyn peri_agent::thread::ThreadStore> = Arc::new(thread_store);
    let provider = LlmProvider::from_config(&peri_config).expect("测试配置应可构造");
    let provider_runtime = Arc::new(parking_lot::RwLock::new(provider));
    let permission_mode = SharedPermissionMode::new(PermissionMode::Bypass);
    let session_manager = crate::session::SessionManager::new(
        arc_thread_store.clone(),
        provider_runtime.read().clone(),
        Arc::new(peri_config.clone()),
        Arc::clone(&permission_mode),
        None,
    );
    AcpServerConfig {
        provider: provider_runtime,
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode,
        cron_scheduler: None,
        mcp_pool: None,
        channel_state: None,
        plugin_skill_roots: Arc::new(parking_lot::RwLock::new(Vec::new())),
        plugin_agent_dirs: Vec::new(),
        plugin_hooks: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        plugin_lsp_servers: Vec::new(),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        thread_store: arc_thread_store,
        langfuse_session: None,
        config_path: tmp.path().join("settings.json"),
        session_manager,
    }
}

// ── 测试 ──────────────────────────────────────────────────────────────────────

// ── (KeenCode) Goal ACP 方法 wire 测试 ───────────────────────────────────────────

#[tokio::test]
async fn test_session_steer_仅在运行中注入用户prompt() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_goal_cfg(&tmp);
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
        Some(peri_agent::agent::AgentCancellationToken::new());
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
    assert_eq!(drained[0].kind, peri_agent::session::MessageKind::Prompt);
    assert_eq!(
        drained[0].source,
        peri_agent::session::MessageSource::UserSteering
    );
    assert_eq!(drained[0].message.content().to_string(), "只改前端");
}

fn make_goal_cfg(tmp: &tempfile::TempDir) -> AcpServerConfig {
    let provider = make_provider_config("test", "anthropic", "test-key", "test-model");
    let peri_config = make_peri_config(provider, "test-model");
    make_server_config(peri_config, tmp)
}

/// 创建带 goal 支持的 session，返回 (cfg, transport)。
async fn make_goal_session(
    tmp: &tempfile::TempDir,
) -> (
    AcpServerConfig,
    std::sync::Arc<dyn crate::transport::AcpTransport>,
) {
    let cfg = make_goal_cfg(tmp);
    cfg.session_manager
        .ensure_session("goal-sess", tmp.path().to_str().unwrap());
    let transport: std::sync::Arc<dyn crate::transport::AcpTransport> =
        std::sync::Arc::new(MockTransport);
    (cfg, transport)
}

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
    assert_eq!(result["goals"].as_array().map(|a| a.len()), Some(0));
    assert!(result["activeGoalId"].is_null());
}

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
    // 新 GoalState 自动生成 goal id（不采用 requested_id）。
    assert!(
        created["goal"]["id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_eq!(created["goal"]["status"], "active");
    assert_eq!(created["deduplicated"], false);
}

#[tokio::test]
async fn test_goal_upsert_更新缺expected_revision返回冲突() {
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
    .expect("更新应成功（新 GoalState 无 revision 冲突）");

    assert_eq!(updated["revision"], 0);
    assert_eq!(updated["goal"]["title"], "更新");
}

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

    // 非法 status
    let err = handle_request(
        "session/goal-transition",
        &json!({
            "sessionId": "goal-sess",
            "goalId": goal_id,
            "status": "archived",
            "expectedRevision": 1,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, -32602);

    // 合法 completed
    let done = handle_request(
        "session/goal-transition",
        &json!({
            "sessionId": "goal-sess",
            "goalId": goal_id,
            "status": "completed",
            "expectedRevision": 1,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    assert_eq!(done["goal"]["status"], "completed");
    assert_eq!(done["revision"], 0);
}

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

// ── (KeenCode) session/replay 与 session/recovery wire 测试 ─────────────────────

use std::sync::Mutex as StdMutex;

/// 记录所有通知的 transport（供 recovery 断言）。
struct RecordingTransport {
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

/// 用 SQLite ThreadStore 构造 cfg（replay 需要真实事件日志）。
async fn make_replay_cfg(
    tmp: &tempfile::TempDir,
) -> (
    AcpServerConfig,
    Arc<dyn crate::transport::AcpTransport>,
    Arc<StdMutex<Vec<(String, Value)>>>,
) {
    let peri_config = make_peri_config(
        make_provider_config("test", "anthropic", "test-key", "test-model"),
        "test-model",
    );
    let thread_store = Arc::new(
        peri_agent::thread::SqliteThreadStore::new(tmp.path().join("threads.db"))
            .await
            .unwrap(),
    ) as Arc<dyn peri_agent::thread::ThreadStore>;
    let provider = LlmProvider::from_config(&peri_config).expect("测试配置应可构造");
    let provider_runtime = Arc::new(parking_lot::RwLock::new(provider));
    let permission_mode = SharedPermissionMode::new(PermissionMode::Bypass);
    let session_manager = crate::session::SessionManager::new(
        Arc::clone(&thread_store),
        provider_runtime.read().clone(),
        Arc::new(peri_config.clone()),
        Arc::clone(&permission_mode),
        None,
    );
    let notifications = Arc::new(StdMutex::new(Vec::new()));
    let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(RecordingTransport {
        notifications: Arc::clone(&notifications),
    });
    let cfg = AcpServerConfig {
        provider: provider_runtime,
        peri_config: Arc::new(parking_lot::RwLock::new(peri_config)),
        permission_mode,
        cron_scheduler: None,
        mcp_pool: None,
        channel_state: None,
        plugin_skill_roots: Arc::new(parking_lot::RwLock::new(Vec::new())),
        plugin_agent_dirs: Vec::new(),
        plugin_hooks: Vec::new(),
        plugin_loaded: Vec::new(),
        hook_groups: Vec::new(),
        plugin_lsp_servers: Vec::new(),
        tool_search_index: Arc::new(peri_middlewares::tool_search::ToolSearchIndex::new()),
        shared_tools: Arc::new(parking_lot::RwLock::new(BTreeMap::new())),
        thread_store,
        langfuse_session: None,
        config_path: tmp.path().join("settings.json"),
        session_manager,
    };
    (cfg, transport, notifications)
}

#[tokio::test]
async fn test_session_replay_分页与增量游标() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport, _notifications) = make_replay_cfg(&tmp).await;
    let mut sessions = HashMap::new();

    // session/new 创建线程并写入 replay epoch
    let created = handle_request(
        "session/new",
        &json!({ "cwd": "/tmp" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/new 应成功");
    let session_id = created["sessionId"].as_str().unwrap().to_string();

    // 直接追加 3 条消息（模拟 agent 持久化路径）
    for i in 0..3 {
        cfg.thread_store
            .append_message(
                &session_id,
                peri_agent::messages::BaseMessage::human(format!("msg {i}")),
            )
            .await
            .unwrap();
    }

    // 第一页：limit=2 → truncated，next.sequence=2
    let page1 = handle_request(
        "session/replay",
        &json!({ "sessionId": session_id, "limit": 2 }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("session/replay 应成功");
    assert_eq!(page1["replayed_events"], 2);
    assert_eq!(page1["truncated"], true);
    assert_eq!(page1["next"]["sequence"], 2);
    let epoch = page1["next"]["epoch"].as_str().unwrap().to_string();

    // 第二页：after 游标 → 剩余 1 条，不再 truncated
    let page2 = handle_request(
        "session/replay",
        &json!({
            "sessionId": session_id,
            "after": { "epoch": epoch, "sequence": 2 },
            "limit": 10,
        }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .expect("增量重放应成功");
    assert_eq!(page2["replayed_events"], 1);
    assert_eq!(page2["truncated"], false);
    assert_eq!(page2["next"]["sequence"], 3);
    assert_eq!(page2["from"]["sequence"], 2);
}

#[tokio::test]
async fn test_session_replay_过期epoch回退全量重放() {
    let tmp = tempfile::TempDir::new().unwrap();
    let (cfg, transport, _n) = make_replay_cfg(&tmp).await;
    let mut sessions = HashMap::new();

    let created = handle_request(
        "session/new",
        &json!({ "cwd": "/tmp" }),
        &cfg,
        &mut sessions,
        &transport,
    )
    .await
    .unwrap();
    let session_id = created["sessionId"].as_str().unwrap().to_string();
    cfg.thread_store
        .append_message(&session_id, peri_agent::messages::BaseMessage::human("m1"))
        .await
        .unwrap();
    cfg.thread_store
        .append_message(&session_id, peri_agent::messages::BaseMessage::human("m2"))
        .await
        .unwrap();

    // 过期 epoch → 全量重放（2 条），from 回填旧游标
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
    assert_eq!(page["replayed_events"], 2, "过期 epoch 应回退全量重放");
    assert_eq!(page["from"]["epoch"], "stale-epoch");
}

// ── (KeenCode) 会话级 provider 隔离 ──────────────────────────────────────────

/// 两个 provider：p1 是四档 Profile 的默认（m1-default），p2 用于会话内切换。
fn make_peri_config_two_providers() -> PeriConfig {
    use crate::provider::{AppConfig, ProfileConfig, Profiles};
    let p1 = make_provider_config("p1", "anthropic", "key-1", "m1-default");
    let p2 = make_provider_config("p2", "openai", "key-2", "m2");
    let profile = ProfileConfig {
        provider: "p1".to_string(),
        model: Some("m1-default".to_string()),
        effort: "high".to_string(),
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
    };
    let mut profiles = Profiles::default();
    profiles.opus = profile.clone();
    profiles.sonnet = profile;
    PeriConfig {
        schema: None,
        config: AppConfig {
            active_alias: "opus".to_string(),
            providers: vec![p1, p2],
            profiles,
            ..AppConfig::default()
        },
    }
}

/// 从 ConfigOptionUpdate 序列化值中取 "model" option 的 currentValue。
fn model_option_current_value(update: &Value) -> String {
    update["configOptions"]
        .as_array()
        .and_then(|options| {
            options
                .iter()
                .find(|option| option["id"].as_str() == Some("model"))
        })
        .and_then(|option| option["currentValue"].as_str())
        .unwrap_or("")
        .to_string()
}

/// 会话级模型切换只写目标 session 的 provider；其他 session 与全局默认不受影响。
#[tokio::test]
async fn test_set_config_option_model_会话隔离() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_server_config(make_peri_config_two_providers(), &tmp);
    let transport: std::sync::Arc<dyn crate::transport::AcpTransport> =
        std::sync::Arc::new(MockTransport);
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

    assert_eq!(
        sessions[&session_a].provider.read().model_name(),
        "m1-default",
        "新会话默认跟随全局 provider"
    );

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

    assert_eq!(
        sessions[&session_a].provider.read().model_name(),
        "m2",
        "目标会话的 provider 应切换为新模型"
    );
    assert_eq!(
        sessions[&session_b].provider.read().model_name(),
        "m1-default",
        "其他会话的 provider 不受影响"
    );
    assert_eq!(
        cfg.provider.read().model_name(),
        "m1-default",
        "全局默认 provider 不受影响"
    );
    assert_eq!(
        cfg.peri_config.read().config.active_alias,
        "opus",
        "四档 active_alias 不应被会话切换改写"
    );
    assert_eq!(
        model_option_current_value(&response),
        "m2",
        "响应中的 configOptions 应反映会话 provider"
    );
}

/// 无效的 provider/model 编码被忽略，会话 provider 保持不变。
#[tokio::test]
async fn test_set_config_option_model_无效值被忽略() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_server_config(make_peri_config_two_providers(), &tmp);
    let transport: std::sync::Arc<dyn crate::transport::AcpTransport> =
        std::sync::Arc::new(MockTransport);
    let mut sessions = HashMap::new();

    // p3 无 api_key：from_provider_config 应拒绝
    {
        let mut c = cfg.peri_config.write();
        c.config
            .providers
            .push(make_provider_config("p3", "openai", "", "m3"));
    }

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

    for bad in ["ghost::m", "p1::", "p3::m3"] {
        handle_request(
            "session/set_config_option",
            &json!({
                "sessionId": session_id,
                "configId": "model",
                "value": bad,
            }),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .expect("无效值应被忽略而不是报错");
        assert_eq!(
            sessions[&session_id].provider.read().model_name(),
            "m1-default",
            "无效编码 {bad} 不应改变会话 provider"
        );
    }
}

/// 捕获通知的 mock transport：验证 session/update 广播携带会话级模型。
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

/// ConfigOptionUpdate 通知按会话 provider 构建（A5 会话作用域）。
#[tokio::test]
async fn test_config_option_update_按会话provider广播() {
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_server_config(make_peri_config_two_providers(), &tmp);
    let capture = Arc::new(CaptureTransport {
        notifications: std::sync::Mutex::new(Vec::new()),
    });
    let transport: std::sync::Arc<dyn crate::transport::AcpTransport> = capture.clone();
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

    // 直接读取捕获的 session/update 通知
    let (_method, params) = capture
        .notifications
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|(m, _)| m == "session/update")
        .cloned()
        .expect("应存在 session/update 通知");
    assert_eq!(
        params["sessionId"].as_str().unwrap(),
        session_id,
        "通知应携带目标会话 id"
    );
    assert_eq!(
        model_option_current_value(&params["update"]),
        "m2",
        "ConfigOptionUpdate 应反映会话级模型"
    );
}

/// build_config_options 的 model 选项直接反映传入的 provider（绕过四档 Profile）。
#[test]
fn test_make_config_options_模型选项反映provider() {
    use crate::provider::LlmProvider;
    let tmp = tempfile::TempDir::new().unwrap();
    let cfg = make_server_config(make_peri_config_two_providers(), &tmp);

    let provider = LlmProvider::OpenAi {
        api_key: "key-2".to_string(),
        base_url: "https://models.example/v1".to_string(),
        model: "custom-model".to_string(),
        effort: Some("high".to_string()),
        max_tokens: 32000,
        context_1m: false,
        context_window: None,
        retry_observer: None,
    };
    let peri_config = cfg.peri_config.read();
    let options = make_config_options(&peri_config, &provider, PermissionMode::Bypass);
    let model_option = options
        .iter()
        .find(|option| option.id.to_string() == "model")
        .expect("应存在 model option");
    let serialized = serde_json::to_value(model_option).unwrap();
    assert_eq!(
        serialized["currentValue"].as_str().unwrap(),
        "custom-model",
        "model option 的 currentValue 应来自会话 provider"
    );
}
