use futures_executor::block_on;
use serde_json::{Value, json};

use crate::{
    ContentBlock, Message, MessageRole, ModelError, ModelProvider, ModelRequest, ModelStreamEvent,
    OpaqueReasoningState, ProviderCapabilities, ProviderProtocol, ReasoningContent,
    ResponseMetadata, ScriptedProvider, ScriptedReply, StopReason, StructuredOutputConfig,
    StructuredOutputEnforcement, StructuredOutputFailureKind, TokenUsage, ToolCall, ToolChoice,
    ToolDefinition, ToolResult,
};

fn user_request() -> ModelRequest {
    ModelRequest::new(
        "test-model",
        vec![Message::text(MessageRole::User, "读取项目说明")],
    )
}

fn message_start() -> ModelStreamEvent {
    ModelStreamEvent::MessageStart {
        metadata: ResponseMetadata {
            response_id: Some("response-1".to_owned()),
            model: Some("test-model".to_owned()),
        },
    }
}

fn structured_response(text: &str, stop_reason: StopReason) -> crate::ModelResponse {
    crate::ModelResponse::new(
        ResponseMetadata {
            response_id: Some("structured-1".to_owned()),
            model: Some("test-model".to_owned()),
        },
        vec![ContentBlock::text(text)],
        TokenUsage::unknown(),
        stop_reason,
    )
}

#[test]
fn provider_protocol_serialization_is_stable() {
    assert_eq!(
        serde_json::to_value(ProviderProtocol::Messages).unwrap(),
        json!("messages")
    );
    assert_eq!(
        serde_json::to_value(ProviderProtocol::ChatCompletions).unwrap(),
        json!("chat_completions")
    );
    assert_eq!(
        serde_json::to_value(ProviderProtocol::Responses).unwrap(),
        json!("responses")
    );
}

#[test]
fn message_and_tool_call_round_trip_without_protocol_fields() {
    let message = Message::new(
        MessageRole::Assistant,
        vec![
            ContentBlock::text("准备读取"),
            ContentBlock::ToolCall {
                tool_call: ToolCall::new("call-1", "Read", json!({ "path": "README.md" })),
            },
        ],
    );

    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(value["role"], json!("assistant"));
    assert_eq!(value["content"][1]["type"], json!("tool_call"));
    assert_eq!(value["content"][1]["tool_call"]["id"], json!("call-1"));
    assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);
}

#[test]
fn missing_usage_and_explicit_zero_are_distinct() {
    let unknown = TokenUsage::unknown();
    let explicit_zero = TokenUsage {
        input_tokens: Some(0),
        ..TokenUsage::unknown()
    };

    assert_ne!(unknown, explicit_zero);
    assert!(!unknown.is_reported());
    assert!(explicit_zero.is_reported());
    assert_eq!(
        serde_json::to_value(&unknown).unwrap()["inputTokens"],
        serde_json::Value::Null
    );
    assert_eq!(
        serde_json::to_value(&explicit_zero).unwrap()["inputTokens"],
        json!(0)
    );
}

#[test]
fn partial_usage_snapshots_preserve_previous_reported_values() {
    let mut usage = TokenUsage {
        input_tokens: Some(12),
        cache_read_tokens: Some(0),
        ..TokenUsage::unknown()
    };
    usage.update_from(&TokenUsage {
        output_tokens: Some(7),
        cache_read_tokens: None,
        ..TokenUsage::unknown()
    });

    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.cache_read_tokens, Some(0));
}

#[test]
fn request_invariants_reject_duplicate_and_unknown_tools() {
    let definition = ToolDefinition::new(
        "Read",
        "读取文本文件",
        json!({ "type": "object", "properties": {} }),
    );
    let mut duplicate = user_request();
    duplicate.tools = vec![definition.clone(), definition.clone()];
    assert!(matches!(
        duplicate.validate(),
        Err(ModelError::InvalidRequest { .. })
    ));

    let mut unknown_choice = user_request();
    unknown_choice.tools = vec![definition];
    unknown_choice.tool_choice = ToolChoice::Specific {
        name: "Write".to_owned(),
    };
    assert!(matches!(
        unknown_choice.validate(),
        Err(ModelError::InvalidRequest { .. })
    ));
}

