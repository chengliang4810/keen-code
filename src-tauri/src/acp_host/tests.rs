use super::{
    HostFailure, TerminalTurn, initialize_unpublished_session_mcp, map_session_mcp_failure,
    model_config_option, operation_id, prompt_stop_reason, prompt_turn_id,
};
use keencode_acp::schema;
use keencode_resources::{
    SessionForkRequest as RuntimeForkRequest, SessionId, TurnStatus, TurnStopReason,
};
use keencode_runtime::RuntimeError;
use tempfile::tempdir;

/// 模型配置错误仅传固定分类，不把内部诊断、路径、连接地址或凭据送到客户端。
#[test]
fn provider_diagnostics_are_safe_namespaced_rpc_data() {
    use crate::agent_runtime::AgentRuntimeError;

    for (error, expected) in [
        (
            AgentRuntimeError::ProviderNotConfigured,
            "provider_not_configured",
        ),
        (
            AgentRuntimeError::ProviderReloadFailed,
            "provider_reload_failed",
        ),
    ] {
        let rpc = super::map_runtime_failure(error).rpc_error();
        let value = serde_json::to_value(rpc).expect("应编码官方错误对象");
        assert_eq!(
            value,
            serde_json::json!({
                "code": -32603,
                "message": "Internal error",
                "data": {"keencode/errorCode": expected}
            })
        );
    }
}

/// 非配置错误继续使用官方内部错误，不能误提示用户重选模型。
#[test]
fn unrelated_runtime_failures_do_not_claim_provider_recovery() {
    use crate::agent_runtime::AgentRuntimeError;

    for error in [
        AgentRuntimeError::StateUnavailable,
        AgentRuntimeError::RecoveryRequired,
    ] {
        assert_eq!(super::map_runtime_failure(error), HostFailure::Internal);
        assert_eq!(
            serde_json::to_value(super::map_runtime_failure(error).rpc_error()).unwrap(),
            serde_json::json!({"code": -32603, "message": "Internal error"})
        );
    }
}

/// 有可选模型但尚未选择时必须如实显示未配置，不能把目录第一项当作实际配置。
#[test]
fn model_config_does_not_invent_a_selected_model() {
    let option = model_config_option(
        None,
        vec![schema::SessionConfigSelectOption::new(
            "provider::first",
            "First",
        )],
    );
    let json = serde_json::to_value(option).unwrap();
    assert_eq!(json["currentValue"], "unconfigured");
    assert_eq!(json["options"][0]["value"], "unconfigured");
    assert_eq!(json["options"][1]["value"], "provider::first");
}

/// 目录变化后保留当前选择的真实标识，并允许客户端改选仍可用的模型。
#[test]
fn model_config_keeps_an_unavailable_current_selection_visible() {
    let option = model_config_option(
        Some("provider::removed".to_owned()),
        vec![schema::SessionConfigSelectOption::new(
            "provider::first",
            "First",
        )],
    );
    let json = serde_json::to_value(option).unwrap();
    assert_eq!(json["currentValue"], "provider::removed");
    assert_eq!(json["options"][0]["value"], "provider::removed");
    assert_eq!(json["options"][1]["value"], "provider::first");
}

/// 未携带私有元数据的标准请求每次都必须获得新的业务身份，不能由请求 ID 派生。
#[test]
fn standard_request_identity_is_fresh_without_private_metadata() {
    let first = operation_id(None).unwrap();
    let second = operation_id(None).unwrap();
    assert_ne!(first, second);
    assert!(first.starts_with("operation-"));
    assert!(second.starts_with("operation-"));

    // JSON-RPC ID 只负责响应关联；同一个标准请求 ID 的两次 Prompt 仍获得
    // 不同的持久 Turn 身份，避免重连复用 ID 时误命中旧 Turn。
    let first_turn = prompt_turn_id(None).unwrap();
    let second_turn = prompt_turn_id(None).unwrap();
    assert_ne!(first_turn, second_turn);

    let meta = serde_json::Map::from_iter([(
        "keencode/operationId".to_owned(),
        serde_json::json!("explicit-operation"),
    )]);
    assert_eq!(operation_id(Some(&meta)).unwrap(), "explicit-operation");
    assert_eq!(operation_id(Some(&meta)).unwrap(), "explicit-operation");

    let turn_meta = serde_json::Map::from_iter([(
        "keencode/turnId".to_owned(),
        serde_json::json!("explicit-turn"),
    )]);
    assert_eq!(prompt_turn_id(Some(&turn_meta)).unwrap(), "explicit-turn");
    assert_eq!(prompt_turn_id(Some(&turn_meta)).unwrap(), "explicit-turn");
}

