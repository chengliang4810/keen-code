//! 会话标题请求与候选净化测试。

use serde_json::json;

use super::{
    build_session_title_messages, build_session_title_response, normalize_session_title_candidate,
    parse_session_title_request, SessionTitleRequest, TITLE_INPUT_MAX_CHARS,
};

#[test]
fn parse_session_title_request_读取完整首轮() {
    let request = parse_session_title_request(&json!({
        "sessionId": "session-1",
        "userMessage": " 你好啊，你是谁。 ",
        "assistantMessage": " 我是 KeenCode。 ",
    }))
    .expect("标题请求应解析成功");
    assert_eq!(request.session_id, "session-1");
    assert_eq!(request.user_message, "你好啊，你是谁。");
    assert_eq!(request.assistant_message, "我是 KeenCode。");
}

#[test]
fn parse_session_title_request_拒绝空消息() {
    let error = parse_session_title_request(&json!({
        "sessionId": "session-1",
        "userMessage": "你好",
        "assistantMessage": "   ",
    }))
    .expect_err("空 Assistant 回复必须被拒绝");
    assert_eq!(error.code, -32602);
    assert!(error.message.contains("assistantMessage"));
}

#[test]
fn parse_session_title_request_拒绝空会话标识() {
    let error = parse_session_title_request(&json!({
        "sessionId": "  ",
        "userMessage": "你好",
        "assistantMessage": "你好，有什么可以帮你？",
    }))
    .expect_err("空 Session 标识必须被拒绝");
    assert_eq!(error.code, -32602);
    assert_eq!(error.message, "missing sessionId");
}

#[test]
fn build_session_title_messages_隔离并裁剪输入() {
    let request = SessionTitleRequest {
        session_id: "session-1".to_string(),
        user_message: "用一句话说明需求".to_string(),
        assistant_message: "答".repeat(TITLE_INPUT_MAX_CHARS + 20),
    };
    let messages = build_session_title_messages(&request);
    assert_eq!(messages.len(), 2);
    assert!(messages[0].content().contains("Output exactly one title"));
    assert_eq!(
        messages[1].content().matches('答').count(),
        TITLE_INPUT_MAX_CHARS
    );
}

#[test]
fn normalize_session_title_candidate_只保留首个非空行() {
    assert_eq!(
        normalize_session_title_candidate("\n  询问助手身份  \n额外解释"),
        "询问助手身份"
    );
    assert!(normalize_session_title_candidate(" \n\t ").is_empty());
}

#[test]
fn build_session_title_response_拒绝空标题() {
    let request = SessionTitleRequest {
        session_id: "session-empty-title".to_string(),
        user_message: "生成标题".to_string(),
        assistant_message: "好的".to_string(),
    };
    let error =
        build_session_title_response(request, "  \n\t").expect_err("模型返回空标题时必须显式失败");
    assert_eq!(error.code, -32603);
    assert_eq!(
        error.message,
        "session title generation returned empty text"
    );
}
