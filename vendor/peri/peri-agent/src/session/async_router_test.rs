//! Tests for async_router

use super::*;
use peri_acp_types::session::SessionInbox;
use peri_acp_types::tasks::BgTaskKind;
use std::sync::Arc;

fn make_inbox() -> (SessionInbox, InboxHandle) {
    let queue = Arc::new(peri_acp_types::session::MessageQueue::new());
    let inbox = SessionInbox::new(queue);
    let handle = inbox.handle();
    (inbox, handle)
}

fn make_bg_result(task_id: &str, agent_name: &str, output: &str) -> BackgroundTaskResult {
    BackgroundTaskResult {
            agent_path: None,
        task_id: task_id.to_string(),
        agent_name: agent_name.to_string(),
        prompt_summary: "test prompt".to_string(),
        success: true,
        output: output.to_string(),
        tool_calls_count: 3,
        duration_ms: 1500,
        child_thread_id: None,
        timed_out: false,
    }
}

#[test]
fn test_route_bg_result_pushes_defer() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("abc123", "test-agent", "done");

    router.route_bg_result(&result, BgTaskKind::Agent);

    assert_eq!(inbox.queue().len(), 1);
    assert!(inbox.queue().has_wake_up(), "Defer should wake the inbox");
}

#[test]
fn test_route_bg_result_uses_subagent_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("abc123", "test-agent", "done");

    router.route_bg_result(&result, BgTaskKind::Agent);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
}

#[test]
fn test_route_bg_result_shell_uses_shell_complete_source() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("shell-123", "Bash", "done");

    router.route_bg_result(&result, BgTaskKind::Shell);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].source, MessageSource::ShellComplete);
}

#[test]
fn test_route_bg_result_notification_text_contains_task_info() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);
    let result = make_bg_result("task-12345", "my-agent", "output text");

    router.route_bg_result(&result, BgTaskKind::Agent);

    let msgs = inbox.queue().drain_all();
    let text = msgs[0].message.content();
    assert!(text.contains("Message Type: FINAL_ANSWER"), "{text}");
    assert!(text.contains("Sender: /root/my-agent"), "{text}");
    assert!(text.contains("output text"), "should contain output");
}

#[test]
fn test_multiple_routes_accumulate_in_queue() {
    let (inbox, handle) = make_inbox();
    let router = AsyncRouter::new(handle);

    let result1 = make_bg_result("task-1", "agent-a", "output-a");
    let result2 = make_bg_result("task-2", "agent-b", "output-b");
    router.route_bg_result(&result1, BgTaskKind::Agent);
    router.route_bg_result(&result2, BgTaskKind::Agent);

    assert_eq!(inbox.queue().len(), 2);

    let msgs = inbox.queue().drain_all();
    assert_eq!(msgs[0].source, MessageSource::SubAgentComplete);
    assert_eq!(msgs[1].source, MessageSource::SubAgentComplete);
}
