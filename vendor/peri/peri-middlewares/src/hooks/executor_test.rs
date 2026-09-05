use std::{path::PathBuf, time::Duration};

#[cfg(unix)]
use std::fs;

use super::*;
use crate::hooks::types::HookEvent;

fn make_registered() -> RegisteredHook {
    RegisteredHook {
        hook: serde_json::from_str(r#"{"type":"command","command":"echo"}"#).unwrap(),
        event: HookEvent::PreToolUse,
        matcher: None,
        plugin_name: "test-plugin".to_string(),
        plugin_id: "test-id".to_string(),
        plugin_root: PathBuf::from("/tmp/test-plugin"),
        plugin_data_dir: PathBuf::from("/tmp/test-plugin-data"),
        plugin_options: std::collections::HashMap::new(),
    }
}

fn make_hook_input() -> HookInput {
    HookInput::session_start(
        "sess-1",
        "/tmp/transcript.json",
        "/project",
        "startup",
        "model-a",
    )
}

fn make_command_hook(command: &str) -> HookType {
    serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": command
    }))
    .unwrap()
}

#[tokio::test]
async fn test_command_hook_echo_plain_text() {
    let hook = make_command_hook("cat");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_exit_code_2_blocks() {
    let hook = make_command_hook("exit 2");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Block { .. }));
}

#[tokio::test]
async fn test_command_hook_exit_code_1_allows() {
    let hook = make_command_hook("echo 'error msg' >&2 && exit 1");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_json_output_continue_false() {
    let hook = make_command_hook(r#"echo '{"continue":false,"stopReason":"test stop"}'"#);
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(
        action,
        HookAction::PreventContinuation {
            stop_reason: Some(ref s)
        } if s == "test stop"
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_json_output_block() {
    let hook = make_command_hook(r#"echo '{"decision":"block","reason":"not allowed"}'"#);
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(
        action,
        HookAction::Block {
            reason: ref r
        } if r == "not allowed"
    ));
}

#[tokio::test]
async fn test_command_hook_timeout() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": "sleep 10",
        "timeout": 1
    }))
    .unwrap();
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

/// 根 shell 提前退出时，仍继承输出管道的短命后代输出应被完整 drain，
/// 而不是让 command hook 等到超时或永久挂起。
#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_drains_output_from_post_root_descendant() {
    let dir = tempfile::tempdir().expect("应能创建 hook 测试目录");
    let script = dir.path().join("late-output.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
(sleep 0.2; printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"late descendant"}}') &
exit 0
"#,
    )
    .expect("应能写入 hook 测试脚本");

    let hook = make_command_hook(&format!("sh {}", script.display()));
    let action = tokio::time::timeout(
        Duration::from_secs(4),
        execute_command_hook(&hook, &make_hook_input(), &make_registered()),
    )
    .await
    .expect("后代关闭输出管道后 hook 应在有限时间内完成");

    assert!(
        matches!(
            action,
            HookAction::AdditionalContext { ref context } if context == "late descendant"
        ),
        "应保留根进程退出后的后代输出，实际结果：{action:?}"
    );
}

/// command hook 超时必须终止独立进程组中的后代，不能只杀掉根 shell。
#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_timeout_kills_process_group_descendant() {
    let dir = tempfile::tempdir().expect("应能创建 hook 测试目录");
    let marker = dir.path().join("timeout-descendant.marker");
    let marker_path = marker.to_string_lossy().replace('\'', "'\\''");
    let script = dir.path().join("timeout.sh");
    fs::write(
        &script,
        format!("(sleep 2; touch '{marker_path}') &\nsleep 30\n"),
    )
    .expect("应能写入 hook 测试脚本");

    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": format!("sh {}", script.display()),
        "timeout": 1
    }))
    .unwrap();
    let action = tokio::time::timeout(
        Duration::from_secs(4),
        execute_command_hook(&hook, &make_hook_input(), &make_registered()),
    )
    .await
    .expect("超时路径应在有限时间内返回");
    assert!(matches!(action, HookAction::Allow));

    // 等待超过后代原本的 touch 时间；若只杀根 shell，marker 会被写出。
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !marker.exists(),
        "超时应终止整个 Unix 进程组，不能留下创建 marker 的后代"
    );
}

/// 上层取消 command hook future 时，guard 的 Drop 路径必须清理整棵进程树。
#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_future_drop_kills_process_group_descendant() {
    let dir = tempfile::tempdir().expect("应能创建 hook 测试目录");
    let marker = dir.path().join("drop-descendant.marker");
    let marker_path = marker.to_string_lossy().replace('\'', "'\\''");
    let script = dir.path().join("drop.sh");
    fs::write(
        &script,
        format!("(sleep 2; touch '{marker_path}') &\nsleep 30\n"),
    )
    .expect("应能写入 hook 测试脚本");

    let hook = make_command_hook(&format!("sh {}", script.display()));
    let input = make_hook_input();
    let registered = make_registered();
    let task = tokio::spawn(async move { execute_command_hook(&hook, &input, &registered).await });

    // 确保 spawn 已进入 command hook 的等待阶段，再模拟上层 future drop。
    tokio::time::sleep(Duration::from_millis(200)).await;
    task.abort();
    let _ = task.await;

    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        !marker.exists(),
        "future drop 应通过 ProcessTreeGuard 终止整个 Unix 进程组"
    );
}

