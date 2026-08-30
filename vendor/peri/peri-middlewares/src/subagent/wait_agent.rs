use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::session::{MessageSource, SessionInbox};
use peri_agent::agent::async_tasks::TaskManager;
use peri_agent::agent::events::BackgroundTaskResult;
use peri_agent::messages::{BaseMessage, MessageContent};
use peri_agent::tools::BaseTool;
use serde_json::json;
use tokio_util::sync::CancellationToken;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MIN_TIMEOUT_MS: u64 = 10_000;
const MAX_TIMEOUT_MS: u64 = 3_600_000;
const WAIT_AGENT_DESCRIPTION: &str = include_str!("descriptions/wait_agent.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitOutcome {
    AgentStateChanged,
    UserInput,
    TurnCancelled,
    TimedOut,
    NoRunningAgents,
}

struct WaitResult {
    outcome: WaitOutcome,
    running_agents: Vec<(String, String)>,
    /// 挂起期间完成的 Agent 结果(排空即视为已交付)。
    harvested: Vec<BackgroundTaskResult>,
}

struct IdleSuspendedGuard {
    flag: Arc<AtomicBool>,
    previous: bool,
}

impl IdleSuspendedGuard {
    fn new(flag: Arc<AtomicBool>) -> Self {
        let previous = flag.swap(true, Ordering::AcqRel);
        Self { flag, previous }
    }
}

impl Drop for IdleSuspendedGuard {
    fn drop(&mut self) {
        self.flag.store(self.previous, Ordering::Release);
    }
}

/// 显式等待后台 Agent 状态变化；Shell 生命周期不参与唤醒。
pub struct WaitAgentTool {
    task_manager: Arc<TaskManager>,
    inbox: Arc<SessionInbox>,
    idle_suspended: Arc<AtomicBool>,
    turn_cancel: Arc<CancellationToken>,
}

impl WaitAgentTool {
    pub fn new(
        task_manager: Arc<TaskManager>,
        inbox: Arc<SessionInbox>,
        idle_suspended: Arc<AtomicBool>,
        turn_cancel: Arc<CancellationToken>,
    ) -> Self {
        Self {
            task_manager,
            inbox,
            idle_suspended,
            turn_cancel,
        }
    }

    async fn wait(&self, timeout: Duration) -> WaitResult {
        // 先暴露挂起状态，避免用户输入落在快照检查与 select 之间而阻塞于 prompt lock。
        let _idle_guard = IdleSuspendedGuard::new(Arc::clone(&self.idle_suspended));
        // 必须先订阅再读快照：完成发生在两步之间时，has_changed 仍能识别。
        let mut changes = self.task_manager.subscribe_agent_changes();
        let running_agents = self.task_manager.running_agent_tasks();
        if changes.has_changed().unwrap_or(true) {
            drop(_idle_guard);
            return self.finish(WaitOutcome::AgentStateChanged);
        }
        if running_agents.is_empty() {
            drop(_idle_guard);
            return self.finish(WaitOutcome::NoRunningAgents);
        }

        let outcome = tokio::select! {
            biased;
            _ = self.turn_cancel.cancelled() => WaitOutcome::TurnCancelled,
            _ = self.inbox.await_prompt() => WaitOutcome::UserInput,
            _ = changes.changed() => WaitOutcome::AgentStateChanged,
            _ = tokio::time::sleep(timeout) => WaitOutcome::TimedOut,
        };
        drop(_idle_guard);
        self.finish(outcome)
    }

    /// 结束挂起后收割期间累积的完成结果。完成回调读取挂起状态与写入 harvest
    /// 时持有同一把队列锁,因此 guard 释放后的 drain 不存在结果滞留窗口。
    /// 非收割交付结局（用户输入抢占/超时/取消）下，排空的结果按既有 Defer
    /// 路径补投递，保证完成通知不因等待退出而丢失。
    fn finish(&self, outcome: WaitOutcome) -> WaitResult {
        let harvested = self.task_manager.drain_agent_harvest();
        if outcome != WaitOutcome::AgentStateChanged {
            for r in &harvested {
                let msg = BaseMessage::human(MessageContent::text(r.to_notification()));
                self.inbox.handle().push_defer(MessageSource::SubAgentComplete, msg);
            }
        }
        WaitResult {
            outcome,
            running_agents: self.task_manager.running_agent_tasks(),
            harvested,
        }
    }

