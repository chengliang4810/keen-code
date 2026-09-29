//! Host Prompt 队列的共享驱动原语。
//!
//! admission 账本与全部状态机语义都在 [`HostPromptQueue`]；本模块只收敛
//! Desktop 与 Headless 两侧 Host 都需要的驱动时序：等待自己位于队首再
//! claim、等待执行身份绑定、等待终态，以及每次状态迁移后唤醒全部等待者。
//! 所有等待都用同一个 [`Notify`]，替代每侧各自实现的轮询或私有唤醒通道。

use std::sync::Arc;

use keencode_acp::OperationId;
use tokio::sync::Notify;

use crate::host_core::{
    ClaimedPrompt, ElicitationAnswerResult, ExecutionIdentity, HostCoreError, HostPromptQueue,
    OperationState, OperationStatus, OperationTerminal,
};

/// [`PromptQueueDriver::wait_for_terminal`] 遇到 `NeedsInput` 时的行为。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NeedsInputBehavior {
    /// 继续等待真正的终态；交互式客户端在连接内应答 Elicitation。
    Wait,
    /// 立即返回 `NeedsInput` 状态，由调用方向客户端转发输入请求
    /// （非交互 CLI 以稳定错误码退出，等待其它客户端接管）。
    Return,
}

/// [`HostPromptQueue`] 的驱动协调器。
///
/// 同一 Host 的所有调用方必须共享同一实例（通过 `Clone` 传播），
/// 否则部分等待者会错过唤醒。状态迁移方法（claim 除外）在成功后
/// 自动唤醒全部等待者；调用方不得再各自维护唤醒通道。
#[derive(Clone)]
pub struct PromptQueueDriver {
    queue: Arc<HostPromptQueue>,
    wakeup: Arc<Notify>,
}

impl PromptQueueDriver {
    /// 为指定队列创建驱动协调器。
    pub fn new(queue: Arc<HostPromptQueue>) -> Self {
        Self {
            queue,
            wakeup: Arc::new(Notify::new()),
        }
    }

    /// 底层 admission 账本；只读查询和未收敛的迁移从这里访问。
    pub fn queue(&self) -> &HostPromptQueue {
        &self.queue
    }

    /// 唤醒全部等待者；仅用于无法通过本类型收敛的同步迁移路径。
    pub fn notify_waiters(&self) {
        self.wakeup.notify_waiters();
    }

    /// 等待指定 operation 位于 Session 队首且无 active 操作时 claim 它。
    ///
    /// 返回 `None` 表示 operation 已达终态（例如排队中被取消），
    /// 后台驱动应直接退出，不能再执行。
    pub async fn claim_when_ready(
        &self,
        session_id: &str,
        operation_id: &OperationId,
    ) -> Result<Option<ClaimedPrompt>, HostCoreError> {
        loop {
            let notified = self.wakeup.notified();
            if let Some(claimed) = self.queue.claim_operation(session_id, operation_id)? {
                return Ok(Some(claimed));
            }
            let status = self.queue.status(operation_id)?;
            if status.state.is_terminal() {
                return Ok(None);
            }
            notified.await;
        }
    }

    /// 等待 operation 绑定稳定执行身份或进入终态；供 detach 调用安全返回。
    pub async fn wait_for_execution(
        &self,
        operation_id: &OperationId,
    ) -> Result<OperationStatus, HostCoreError> {
        loop {
            let notified = self.wakeup.notified();
            let status = self.queue.status(operation_id)?;
            if status.execution.is_some() || status.state.is_terminal() {
                return Ok(status);
            }
            notified.await;
        }
    }

    /// 等待 operation 终态；`NeedsInput` 的处理方式由 `needs_input` 决定。
    pub async fn wait_for_terminal(
        &self,
        operation_id: &OperationId,
        needs_input: NeedsInputBehavior,
    ) -> Result<OperationStatus, HostCoreError> {
        loop {
            let notified = self.wakeup.notified();
            let status = self.queue.status(operation_id)?;
            if status.state.is_terminal() {
                return Ok(status);
            }
            if needs_input == NeedsInputBehavior::Return
                && status.state == OperationState::NeedsInput
            {
                return Ok(status);
            }
            notified.await;
        }
    }

    /// 绑定 Runtime 已分配的稳定执行身份并唤醒等待 execution 的调用方。
    pub fn bind_execution(
        &self,
        operation_id: &OperationId,
        execution: ExecutionIdentity,
    ) -> Result<OperationStatus, HostCoreError> {
        let status = self.queue.bind_execution(operation_id, execution)?;
        self.wakeup.notify_waiters();
        Ok(status)
    }

    /// 将运行中操作置为等待用户输入并唤醒等待者；不释放 active slot。
    pub fn mark_needs_input(
        &self,
        operation_id: &OperationId,
        elicitation_id: impl Into<String>,
    ) -> Result<OperationStatus, HostCoreError> {
        let status = self.queue.mark_needs_input(operation_id, elicitation_id)?;
        self.wakeup.notify_waiters();
        Ok(status)
    }

