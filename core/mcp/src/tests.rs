//! keencode-mcp 不依赖网络和外部进程的协议与 OAuth 单元测试。

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::oauth::{
    OAuthAuthorizationServerMetadata, OAuthCallback, OAuthConfig, OAuthError, OAuthMachine,
    OAuthMetadataFetcher, OAuthProtectedResourceMetadata, OAuthSnapshot, OAuthStatus,
    OAuthTokenSet, ReqwestOAuthMetadataFetcher, authorization_server_metadata_urls,
    discover_oauth_config, parse_www_authenticate, protected_resource_metadata_urls,
};
use crate::protocol::{IncomingMessage, RequestId, parse_incoming, server_request_response};
use crate::types::{McpTaskSupport, McpTool, McpToolAnnotations, McpToolEffect, McpToolSet};

#[test]
fn json_rpc_response_requires_matching_id() {
    let message =
        parse_incoming(br#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#).expect("响应应可解析");
    let IncomingMessage::Response(response) = message else {
        panic!("应解析为响应");
    };
    let error = response
        .into_result(&RequestId::Number(8))
        .expect_err("ID 不匹配必须失败");
    assert!(error.to_string().contains("ID 不匹配"));
}

#[test]
fn json_rpc_error_and_notification_are_classified() {
    let error = parse_incoming(
        br#"{"jsonrpc":"2.0","id":"abc","error":{"code":-32000,"message":"failed","data":{"retry":false}}}"#,
    )
    .expect("错误响应应可解析");
    let IncomingMessage::Response(error) = error else {
        panic!("应解析为错误响应");
    };
    assert!(
        error
            .into_result(&RequestId::String("abc".to_owned()))
            .expect_err("RPC 错误必须归一化")
            .to_string()
            .contains("-32000")
    );

    let notification = parse_incoming(
        br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{}}"#,
    )
    .expect("通知应可解析");
    let IncomingMessage::Notification(notification) = notification else {
        panic!("应解析为通知");
    };
    assert_eq!(notification.method, "notifications/tools/list_changed");
}

#[test]
fn malformed_json_rpc_is_rejected() {
    for invalid in [
        br#"{"id":1,"result":{}}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":1,"result":{},"error":{"code":1,"message":"bad"}}"#,
        br#"{"jsonrpc":"2.0","id":null,"result":{}}"#,
        br#"{"jsonrpc":"2.0","method":"x","params":1}"#,
    ] {
        assert!(parse_incoming(invalid).is_err(), "{invalid:?} 必须被拒绝");
    }
}

#[test]
fn ping_response_is_empty_and_unimplemented_capabilities_are_rejected() {
    assert_eq!(
        server_request_response(RequestId::String("ping-id".to_owned()), "ping"),
        json!({ "jsonrpc": "2.0", "id": "ping-id", "result": {} })
    );
    let mut options = crate::McpClientOptions::default();
    options.capabilities.tasks = Some(json!({ "requests": {} }));
    assert!(options.validate().is_err());
}

#[test]
fn required_task_support_is_preserved_and_detected() {
    let tool: McpTool = serde_json::from_value(json!({
        "name": "task-only",
        "inputSchema": { "type": "object" },
        "execution": { "taskSupport": "required" }
    }))
    .expect("execution.taskSupport 应可反序列化");
    assert!(tool.requires_task());
    assert_eq!(
        tool.execution.expect("execution 应保留").task_support,
        McpTaskSupport::Required
    );
}

#[test]
fn unknown_or_unannotated_tools_default_to_state_changes() {
    let mut tools = McpToolSet::new(vec![tool("readonly", Some(true)), tool("missing", None)]);
    assert_eq!(
        tools.effect_for("readonly"),
        McpToolEffect::ChangesState,
        "不可信服务端 readOnlyHint 不得降低本地权限"
    );
    assert_eq!(tools.effect_for("missing"), McpToolEffect::ChangesState);
    assert_eq!(tools.effect_for("unknown"), McpToolEffect::ChangesState);
    assert!(tools.set_local_effect("readonly", McpToolEffect::ReadOnly));
    assert_eq!(tools.effect_for("readonly"), McpToolEffect::ReadOnly);
    assert!(!tools.set_local_effect("unknown", McpToolEffect::ReadOnly));
}

