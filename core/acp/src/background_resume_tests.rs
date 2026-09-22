//! 后台子 Agent 恢复扩展的严格请求、响应编解码回归。

use crate::{
    AcpRequest, AcpRequestDecoder, AcpResponseDecoder, AcpResponseEncoder,
    ResumeBackgroundTaskResponse, schema::RequestId,
};
use serde_json::{Value, json};

/// 恢复请求只接受明确的 Session、childThreadId 和可选 operationId 元数据。
#[test]
fn background_resume_request_decodes_only_current_shape() {
    let decoder = AcpRequestDecoder::new();
    let request = decoder
        .decode_request(
            "keencode/background/resume",
            json!({
                "sessionId": "session-a",
                "childThreadId": "child-a",
                "_meta": { "keencode/operationId": "resume-a" }
            }),
        )
        .expect("恢复请求应解码");
    assert!(matches!(
        request,
        AcpRequest::ResumeBackgroundTask(ref value)
            if value.session_id == "session-a" && value.child_thread_id == "child-a" && value.meta.is_some()
    ));
    assert_eq!(request.method(), "keencode/background/resume");

    for params in [
        json!({}),
        json!({ "sessionId": "session-a" }),
        json!({ "sessionId": "session-a", "childThreadId": "" }),
        json!({ "sessionId": "session-a", "childThreadId": "child-a", "taskId": "turn-a" }),
    ] {
        assert!(
            decoder
                .decode_request("keencode/background/resume", params)
                .is_err()
        );
    }
    assert!(
        decoder
            .decode_request(
                "keencode/background/cancel",
                json!({ "sessionId": "session-a", "childThreadId": "child-a" }),
            )
            .is_err()
    );
}

/// 恢复响应只返回本次新 Turn 的 taskId，并可在 JSON-RPC 结果中完整往返。
#[test]
fn background_resume_response_round_trips_and_rejects_invalid_fields() {
    let expected = ResumeBackgroundTaskResponse::new("session-a", "child-a", "turn-new");
    let raw = AcpResponseEncoder::new()
        .encode_result(RequestId::Number(7), &expected)
        .expect("恢复响应应编码");
    let encoded: Value = serde_json::from_slice(&raw).expect("恢复响应应为 JSON");
    assert_eq!(
        encoded["result"],
        json!({
            "sessionId": "session-a",
            "childThreadId": "child-a",
            "taskId": "turn-new"
        })
    );
    let recovered = AcpResponseDecoder::new()
        .decode_result::<ResumeBackgroundTaskResponse>(&raw)
        .expect("恢复响应应解码");
    assert_eq!(recovered.result(), &expected);

    for (field, invalid) in [
        ("sessionId", json!("")),
        ("childThreadId", json!("")),
        ("taskId", json!("")),
    ] {
        let mut value = serde_json::to_value(&expected).unwrap();
        value[field] = invalid;
        let response: ResumeBackgroundTaskResponse = serde_json::from_value(value.clone()).unwrap();
        assert!(response.validate().is_err());
        assert!(
            AcpResponseEncoder::new()
                .encode_result(RequestId::Number(7), &response)
                .is_err()
        );
    }
    let mut unknown = serde_json::to_value(&expected).unwrap();
    unknown["extra"] = json!(true);
    assert!(serde_json::from_value::<ResumeBackgroundTaskResponse>(unknown).is_err());
}
