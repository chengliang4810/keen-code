//! Tests for client

use super::*;

#[test]
fn test_pool_get_all_clients_filters_disconnected() {
    let pool = McpClientPool::new_empty();
    assert!(pool.get_all_clients().is_empty());
}
#[test]
fn test_pool_has_no_resources() {
    assert!(!McpClientPool::new_empty().has_resources());
}
#[test]
fn test_resource_summary_empty() {
    assert!(McpClientPool::new_empty().resource_summary().is_empty());
}
#[test]
fn test_client_status_equality() {
    assert_eq!(ClientStatus::Connected, ClientStatus::Connected);
    assert_ne!(
        ClientStatus::Failed("a".into()),
        ClientStatus::Failed("b".into())
    );
}
#[test]
fn test_mcp_init_status_equality() {
    assert_eq!(McpInitStatus::Pending, McpInitStatus::Pending);
    assert_eq!(
        McpInitStatus::Initializing {
            connected: 1,
            total: 2
        },
        McpInitStatus::Initializing {
            connected: 1,
            total: 2
        }
    );
    assert_ne!(
        McpInitStatus::Ready { total: 3 },
        McpInitStatus::Ready { total: 4 }
    );
}
#[test]
fn test_new_pending_creates_empty_pool() {
    let pool = McpClientPool::new_pending();
    assert!(pool.clients.read().is_empty());
}

/// 显式文件初始化只能读取调用方给出的文件；禁用项不得启动子进程。
#[tokio::test]
async fn test_initialize_from_explicit_path_registers_disabled_server() {
    let temporary = tempfile::tempdir().expect("创建临时目录");
    let config_path = temporary.path().join("mcp-runtime.json");
    std::fs::write(
        &config_path,
        r#"{"mcpServers":{"disabled-test":{"command":"command-that-must-not-run","disabled":true}}}"#,
    )
    .expect("写入测试配置");
    let pool = Arc::new(McpClientPool::new_pending());
    let (status_tx, _status_rx) = tokio::sync::watch::channel(McpInitStatus::Pending);

    McpClientPool::run_initialize_from_path(pool.clone(), &config_path, status_tx, None, None)
        .await
        .expect("显式配置应初始化成功");

    let infos = pool.all_server_infos();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].name, "disabled-test");
    assert_eq!(infos[0].status, ClientStatus::Disabled);
    assert!(matches!(
        &*pool.init_status.read(),
        McpInitStatus::Ready { total: 0 }
    ));
}

/// 配置热重载必须清除旧服务器状态，同时保留宿主级回调配置。
#[tokio::test]
async fn test_reset_for_reinitialize_restores_pending_state() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.configs.write().insert(
        "old".to_owned(),
        McpServerConfig {
            command: Some("old-command".to_owned()),
            args: None,
            env: None,
            url: None,
            headers: None,
            oauth: None,
            disabled: Some(true),
            source: None,
        },
    );
    pool.clients.write().insert(
        "old".to_owned(),
        Arc::new(McpClientHandle {
            name: "old".to_owned(),
            peer: None,
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Disabled,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            channel_capable: false,
        }),
    );
    pool.mark_initialized();

    pool.reset_for_reinitialize().await;

    assert!(pool.clients.read().is_empty());
    assert!(pool.configs.read().is_empty());
    assert!(matches!(&*pool.init_status.read(), McpInitStatus::Pending));
    assert!(!pool.initialized.load(std::sync::atomic::Ordering::SeqCst));
}

/// MCP 快照必须提供稳定状态、OAuth 状态与可直接展示的错误文本。
#[test]
fn test_snapshot_serializes_stable_oauth_and_error_fields() {
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "oauth-server".to_owned(),
        Arc::new(McpClientHandle {
            name: "oauth-server".to_owned(),
            peer: None,
            tools: Vec::new(),
            resources: Vec::new(),
            status: ClientStatus::Failed("需要登录".to_owned()),
            oauth_status: OAuthStatus::NeedsAuthorization,
            source: None,
            url: Some("https://example.invalid/mcp".to_owned()),
            channel_capable: false,
        }),
    );
    *pool.init_status.write() = McpInitStatus::Failed("初始化失败".to_owned());

    let snapshot = peri_acp_types::ports::McpPoolPort::snapshot(&pool);

    assert_eq!(snapshot.as_object().map(|value| value.len()), Some(2));
    assert_eq!(snapshot["initPhase"], "failed");
    let server = &snapshot["servers"][0];
    assert_eq!(server.as_object().map(|value| value.len()), Some(6));
    assert_eq!(server["name"], "oauth-server");
    assert_eq!(server["status"], "failed");
    assert_eq!(server["oauthStatus"], "needs_authorization");
    assert_eq!(server["error"], "需要登录");
    assert_eq!(server["transport"], "http");
    assert_eq!(server["toolsCount"], 0);
}

#[test]
fn test_server_infos_empty_pool() {
    assert!(McpClientPool::new_pending().server_infos().is_empty());
}
#[tokio::test]
async fn test_insert_failed() {
    let pool = Arc::new(McpClientPool::new_pending());
    McpClientPool::insert_failed(&pool, "s", "err".into());
    assert_eq!(
        pool.server_infos()[0].status,
        ClientStatus::Failed("err".into())
    );
}
#[tokio::test]
async fn test_remove_server() {
    let pool = Arc::new(McpClientPool::new_pending());
    pool.clients.write().insert(
        "a".into(),
        Arc::new(McpClientHandle {
            name: "a".into(),
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            channel_capable: false,
        }),
    );
    pool.remove_server("a").await;
    assert!(pool.server_infos().is_empty());
}
#[tokio::test]
async fn test_get_tools_resources() {
    let pool = McpClientPool::new_pending();
    pool.clients.write().insert(
        "s".into(),
        Arc::new(McpClientHandle {
            name: "s".into(),
            peer: None,
            tools: vec![],
            resources: vec![],
            status: ClientStatus::Connected,
            oauth_status: OAuthStatus::default(),
            source: None,
            url: None,
            channel_capable: false,
        }),
    );
    assert!(pool.get_tools("s").is_empty());
    assert!(pool.get_tools("x").is_empty());
}

#[test]
fn test_plugin_source_of_empty_pool_returns_none() {
    let pool = McpClientPool::new_pending();
    assert!(pool.plugin_source_of("any").is_none());
}

#[test]
fn test_plugin_source_of_after_write_returns_value() {
    let pool = McpClientPool::new_pending();
    pool.plugin_sources
        .write()
        .insert("p1__srv1".to_string(), "p1@marketplace_a".to_string());
    assert_eq!(
        pool.plugin_source_of("p1__srv1"),
        Some("p1@marketplace_a".to_string())
    );
}

#[test]
fn test_plugin_source_of_nonexistent_returns_none() {
    let pool = McpClientPool::new_pending();
    pool.plugin_sources
        .write()
        .insert("p1__srv1".to_string(), "p1@alpha".to_string());
    assert!(pool.plugin_source_of("nonexistent").is_none());
}