#[test]
fn oauth_pkce_snapshot_callback_and_refresh_round_trip() {
    let config = oauth_config();
    let mut machine = OAuthMachine::new(config.clone()).expect("OAuth 配置应有效");
    let authorization = machine.begin_authorization(1_000).expect("应生成授权请求");
    assert!(
        authorization
            .authorization_url
            .contains("code_challenge_method=S256")
    );
    assert_eq!(
        machine.snapshot().status(),
        OAuthStatus::AwaitingAuthorization
    );

    let snapshot_json = serde_json::to_string(machine.snapshot()).expect("快照应可序列化");
    let snapshot: OAuthSnapshot = serde_json::from_str(&snapshot_json).expect("快照应可反序列化");
    let mut restored = OAuthMachine::restore(config, snapshot).expect("快照应可恢复");
    let exchange = restored
        .handle_callback(
            OAuthCallback {
                state: authorization.state,
                code: Some("authorization-code".to_owned()),
                error: None,
                error_description: None,
            },
            1_001,
        )
        .expect("合法回调应生成交换请求");
    let verifier = exchange.code_verifier.expect("授权码交换必须包含 verifier");
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(challenge, authorization.code_challenge);
    assert_eq!(exchange.grant_type, "authorization_code");
    assert_eq!(exchange.resource, "https://mcp.example.test/mcp");

    restored
        .accept_token(OAuthTokenSet {
            access_token: "access".to_owned(),
            token_type: "Bearer".to_owned(),
            expires_at: Some(2_000),
            refresh_token: Some("refresh".to_owned()),
            scope: Some("mcp".to_owned()),
        })
        .expect("令牌应被接受");
    assert_eq!(restored.access_token(1_500).expect("令牌应有效"), "access");
    let refresh = restored.begin_refresh().expect("应生成刷新请求");
    assert_eq!(refresh.grant_type, "refresh_token");
    assert_eq!(refresh.refresh_token.as_deref(), Some("refresh"));
    restored.reject_refresh(1_600);
    assert_eq!(restored.snapshot().status(), OAuthStatus::Authorized);
    assert_eq!(restored.snapshot().last_error(), Some("刷新令牌请求失败"));
    restored.begin_refresh().expect("应允许再次刷新");
    restored
        .accept_token(OAuthTokenSet {
            access_token: "new-access".to_owned(),
            token_type: "Bearer".to_owned(),
            expires_at: None,
            refresh_token: None,
            scope: None,
        })
        .expect("刷新响应可省略未轮换的 refresh_token 与 scope");
    let refreshed = restored
        .snapshot()
        .token_set()
        .expect("刷新后应保留令牌集合");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh"));
    assert_eq!(refreshed.scope.as_deref(), Some("mcp"));
    restored
        .begin_refresh()
        .expect("保留的刷新令牌应可继续使用");
    restored.reject_refresh(9_999);
    assert_eq!(
        restored.snapshot().status(),
        OAuthStatus::Authorized,
        "没有明确过期时间的旧令牌仍应视为可用"
    );
}

#[test]
fn oauth_discovery_binds_resource_issuer_scope_and_pkce() {
    let challenge = parse_www_authenticate(
        r#"Bearer resource_metadata="https://mcp.example.test/metadata", scope="files:read files:write""#,
    )
    .expect("Bearer challenge 应可解析");
    assert_eq!(
        protected_resource_metadata_urls("https://mcp.example.test/mcp", Some(&challenge))
            .expect("显式 PRM 地址应有效"),
        vec!["https://mcp.example.test/metadata"]
    );
    assert_eq!(
        protected_resource_metadata_urls("https://mcp.example.test/public/mcp", None)
            .expect("PRM fallback 地址应有效"),
        vec![
            "https://mcp.example.test/.well-known/oauth-protected-resource/public/mcp",
            "https://mcp.example.test/.well-known/oauth-protected-resource"
        ]
    );
    assert_eq!(
        authorization_server_metadata_urls("https://auth.example.test/tenant")
            .expect("AS discovery 地址应有效"),
        vec![
            "https://auth.example.test/.well-known/oauth-authorization-server/tenant",
            "https://auth.example.test/.well-known/openid-configuration/tenant",
            "https://auth.example.test/tenant/.well-known/openid-configuration"
        ]
    );
    let protected = OAuthProtectedResourceMetadata {
        resource: "https://mcp.example.test/mcp".to_owned(),
        authorization_servers: vec!["https://auth.example.test".to_owned()],
        scopes_supported: vec!["fallback".to_owned()],
    };
    let authorization = OAuthAuthorizationServerMetadata {
        issuer: "https://auth.example.test".to_owned(),
        authorization_endpoint: "https://auth.example.test/authorize".to_owned(),
        token_endpoint: "https://auth.example.test/token".to_owned(),
        code_challenge_methods_supported: vec!["S256".to_owned()],
    };
    let config = OAuthConfig::from_discovery(
        "https://mcp.example.test/mcp",
        "keencode-test",
        "http://127.0.0.1:32123/callback",
        &protected,
        &authorization,
        Some(&challenge),
    )
    .expect("匹配的发现文档应生成配置");
    assert_eq!(config.authorization_server_issuer, authorization.issuer);
    assert_eq!(config.scopes, vec!["files:read", "files:write"]);
    let mut machine = OAuthMachine::new(config).expect("发现配置应通过校验");
    let request = machine.begin_authorization(1).expect("应生成授权 URL");
    assert!(
        request
            .authorization_url
            .contains("resource=https%3A%2F%2Fmcp.example.test%2Fmcp")
    );
}

