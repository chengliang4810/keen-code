//! 后台任务列表的显式 Session 作用域与严格编解码回归。

use crate::{
    AcpRequest, AcpRequestDecoder, AcpResponseDecoder, AcpResponseEncoder, BackgroundTaskInfo,
    BackgroundTaskKind, ListBackgroundTasksResponse, schema::RequestId,
};
use serde_json::{Value, json};

/// 构造包含全部公开字段的真实形状 Shell 列表。
fn sample_background_list() -> ListBackgroundTasksResponse {
    ListBackgroundTasksResponse {
        session_id: "session-a".to_owned(),
        tasks: vec![BackgroundTaskInfo {
            session_id: "session-a".to_owned(),
            task_id: "shell-a".to_owned(),
            kind: BackgroundTaskKind::Shell,
            child_thread_id: None,
            summary: "运行测试".to_owned(),
            started_at: "2026-09-05T00:00:00.000Z".to_owned(),
            duration_ms: 1_250,
            pid: Some(1234),
        }],
    }
}

/// 不允许隐式依赖桌面焦点，也不引入旧 IPC 或标准命名空间别名。
#[test]
fn background_list_request_requires_explicit_session_and_exact_method() {
    let decoder = AcpRequestDecoder::new();
    let request = decoder
        .decode_request("keencode/background/list", json!({"sessionId":"session-a"}))
        .expect("显式 Session 请求应解码");
    assert!(
        matches!(request, AcpRequest::ListBackgroundTasks(ref value) if value.session_id == "session-a")
    );
    assert_eq!(request.method(), "keencode/background/list");
    for params in [
        json!({}),
        json!({"sessionId":""}),
        json!({"sessionId":"session\n-a"}),
        json!({"sessionId":"session-a", "all":true}),
    ] {
        assert!(
            decoder
                .decode_request("keencode/background/list", params)
                .is_err()
        );
    }
    for method in [
        "background_tasks_list",
        "session/background/list",
        "keencode/background/list_all",
    ] {
        assert!(
            decoder
                .decode_request(method, json!({"sessionId":"session-a"}))
                .is_err()
        );
    }
}

/// 空列表、Shell 和 Agent 均通过相同的类型化 JSON-RPC 响应往返。
#[test]
fn background_list_response_round_trips_shell_agent_and_empty_lists() {
    let shell = sample_background_list();
    let mut agent = shell.clone();
    agent.tasks[0].kind = BackgroundTaskKind::Agent;
    agent.tasks[0].child_thread_id = Some("child-a".to_owned());
    agent.tasks[0].pid = None;
    let empty = ListBackgroundTasksResponse {
        session_id: "session-a".to_owned(),
        tasks: vec![],
    };
    for expected in [shell, agent, empty] {
        let raw = AcpResponseEncoder::new()
            .encode_result(RequestId::Number(7), &expected)
            .unwrap();
        let recovered = AcpResponseDecoder::new()
            .decode_result::<ListBackgroundTasksResponse>(&raw)
            .unwrap();
        assert_eq!(recovered.result(), &expected);
        let encoded: Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(encoded["result"]["sessionId"], "session-a");
        assert_eq!(encoded["result"], serde_json::to_value(&expected).unwrap());
    }
}

/// 跨 Session、重复任务和不一致的类别字段必须在入站及出站边界均被拒绝。
#[test]
fn background_list_rejects_scope_duplicates_and_invalid_task_fields() {
    let original = serde_json::to_value(sample_background_list()).unwrap();
    let mut invalid_values = Vec::new();
    for (field, invalid) in [
        ("sessionId", json!("session-other")),
        ("taskId", json!("")),
        ("durationMs", json!(9_007_199_254_740_992_u64)),
        ("pid", json!(0)),
        ("childThreadId", json!("child-on-shell")),
        ("kind", json!("agent")),
    ] {
        let mut value = original.clone();
        value["tasks"][0][field] = invalid;
        invalid_values.push(value);
    }
    let mut duplicate = original.clone();
    duplicate["tasks"]
        .as_array_mut()
        .unwrap()
        .push(original["tasks"][0].clone());
    invalid_values.push(duplicate);
    for value in invalid_values {
        let response: ListBackgroundTasksResponse = serde_json::from_value(value.clone()).unwrap();
        assert!(response.validate().is_err());
        assert!(
            AcpResponseEncoder::new()
                .encode_result(RequestId::Number(7), &response)
                .is_err()
        );
        let raw = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":7,"result":value})).unwrap();
        assert!(
            AcpResponseDecoder::new()
                .decode_result::<ListBackgroundTasksResponse>(&raw)
                .is_err()
        );
    }
    let mut unknown = original;
    unknown["tasks"][0]["privateField"] = json!(true);
    assert!(serde_json::from_value::<ListBackgroundTasksResponse>(unknown).is_err());
}

/// 有界任务数及 JavaScript 可精确表示的持续时间均覆盖等于上限和超限。
#[test]
fn background_list_enforces_task_count_and_safe_integer_boundaries() {
    let mut response = sample_background_list();
    response.tasks[0].duration_ms = 9_007_199_254_740_991;
    let template = response.tasks[0].clone();
    response.tasks = (0..1_024)
        .map(|index| BackgroundTaskInfo {
            task_id: format!("task-{index}"),
            ..template.clone()
        })
        .collect();
    assert!(response.validate().is_ok());
    response.tasks.push(BackgroundTaskInfo {
        task_id: "task-overflow".to_owned(),
        ..template
    });
    assert!(response.validate().is_err());
}