    /// 记录 Elicitation 的首个赢家回答并唤醒等待者。
    pub fn answer_elicitation(
        &self,
        operation_id: &OperationId,
        elicitation_id: impl Into<String>,
        answer_id: OperationId,
        answer_digest: impl Into<String>,
    ) -> Result<ElicitationAnswerResult, HostCoreError> {
        let result = self.queue.answer_elicitation(
            operation_id,
            elicitation_id,
            answer_id,
            answer_digest,
        )?;
        self.wakeup.notify_waiters();
        Ok(result)
    }

    /// 写入终态并释放 Session active slot；operation 已终态时幂等成功。
    ///
    /// 后台驱动与终态归约路径可能对同一 operation 重复收口（例如取消
    /// 与正常完成竞态），重复 finish 不构成错误。
    pub fn finish_operation(
        &self,
        operation_id: &OperationId,
        state: OperationState,
        terminal: OperationTerminal,
    ) -> Result<(), HostCoreError> {
        if self
            .queue
            .status(operation_id)
            .is_ok_and(|status| status.state.is_terminal())
        {
            return Ok(());
        }
        self.queue.finish(operation_id, state, terminal)?;
        self.wakeup.notify_waiters();
        Ok(())
    }

    /// 取消尚未 claim 的 pending 操作并唤醒等待者，让队首得以推进。
    pub fn cancel_pending(
        &self,
        operation_id: &OperationId,
        terminal: OperationTerminal,
    ) -> Result<OperationStatus, HostCoreError> {
        let status = self.queue.cancel_pending(operation_id, terminal)?;
        self.wakeup.notify_waiters();
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_core::{AdmissionDisposition, PromptAdmissionRequest};
    use keencode_acp::ConnectionId;
    use std::time::Duration;

    struct Fixture {
        queue: Arc<HostPromptQueue>,
        driver: PromptQueueDriver,
        connection: ConnectionId,
    }

    impl Fixture {
        fn new() -> Self {
            let queue = Arc::new(HostPromptQueue::with_defaults());
            let driver = PromptQueueDriver::new(Arc::clone(&queue));
            Self {
                queue,
                driver,
                connection: ConnectionId::new("conn-driver-test").expect("测试连接 id 合法"),
            }
        }

        fn admit(&self, operation_id: &str) -> crate::host_core::AdmissionReceipt {
            use sha2::{Digest, Sha256};
            let prompt = format!("prompt {operation_id}");
            let payload_digest = format!("{:x}", Sha256::digest(prompt.as_bytes()));
            self.queue
                .admit(PromptAdmissionRequest {
                    connection_id: self.connection.clone(),
                    session_id: "session-driver".to_owned(),
                    operation_id: OperationId::new(operation_id).expect("测试 operation id 合法"),
                    prompt,
                    payload_digest,
                    detached: false,
                })
                .expect("admission 输入有效")
        }

        fn claim_now(&self, operation_id: &str) -> Option<ClaimedPrompt> {
            self.queue
                .claim_operation(
                    "session-driver",
                    &OperationId::new(operation_id).expect("测试 operation id 合法"),
                )
                .expect("claim 输入有效")
        }

        fn terminal(code: &'static str) -> OperationTerminal {
            OperationTerminal::new(code, None::<String>).expect("终态输入有效")
        }

        fn execution_for(operation_id: &str) -> ExecutionIdentity {
            ExecutionIdentity::new(
                "session-driver".to_owned(),
                format!("turn-{operation_id}"),
                format!("task-{operation_id}"),
            )
            .expect("执行身份有效")
        }
    }

    /// 断言挂起的等待任务尚未完成，避免迁移前就已返回造成虚假通过。
    async fn assert_still_waiting<T>(handle: &tokio::task::JoinHandle<T>) {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!handle.is_finished(), "等待任务不应在状态迁移前完成");
    }

    /// 等待挂起任务完成并解开 JoinHandle 的双层结果。
    async fn join_waiter<T>(handle: tokio::task::JoinHandle<T>) -> T {
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("等待者必须在状态迁移后被唤醒")
            .expect("等待任务自身不失败")
    }

    #[tokio::test]
    async fn claim_when_ready_claims_idle_operation_immediately() {
        let fixture = Fixture::new();
        let receipt = fixture.admit("op-idle");
        assert_eq!(receipt.disposition, AdmissionDisposition::Accepted);

        let claimed = fixture
            .driver
            .claim_when_ready("session-driver", &receipt.operation_id)
            .await
            .expect("claim 输入有效")
            .expect("空闲 Session 的队首必须立即可 claim");
        assert_eq!(claimed.prompt, "prompt op-idle");
        assert!(!claimed.detached);
    }

