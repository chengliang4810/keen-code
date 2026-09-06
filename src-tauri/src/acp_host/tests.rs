use super::{
    HostFailure, TerminalTurn, model_config_option, operation_id, prompt_stop_reason,
    prompt_turn_id,
};
use keencode_acp::schema;
use keencode_resources::{TurnStatus, TurnStopReason};

/// 模型配置错误仅传固定分类，不把内部诊断、路径、连接地址或凭据送到客户端。
#[test]
fn provider_diagnostics_are_safe_namespaced_rpc_data() {
    use crate::agent_runtime::AgentRuntimeError;

    for (error, expected) in [
        (
            AgentRuntimeError::ProviderConfigurationChanged,
            "provider_configuration_changed",
        ),
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
