use super::*;
use rcode_agent::{AgentId, PlanGuard, SessionId, TurnId};
use rcode_model::{
    Message, MessageRole, ModelStreamEvent, ProviderCapabilities, ResponseMetadata,
    ScriptedProvider, ScriptedReply, StopReason,
};
use serde_json::json;
use std::sync::Mutex;

#[test]
fn lease_enforces_session_exclusion_and_cleans_pending_client_requests() {
    let runtime = AgentRuntime::default();
    let lease = runtime.register("run-1", "session-1").unwrap();
    assert!(runtime.register("run-2", "session-1").is_err());
    assert!(runtime.register("run-1", "session-2").is_err());
    let token = lease.cancellation.clone();
    let (id, receiver) = runtime.request_client_tool("run-1", "call-1").unwrap();
    assert!(runtime.request_client_tool("run-1", "call-1").is_err());
    drop(lease);
    assert!(token.is_cancelled());
    assert!(receiver.blocking_recv().is_err());
    assert!(runtime.respond_to_tool(&id, json!(true)).is_err());
    assert!(runtime.register("run-2", "session-1").is_ok());
    assert!(runtime.register("", "session-1").is_err());
}

#[test]
fn oversized_client_response_preserves_request_and_valid_response_is_consumed_once() {
    let runtime = AgentRuntime::default();
    let _lease = runtime.register("run-1", "session-1").unwrap();
    let (id, receiver) = runtime.request_client_tool("run-1", "call-1").unwrap();
    assert!(runtime
        .respond_to_tool(&id, json!("x".repeat(1024 * 1024)))
        .is_err());
    runtime.respond_to_tool(&id, json!({"ok":true})).unwrap();
    assert!(runtime.respond_to_tool(&id, json!(false)).is_err());
    assert_eq!(receiver.blocking_recv().unwrap(), json!({"ok":true}));
}

#[tokio::test]
async fn runtime_runs_and_commits_without_desktop_and_rejects_foreign_leases() {
    let runtime = AgentRuntime::default();
    let lease = runtime
        .register("headless-run", "headless-session")
        .unwrap();
    let provider = Arc::new(ScriptedProvider::new(
        ProviderCapabilities {
            streaming: true,
            ..Default::default()
        },
        [ScriptedReply::events([
            ModelStreamEvent::MessageStart {
                metadata: ResponseMetadata::default(),
            },
            ModelStreamEvent::TextDelta {
                index: 0,
                delta: "headless complete".into(),
            },
            ModelStreamEvent::MessageEnd {
                stop_reason: StopReason::Completed,
            },
        ])],
    ));
    let received = Arc::new(Mutex::new(Vec::new()));
    let output = received.clone();
    let events = EventBridge::new(move |event| {
        output.lock().unwrap().push(event);
        Ok(())
    });
    let request = || {
        TurnRequest::new(
            SessionId::new("headless-session").unwrap(),
            TurnId::new("headless-run").unwrap(),
            AgentId::new("main").unwrap(),
            "fixture",
            vec![Message::text(MessageRole::User, "complete")],
            PlanGuard::inactive(),
        )
    };
    assert!(AgentRuntime::default()
        .run_turn(
            &lease,
            provider.clone(),
            ToolRegistry::new(),
            request(),
            events.clone(),
            PermissionMode::Ask,
        )
        .await
        .is_err());
    assert!(provider.requests().unwrap().is_empty());
    let result = runtime
        .run_turn(
            &lease,
            provider.clone(),
            ToolRegistry::new(),
            request(),
            events,
            PermissionMode::Ask,
        )
        .await
        .unwrap();
    assert!(result.error.is_none());
    let received = received.lock().unwrap();
    assert!(received
        .iter()
        .any(|event| event["event"]["delta"] == "headless complete"));
    assert_eq!(
        received
            .iter()
            .filter(|event| event["type"] == "transcript")
            .count(),
        1
    );
    assert_eq!(provider.requests().unwrap().len(), 1);
}

#[test]
fn shutdown_cancels_runs_and_drops_pending_client_responses() {
    let runtime = AgentRuntime::default();
    let lease = runtime.register("run", "session").unwrap();
    let (_, receiver) = runtime.request_client_tool("run", "call").unwrap();
    runtime.shutdown();
    assert!(lease.cancellation.is_cancelled());
    assert!(receiver.blocking_recv().is_err());
}