    #[tokio::test]
    async fn claim_waiter_takes_next_slot_in_fifo_after_finish() {
        let fixture = Fixture::new();
        let first = fixture.admit("op-first");
        let second = fixture.admit("op-second");
        let claimed_first = fixture.claim_now("op-first").expect("队首可 claim");

        let driver = fixture.driver.clone();
        let second_id = second.operation_id.clone();
        let waiter = tokio::spawn(async move {
            driver
                .claim_when_ready("session-driver", &second_id)
                .await
                .expect("claim 输入有效")
                .expect("释放 active 后必须 claim 到排队操作")
        });
        assert_still_waiting(&waiter).await;

        fixture
            .driver
            .finish_operation(
                &first.operation_id,
                OperationState::Completed,
                Fixture::terminal("completed"),
            )
            .expect("finish 收口");
        let claimed_second = join_waiter(waiter).await;
        assert_eq!(claimed_second.operation_id, second.operation_id);
        drop(claimed_first);
    }

    #[tokio::test]
    async fn pending_cancel_releases_claim_waiter_without_execution() {
        let fixture = Fixture::new();
        let first = fixture.admit("op-first");
        let second = fixture.admit("op-second");
        fixture.claim_now("op-first").expect("队首可 claim");

        let driver = fixture.driver.clone();
        let second_id = second.operation_id.clone();
        let waiter = tokio::spawn(async move {
            driver
                .claim_when_ready("session-driver", &second_id)
                .await
                .expect("claim 输入有效")
        });
        assert_still_waiting(&waiter).await;

        fixture
            .driver
            .cancel_pending(&second.operation_id, Fixture::terminal("cancelled"))
            .expect("pending 操作可取消");
        let outcome = join_waiter(waiter).await;
        assert!(outcome.is_none(), "取消后的 operation 不允许再执行");
        drop(first);
    }

    #[tokio::test]
    async fn wait_for_execution_returns_after_bind_wakes_waiter() {
        let fixture = Fixture::new();
        let receipt = fixture.admit("op-exec");
        let claimed = fixture
            .driver
            .claim_when_ready("session-driver", &receipt.operation_id)
            .await
            .expect("claim 输入有效")
            .expect("空闲 Session 的队首必须立即可 claim");

        let driver = fixture.driver.clone();
        let operation_id = receipt.operation_id.clone();
        let waiter = tokio::spawn(async move {
            driver
                .wait_for_execution(&operation_id)
                .await
                .expect("status 查询有效")
        });
        assert_still_waiting(&waiter).await;

        fixture
            .driver
            .bind_execution(&claimed.operation_id, Fixture::execution_for("op-exec"))
            .expect("claim 后可绑定执行身份");
        let status = join_waiter(waiter).await;
        assert!(status.execution.is_some());
    }

    #[tokio::test]
    async fn wait_for_terminal_respects_needs_input_behavior() {
        let fixture = Fixture::new();
        let receipt = fixture.admit("op-input");
        let claimed = fixture
            .driver
            .claim_when_ready("session-driver", &receipt.operation_id)
            .await
            .expect("claim 输入有效")
            .expect("空闲 Session 的队首必须立即可 claim");
        fixture
            .driver
            .bind_execution(&claimed.operation_id, Fixture::execution_for("op-input"))
            .expect("claim 后可绑定执行身份");
        fixture
            .driver
            .mark_needs_input(&receipt.operation_id, "elicitation-1")
            .expect("运行中操作可等待输入");

        let returned = fixture
            .driver
            .wait_for_terminal(&receipt.operation_id, NeedsInputBehavior::Return)
            .await
            .expect("status 查询有效");
        assert_eq!(returned.state, OperationState::NeedsInput);

        let driver = fixture.driver.clone();
        let operation_id = receipt.operation_id.clone();
        let waiter = tokio::spawn(async move {
            driver
                .wait_for_terminal(&operation_id, NeedsInputBehavior::Wait)
                .await
                .expect("status 查询有效")
        });
        assert_still_waiting(&waiter).await;

        fixture
            .driver
            .finish_operation(
                &receipt.operation_id,
                OperationState::Completed,
                Fixture::terminal("completed"),
            )
            .expect("finish 收口");
        let finished = join_waiter(waiter).await;
        assert_eq!(finished.state, OperationState::Completed);
    }

    #[tokio::test]
    async fn finish_operation_is_idempotent_after_terminal() {
        let fixture = Fixture::new();
        let receipt = fixture.admit("op-finish");
        let claimed = fixture
            .driver
            .claim_when_ready("session-driver", &receipt.operation_id)
            .await
            .expect("claim 输入有效")
            .expect("空闲 Session 的队首必须立即可 claim");

        fixture
            .driver
            .finish_operation(
                &claimed.operation_id,
                OperationState::Completed,
                Fixture::terminal("completed"),
            )
            .expect("首次 finish 收口");
        fixture
            .driver
            .finish_operation(
                &claimed.operation_id,
                OperationState::Failed,
                Fixture::terminal("failed"),
            )
            .expect("终态后的重复 finish 必须幂等成功");
        let status = fixture
            .driver
            .queue()
            .status(&receipt.operation_id)
            .expect("已入账");
        assert_eq!(status.state, OperationState::Completed);
    }
}