#[test]
fn tool_definition_validates_schema_and_each_input_before_execution() {
    let definition = ToolDefinition::new(
        "Read",
        "读取文本文件",
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1 }
            },
            "required": ["file_path"],
            "additionalProperties": false
        }),
    );

    definition.validate().unwrap();
    definition
        .validate_input(&json!({"file_path": "README.md", "limit": 20}))
        .unwrap();
    assert!(matches!(
        definition.validate_input(&json!({"file_path": "", "unexpected": true})),
        Err(ModelError::InvalidRequest { .. })
    ));

    let unsupported = ToolDefinition::new(
        "Unsupported",
        "拒绝运行时没有实现的 Schema 规则",
        json!({"type": "object", "$ref": "#/$defs/input"}),
    );
    assert!(matches!(
        unsupported.validate(),
        Err(ModelError::InvalidRequest { .. })
    ));
}

#[test]
fn tool_names_are_portable_across_all_provider_protocols() {
    let schema = json!({ "type": "object", "properties": {} });
    for name in ["Read", "mcp__server-1__lookup", &"a".repeat(64)] {
        ToolDefinition::new(name, "可移植工具", schema.clone())
            .validate()
            .unwrap();
        ToolCall::new("call-1", name, json!({})).validate().unwrap();
    }

    for name in ["", "包含空格", "server.tool", &"a".repeat(65)] {
        assert!(matches!(
            ToolDefinition::new(name, "非法工具", schema.clone()).validate(),
            Err(ModelError::InvalidRequest { .. })
        ));
        assert!(matches!(
            ToolCall::new("call-1", name, json!({})).validate(),
            Err(ModelError::Protocol { .. })
        ));
    }
}

#[test]
fn request_invariants_reject_non_object_tool_arguments() {
    let tool_call = ToolCall::new("call-1", "Read", json!(["README.md"]));
    assert!(matches!(
        tool_call.validate(),
        Err(ModelError::Protocol { .. })
    ));
}

#[test]
fn message_roles_reject_cross_role_content() {
    let assistant_result = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::ToolResult {
            tool_result: ToolResult::text("call-1", "完成", false),
        }],
    );
    assert!(matches!(
        assistant_result.validate(),
        Err(ModelError::InvalidRequest { .. })
    ));

    let tool_result = Message::new(
        MessageRole::Tool,
        vec![ContentBlock::ToolResult {
            tool_result: ToolResult::text("call-1", "完成", false),
        }],
    );
    assert!(tool_result.validate().is_ok());
}

/// 文本内容只拒绝精确空串，不能把有意保留的空白改写为无内容。
#[test]
fn message_validation_rejects_empty_text_but_preserves_whitespace() {
    assert!(matches!(
        Message::text(MessageRole::User, "").validate(),
        Err(ModelError::InvalidRequest { .. })
    ));
    Message::text(MessageRole::User, " ")
        .validate()
        .expect("有意保留的空白文本应保持有效");
}

/// 推理必须具有有效载荷，显式空摘要不可伪装为缺失摘要。
#[test]
fn reasoning_validation_rejects_empty_payload_and_present_empty_summary() {
    let empty = ReasoningContent {
        text: String::new(),
        summary: None,
        continuation: None,
    };
    assert!(matches!(empty.validate(), Err(ModelError::Protocol { .. })));

    let empty_summary = ReasoningContent {
        text: "仍有推理正文".to_owned(),
        summary: Some(String::new()),
        continuation: None,
    };
    assert!(matches!(
        empty_summary.validate(),
        Err(ModelError::Protocol { .. })
    ));
}

/// 只有不透明续传状态的推理是厂商协议需要保留的合法形态。
#[test]
fn reasoning_validation_accepts_continuation_only_payload() {
    ReasoningContent {
        text: String::new(),
        summary: None,
        continuation: Some(OpaqueReasoningState::new(
            "adapter-state-v1",
            json!({ "encrypted": "synthetic-test-state" }),
        )),
    }
    .validate()
    .expect("仅含续传状态的推理应保持有效");
}

