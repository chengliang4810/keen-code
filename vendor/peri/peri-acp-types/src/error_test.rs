use super::AgentError;

#[test]
fn stream_interruption_does_not_blame_api_configuration() {
    let error = AgentError::LlmError("model stream interrupted from openai-compatible".to_string());

    assert_eq!(
        error.user_facing_message(),
        "The model response stream ended unexpectedly after output had started. Retry the request. If this keeps happening, check your network or switch the provider or model."
    );
}

#[test]
fn generic_llm_failure_does_not_assume_configuration_is_invalid() {
    let error = AgentError::LlmError("model protocol error: invalid event".to_string());

    assert_eq!(
        error.user_facing_message(),
        "The model request failed. Please retry; if this keeps happening, check the provider or model status."
    );
}

#[test]
fn user_facing_codes_are_stable_and_language_neutral() {
    assert_eq!(
        AgentError::LlmError("model stream interrupted from openai-compatible".into())
            .user_facing_code(),
        "model_stream_interrupted"
    );
    assert_eq!(
        AgentError::SerializationError(serde_json::from_str::<serde_json::Value>("{").unwrap_err())
            .user_facing_code(),
        "serialization_error"
    );
    assert_eq!(
        AgentError::MaxIterationsExceeded(10).user_facing_code(),
        "max_iterations_exceeded"
    );
}
