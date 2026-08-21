//! AsyncRouter — unified routing for async results into the Session inbox.
//!
//! L5：自 `peri-acp/src/session/async_router.rs` 物理迁入（仅依赖 peri-acp-types，
//! 干净迁入；ACP 侧保留 re-export 桥）。
//!
//! Replaces the executor's direct push to the raw `v2_message_queue` with a
//! unified path through [`InboxHandle`], so that [`SessionInbox::await_wake`] is
//! properly triggered when the agent is idle.
//!
//! Background task results (`route_bg_result`) from independent agents or
//! background shells are pushed as `Defer` with a source derived from
//! [`BgTaskKind`].
//!
//! Both use `Defer` semantics: consumed by `drain_all` during the Receive stage
//! (RCRA), or detectable by `drain_for_end` for external callers.

use peri_acp_types::event::BackgroundTaskResult;
use peri_acp_types::messages::{BaseMessage, MessageContent};
use peri_acp_types::session::InboxHandle;
use peri_acp_types::session::MessageSource;
use peri_acp_types::tasks::BgTaskKind;
use tracing::debug;

/// Routes async background-task results into the Session inbox.
///
/// Holds an [`InboxHandle`] which wraps the session-shared `MessageQueue` + wake
/// `Notify`. Every route call pushes a `Defer` message and triggers the wake, so
/// that an idle `run_session_loop` resumes via [`SessionInbox::await_wake`].
#[derive(Clone)]
pub struct AsyncRouter {
    inbox: InboxHandle,
}

impl AsyncRouter {
    /// Create a new AsyncRouter from the given inbox handle.
    ///
    /// The handle is typically obtained from `SessionInbox::handle()` during
    /// session initialization.
    pub fn new(inbox: InboxHandle) -> Self {
        Self { inbox }
    }

    /// Route a background task result into the session inbox.
    ///
    /// Converts the [`BackgroundTaskResult`] into a notification string via
    /// [`BackgroundTaskResult::to_notification`] and pushes it as a `Defer`
    /// message. Independent agents use `MessageSource::SubAgentComplete`; shells
    /// use `MessageSource::ShellComplete`.
    ///
    /// This replaces the executor's direct `v2_message_queue.push(QueuedMessage::new(
    /// Defer, ..., human(result.to_notification())))` — the only difference is
    /// that this path also triggers the inbox wake `Notify`.
    pub fn route_bg_result(&self, result: &BackgroundTaskResult, kind: BgTaskKind) {
        tracing::info!(
            task_id = %result.task_id,
            agent_name = %result.agent_name,
            success = result.success,
            output_len = result.output.len(),
            "[bg-diag] route_bg_result: calling push_defer"
        );
        let msg = BaseMessage::human(MessageContent::text(result.to_notification()));
        let source = match kind {
            BgTaskKind::Agent => MessageSource::SubAgentComplete,
            BgTaskKind::Shell => MessageSource::ShellComplete,
        };
        self.inbox.push_defer(source, msg);
        debug!(
            task_id = %result.task_id,
            agent_name = %result.agent_name,
            success = result.success,
            "AsyncRouter: routed bg SubAgent result to inbox"
        );
    }
}

impl std::fmt::Debug for AsyncRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncRouter").finish()
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "async_router_test.rs"]
mod tests;