    fn render(result: WaitResult) -> String {
        let (outcome, message) = match result.outcome {
            WaitOutcome::AgentStateChanged => (
                "agent_state_changed",
                "An Agent task changed state. Continue with available AgentResult messages or wait again if needed.",
            ),
            WaitOutcome::UserInput => (
                "user_input",
                "The user sent new input. Return to Receive and handle it before waiting again.",
            ),
            WaitOutcome::TurnCancelled => (
                "turn_cancelled",
                "The current main-agent turn was cancelled.",
            ),
            WaitOutcome::TimedOut => (
                "timeout",
                "No Agent state changed before the timeout. Continue independent work or call WaitAgent again.",
            ),
            WaitOutcome::NoRunningAgents => (
                "no_running_agents",
                "There are no running Agent tasks.",
            ),
        };
        let running_agents = result
            .running_agents
            .into_iter()
            .map(|(_, child_thread_id)| {
                json!({
                    "child_thread_id": child_thread_id,
                })
            })
            .collect::<Vec<_>>();
        let mut payload = json!({
            "outcome": outcome,
            "message": message,
            "running_agents": running_agents,
        });
        // 收割交付:挂起期间完成的 Agent 结果直接随工具结果返回
        // (消息头为结构化 FINAL_ANSWER),不再等下一轮 Defer 注入。
        if result.outcome == WaitOutcome::AgentStateChanged && !result.harvested.is_empty() {
            let results: Vec<_> = result
                .harvested
                .iter()
                .map(|r| {
                    json!({
                        "task_name": r.agent_name,
                        "child_thread_id": r.child_thread_id,
                        "message_type": "FINAL_ANSWER",
                        "payload": r.output,
                    })
                })
                .collect();
            payload["results"] = json!(results);
        }
        payload.to_string()
    }
}

#[async_trait]
impl BaseTool for WaitAgentTool {
    fn name(&self) -> &str {
        "WaitAgent"
    }

