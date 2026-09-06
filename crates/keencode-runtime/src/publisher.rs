//! Session 隔离的实时事件投递与显式追赶契约。

use std::sync::{Arc, Mutex};

use keencode_agent::{AgentEventFuture, AgentEventSink, AgentEventSinkError, AgentStreamEvent};
use keencode_resources::SessionEventRecord;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::RuntimeError;

/// Runtime 统一实时序列中保持类型边界的临时流、权威 Journal 事实或控制信号。
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeEventPayload {
    /// 尚未成为权威 Session 事实的 Provider/Agent 实时流事件。
    Transient(AgentStreamEvent),
    /// 已经成功追加到 Session Journal 的唯一权威事件记录。
    Authoritative(SessionEventRecord),
    /// Runtime 投递通道自身的生命周期控制信号。
    Control(RuntimeControlEvent),
}

/// 不属于 Session Journal、只描述本地实时通道生命周期的控制信号。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeControlEvent {
    /// Session 已完成在途 Turn 收尾，当前订阅不会再收到后续事件。
    SessionClosed,
}

/// 一条已经由所属 Session 分配本地投递序号的类型化 Runtime 事件。
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeEventDelivery {
    /// 当前 Session 内严格递增且不跨 Session 共享的实时投递序号。
    pub delivery_sequence: u64,
    /// 当前投递所属且由同一 Session 全部事件共享存储的 Session 标识。
    pub session_id: Arc<str>,
    /// 临时 Agent 流事件、已确认写入 Journal 的权威记录或通道控制信号。
    pub payload: RuntimeEventPayload,
}

impl RuntimeEventDelivery {
    /// 返回临时、权威或控制载荷共同绑定的 Session 标识文本。
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// 订阅者落后于有界实时缓冲区后必须执行的权威追赶动作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCatchUpDirective {
    /// 重新读取 Runtime Snapshot，并按 Journal sequence 分页重放缺失的权威事实。
    ReloadSnapshotAndReplayJournal,
}

/// 慢订阅者已经丢失的连续实时投递范围。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEventLag {
    /// 被有界广播缓冲覆盖的实时事件数量。
    pub missed_events: u64,
    /// 当前订阅者未观察到的首个 Session 本地投递序号。
    pub first_missed_delivery_sequence: u64,
    /// 当前订阅者未观察到的最后一个 Session 本地投递序号。
    pub last_missed_delivery_sequence: u64,
    /// 恢复权威状态时必须执行的固定追赶动作。
    pub catch_up: RuntimeCatchUpDirective,
}

/// 接收 Session 实时事件时可被调用方明确处理的非事件结果。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RuntimeEventReceiveError {
    /// 订阅者落后且必须通过 Snapshot 与 Journal 追赶权威事实。
    #[error("实时事件订阅已落后，必须重新读取 Snapshot 并分页重放 Journal")]
    Lagged(RuntimeEventLag),
    /// 所属 Runtime Session 已经释放，实时通道不会再产生事件。
    #[error("Runtime Session 实时事件通道已关闭")]
    Closed,
}

/// 一个绑定单一 Session 且记录自身已观察序号的实时订阅。
pub struct RuntimeEventSubscription {
    /// Tokio 有界广播通道为当前订阅者维护的独立读取游标。
    receiver: broadcast::Receiver<RuntimeEventDelivery>,
    /// 创建订阅时的水位或最近一次事件、Lag 信号覆盖到的投递序号。
    last_observed_delivery_sequence: u64,
    /// 收到 SessionClosed 控制信号后让后续 recv 立即返回 Closed。
    closed_after_delivery: bool,
}

impl RuntimeEventSubscription {
    /// 等待下一条实时事件，落后时先返回显式追赶信号而不伪造事件补发。
    pub async fn recv(&mut self) -> Result<RuntimeEventDelivery, RuntimeEventReceiveError> {
        if self.closed_after_delivery {
            return Err(RuntimeEventReceiveError::Closed);
        }
        match self.receiver.recv().await {
            Ok(delivery) => {
                self.last_observed_delivery_sequence = delivery.delivery_sequence;
                self.closed_after_delivery = matches!(
                    &delivery.payload,
                    RuntimeEventPayload::Control(RuntimeControlEvent::SessionClosed)
                );
                Ok(delivery)
            }
            Err(broadcast::error::RecvError::Lagged(missed_events)) => {
                let first_missed_delivery_sequence =
                    self.last_observed_delivery_sequence.saturating_add(1);
                let last_missed_delivery_sequence = self
                    .last_observed_delivery_sequence
                    .saturating_add(missed_events);
                self.last_observed_delivery_sequence = last_missed_delivery_sequence;
                Err(RuntimeEventReceiveError::Lagged(RuntimeEventLag {
                    missed_events,
                    first_missed_delivery_sequence,
                    last_missed_delivery_sequence,
                    catch_up: RuntimeCatchUpDirective::ReloadSnapshotAndReplayJournal,
                }))
            }
            Err(broadcast::error::RecvError::Closed) => Err(RuntimeEventReceiveError::Closed),
        }
    }
}