#[tokio::test]
async fn oauth_discovery_orchestrates_prm_fallback_and_as_metadata() {
    let fetcher = MockMetadataFetcher {
        documents: BTreeMap::from([
            (
                "https://mcp.example.test/.well-known/oauth-protected-resource".to_owned(),
                json!({
                    "resource": "https://mcp.example.test/mcp",
                    "authorization_servers": ["https://auth.example.test"],
                    "scopes_supported": ["mcp"]
                }),
            ),
            (
                "https://auth.example.test/.well-known/oauth-authorization-server".to_owned(),
                json!({
                    "issuer": "https://auth.example.test",
                    "authorization_endpoint": "https://auth.example.test/authorize",
                    "token_endpoint": "https://auth.example.test/token",
                    "code_challenge_methods_supported": ["S256"]
                }),
            ),
        ]),
    };
    let config = discover_oauth_config(
        &fetcher,
        "https://mcp.example.test/mcp",
        "client",
        "http://localhost/callback",
        None,
    )
    .await
    .expect("mock 文档应完成 PRM 与 AS discovery");
    assert_eq!(
        config.authorization_server_issuer,
        "https://auth.example.test"
    );
    assert_eq!(config.resource, "https://mcp.example.test/mcp");
    assert_eq!(config.scopes, vec!["mcp"]);
}

#[tokio::test]
async fn oauth_discovery_rejects_cross_issuer_and_private_metadata_targets() {
    let fetcher = MockMetadataFetcher {
        documents: BTreeMap::from([
            (
                "https://mcp.example.test/.well-known/oauth-protected-resource".to_owned(),
                json!({
                    "resource": "https://mcp.example.test/mcp",
                    "authorization_servers": [
                        "https://auth-a.example.test",
                        "https://auth-b.example.test"
                    ]
                }),
            ),
            (
                "https://auth-a.example.test/.well-known/oauth-authorization-server".to_owned(),
                json!({
                    "issuer": "https://auth-b.example.test",
                    "authorization_endpoint": "https://auth-b.example.test/authorize",
                    "token_endpoint": "https://auth-b.example.test/token",
                    "code_challenge_methods_supported": ["S256"]
                }),
            ),
        ]),
    };
    assert!(matches!(
        discover_oauth_config(
            &fetcher,
            "https://mcp.example.test/mcp",
            "client",
            "http://localhost/callback",
            None,
        )
        .await,
        Err(OAuthError::InvalidDiscovery(_))
    ));

    let fetcher =
        ReqwestOAuthMetadataFetcher::new(Duration::from_secs(1), 1024).expect("读取器配置应有效");
    assert!(matches!(
        fetcher.fetch_json("https://127.0.0.1/metadata").await,
        Err(OAuthError::InvalidDiscovery(_))
    ));
}

