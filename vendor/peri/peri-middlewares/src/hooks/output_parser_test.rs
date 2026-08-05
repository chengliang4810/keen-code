use super::*;
use crate::hooks::types::PermissionDecision;

// === parse_command_hook_output tests ===

#[test]
fn test_parse_command_plain_text() {
    assert!(matches!(
        parse_command_hook_output("hello world"),
        HookAction::Allow
    ));
}

#[test]
fn test_parse_command_continue_false() {
    assert!(matches!(
        parse_command_hook_output(r#"{"continue": false}"#),
        HookAction::PreventContinuation { stop_reason: None }
    ));
}

#[test]
fn test_parse_command_decision_block() {
    assert!(matches!(
        parse_command_hook_output(r#"{"decision": "block", "reason": "test"}"#),
        HookAction::Block { reason } if reason == "test"
    ));
}

#[test]
fn test_parse_command_system_message() {
    assert!(matches!(
        parse_command_hook_output(r#"{"systemMessage": "warning"}"#),
        HookAction::SystemMessage { message } if message == "warning"
    ));
}

#[test]
fn test_parse_command_invalid_json() {
    assert!(matches!(
        parse_command_hook_output("{invalid json}"),
        HookAction::Allow
    ));
}

#[test]
fn test_parse_command_empty() {
    assert!(matches!(parse_command_hook_output(""), HookAction::Allow));
}

// === parse_http_hook_response tests ===

#[test]
fn test_parse_http_empty_body() {
    assert!(matches!(parse_http_hook_response(""), HookAction::Allow));
}

#[test]
fn test_parse_http_whitespace_body() {
    assert!(matches!(parse_http_hook_response("   "), HookAction::Allow));
}

#[test]
fn test_parse_http_non_json_body() {
    assert!(matches!(
        parse_http_hook_response("plain text"),
        HookAction::Allow
    ));
}

#[test]
fn test_parse_http_valid_json() {
    assert!(matches!(
        parse_http_hook_response(r#"{"continue": false, "stopReason": "test"}"#),
        HookAction::PreventContinuation { stop_reason } if stop_reason.as_deref() == Some("test")
    ));
}

#[test]
fn test_parse_http_invalid_json() {
    assert!(matches!(
        parse_http_hook_response("{invalid}"),
        HookAction::Allow
    ));
}

// === sync_response_to_action tests ===

#[test]
fn test_sync_response_priority_continue_over_decision() {
    let resp = SyncHookResponse {
        continue_run: Some(false),
        decision: Some(HookDecision::Block),
        reason: Some("blocked".into()),
        ..Default::default()
    };
    // continue=false 优先级高于 decision=block
    assert!(matches!(
        sync_response_to_action(&resp),
        HookAction::PreventContinuation { .. }
    ));
}

#[test]
fn test_sync_response_decision_block() {
    let resp = SyncHookResponse {
        decision: Some(HookDecision::Block),
        reason: Some("blocked".into()),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp),
        HookAction::Block { reason } if reason == "blocked"
    ));
}

#[test]
fn test_sync_response_system_message() {
    let resp = SyncHookResponse {
        system_message: Some("msg".into()),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp),
        HookAction::SystemMessage { message } if message == "msg"
    ));
}

#[test]
fn test_sync_response_hook_specific_updated_input() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            updated_input: Some(serde_json::json!({"key": "val"})),
            permission_decision: None,
            permission_decision_reason: None,
            additional_context: None,
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp),
        HookAction::ModifyInput { new_input } if new_input["key"] == "val"
    ));
}

#[test]
fn test_sync_response_hook_specific_permission_decision() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::PreToolUse {
            permission_decision: Some(PermissionDecision::Deny),
            permission_decision_reason: Some("not allowed".into()),
            updated_input: None,
            additional_context: None,
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp),
        HookAction::PermissionOverride { decision, .. } if decision == PermissionDecision::Deny
    ));
}

#[test]
fn test_sync_response_hook_specific_user_prompt_context() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::UserPromptSubmit {
            additional_context: Some("extra context".into()),
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp),
        HookAction::AdditionalContext { context } if context == "extra context"
    ));
}

#[test]
fn test_sync_response_hook_specific_session_start_message() {
    let resp = SyncHookResponse {
        hook_specific_output: Some(HookSpecificOutput::SessionStart {
            additional_context: None,
            initial_user_message: Some("start msg".into()),
            watch_paths: None,
        }),
        ..Default::default()
    };
    assert!(matches!(
        sync_response_to_action(&resp),
        HookAction::InitialUserMessage { message } if message == "start msg"
    ));
}

#[test]
fn test_sync_response_default_allow() {
    let resp = SyncHookResponse::default();
    assert!(matches!(sync_response_to_action(&resp), HookAction::Allow));
}

#[test]
fn test_sync_response_decision_approve_is_allow() {
    let resp = SyncHookResponse {
        decision: Some(HookDecision::Approve),
        ..Default::default()
    };
    // Approve is not Block, so falls through to Allow
    assert!(matches!(sync_response_to_action(&resp), HookAction::Allow));
}