#[test]
fn full_request_serialization_round_trip_preserves_neutral_configuration() {
    let mut request = user_request();
    request.tools = vec![ToolDefinition::new(
        "Read",
        "读取文本文件",
        json!({ "type": "object", "properties": {} }),
    )];
    request.tool_choice = ToolChoice::Specific {
        name: "Read".to_owned(),
    };
    request.parallel_tool_calls = Some(false);
    request.structured_output = Some(StructuredOutputConfig::new(
        "result",
        json!({ "type": "object", "properties": { "summary": { "type": "string" } } }),
    ));
    request.max_output_tokens = Some(256);

    request.validate().unwrap();
    let serialized = serde_json::to_value(&request).unwrap();
    assert_eq!(serialized["parallelToolCalls"], json!(false));
    assert_eq!(serialized["structuredOutput"]["name"], json!("result"));
    assert_eq!(
        serde_json::from_value::<ModelRequest>(serialized).unwrap(),
        request
    );
}

#[test]
fn structured_schema_rejects_unknown_keywords_before_provider_call() {
    let config = StructuredOutputConfig::new(
        "result",
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "$ref": "#/$defs/result"
        }),
    );

    assert!(matches!(
        config.validate(),
        Err(ModelError::InvalidRequest { message })
            if message.contains("不支持的关键字 $ref")
    ));
}

#[test]
fn structured_value_validates_nested_supported_schema_subset() {
    let config = StructuredOutputConfig::new(
        "result",
        json!({
            "type": "object",
            "properties": {
                "status": {"enum": ["ok", "failed"]},
                "score": {"type": "number", "minimum": 0, "maximum": 1},
                "tags": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 2,
                    "items": {"type": "string", "minLength": 2, "maxLength": 8}
                }
            },
            "required": ["status", "score", "tags"],
            "additionalProperties": false
        }),
    );

    config
        .validate_value(
            &json!({"status":"ok", "score":0.5, "tags":["rs"]}),
            StructuredOutputEnforcement::Native,
        )
        .expect("有效结构化结果应通过校验");
    let error = config
        .validate_value(
            &json!({"status":"ok", "score":2, "tags":["x"], "extra":true}),
            StructuredOutputEnforcement::Native,
        )
        .expect_err("越界结果必须失败");
    assert!(matches!(
        error,
        ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::SchemaViolation,
            ..
        }
    ));
}

#[test]
fn structured_value_supports_union_and_composition_rules() {
    let config = StructuredOutputConfig::new(
        "result",
        json!({
            "allOf": [
                {"type": ["string", "null"]},
                {"anyOf": [
                    {"const": null},
                    {"type": "string", "minLength": 2}
                ]}
            ]
        }),
    );

    for value in [json!(null), json!("ok")] {
        config
            .validate_value(&value, StructuredOutputEnforcement::ToolEmulated)
            .expect("联合类型有效值应通过校验");
    }
    assert!(
        config
            .validate_value(&json!("x"), StructuredOutputEnforcement::ToolEmulated)
            .is_err()
    );

    let exclusive = StructuredOutputConfig::new(
        "exclusive",
        json!({"oneOf": [{"type":"number"}, {"const": 1}]}),
    );
    assert!(
        exclusive
            .validate_value(&json!(1), StructuredOutputEnforcement::ToolEmulated)
            .is_err()
    );
}