/// Windows 没有 POSIX 进程组；Job Object/taskkill 路径仍应让超时 hook 快速返回。
#[cfg(windows)]
#[tokio::test]
async fn test_command_hook_windows_timeout_terminates_process() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": "Start-Sleep -Seconds 30",
        "timeout": 1
    }))
    .unwrap();

    let action = tokio::time::timeout(
        Duration::from_secs(4),
        execute_command_hook(&hook, &make_hook_input(), &make_registered()),
    )
    .await
    .expect("Windows 超时路径应在有限时间内返回");
    assert!(matches!(action, HookAction::Allow));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_exit_code_2_with_stdout_reason() {
    let hook = make_command_hook("echo 'custom block reason' && exit 2");
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(
        action,
        HookAction::Block {
            reason: ref r
        } if r == "custom block reason"
    ));
}

#[tokio::test]
async fn test_command_hook_plugin_options_env() {
    let mut registered = make_registered();
    registered
        .plugin_options
        .insert("api_key".to_string(), serde_json::json!("sk-test-123"));

    let hook = make_command_hook("echo $CLAUDE_PLUGIN_OPTION_API_KEY");
    let input = make_hook_input();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[cfg(unix)]
#[tokio::test]
async fn test_command_hook_exposes_codex_plugin_data() {
    let hook = make_command_hook(
        r#"test "$PLUGIN_DATA" = "$CLAUDE_PLUGIN_DATA" && echo '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"active"}}'"#,
    );
    let action = execute_command_hook(&hook, &make_hook_input(), &make_registered()).await;
    assert!(
        matches!(
            action,
            HookAction::AdditionalContext { ref context } if context == "active"
        ),
        "unexpected action: {action:?}"
    );
}

// === sanitize_header_value tests ===

#[test]
fn test_sanitize_crlf_injection() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("value\r\nX-Injected: evil", &allowed);
    assert_eq!(result, "valueX-Injected: evil");
}

#[test]
fn test_sanitize_lf_only() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("value\nX-Injected: evil", &allowed);
    assert_eq!(result, "valueX-Injected: evil");
}

#[test]
fn test_sanitize_cr_only() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("value\rX-Injected: evil", &allowed);
    assert_eq!(result, "valueX-Injected: evil");
}

#[test]
fn test_sanitize_env_var_expansion_allowed() {
    std::env::set_var("TEST_SANITIZE_HOOK_VAR", "secret-value");
    let allowed: HashSet<String> = ["TEST_SANITIZE_HOOK_VAR".to_string()].into_iter().collect();
    let result = sanitize_header_value("Bearer ${TEST_SANITIZE_HOOK_VAR}", &allowed);
    assert_eq!(result, "Bearer secret-value");
    std::env::remove_var("TEST_SANITIZE_HOOK_VAR");
}

#[test]
fn test_sanitize_env_var_expansion_not_allowed() {
    let allowed: HashSet<String> = HashSet::new();
    let result = sanitize_header_value("Bearer ${SECRET_KEY}", &allowed);
    assert_eq!(result, "Bearer ${SECRET_KEY}");
}

#[test]
fn test_sanitize_env_var_brace_expansion() {
    std::env::set_var("TEST_SANITIZE_HOOK_BRACE", "expanded");
    let allowed: HashSet<String> = ["TEST_SANITIZE_HOOK_BRACE".to_string()]
        .into_iter()
        .collect();
    let result = sanitize_header_value("token-${TEST_SANITIZE_HOOK_BRACE}", &allowed);
    assert_eq!(result, "token-expanded");
    std::env::remove_var("TEST_SANITIZE_HOOK_BRACE");
}

// === HTTP hook tests (no mock server, just SSRF/blocking logic) ===

#[tokio::test]
async fn test_http_hook_ssrf_blocked() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "http",
        "url": "http://192.168.1.1/hook",
        "timeout": 5
    }))
    .unwrap();
    let input = make_hook_input();
    let action = execute_http_hook(&hook, &input).await;
    assert!(matches!(action, HookAction::Block { .. }));
}

#[tokio::test]
async fn test_http_hook_invalid_url() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "http",
        "url": "not-a-valid-url",
        "timeout": 5
    }))
    .unwrap();
    let input = make_hook_input();
    let action = execute_http_hook(&hook, &input).await;
    assert!(matches!(action, HookAction::Block { .. }));
}

// === Wrong hook type dispatch tests ===

#[tokio::test]
async fn test_command_hook_wrong_type_returns_allow() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "http",
        "url": "http://example.com"
    }))
    .unwrap();
    let input = make_hook_input();
    let registered = make_registered();
    let action = execute_command_hook(&hook, &input, &registered).await;
    assert!(matches!(action, HookAction::Allow));
}

#[tokio::test]
async fn test_prompt_hook_wrong_type_returns_allow() {
    let hook: HookType = serde_json::from_value(serde_json::json!({
        "type": "command",
        "command": "echo test"
    }))
    .unwrap();
    let input = make_hook_input();
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!());
    let action = execute_prompt_hook(&hook, &input, &llm_factory).await;
    assert!(matches!(action, HookAction::Allow));
}

#[tokio::test]
async fn test_http_hook_wrong_type_returns_allow() {
    let hook = make_command_hook("echo test");
    let input = make_hook_input();
    let action = execute_http_hook(&hook, &input).await;
    assert!(matches!(action, HookAction::Allow));
}

#[tokio::test]
async fn test_agent_hook_wrong_type_returns_allow() {
    let hook = make_command_hook("echo test");
    let input = make_hook_input();
    let llm_factory: Arc<dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync> =
        Arc::new(|| unimplemented!());
    let action = execute_agent_hook(&hook, &input, &llm_factory, "/tmp").await;
    assert!(matches!(action, HookAction::Allow));
}
