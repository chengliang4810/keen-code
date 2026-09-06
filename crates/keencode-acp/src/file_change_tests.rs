use super::*;
use crate::schema::RequestId;
use base64::Engine as _;
use serde_json::{Value, json};

/// 构造一个完整的 Hello 快照读取响应。
fn hello_response() -> ReadFileChangeResponse {
    ReadFileChangeResponse {
        session_id: "session-a".to_owned(),
        request_id: "request-a".to_owned(),
        side: FileChangeSide::After,
        offset: 0,
        total_bytes: 5,
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned(),
        data: "aGVsbG8=".to_owned(),
        eof: true,
    }
}

#[test]
fn file_change_request_uses_strict_wire_shape_and_method() {
    let request =
        ReadFileChangeRequest::new("session-a", "request-a", FileChangeSide::Before, 7, 128);
    assert_eq!(
        serde_json::to_value(&request).expect("读取请求应序列化"),
        json!({
            "sessionId": "session-a",
            "requestId": "request-a",
            "side": "before",
            "offset": 7,
            "length": 128,
        })
    );
    let decoded = AcpRequestDecoder::new()
        .decode_request(
            "keencode/session/file-change/read",
            serde_json::to_value(&request).expect("读取请求 JSON 应生成"),
        )
        .expect("读取请求应严格解码");
    assert!(matches!(
        &decoded,
        AcpRequest::ReadFileChange(value) if value == &request
    ));
    assert_eq!(decoded.method(), "keencode/session/file-change/read");
}

#[test]
fn file_change_request_rejects_unknown_fields_and_invalid_bounds() {
    let decoder = AcpRequestDecoder::new();
    let mut unknown = json!({
        "sessionId": "session-a",
        "requestId": "request-a",
        "side": "after",
        "offset": 0,
        "length": 1,
    });
    unknown["unexpected"] = Value::Bool(true);
    assert!(
        decoder
            .decode_request("keencode/session/file-change/read", unknown)
            .is_err()
    );

    for params in [
        json!({
            "sessionId": "session-a",
            "requestId": "request-a",
            "side": "after",
            "offset": 0,
            "length": 0,
        }),
        json!({
            "sessionId": "session-a",
            "requestId": "request-a",
            "side": "after",
            "offset": 0,
            "length": MAX_FILE_CHANGE_READ_BYTES + 1,
        }),
        json!({
            "sessionId": "session-a",
            "requestId": "request-a",
            "side": "after",
            "offset": MAX_FILE_CHANGE_BYTES + 1,
            "length": 1,
        }),
        json!({
            "sessionId": "session-a",
            "requestId": "request-a",
            "side": "after",
            "offset": 0,
            "length": 1.5,
        }),
        json!({
            "sessionId": "",
            "requestId": "request-a",
            "side": "after",
            "offset": 0,
            "length": 1,
        }),
    ] {
        assert!(
            decoder
                .decode_request("keencode/session/file-change/read", params)
                .is_err(),
            "越界或错误形状必须拒绝"
        );
    }
}

#[test]
fn file_change_request_allows_maximum_offset_without_client_snapshot_size() {
    let request = ReadFileChangeRequest::new(
        "session-a",
        "request-a",
        FileChangeSide::After,
        MAX_FILE_CHANGE_BYTES,
        1,
    );
    assert!(request.validate().is_ok());
    let near_limit = ReadFileChangeRequest::new(
        "session-a",
        "request-a",
        FileChangeSide::After,
        MAX_FILE_CHANGE_BYTES - 1,
        MAX_FILE_CHANGE_READ_BYTES,
    );
    assert!(near_limit.validate().is_ok());
}

#[test]
fn file_change_response_round_trips_through_typed_encoder_and_decoder() {
    let response = hello_response();
    let raw = AcpResponseEncoder::new()
        .encode_result(RequestId::Number(1), &response)
        .expect("读取响应应编码");
    let value: Value = serde_json::from_slice(&raw).expect("读取响应应为 JSON");
    assert_eq!(value["result"]["side"], "after");
    assert_eq!(value["result"]["data"], "aGVsbG8=");
    let restored = AcpResponseDecoder::new()
        .decode_result::<ReadFileChangeResponse>(&raw)
        .expect("读取响应应严格恢复");
    assert_eq!(restored.result(), &response);
}

#[test]
fn file_change_response_rejects_bad_base64_hash_eof_length_and_unknown_fields() {
    let decoder = AcpResponseDecoder::new();
    let valid = serde_json::to_value(hello_response()).expect("响应 JSON 应生成");
    let mut unknown = valid.clone();
    unknown["unexpected"] = Value::Bool(true);
    assert!(
        decoder
            .decode_result::<ReadFileChangeResponse>(
                &serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":unknown}))
                    .expect("未知字段响应应序列化"),
            )
            .is_err()
    );

    let mut bad_base64 = valid.clone();
    bad_base64["data"] = json!("aGVsbG8");
    let mut bad_eof = valid.clone();
    bad_eof["eof"] = json!(false);
    let mut bad_hash = valid.clone();
    bad_hash["sha256"] = json!("00".repeat(32));
    let mut bad_empty_page = valid.clone();
    bad_empty_page["data"] = json!("");
    bad_empty_page["totalBytes"] = json!(5);
    bad_empty_page["eof"] = json!(false);
    for result in [bad_base64, bad_eof, bad_hash, bad_empty_page] {
        let raw = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"result":result}))
            .expect("非法读取响应应序列化");
        assert!(
            decoder
                .decode_result::<ReadFileChangeResponse>(&raw)
                .is_err()
        );
    }
}

#[test]
fn file_change_response_default_budget_accepts_maximum_page() {
    let data =
        base64::engine::general_purpose::STANDARD
            .encode(vec![0x5a; MAX_FILE_CHANGE_READ_BYTES as usize]);
    let response = ReadFileChangeResponse {
        session_id: "session-a".to_owned(),
        request_id: "request-a".to_owned(),
        side: FileChangeSide::After,
        offset: 1,
        total_bytes: 2 + MAX_FILE_CHANGE_READ_BYTES as u64,
        sha256: "00".repeat(32),
        data,
        eof: false,
    };
    let raw = AcpResponseEncoder::new()
        .encode_result(RequestId::Number(1), &response)
        .expect("最大读取页仍应适配默认响应预算");
    assert!(raw.len() <= AcpResponseLimits::default().max_payload_bytes());
}

#[test]
fn file_change_snapshot_descriptors_reject_unknown_fields_and_bad_empty_hash() {
    let valid = json!({
        "sizeBytes": 0,
        "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    });
    assert!(
        serde_json::from_value::<FileSnapshotInfo>(valid.clone())
            .expect("空快照描述应恢复")
            .validate()
            .is_ok()
    );
    let mut bad_hash = valid.clone();
    bad_hash["sha256"] = json!("00".repeat(32));
    assert!(
        serde_json::from_value::<FileSnapshotInfo>(bad_hash)
            .expect("错误摘要形状仍应反序列化")
            .validate()
            .is_err()
    );
    let mut unknown = valid;
    unknown["extra"] = Value::Bool(true);
    assert!(serde_json::from_value::<FileSnapshotInfo>(unknown).is_err());
}