/// 标准 new/fork 的目标尚未公开时，MCP 初始化失败必须关闭进程内所有权；
/// 标准 load 已有明确身份，失败则保留旧 Session/目录，三条路径都可安全重试。
#[tokio::test]
async fn standard_session_mcp_failures_preserve_cleanup_and_retry_ownership() {
    let fixture = tempdir().expect("应创建 Host MCP 临时根");
    let storage_root = fixture.path().join("data");
    let project_root = fixture.path().join("project");
    std::fs::create_dir_all(&project_root).expect("应创建测试项目根");
    let runtime = crate::agent_runtime::AgentRuntime::new_for_control_test(&storage_root)
        .expect("应创建控制面 Runtime");
    let rejected_server = || {
        schema::McpServer::Sse(schema::McpServerSse::new(
            "unsupported-sse",
            "https://example.test/sse",
        ))
    };

    // session/new：失败响应没有公开确定性 ID，因此关闭进程内句柄但保留磁盘事实。
    let new_session = runtime
        .open_or_create_session(&project_root, None, "host-new-mcp-retry")
        .expect("应创建 new 目标");
    let new_id = new_session.session_id().as_str().to_owned();
    drop(new_session);
    assert_eq!(
        initialize_unpublished_session_mcp(
            &runtime,
            &new_id,
            &project_root,
            vec![rejected_server()],
        )
        .await,
        Err(HostFailure::InvalidParams)
    );
    assert!(matches!(
        runtime.runtime_manager().get(new_id.clone()),
        Err(RuntimeError::SessionNotRegistered)
    ));
    assert!(
        runtime
            .runtime_manager()
            .stored_session_metadata(&new_id)
            .is_ok(),
        "失败清理不得破坏相同 operationId 的持久重试锚点"
    );
    let retried_new = runtime
        .open_or_create_session(&project_root, None, "host-new-mcp-retry")
        .expect("相同 new operationId 应重新打开原目标");
    assert_eq!(retried_new.session_id().as_str(), new_id);
    initialize_unpublished_session_mcp(&runtime, &new_id, &project_root, Vec::new())
        .await
        .expect("修正配置后的 new 重试应成功");

    // session/load：调用方已知 Session ID，替换失败必须保留当前打开 Session 和旧目录。
    let load_error = runtime
        .replace_session_mcp_servers(&new_id, &project_root, vec![rejected_server()])
        .await
        .expect_err("load 的不支持传输必须失败");
    assert_eq!(
        map_session_mcp_failure(load_error),
        HostFailure::InvalidParams
    );
    assert!(runtime.runtime_manager().get(new_id.clone()).is_ok());
    runtime
        .replace_session_mcp_servers(&new_id, &project_root, Vec::new())
        .await
        .expect("修正配置后的 load 重试应成功");

    // session/fork：持久分支事务保持幂等，未公开目标只释放进程内所有权。
    let source = runtime
        .open_or_create_session(&project_root, None, "host-fork-source")
        .expect("应创建 fork 源");
    let source_id = source.session_id().as_str().to_owned();
    drop(source);
    runtime
        .close_session(&source_id)
        .await
        .expect("fork 前应关闭源 Session");
    let fork_request = RuntimeForkRequest {
        source_session_id: SessionId::new(source_id).expect("源 ID 应有效"),
        operation_id: "host-fork-mcp-retry".to_owned(),
        title: None,
    };
    let first_fork = runtime
        .runtime_manager()
        .fork_closed_session(fork_request.clone())
        .expect("应创建持久 fork 目标");
    let target_id = first_fork.session_id.as_str().to_owned();
    let target = runtime
        .open_or_create_session(&project_root, Some(&target_id), "unused")
        .expect("应打开 fork 目标");
    drop(target);
    assert_eq!(
        initialize_unpublished_session_mcp(
            &runtime,
            &target_id,
            &project_root,
            vec![rejected_server()],
        )
        .await,
        Err(HostFailure::InvalidParams)
    );
    assert!(matches!(
        runtime.runtime_manager().get(target_id.clone()),
        Err(RuntimeError::SessionNotRegistered)
    ));
    let retried_fork = runtime
        .runtime_manager()
        .fork_closed_session(fork_request)
        .expect("相同 fork operationId 应复用持久目标");
    assert_eq!(retried_fork.session_id.as_str(), target_id);
    let retried_target = runtime
        .open_or_create_session(&project_root, Some(&target_id), "unused")
        .expect("应重新打开原 fork 目标");
    drop(retried_target);
    initialize_unpublished_session_mcp(&runtime, &target_id, &project_root, Vec::new())
        .await
        .expect("修正配置后的 fork 重试应成功");

    runtime
        .close_session(&new_id)
        .await
        .expect("应关闭 new/load 测试 Session");
    runtime
        .close_session(&target_id)
        .await
        .expect("应关闭 fork 测试 Session");
}

/// 模型 Token 上限、Runtime 轮次上限和拒答必须返回不同的标准 ACP 停止原因。
#[test]
fn prompt_preserves_authoritative_stop_reasons() {
    for (status, reason, expected) in [
        (TurnStatus::Completed, None, schema::StopReason::EndTurn),
        (
            TurnStatus::Cancelled,
            Some(TurnStopReason::Cancelled),
            schema::StopReason::Cancelled,
        ),
        (
            TurnStatus::Failed,
            Some(TurnStopReason::LimitReached),
            schema::StopReason::MaxTurnRequests,
        ),
        (
            TurnStatus::Failed,
            Some(TurnStopReason::ModelOutputLimit),
            schema::StopReason::MaxTokens,
        ),
        (
            TurnStatus::Failed,
            Some(TurnStopReason::ModelRefusal),
            schema::StopReason::Refusal,
        ),
    ] {
        assert_eq!(
            prompt_stop_reason(&TerminalTurn {
                status,
                stop_reason: reason
            }),
            Ok(expected)
        );
    }
}

/// 故障、未结束 Turn 或不一致的持久状态不能伪装成正常模型停止。
#[test]
fn prompt_rejects_failure_and_inconsistent_terminal_states() {
    for (status, reason) in [
        (TurnStatus::Running, None),
        (TurnStatus::Failed, Some(TurnStopReason::Failed)),
        (TurnStatus::Failed, Some(TurnStopReason::ContextBlocked)),
        (TurnStatus::Failed, None),
        (TurnStatus::Completed, Some(TurnStopReason::ModelRefusal)),
        (TurnStatus::Cancelled, None),
    ] {
        assert_eq!(
            prompt_stop_reason(&TerminalTurn {
                status,
                stop_reason: reason
            }),
            Err(HostFailure::Internal)
        );
    }
}