    fn description(&self) -> &str {
        WAIT_AGENT_DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "timeout_ms": {
                    "type": "integer",
                    "minimum": MIN_TIMEOUT_MS,
                    "maximum": MAX_TIMEOUT_MS,
                    "default": DEFAULT_TIMEOUT_MS,
                    "description": "Maximum time to wait in milliseconds."
                }
            }
        })
    }

    fn timeout(&self) -> Option<Duration> {
        None
    }

    async fn invoke(
        &self,
        input: serde_json::Value,
        _ctx: peri_agent::tools::ToolContext<'_>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let timeout_ms = match input.get("timeout_ms") {
            None | Some(serde_json::Value::Null) => DEFAULT_TIMEOUT_MS,
            Some(value) => value.as_u64().ok_or("timeout_ms must be an integer")?,
        };
        if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
            return Err(format!(
                "timeout_ms must be between {MIN_TIMEOUT_MS} and {MAX_TIMEOUT_MS}"
            )
            .into());
        }
        let result = self.wait(Duration::from_millis(timeout_ms)).await;
        Ok(Self::render(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::session::{MessageQueue, MessageSource};
    use peri_agent::agent::async_tasks::{
        BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind,
    };
    use peri_agent::agent::events::BackgroundTaskResult;
    use peri_agent::messages::BaseMessage;

    fn task(id: &str, kind: BgTaskKind) -> BackgroundTask {
        BackgroundTask {
            id: id.to_string(),
            agent_name: "test".to_string(),
            prompt_summary: "test".to_string(),
            status: BackgroundTaskStatus::Running,
            started_at: std::time::Instant::now(),
            chrono_started_at: chrono::Utc::now(),
            kind,
            child_thread_id: (kind == BgTaskKind::Agent).then(|| format!("thread-{id}")),
            cancel_handle: BgCancelHandle::Kill(Some(Box::new(|| {}))),
            cancel_token: None,
            agent_followup: None,
            pid: None,
            output_preview: None,
        }
    }

    fn result(id: &str) -> BackgroundTaskResult {
        BackgroundTaskResult {
            agent_path: None,
            task_id: id.to_string(),
            agent_name: "test".to_string(),
            prompt_summary: "test".to_string(),
            success: true,
            output: "full Agent body".to_string(),
            tool_calls_count: 0,
            duration_ms: 1,
            child_thread_id: Some(format!("thread-{id}")),
            timed_out: false,
        }
    }

    fn setup() -> (
        Arc<TaskManager>,
        Arc<SessionInbox>,
        Arc<AtomicBool>,
        Arc<CancellationToken>,
        Arc<WaitAgentTool>,
    ) {
        let task_manager = Arc::new(TaskManager::new());
        let inbox = Arc::new(SessionInbox::new(Arc::new(MessageQueue::new())));
        let idle = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(CancellationToken::new());
        let tool = Arc::new(WaitAgentTool::new(
            Arc::clone(&task_manager),
            Arc::clone(&inbox),
            Arc::clone(&idle),
            Arc::clone(&cancel),
        ));
        (task_manager, inbox, idle, cancel, tool)
    }

    async fn wait_until_suspended(flag: &AtomicBool) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !flag.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("WaitAgent should enter suspended state");
    }

    #[tokio::test]
    async fn completion_wakes_after_result_is_available() {
        let (tasks, _, idle, _, tool) = setup();
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let mut wait = tokio::spawn(async move { tool.wait(Duration::from_secs(1)).await });
        wait_until_suspended(&idle).await;

        let routed = Arc::new(AtomicBool::new(false));
        let routed_in_callback = Arc::clone(&routed);
        assert!(tasks.complete_with("agent-1", result("agent-1"), move |_| {
            routed_in_callback.store(true, Ordering::Release);
        }));
        let result = (&mut wait).await.unwrap();

        assert_eq!(result.outcome, WaitOutcome::AgentStateChanged);
        assert!(routed.load(Ordering::Acquire));
        assert!(result.running_agents.is_empty());
        assert!(!idle.load(Ordering::Acquire));
    }

    #[test]
    fn completion_between_subscription_and_snapshot_remains_observable() {
        let tasks = TaskManager::new();
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let changes = tasks.subscribe_agent_changes();

        tasks.complete("agent-1", result("agent-1"));

        assert!(changes.has_changed().unwrap());
        assert!(tasks.running_agent_tasks().is_empty());
    }

    #[tokio::test]
    async fn stop_wakes_waiter() {
        let (tasks, _, idle, _, tool) = setup();
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let wait = tokio::spawn(async move { tool.wait(Duration::from_secs(1)).await });
        wait_until_suspended(&idle).await;
        tasks.cancel("agent-1").unwrap();

        let result = wait.await.unwrap();
        assert_eq!(result.outcome, WaitOutcome::AgentStateChanged);
        assert!(result.running_agents.is_empty());
    }

    /// 挂起期间完成的 Agent 结果进入 harvest,由 wait 直接收割交付。
    #[tokio::test]
    async fn completed_agent_result_is_harvested_into_wait() {
        let (tasks, _, idle, _, tool) = setup();
        tasks.set_idle_suspended_flag(Arc::clone(&idle));
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let wait = tokio::spawn(async move { tool.wait(Duration::from_secs(1)).await });
        wait_until_suspended(&idle).await;

        tasks
            .complete_with("agent-1", result("agent-1"), |_| {})
            .then_some(())
            .unwrap();

        let wait_result = wait.await.unwrap();
        assert_eq!(wait_result.outcome, WaitOutcome::AgentStateChanged);
        assert_eq!(wait_result.harvested.len(), 1);
        assert_eq!(wait_result.harvested[0].output, "full Agent body");
    }

    /// 用户输入抢占退出时,已收割的结果按 Defer 路径补投递,不丢失。
    #[tokio::test]
    async fn harvest_requeues_defer_on_user_input_exit() {
        let (tasks, inbox, idle, _, tool) = setup();
        tasks.set_idle_suspended_flag(Arc::clone(&idle));
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let wait = tokio::spawn(async move { tool.wait(Duration::from_secs(1)).await });
        wait_until_suspended(&idle).await;

        // 先注入用户输入(biased select 下 inbox 优先于 changes → UserInput 结局),
        // 再完成 Agent:结果进 harvest(挂起中),wait 退出时补投递。
        inbox
            .handle()
            .push_prompt(MessageSource::UserInput, BaseMessage::human("new input"));
        tasks
            .complete_with("agent-1", result("agent-1"), |_| {})
            .then_some(())
            .unwrap();

        let wait_result = wait.await.unwrap();
        assert_eq!(wait_result.outcome, WaitOutcome::UserInput);
        assert_eq!(wait_result.harvested.len(), 1);
        let deferred = inbox.queue().drain_all();
        assert!(deferred.iter().any(|m| m.source == MessageSource::SubAgentComplete));
    }

    #[tokio::test]
    async fn user_prompt_interrupts_without_consuming_it() {
        let (tasks, inbox, idle, _, tool) = setup();
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let wait = tokio::spawn(async move { tool.wait(Duration::from_secs(1)).await });
        wait_until_suspended(&idle).await;
        inbox
            .handle()
            .push_prompt(MessageSource::UserInput, BaseMessage::human("new input"));

        let result = wait.await.unwrap();
        assert_eq!(result.outcome, WaitOutcome::UserInput);
        assert!(inbox.queue().has_pending_prompt());
        assert!(!idle.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn turn_cancel_interrupts_wait() {
        let (tasks, _, idle, cancel, tool) = setup();
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let wait = tokio::spawn(async move { tool.wait(Duration::from_secs(1)).await });
        wait_until_suspended(&idle).await;
        cancel.cancel();

        assert_eq!(wait.await.unwrap().outcome, WaitOutcome::TurnCancelled);
    }

    #[tokio::test]
    async fn timeout_restores_previous_idle_flag() {
        let (tasks, _, idle, _, tool) = setup();
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        idle.store(true, Ordering::Release);

        let result = tool.wait(Duration::from_millis(5)).await;
        assert_eq!(result.outcome, WaitOutcome::TimedOut);
        assert_eq!(result.running_agents.len(), 1);
        assert!(idle.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn no_running_agents_returns_immediately() {
        let (_, _, idle, _, tool) = setup();
        let result = tokio::time::timeout(Duration::from_millis(50), tool.wait(Duration::MAX))
            .await
            .unwrap();
        assert_eq!(result.outcome, WaitOutcome::NoRunningAgents);
        assert!(!idle.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn shell_lifecycle_does_not_wake_waiter() {
        let (tasks, _, idle, _, tool) = setup();
        tasks
            .register_with_kind(task("agent-1", BgTaskKind::Agent))
            .unwrap();
        let mut wait = tokio::spawn(async move { tool.wait(Duration::from_secs(1)).await });
        wait_until_suspended(&idle).await;

        tasks
            .register_with_kind(task("shell-1", BgTaskKind::Shell))
            .unwrap();
        tasks.cancel("shell-1").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut wait)
                .await
                .is_err(),
            "Shell registration/cancellation must not wake WaitAgent"
        );

        tasks.complete("agent-1", result("agent-1"));
        assert_eq!(wait.await.unwrap().outcome, WaitOutcome::AgentStateChanged);
    }

    #[tokio::test]
    async fn timeout_input_is_validated() {
        let (_, _, _, _, tool) = setup();
        let error = tool
            .invoke(
                json!({ "timeout_ms": MIN_TIMEOUT_MS - 1 }),
                peri_agent::tools::ToolContext::new(&[], "."),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("between 10000 and 3600000"));
    }
}
