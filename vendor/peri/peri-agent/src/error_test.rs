use super::*;

#[test]
fn test_retryable_http_429() {
    let err = AgentError::LlmHttpError {
        status: 429,
        message: "rate limited".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_retryable_http_503() {
    let err = AgentError::LlmHttpError {
        status: 503,
        message: "unavailable".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_retryable_http_408() {
    let err = AgentError::LlmHttpError {
        status: 408,
        message: "timeout".into(),
    };
    assert!(err.is_retryable());
}

#[test]
fn test_not_retryable_http_400() {
    let err = AgentError::LlmHttpError {
        status: 400,
        message: "bad request".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_http_401() {
    let err = AgentError::LlmHttpError {
        status: 401,
        message: "unauthorized".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_http_404() {
    let err = AgentError::LlmHttpError {
        status: 404,
        message: "not found".into(),
    };
    assert!(!err.is_retryable());
}

#[test]
fn test_retryable_network_connection() {
    let err = AgentError::LlmError("connection refused".into());
    assert!(err.is_retryable());
}

#[test]
fn test_retryable_connection_reset() {
    let err = AgentError::LlmError("connection reset by peer".into());
    assert!(err.is_retryable());
}

#[test]
fn test_not_retryable_connection_pool() {
    let err = AgentError::LlmError("connection pool is full".into());
    assert!(!err.is_retryable(), "connection pool 满不是临时网络错误");
}

#[test]
fn test_retryable_network_timeout() {
    let err = AgentError::LlmError("reqwest timeout exceeded".into());
    assert!(err.is_retryable());
}

#[test]
fn test_not_retryable_parse_error() {
    let err = AgentError::LlmError("parse error".into());
    assert!(!err.is_retryable());
}

#[test]
fn test_not_retryable_other_errors() {
    let err = AgentError::ToolNotFound("x".into());
    assert!(!err.is_retryable());
}