#[test]
fn oauth_metadata_urls_reject_query_fragment_and_bearer_is_case_insensitive() {
    assert!(parse_www_authenticate(r#"bEaReR scope="read""#).is_ok());
    assert_eq!(
        parse_www_authenticate("Bearer")
            .expect("无参数 Bearer challenge 应有效")
            .scopes,
        Vec::<String>::new()
    );
    let challenge = parse_www_authenticate(
        r#"Basic realm="legacy", bEaReR RESOURCE_METADATA="https://mcp.example.test/metadata", SCOPE="read write", Digest realm="ignored""#,
    )
    .expect("多 challenge 中应选择 Bearer 且参数名不区分大小写");
    assert_eq!(
        challenge.resource_metadata.as_deref(),
        Some("https://mcp.example.test/metadata")
    );
    assert_eq!(challenge.scopes, vec!["read", "write"]);
    assert!(parse_www_authenticate(r#"Bearer scope="read", scope="write""#).is_err());
    assert!(
        parse_www_authenticate(
            r#"Bearer resource_metadata="https://mcp.example.test/metadata?token=secret""#,
        )
        .is_err()
    );
    assert!(authorization_server_metadata_urls("https://auth.example.test?tenant=a").is_err());
    assert!(authorization_server_metadata_urls("https://auth.example.test/#fragment").is_err());
}

#[test]
fn oauth_rejects_insecure_endpoints_missing_pkce_and_mismatched_resource() {
    let mut config = oauth_config();
    config.authorization_endpoint = "http://127.0.0.1/authorize".to_owned();
    assert!(OAuthMachine::new(config).is_err());

    let mut config = oauth_config();
    config.code_challenge_methods_supported.clear();
    assert!(OAuthMachine::new(config).is_err());

    let protected = OAuthProtectedResourceMetadata {
        resource: "https://other.example.test/mcp".to_owned(),
        authorization_servers: vec!["https://auth.example.test".to_owned()],
        scopes_supported: Vec::new(),
    };
    let authorization = OAuthAuthorizationServerMetadata {
        issuer: "https://auth.example.test".to_owned(),
        authorization_endpoint: "https://auth.example.test/authorize".to_owned(),
        token_endpoint: "https://auth.example.test/token".to_owned(),
        code_challenge_methods_supported: vec!["S256".to_owned()],
    };
    assert!(matches!(
        OAuthConfig::from_discovery(
            "https://mcp.example.test/mcp",
            "client",
            "http://localhost/callback",
            &protected,
            &authorization,
            None,
        ),
        Err(OAuthError::InvalidDiscovery(_))
    ));
}

#[test]
fn fixed_protocol_and_error_display_do_not_expose_details() {
    let options = crate::McpClientOptions {
        protocol_version: "2025-03-26".to_owned(),
        ..crate::McpClientOptions::default()
    };
    assert!(options.validate().is_err());

    let fake_api_key = "sk-fake-unit-test-only-not-a-key";
    let error = crate::McpError::Rpc {
        code: -32000,
        message: format!("{fake_api_key}\r\n{}", "vendor-body".repeat(1024)),
        data: Some(json!({ "access_token": fake_api_key })),
    };
    let rendered = error.to_string();
    assert_eq!(rendered, "MCP RPC 错误 -32000：服务端返回错误");
    assert_eq!(format!("{error:?}"), rendered);
    assert!(!rendered.contains(fake_api_key));
    assert!(!rendered.contains('\r'));
    assert!(!rendered.contains('\n'));

    let cancellation = crate::McpError::Cancelled {
        method: "tools/call".to_owned(),
        reason: Some(format!(
            "{fake_api_key}\r\n\u{1b}[31m{}",
            "remote-reason".repeat(1024)
        )),
    };
    let cancellation = format!("{cancellation:?}");
    assert_eq!(cancellation, "MCP 请求 tools/call 已取消");
    assert!(!cancellation.contains('\r'));
    assert!(!cancellation.contains('\n'));
    assert!(!cancellation.contains('\u{1b}'));
    assert!(!cancellation.contains(fake_api_key));

    let denied = OAuthError::AuthorizationDenied {
        code: format!("{fake_api_key}\r\n{}", "remote-code".repeat(1024)),
        description: Some(format!("Bearer {fake_api_key}")),
    };
    let denied = format!("{denied:?}");
    assert_eq!(denied, "授权被拒绝");
    assert!(!denied.contains(fake_api_key));
    assert!(!denied.contains('\r'));
    assert!(!denied.contains('\n'));

    let string_id = RequestId::String("response-id-secret".to_owned());
    assert!(!format!("{string_id:?}").contains("response-id-secret"));

    let config =
        crate::StreamableHttpConfig::new("https://mcp.example.test/mcp?access_token=secret-query");
    assert!(!format!("{config:?}").contains("secret-query"));

    let mut oauth = oauth_config();
    oauth.token_endpoint = "https://auth.example.test/token?secret=query".to_owned();
    assert!(!format!("{oauth:?}").contains("secret=query"));
}

#[test]
fn streamable_http_requires_tls_outside_loopback() {
    for endpoint in [
        "http://example.test/mcp",
        "http://192.168.1.10/mcp",
        "http://localhost.example.test/mcp",
    ] {
        assert!(
            crate::StreamableHttpConfig::new(endpoint)
                .validate()
                .is_err(),
            "远端明文端点必须拒绝：{endpoint}"
        );
    }
    for endpoint in [
        "https://example.test/mcp",
        "http://localhost/mcp",
        "http://127.0.0.2/mcp",
        "http://[::1]/mcp",
    ] {
        assert!(
            crate::StreamableHttpConfig::new(endpoint)
                .validate()
                .is_ok(),
            "TLS 或真实回环端点应允许：{endpoint}"
        );
    }
}

#[test]
fn oauth_restore_rejects_inconsistent_snapshots_and_invalid_tokens() {
    let missing_tokens: OAuthSnapshot = serde_json::from_value(json!({
        "status": "authorized",
        "pending": null,
        "tokenSet": null,
        "lastError": null
    }))
    .expect("测试快照结构应可反序列化");
    assert!(matches!(
        OAuthMachine::restore(oauth_config(), missing_tokens),
        Err(OAuthError::InvalidTransition(_))
    ));

    let missing_pending: OAuthSnapshot = serde_json::from_value(json!({
        "status": "awaiting_authorization",
        "pending": null,
        "tokenSet": null,
        "lastError": null
    }))
    .expect("测试快照结构应可反序列化");
    assert!(matches!(
        OAuthMachine::restore(oauth_config(), missing_pending),
        Err(OAuthError::InvalidTransition(_))
    ));

    let mut machine = OAuthMachine::new(oauth_config()).expect("OAuth 配置应有效");
    let authorization = machine.begin_authorization(100).expect("应开始授权");
    machine
        .handle_callback(
            OAuthCallback {
                state: authorization.state,
                code: Some("code".to_owned()),
                error: None,
                error_description: None,
            },
            101,
        )
        .expect("应进入令牌交换状态");
    assert!(matches!(
        machine.accept_token(OAuthTokenSet {
            access_token: "access".to_owned(),
            token_type: "Basic".to_owned(),
            expires_at: None,
            refresh_token: None,
            scope: None,
        }),
        Err(OAuthError::InvalidCallback(_))
    ));
    assert!(matches!(
        machine.accept_token(OAuthTokenSet {
            access_token: "x".repeat(20 * 1024),
            token_type: "Bearer".to_owned(),
            expires_at: None,
            refresh_token: None,
            scope: None,
        }),
        Err(OAuthError::InvalidCallback(_))
    ));
    assert!(matches!(
        machine.accept_token(OAuthTokenSet {
            access_token: "access".to_owned(),
            token_type: "Bearer".to_owned(),
            expires_at: None,
            refresh_token: Some(String::new()),
            scope: None,
        }),
        Err(OAuthError::InvalidCallback(_))
    ));
    machine
        .accept_token(OAuthTokenSet {
            access_token: "access".to_owned(),
            token_type: "bearer".to_owned(),
            expires_at: None,
            refresh_token: Some("refresh".to_owned()),
            scope: Some("read write".to_owned()),
        })
        .expect("大小写不同的 Bearer 应规范化后接受");
    assert_eq!(
        machine
            .snapshot()
            .token_set()
            .expect("应保存令牌")
            .token_type,
        "Bearer"
    );
}

#[test]
fn oauth_snapshot_and_session_debug_hide_external_details() {
    let denied_snapshot: OAuthSnapshot = serde_json::from_value(json!({
        "status": "denied",
        "pending": null,
        "tokenSet": null,
        "lastError": "server-secret-detail"
    }))
    .expect("拒绝快照应可反序列化");
    assert!(!format!("{denied_snapshot:?}").contains("server-secret-detail"));

    let initialize: crate::InitializeResult = serde_json::from_value(json!({
        "protocolVersion": crate::DEFAULT_PROTOCOL_VERSION,
        "capabilities": { "experimental": { "secret": "capability-secret" } },
        "serverInfo": { "name": "server-secret", "version": "1" },
        "instructions": "instruction-secret"
    }))
    .expect("初始化结果应可反序列化");
    let session: crate::McpServerSession = initialize.into();
    let rendered = format!("{session:?}");
    assert!(!rendered.contains("server-secret"));
    assert!(!rendered.contains("capability-secret"));
    assert!(!rendered.contains("instruction-secret"));
}

#[test]
fn oauth_rejects_csrf_denial_and_expired_authorization() {
    let mut csrf = OAuthMachine::new(oauth_config()).expect("配置应有效");
    csrf.begin_authorization(100).expect("应开始授权");
    assert_eq!(
        csrf.handle_callback(
            OAuthCallback {
                state: "wrong".to_owned(),
                code: Some("code".to_owned()),
                error: None,
                error_description: None,
            },
            101,
        )
        .expect_err("错误 state 必须失败"),
        OAuthError::InvalidState
    );

    let mut denied = OAuthMachine::new(oauth_config()).expect("配置应有效");
    let request = denied.begin_authorization(100).expect("应开始授权");
    let error = denied
        .handle_callback(
            OAuthCallback {
                state: request.state,
                code: None,
                error: Some("sk-fake-oauth-code-test-only".to_owned()),
                error_description: Some("sk-fake-oauth-description-test-only".to_owned()),
            },
            101,
        )
        .expect_err("拒绝回调必须失败");
    assert!(matches!(&error, OAuthError::AuthorizationDenied { .. }));
    assert_eq!(error.to_string(), "授权被拒绝");
    assert_eq!(denied.snapshot().status(), OAuthStatus::Denied);
    assert_eq!(
        denied.snapshot().last_error(),
        Some("授权被拒绝"),
        "服务端 error code 与 description 不得进入持久快照"
    );

    let mut expired = OAuthMachine::new(oauth_config()).expect("配置应有效");
    let request = expired.begin_authorization(100).expect("应开始授权");
    assert_eq!(
        expired
            .handle_callback(
                OAuthCallback {
                    state: request.state,
                    code: Some("code".to_owned()),
                    error: None,
                    error_description: None,
                },
                111,
            )
            .expect_err("过期回调必须失败"),
        OAuthError::AuthorizationExpired
    );
    assert_eq!(expired.snapshot().status(), OAuthStatus::Expired);
}

/// 构造测试工具定义。
fn tool(name: &str, read_only_hint: Option<bool>) -> McpTool {
    McpTool {
        name: name.to_owned(),
        title: None,
        description: None,
        input_schema: json!({ "type": "object" }),
        output_schema: None,
        annotations: read_only_hint.map(|read_only_hint| McpToolAnnotations {
            read_only_hint: Some(read_only_hint),
            ..McpToolAnnotations::default()
        }),
        execution: None,
        icons: Vec::new(),
        meta: None,
    }
}

/// 构造只允许十秒回调等待的本机 OAuth 配置。
/// 当前唯一 OAuth 配置必须保留可校验的签发方身份，不接受无 issuer 的旧结构。
#[test]
fn oauth_config_requires_safe_authorization_server_identity() {
    let config = oauth_config();
    let mut encoded = serde_json::to_value(&config).unwrap();
    assert_eq!(
        encoded["authorizationServerIssuer"],
        "https://auth.example.test"
    );
    encoded
        .as_object_mut()
        .unwrap()
        .remove("authorizationServerIssuer");
    assert!(serde_json::from_value::<OAuthConfig>(encoded).is_err());
    for issuer in [
        "",
        "http://auth.example.test",
        "https://user:secret@auth.example.test",
        "https://auth.example.test#fragment",
        "https://auth.example.test?query=secret",
    ] {
        let mut invalid = config.clone();
        invalid.authorization_server_issuer = issuer.to_owned();
        assert!(invalid.validate().is_err());
    }
}

/// 构造绑定到固定测试签发方的有效 OAuth 配置。
fn oauth_config() -> OAuthConfig {
    OAuthConfig {
        authorization_server_issuer: "https://auth.example.test".to_owned(),
        authorization_endpoint: "https://auth.example.test/authorize".to_owned(),
        token_endpoint: "https://auth.example.test/token".to_owned(),
        resource: "https://mcp.example.test/mcp".to_owned(),
        client_id: "keencode-test".to_owned(),
        redirect_uri: "http://127.0.0.1:32123/callback".to_owned(),
        scopes: vec!["mcp".to_owned()],
        code_challenge_methods_supported: vec!["S256".to_owned()],
        authorization_timeout_seconds: 10,
    }
}

struct MockMetadataFetcher {
    documents: BTreeMap<String, serde_json::Value>,
}

#[async_trait]
impl OAuthMetadataFetcher for MockMetadataFetcher {
    async fn fetch_json(&self, url: &str) -> Result<serde_json::Value, OAuthError> {
        self.documents
            .get(url)
            .cloned()
            .ok_or_else(|| OAuthError::DiscoveryTransport(format!("mock 404: {url}")))
    }
}