#[test]
fn structured_response_requires_one_complete_json_value() {
    let config = StructuredOutputConfig::new(
        "result",
        json!({
            "type": "object",
            "properties": {"ok": {"const": true}},
            "required": ["ok"],
            "additionalProperties": false
        }),
    );
    let parsed = config
        .parse_response(
            &structured_response("{\"ok\":true}", StopReason::Completed),
            StructuredOutputEnforcement::Native,
        )
        .expect("唯一 JSON 值应通过");
    assert_eq!(parsed, json!({"ok": true}));

    let invalid = config
        .parse_response(
            &structured_response("```json\n{\"ok\":true}\n```", StopReason::Completed),
            StructuredOutputEnforcement::Native,
        )
        .expect_err("Markdown 包装不能被静默剥离");
    assert!(matches!(
        invalid,
        ModelError::StructuredOutput {
            enforcement: StructuredOutputEnforcement::Native,
            failure: StructuredOutputFailureKind::InvalidJson,
            ..
        }
    ));

    let incomplete = config
        .parse_response(
            &structured_response("{\"ok\":true}", StopReason::MaxOutputTokens),
            StructuredOutputEnforcement::Native,
        )
        .expect_err("截断响应不能成为结构化成功结果");
    assert!(matches!(
        incomplete,
        ModelError::StructuredOutput {
            failure: StructuredOutputFailureKind::Incomplete,
            ..
        }
    ));
}

#[test]
fn structured_numbers_use_json_schema_mathematical_equality() {
    let constant = StructuredOutputConfig::new("constant", json!({"const": 1}));
    let exponential = serde_json::from_str::<Value>("1e0").unwrap();
    for value in [json!(1), json!(1.0), exponential] {
        constant
            .validate_value(&value, StructuredOutputEnforcement::Native)
            .expect("数学意义相同的 JSON 数值应满足 const");
    }

    let enumeration = StructuredOutputConfig::new("enumeration", json!({"enum": [1]}));
    enumeration
        .validate_value(&json!(1.0), StructuredOutputEnforcement::Native)
        .expect("数学意义相同的 JSON 数值应满足 enum");

    let duplicate = StructuredOutputConfig::new("duplicate", json!({"enum": [1, 1.0]}));
    assert!(matches!(
        duplicate.validate(),
        Err(ModelError::InvalidRequest { message }) if message.contains("enum 包含重复值")
    ));
}

#[test]
fn structured_number_boundaries_do_not_lose_integer_precision() {
    let maximum = serde_json::from_str::<Value>("9007199254740992.0").unwrap();
    let config = StructuredOutputConfig::new("boundary", json!({"maximum": maximum}));
    config
        .validate_value(
            &json!(9_007_199_254_740_992_u64),
            StructuredOutputEnforcement::Native,
        )
        .expect("与浮点写法相等的 2^53 应通过精确边界校验");
    let error = config
        .validate_value(
            &json!(9_007_199_254_740_993_u64),
            StructuredOutputEnforcement::Native,
        )
        .expect_err("2^53 加一不能被 f64 舍入后误判为相等");
    assert!(matches!(
        error,
        ModelError::StructuredOutput { message, .. } if message.contains("大于 maximum")
    ));
}

#[test]
fn structured_large_enum_detects_semantic_duplicate_in_linear_pass() {
    let mut values = (0_u64..5_000).map(Value::from).collect::<Vec<_>>();
    values.push(json!(4_999.0));
    let config = StructuredOutputConfig::new("large-enum", json!({"enum": values}));

    assert!(matches!(
        config.validate(),
        Err(ModelError::InvalidRequest { message }) if message.contains("enum 包含重复值")
    ));
}

#[test]
fn structured_schema_budget_counts_annotation_payloads() {
    let oversized_default = "x".repeat(4 * 1024 * 1024 + 1);
    let config = StructuredOutputConfig::new(
        "oversized-schema",
        json!({"type": "string", "default": oversized_default}),
    );

    assert!(matches!(
        config.validate(),
        Err(ModelError::InvalidRequest { message }) if message.contains("字节上限")
    ));
}

#[test]
fn structured_response_budget_rejects_oversized_text_before_parsing() {
    let oversized_text = "x".repeat(8 * 1024 * 1024 + 1);
    let config = StructuredOutputConfig::new("oversized-output", json!({"type": "string"}));
    let error = config
        .parse_response(
            &structured_response(&oversized_text, StopReason::Completed),
            StructuredOutputEnforcement::Native,
        )
        .expect_err("超大结构化响应必须在 JSON 解析前被拒绝");

    assert!(matches!(
        error,
        ModelError::StructuredOutput { message, .. } if message.contains("字节上限")
    ));
}

