use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::{stream, StreamExt};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{request, ResponsesConfig, ResponsesModel};
use crate::{
    transport::{HttpBody, HttpRequest, HttpResponse, HttpTransport},
    Model, ModelCallContext, ModelMessage, ModelRequest, ModelRequestMode, ModelResult,
    ModelRuntimeConfig, ProviderProtocol, RequestObservation, RequestObservationScope,
    RequestObservationState, ToolDefinition,
};

struct FakeTransport;

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn send(
        &self,
        _request: HttpRequest,
        cancellation: CancellationToken,
    ) -> ModelResult<HttpResponse> {
        let body: HttpBody = Box::pin(stream::iter(vec![Ok(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n\n"
            )
            .as_bytes()
            .to_vec(),
        )]));
        Ok(HttpResponse::new(
            200,
            Some("header-request-1".to_owned()),
            body,
            cancellation,
        ))
    }
}

/// 创建测试使用的 Responses 协议配置。
fn config() -> ResponsesConfig {
    ResponsesConfig::new(
        Url::parse("https://api.example.com/v1").expect("test url"),
        "test-key",
        "gpt-test",
    )
    .with_reasoning_effort("high")
    .with_max_tokens(4096)
}

/// Responses 请求体必须携带 `stream: true`（中转网关拒绝非流式请求）。
#[test]
fn streaming_flag_is_always_present() {
    let request = ModelRequest::new(vec![ModelMessage::user_text("你好")]);
    let body = request::body_for_test(&config(), &request);
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "gpt-test");
    // 中转网关不支持 max_output_tokens 字段，请求体不得包含它。
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["reasoning"]["effort"], "high");
}

/// System 消息必须落到 `instructions` 字段，而不是 input items。
#[test]
fn system_message_goes_to_instructions() {
    let request = ModelRequest::new(vec![
        ModelMessage::system_text("你是助手"),
        ModelMessage::user_text("你好"),
    ]);
    let body = request::body_for_test(&config(), &request);
    assert_eq!(body["instructions"], "你是助手");
    assert_eq!(body["input"][0]["role"], "user");
    // 单文本块按 Responses 兼容的字符串形式序列化。
    assert_eq!(body["input"][0]["content"], "你好");
}

/// Responses input_image.image_url 必须是 URL 或 data URL 字符串，不能嵌套 url 对象。
#[test]
fn image_url_is_a_string() {
    let request = ModelRequest::new(vec![ModelMessage::User {
        content: vec![
            crate::ContentBlock::text("看图"),
            crate::ContentBlock::Image {
                source: crate::ImageSource::Base64 {
                    media_type: crate::MediaType::new("image/jpeg"),
                    data: "AQID".into(),
                },
            },
        ],
    }]);
    let body = request::body_for_test(&config(), &request);
    assert_eq!(
        body["input"][0]["content"][1],
        json!({
            "type": "input_image",
            "image_url": "data:image/jpeg;base64,AQID",
        })
    );
}

/// 工具定义必须使用 Responses 的扁平 function 结构。
#[test]
fn tools_use_flat_function_shape() {
    let request = ModelRequest::new(vec![ModelMessage::user_text("读取文件")]).with_tools(vec![
        ToolDefinition::new(
            "Read",
            crate::JsonObject::from_value(json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
            }))
            .unwrap(),
        ),
    ]);
    let body = request::body_for_test(&config(), &request);
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["name"], "Read");
    assert!(body["tools"][0].get("function").is_none());
}

/// 裸域名、`/v1` 与完整端点必须统一收敛为 `/v1/responses`。
#[test]
fn builds_responses_endpoint() {
    for base_url in [
        "https://api.example.com",
        "https://api.example.com/v1",
        "https://api.example.com/v1/responses",
    ] {
        let endpoint =
            request::responses_endpoint(&Url::parse(base_url).unwrap()).expect("valid endpoint");
        assert_eq!(endpoint.as_str(), "https://api.example.com/v1/responses");
    }
}

/// 工具调用历史必须回放为 function_call items。
#[test]
fn serializes_tool_call_history() {
    let request = ModelRequest::new(vec![
        ModelMessage::assistant(
            Vec::new(),
            vec![crate::ToolCall::new(
                "call-1",
                "Read",
                crate::JsonObject::from_value(json!({"path": "src/main.rs"})).unwrap(),
            )],
        ),
        ModelMessage::tool_result(crate::ToolResult {
            id: None,
            tool_call_id: "call-1".to_string(),
            name: "Read".to_string(),
            content: vec![crate::ContentBlock::text("fn main() {}")],
            is_error: false,
        }),
    ]);
    let body = request::body_for_test(&config(), &request);
    assert_eq!(body["input"][0]["type"], "function_call");
    assert_eq!(body["input"][0]["name"], "Read");
    assert_eq!(body["input"][1]["type"], "function_call_output");
}

#[tokio::test]
async fn request_observer_reports_responses_logical_and_attempt_completion() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let runtime = ModelRuntimeConfig::default().with_request_observer({
        let observed = Arc::clone(&observed);
        Arc::new(move |observation: RequestObservation| {
            observed.lock().expect("observation lock").push(observation);
        })
    });
    let model =
        ResponsesModel::with_transport(config().with_runtime(runtime), Arc::new(FakeTransport));
    let request = ModelRequest::new(vec![ModelMessage::user_text("go")]).with_call_context(
        ModelCallContext {
            logical_request_id: Some("responses-logical-1".into()),
            session_id: Some("session-1".into()),
            turn_id: Some("turn-1".into()),
            agent_id: Some("agent-1".into()),
            purpose: Some("agent".into()),
        },
    );

    let events = model
        .stream(request, CancellationToken::new())
        .await
        .expect("stream")
        .collect::<Vec<_>>()
        .await;
    assert!(events.iter().all(ModelResult::is_ok));

    let observed = observed.lock().expect("observation lock");
    assert_eq!(
        observed
            .iter()
            .map(|event| (event.scope, event.state, event.attempt))
            .collect::<Vec<_>>(),
        vec![
            (
                RequestObservationScope::Logical,
                RequestObservationState::Started,
                0
            ),
            (
                RequestObservationScope::Attempt,
                RequestObservationState::Started,
                1
            ),
            (
                RequestObservationScope::Attempt,
                RequestObservationState::Completed,
                1
            ),
            (
                RequestObservationScope::Logical,
                RequestObservationState::Completed,
                1
            ),
        ]
    );
    assert!(observed.iter().all(|event| {
        event.protocol
            == ProviderProtocol::Other {
                value: "openai_responses".into(),
            }
            && event.mode == ModelRequestMode::Stream
    }));
    assert_eq!(observed[2].provider_request_id.as_deref(), Some("resp-1"));
    assert_eq!(observed[3].session_id.as_deref(), Some("session-1"));
}