/// Publisher 锁内共同维护的发送端与最后已分配序号。
struct PublisherState {
    /// 所有订阅者共享的有界广播发送端。
    sender: broadcast::Sender<RuntimeEventDelivery>,
    /// 当前 Session 已经分配的最后一个实时投递序号。
    last_delivery_sequence: u64,
    /// SessionClosed 控制信号是否已经作为最后一条投递发送。
    closed: bool,
}

/// 可由 Session 内多个绑定 Runner 共享的顺序化实时事件 Publisher。
#[derive(Clone)]
pub(crate) struct SessionEventPublisher {
    /// 当前 Publisher 唯一允许接收的 Session 标识文本。
    session_id: Arc<str>,
    /// 把序号分配和广播写入串行化，避免并发发送与序号顺序倒置。
    state: Arc<Mutex<PublisherState>>,
}

impl SessionEventPublisher {
    /// 使用已验证的 Session 标识和非零有界容量创建 Publisher。
    pub(crate) fn new(session_id: &str, capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            session_id: Arc::from(session_id),
            state: Arc::new(Mutex::new(PublisherState {
                sender,
                last_delivery_sequence: 0,
                closed: false,
            })),
        }
    }

    /// 在同一锁内冻结订阅起始水位并创建独立接收游标。
    pub(crate) fn subscribe(&self) -> Result<RuntimeEventSubscription, RuntimeError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        Ok(RuntimeEventSubscription {
            receiver: state.sender.subscribe(),
            last_observed_delivery_sequence: state.last_delivery_sequence,
            closed_after_delivery: state.closed,
        })
    }

    /// 验证 Session 身份、分配下一序号并以调用顺序广播事件。
    fn publish_transient(&self, event: &AgentStreamEvent) -> Result<(), AgentEventSinkError> {
        if event.session_id().as_str() != self.session_id.as_ref() {
            return Err(AgentEventSinkError::new(
                "Runtime Publisher 拒绝跨 Session 实时事件",
            ));
        }
        self.publish(RuntimeEventPayload::Transient(event.clone()))
            .map_err(|_| AgentEventSinkError::new("Runtime Publisher 状态不可用"))
    }

    /// 仅在 Journal 新追加得到明确回执时发布一次权威记录。
    pub(crate) fn publish_authoritative(
        &self,
        record: SessionEventRecord,
    ) -> Result<(), RuntimeError> {
        if record.session.as_str() != self.session_id.as_ref() {
            return Err(RuntimeError::InvalidTurnRequest);
        }
        self.publish(RuntimeEventPayload::Authoritative(record))
    }

    /// 以当前 Session 最后一条有序投递显式通知全部既有订阅者关闭。
    pub(crate) fn close(&self) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if state.closed {
            return Ok(());
        }
        let delivery_sequence = state
            .last_delivery_sequence
            .checked_add(1)
            .ok_or(RuntimeError::StateUnavailable)?;
        let delivery = RuntimeEventDelivery {
            delivery_sequence,
            session_id: self.session_id.clone(),
            payload: RuntimeEventPayload::Control(RuntimeControlEvent::SessionClosed),
        };
        state.last_delivery_sequence = delivery_sequence;
        state.closed = true;
        let _ = state.sender.send(delivery);
        Ok(())
    }

    /// 在 Publisher 锁内为一种类型化载荷分配严格递增序号并广播。
    fn publish(&self, payload: RuntimeEventPayload) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if state.closed {
            return Err(RuntimeError::SessionClosed);
        }
        let delivery_sequence = state
            .last_delivery_sequence
            .checked_add(1)
            .ok_or(RuntimeError::StateUnavailable)?;
        let delivery = RuntimeEventDelivery {
            delivery_sequence,
            session_id: self.session_id.clone(),
            payload,
        };
        state.last_delivery_sequence = delivery_sequence;
        let _ = state.sender.send(delivery);
        Ok(())
    }
}

/// 先保留调用方原有 Sink 行为，再把成功接收的事件写入 Session Publisher 的组合出口。
pub(crate) struct RuntimeEventFanoutSink {
    /// 按 Session 分配序号并广播给桌面订阅者的 Runtime Publisher。
    publisher: SessionEventPublisher,
    /// AgentRunner 绑定前已经配置的可选诊断或测试 Sink。
    downstream: Arc<dyn AgentEventSink>,
}

impl RuntimeEventFanoutSink {
    /// 创建保持原有 Sink 且增加 Runtime Publisher 的组合出口。
    pub(crate) fn new(
        publisher: SessionEventPublisher,
        downstream: Arc<dyn AgentEventSink>,
    ) -> Self {
        Self {
            publisher,
            downstream,
        }
    }
}

impl AgentEventSink for RuntimeEventFanoutSink {
    /// 先等待原有 Sink 明确接收，再按 Session 顺序发布同一事件。
    fn send<'a>(&'a self, event: &'a AgentStreamEvent) -> AgentEventFuture<'a> {
        Box::pin(async move {
            self.downstream.send(event).await?;
            self.publisher.publish_transient(event)
        })
    }
}