#[test]
fn structured_one_of_propagates_budget_exhaustion_after_a_match() {
    let config = StructuredOutputConfig::new(
        "one-of-budget",
        json!({
            "oneOf": [
                {"type": "array"},
                {"type": "array", "items": {"type": "number"}}
            ]
        }),
    );
    let instance = Value::Array(vec![json!(0); 40_000]);
    let error = config
        .validate_value(&instance, StructuredOutputEnforcement::Native)
        .expect_err("后续 oneOf 分支耗尽预算时不能保留前一分支的成功结果");

    assert!(matches!(
        error,
        ModelError::StructuredOutput { message, .. } if message.contains("预算耗尽")
    ));
}

#[test]
fn structured_any_of_propagates_budget_exhaustion_without_trying_fallback() {
    let config = StructuredOutputConfig::new(
        "any-of-budget",
        json!({
            "anyOf": [
                {
                    "allOf": [
                        {"type": "array", "items": {"type": "number"}},
                        {"type": "string"}
                    ]
                },
                {"type": "array"}
            ]
        }),
    );
    let instance = Value::Array(vec![json!(0); 40_000]);
    let error = config
        .validate_value(&instance, StructuredOutputEnforcement::Native)
        .expect_err("anyOf 分支的预算耗尽不能被当成普通语义不匹配");

    assert!(matches!(
        error,
        ModelError::StructuredOutput { message, .. }
            if message.contains("预算耗尽") && !message.contains("不满足任何 anyOf")
    ));
}

#[test]
fn structured_strict_false_still_runs_local_schema_validation() {
    let mut config = StructuredOutputConfig::new("local-strict", json!({"const": true}));
    config.strict = false;

    assert!(matches!(
        config.validate_value(&json!(false), StructuredOutputEnforcement::Native),
        Err(ModelError::StructuredOutput {
            failure: StructuredOutputFailureKind::SchemaViolation,
            ..
        })
    ));
}

#[test]
fn scripted_provider_is_object_safe_and_collects_tool_calls() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            tool_calling: true,
            ..ProviderCapabilities::default()
        },
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: "先读取文件".to_owned(),
            },
            ModelStreamEvent::ToolCallStart {
                index: 1,
                id: "call-1".to_owned(),
                name: "Read".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 1,
                id: "call-1".to_owned(),
                delta: "{\"path\":".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 1,
                id: "call-1".to_owned(),
                delta: "\"README.md\"}".to_owned(),
            },
            ModelStreamEvent::ToolCallEnd {
                index: 1,
                id: "call-1".to_owned(),
            },
            ModelStreamEvent::Usage {
                usage: TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(4),
                    ..TokenUsage::unknown()
                },
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ])],
    );
    let provider_object: &dyn ModelProvider = &provider;
    let request = user_request();

    let response = block_on(provider_object.complete(request.clone())).unwrap();

    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert_eq!(response.usage.input_tokens, Some(10));
    assert_eq!(response.content.len(), 2);
    assert_eq!(
        response.content[1],
        ContentBlock::ToolCall {
            tool_call: ToolCall::new("call-1", "Read", json!({ "path": "README.md" })),
        }
    );
    assert_eq!(provider.requests().unwrap(), vec![request]);
    assert_eq!(provider.remaining_replies().unwrap(), 0);
}

#[test]
fn scripted_provider_propagates_mid_stream_error() {
    let expected = ModelError::Transport {
        message: "连接中断".to_owned(),
        retryable: true,
    };
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::new(vec![
            Ok(message_start()),
            Err(expected.clone()),
        ])],
    );

    let error = block_on(provider.complete(user_request())).unwrap_err();
    assert_eq!(error, expected);
    assert!(error.is_retryable());
}

#[test]
fn collector_rejects_events_after_end() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: "迟到内容".to_owned(),
            },
        ])],
    );

    assert!(matches!(
        block_on(provider.complete(user_request())),
        Err(ModelError::Protocol { .. })
    ));
}

