//! Session 隔离的实时事件投递与显式追赶契约。

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use keencode_agent::{AgentEventFuture, AgentEventSink, AgentEventSinkError, AgentStreamEvent};
use keencode_resources::{AgentId, SessionEvent, SessionEventRecord, TurnId};
use thiserror::Error;
use tokio::sync::broadcast;

use crate::RuntimeError;

/// 单个 Runtime Session 可保留的在线 retry 投影数量上限；该表不进入 Journal。
const MAX_MODEL_RETRY_STATES: usize = 128;

/// Runtime 统一实时序列中保持类型边界的临时流、权威 Journal 事实或控制信号。
#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeEventPayload {
    /// 尚未成为权威 Session 事实的 Provider/Agent 实时流事件。
    Transient(AgentStreamEvent),
    /// Runtime 已确认当前 Turn 仍可继续执行的模型重试安排。
    ///
    /// 该状态只存在于热实时投递世代；下一次权威快照或冷恢复不会从中恢复，
    /// 因而不能替代 Journal 中的 Turn 生命周期事实。
    ModelRetryScheduled(RuntimeModelRetryScheduled),
    /// 已经成功追加到 Session Journal 的唯一权威事件记录。
    Authoritative(SessionEventRecord),
    /// Runtime 投递通道自身的生命周期控制信号。
    Control(RuntimeControlEvent),
}

/// 由 Runtime 发布给桌面投影的 provider 中立模型重试状态。
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeModelRetryScheduled {
    /// 仍处于重试生命周期中的 Turn 标识。
    pub turn_id: String,
    /// 产生该模型请求的根 Agent 或单层子 Agent 标识。
    pub source_agent_id: String,
    /// 当前即将开始的请求尝试序号，从 1 开始并包含首次请求。
    pub attempt: u32,
    /// 该 Turn 的最大请求尝试数。
    pub max_attempts: u32,
    /// 下一次请求前等待的毫秒数。
    pub delay_ms: u64,
    /// Runtime 接收重试安排时的 Unix Epoch 毫秒时间。
    pub occurred_at_ms: u64,
    /// 已脱敏的短错误摘要；不会进入 V4 snapshot 的事实字段。
    pub message: String,
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
    /// 当前仍有效的热 retry 投影，按 Turn 与 Agent 双重身份隔离。
    model_retries: BTreeMap<(String, String), RuntimeModelRetryScheduled>,
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
                model_retries: BTreeMap::new(),
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
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentEventSinkError::new("Runtime Publisher 状态不可用"))?;
        if state.closed {
            return Err(AgentEventSinkError::new("Runtime Publisher 已关闭"));
        }
        // 任一真实模型流事件表示该 Turn 已重新进入执行；旧 retry 仅是等待态，
        // 不能继续覆盖当前响应。
        state.model_retries.remove(&(
            event.turn_id().as_str().to_owned(),
            event.source_agent_id().as_str().to_owned(),
        ));
        self.publish_locked(&mut state, RuntimeEventPayload::Transient(event.clone()))
            .map_err(|_| AgentEventSinkError::new("Runtime Publisher 状态不可用"))
    }

    /// 校验并发布一次仅存在于热 Runtime 投递世代的模型重试安排。
    pub(crate) fn publish_model_retry(
        &self,
        event: RuntimeModelRetryScheduled,
    ) -> Result<(), RuntimeError> {
        // 这条入口只接受已通过 Provider retry policy 的受信 Runtime 数据，
        // 仍在 publisher 边界拒绝非法身份和不可能的尝试范围。
        TurnId::new(event.turn_id.clone())?;
        AgentId::new(event.source_agent_id.clone())?;
        if event.attempt == 0
            || event.max_attempts == 0
            || event.attempt > event.max_attempts
            || event.message.len() > 16 * 1024
        {
            return Err(RuntimeError::InvalidTurnRequest);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if state.closed {
            return Err(RuntimeError::SessionClosed);
        }
        if state.model_retries.len() >= MAX_MODEL_RETRY_STATES
            && let Some(oldest) = state
                .model_retries
                .iter()
                .min_by_key(|(key, retry)| (retry.occurred_at_ms, key.0.as_str(), key.1.as_str()))
                .map(|(key, _)| key.clone())
        {
            // Bounded memory uses the retry's observed time as the eviction order; key fields
            // only make equal timestamps deterministic and are not a freshness signal.
            state.model_retries.remove(&oldest);
        }
        state.model_retries.insert(
            (event.turn_id.clone(), event.source_agent_id.clone()),
            event.clone(),
        );
        self.publish_locked(&mut state, RuntimeEventPayload::ModelRetryScheduled(event))
    }

    /// 读取当前热 retry；新订阅和 resync 只能从这份 Runtime authority 重建它。
    pub(crate) fn model_retry_for_turn(
        &self,
        turn_id: &str,
        source_agent_id: &str,
    ) -> Result<Option<RuntimeModelRetryScheduled>, RuntimeError> {
        let state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        Ok(state
            .model_retries
            .get(&(turn_id.to_owned(), source_agent_id.to_owned()))
            .cloned())
    }

    /// 仅在 Journal 新追加得到明确回执时发布一次权威记录。
    pub(crate) fn publish_authoritative(
        &self,
        record: SessionEventRecord,
    ) -> Result<(), RuntimeError> {
        if record.session.as_str() != self.session_id.as_ref() {
            return Err(RuntimeError::InvalidTurnRequest);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if state.closed {
            return Err(RuntimeError::SessionClosed);
        }
        clear_model_retry_for_event(&mut state.model_retries, &record.event);
        self.publish_locked(&mut state, RuntimeEventPayload::Authoritative(record))
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

    /// 在调用方已持有 Publisher 锁时分配序号并广播一条载荷。
    fn publish_locked(
        &self,
        state: &mut PublisherState,
        payload: RuntimeEventPayload,
    ) -> Result<(), RuntimeError> {
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

fn clear_model_retry_for_event(
    retries: &mut BTreeMap<(String, String), RuntimeModelRetryScheduled>,
    event: &SessionEvent,
) {
    match event {
        SessionEvent::TurnStarted {
            source_agent_id, ..
        } => retries.retain(|(_, agent_id), _| agent_id != source_agent_id.as_str()),
        SessionEvent::TurnCompleted { turn_id } | SessionEvent::TurnStopped { turn_id, .. } => {
            retries.retain(|(retry_turn_id, _), _| retry_turn_id != turn_id.as_str())
        }
        SessionEvent::AtomicBatch { events } => {
            for event in events {
                clear_model_retry_for_event(retries, event);
            }
        }
        _ => {}
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
