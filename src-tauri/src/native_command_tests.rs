//! 显式原生测试：Rust 通过正式 ACP 启动工具，桌面只观察并点击正式停止按钮。
//! 共享 mailbox 的隔离宿主和只读证据助手，不注入业务状态、不自行发送取消请求。

use std::path::Path;

use serde_json::{Value, json};

use crate::native_mailbox_tests::{
    assert_completed_prompt, capture_stage, recorded_dispatch, require_result,
    run_native_protocol_fixture, session_id_from_response, wait_for_signal, write_json_create_new,
};

/// 两个待取消回合及同一 Session 的恢复回合。
const TURNS: [(&str, &str); 3] = [
    ("native-command-shell", "KC_COMMAND_SHELL_CANCEL"),
    ("native-command-git", "KC_COMMAND_GIT_CANCEL"),
    ("native-command-recover", "KC_COMMAND_RECOVER"),
];

/// 必须收到标准 cancelled 和真实空闲快照，不能以工具超时替代 UI 停止。
fn assert_cancelled_prompt(response: &Value, session_id: &str) -> Result<(), String> {
    let result = require_result(response, "session/prompt")?;
    if result.get("stopReason").and_then(Value::as_str) != Some("cancelled") {
        return Err(format!("未收到 UI 停止所需的 cancelled 终态: {result}"));
    }
    let snapshot = result
        .get("_meta")
        .and_then(|meta| meta.get("keencode/snapshot"))
        .ok_or_else(|| "取消响应缺少权威 Session 快照".to_owned())?;
    if snapshot.get("sessionId").and_then(Value::as_str) != Some(session_id)
        || !snapshot.get("activeTurnId").is_some_and(Value::is_null)
        || !matches!(
            snapshot.get("state").and_then(Value::as_str),
            Some("idle" | "ready")
        )
    {
        return Err(format!("取消后 Session 未回到空闲状态: {snapshot}"));
    }
    Ok(())
}

/// 经正式 ACP 新建一个 Session；外部信号只推进测试线程，不控制产品取消接口。
fn run_command_protocol(
    fixture_root: &Path,
    data_root: &Path,
    workspace_root: &Path,
    operation_id: &str,
) -> Result<(String, usize), String> {
    let initialized = recorded_dispatch(
        fixture_root,
        "host-initialize",
        json!({
            "jsonrpc": "2.0", "id": "native-command-initialize", "method": "initialize",
            "params": {"protocolVersion": 1, "clientInfo": {"name": "KeenCode", "version": "0.0.1"},
                "clientCapabilities": {"elicitation": {"form": {}}}}
        }),
    )?;
    require_result(&initialized, "initialize")?;
    let created = recorded_dispatch(
        fixture_root,
        "host-new",
        json!({
            "jsonrpc": "2.0", "id": "native-command-session-new", "method": "session/new",
            "params": {"cwd": workspace_root, "mcpServers": [],
                "_meta": {"keencode/operationId": operation_id}}
        }),
    )?;
    let session_id = session_id_from_response(&created)?;
    write_json_create_new(
        &fixture_root.join("host-ready.json"),
        &json!({
            "sessionId": session_id, "pid": std::process::id(), "workspace": workspace_root,
            "operationId": operation_id,
            "inputBoundary": "Rust正式ACP启动；UI仅观察及点击停止；非发布EXE完整发送验收"
        }),
    )?;
    wait_for_signal(&fixture_root.join("start"), "start")?;
    for (index, (turn_id, text)) in TURNS.iter().enumerate() {
        let stage = index + 1;
        let mode = recorded_dispatch(
            fixture_root,
            &format!("host-mode-{stage}"),
            json!({
                "jsonrpc": "2.0", "id": format!("native-command-mode-{stage}"),
                "method": "session/set_mode", "params": {"sessionId": session_id, "modeId": "default"}
            }),
        )?;
        require_result(&mode, "session/set_mode")?;
        let response = recorded_dispatch(
            fixture_root,
            &format!("host-turn-{stage}"),
            json!({
                "jsonrpc": "2.0", "id": turn_id, "method": "session/prompt",
                "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": text}],
                    "_meta": {"keencode/turnId": turn_id, "keencode/ultraMode": false}}
            }),
        )?;
        if stage < 3 {
            assert_cancelled_prompt(&response, &session_id)?;
        } else {
            assert_completed_prompt(&response, &session_id, turn_id)?;
        }
        capture_stage(fixture_root, data_root, &session_id, stage)?;
        let signal = ["second", "third", "finish"][index];
        wait_for_signal(&fixture_root.join(signal), signal)?;
    }
    Ok((session_id, TURNS.len()))
}

/// 真实 Windows 桌面仅负责停止，Git/Shell 启动和断言由隔离 Rust 夹具负责。
#[test]
#[ignore = "需要回环模型夹具、隔离目录及真实桌面上的两次停止操作"]
fn native_command_cancel_and_recover_use_formal_acp_dispatch() {
    run_native_protocol_fixture("KEENCODE_NATIVE_COMMAND_FIXTURE_DIR", run_command_protocol);
}