#[test]
fn collector_rejects_incomplete_tool_json() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_owned(),
                name: "Read".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 0,
                id: "call-1".to_owned(),
                delta: "{".to_owned(),
            },
            ModelStreamEvent::ToolCallEnd {
                index: 0,
                id: "call-1".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ])],
    );

    assert!(matches!(
        block_on(provider.complete(user_request())),
        Err(ModelError::Protocol { .. })
    ));
}

/// 验证不同内容块不能复用同一个工具调用标识，避免工具结果路由到错误调用。
#[test]
fn collector_rejects_duplicate_tool_call_ids() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::ToolCallStart {
                index: 0,
                id: "call-1".to_owned(),
                name: "Read".to_owned(),
            },
            ModelStreamEvent::ToolCallArgumentsDelta {
                index: 0,
                id: "call-1".to_owned(),
                delta: "{}".to_owned(),
            },
            ModelStreamEvent::ToolCallEnd {
                index: 0,
                id: "call-1".to_owned(),
            },
            ModelStreamEvent::ToolCallStart {
                index: 1,
                id: "call-1".to_owned(),
                name: "Write".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::ToolUse,
            },
        ])],
    );

    let error =
        block_on(provider.complete(user_request())).expect_err("重复工具调用标识不能形成完整响应");
    assert!(error.message().contains("工具调用标识 call-1 在响应中重复"));
}

#[test]
fn collector_keeps_reasoning_text_and_summary_separate() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::ReasoningSummaryDelta {
                index: 0,
                delta: "先检查边界".to_owned(),
            },
            ModelStreamEvent::ReasoningDelta {
                index: 0,
                delta: "详细推理".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    );

    let response = block_on(provider.complete(user_request())).unwrap();
    assert_eq!(
        response.content,
        vec![ContentBlock::Reasoning {
            reasoning: ReasoningContent {
                text: "详细推理".to_owned(),
                summary: Some("先检查边界".to_owned()),
                continuation: None,
            },
        }]
    );
}

/// 收集器不能把 Provider 的空文本内容提交为有效最终响应。
#[test]
fn collector_rejects_empty_text_content() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: String::new(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    );

    assert!(matches!(
        block_on(provider.complete(user_request())),
        Err(ModelError::Protocol { .. })
    ));
}

/// 收集器不能接受文本、摘要和续传状态都缺失的推理块。
#[test]
fn collector_rejects_empty_reasoning_content() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::ReasoningDelta {
                index: 0,
                delta: String::new(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    );

    assert!(matches!(
        block_on(provider.complete(user_request())),
        Err(ModelError::Protocol { .. })
    ));
}

/// 收集器保留摘要出现语义，以便拒绝 Provider 显式返回的空摘要。
#[test]
fn collector_rejects_present_empty_reasoning_summary() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::ReasoningDelta {
                index: 0,
                delta: "推理正文".to_owned(),
            },
            ModelStreamEvent::ReasoningSummaryDelta {
                index: 0,
                delta: String::new(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    );

    assert!(matches!(
        block_on(provider.complete(user_request())),
        Err(ModelError::Protocol { .. })
    ));
}

/// 验证统一收集器继续拒绝同一内容序号混用推理和可见文本。
#[test]
fn collector_rejects_text_and_reasoning_on_same_index() {
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::ReasoningDelta {
                index: 0,
                delta: "推理".to_owned(),
            },
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: "正文".to_owned(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    );

    assert!(matches!(
        block_on(provider.complete(user_request())),
        Err(ModelError::Protocol { .. })
    ));
}

#[test]
fn collector_preserves_opaque_reasoning_continuation() {
    let continuation = OpaqueReasoningState::new(
        "adapter-state-v1",
        json!({ "encrypted": "synthetic-test-state" }),
    );
    let provider = ScriptedProvider::new(
        ProviderCapabilities::default(),
        [ScriptedReply::events([
            message_start(),
            ModelStreamEvent::ReasoningContinuation {
                index: 0,
                continuation: continuation.clone(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    );

    let response = block_on(provider.complete(user_request())).unwrap();
    assert_eq!(
        response.content,
        vec![ContentBlock::Reasoning {
            reasoning: ReasoningContent {
                text: String::new(),
                summary: None,
                continuation: Some(continuation),
            },
        }]
    );
}
