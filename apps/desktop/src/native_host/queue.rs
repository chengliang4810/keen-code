//! NativeHost 的后续输入队列适配。
//!
//! 队列正文、调度状态和完成收据都由 Runtime Journal 持有。NativeHost 只把
//! 原生动作转换为可恢复的资源层记录，并在 Runtime 事实允许时启动一个事件驱动
//! 的消费任务。

use std::sync::{
    Arc, Weak,
    atomic::{AtomicBool, Ordering},
};

use keencode_resources::{
    ReasoningEffortSnapshot, SessionEvent, SessionInputDelivery, SessionInputDispatch,
    SessionInputKind, SessionInputQueueItem,
};
use keencode_runtime::{RuntimeEventPayload, RuntimeEventReceiveError};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::{
    agent_runtime::unix_time_ms,
    native_ui::model::{AttachmentFact, ModelSelection, NativeUiError, PermissionMode, PlanMode},
};

use super::{NativeHost, ui_error};

/// 队列 admission 冻结同一次发送的正文、附件和模型选项，不能从后续会话设置重建。
pub(super) struct QueuedInputSubmission {
    pub text: String,
    pub attachments: Vec<AttachmentFact>,
    pub model: Option<ModelSelection>,
    pub effort: Option<String>,
    pub permission: PermissionMode,
    pub plan: PlanMode,
}

/// 一个 Session 只允许存在一个队列 supervisor；取消发送端由 Host shutdown 或
/// supervisor 自身结束时消费。任务不持有 Host 强引用，避免关闭流程被后台任务反向延长。
pub(super) struct QueueSupervisorState {
    cancel: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    terminating: AtomicBool,
}

impl QueueSupervisorState {
    fn new(cancel: oneshot::Sender<()>) -> Self {
        Self {
            cancel: std::sync::Mutex::new(Some(cancel)),
            terminating: AtomicBool::new(false),
        }
    }

    /// 取消当前 supervisor；重复取消是幂等的。
    pub(super) fn cancel(&self) {
        self.mark_terminating();
        let sender = self.cancel.lock().ok().and_then(|mut sender| sender.take());
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
    }

    fn is_terminating(&self) -> bool {
        self.terminating.load(Ordering::Acquire)
    }

    fn mark_terminating(&self) {
        self.terminating.store(true, Ordering::Release);
    }
}

impl NativeHost {
    /// 取消所有 Session 队列任务，供 Host shutdown 在释放 Runtime 前调用。
    pub(super) fn cancel_queue_supervisors(&self) {
        if let Ok(supervisors) = self.inner.queue_supervisors.lock() {
            for supervisor in supervisors.values() {
                supervisor.cancel();
            }
        }
    }

    /// 将忙碌时发送的文本写入 Runtime 的持久队列。
    pub(super) fn enqueue_input(
        &self,
        session_id: &str,
        operation_id: &str,
        submission: QueuedInputSubmission,
    ) -> Result<(), NativeUiError> {
        let QueuedInputSubmission {
            text,
            attachments,
            model,
            effort,
            permission,
            plan,
        } = submission;
        if text.trim().is_empty() {
            return Err(ui_error("队列输入不能为空"));
        }

        // 选择值来自当前 Send 命令；只有没有显式选择时才回读 Journal。这样队列
        // admission 不依赖前置的 Session 设置写入，也不会把用户后来改动的设置
        // 误当成这条输入的模型语义。
        let session = self.open_session(session_id)?;
        let current_provider = session
            .read_state(|state| state.provider.clone())
            .map_err(ui_error)?;
        let provider = self.resolve_queue_provider(
            session_id,
            model.as_ref(),
            effort.as_deref(),
            current_provider.as_ref(),
        )?;
        let frozen_attachments = attachments
            .into_iter()
            .map(freeze_attachment)
            .collect::<Result<Vec<_>, _>>()?;
        let followup_mode = session
            .read_state(|state| state.followup_mode)
            .map_err(ui_error)?;
        let delivery = match followup_mode {
            keencode_resources::FollowupMode::Queue => SessionInputDelivery::Queue,
            keencode_resources::FollowupMode::Guide => SessionInputDelivery::Guide,
        };
        let item = SessionInputQueueItem {
            queue_item_id: format!("queue:{operation_id}"),
            source_command_id: operation_id.to_owned(),
            client_id: Some("gpui-owner".to_owned()),
            kind: SessionInputKind::SendText,
            text,
            attachments: frozen_attachments,
            model_selection: Some(frozen_model_value(&provider)),
            mode: Some(permission_name(permission).to_owned()),
            plan_enabled: plan == PlanMode::ReadOnly,
            requested_delivery: delivery,
            admitted_delivery: delivery,
            admission_seq: 0,
            reserve_attempt: 0,
            dispatch: SessionInputDispatch::Queued,
            promoted_turn_id: None,
            admitted_at_unix_ms: unix_time_ms(),
        };
        self.inner
            .runtime
            .enqueue_session_input(session_id, operation_id, item)
            .map(|_| ())
            .map_err(ui_error)
    }

    fn resolve_queue_provider(
        &self,
        session_id: &str,
        model: Option<&ModelSelection>,
        effort: Option<&str>,
        current_provider: Option<&keencode_resources::ProviderSnapshot>,
    ) -> Result<keencode_resources::ProviderSnapshot, NativeUiError> {
        if let Some(model) = model {
            let current_effort = current_provider
                .and_then(|provider| provider.reasoning_effort)
                .map(reasoning_effort_name);
            return self
                .inner
                .runtime
                .workflow_provider_snapshot_for_selection(
                    session_id,
                    &model.provider_id,
                    &model.model,
                    effort.or(current_effort),
                )
                .map_err(ui_error);
        }

        let current = self
            .inner
            .runtime
            .workflow_provider_snapshot(session_id)
            .map_err(ui_error)?;
        if let Some(effort) = effort {
            return self
                .inner
                .runtime
                .workflow_provider_snapshot_for_selection(
                    session_id,
                    &current.provider_id,
                    &current.model,
                    Some(effort),
                )
                .map_err(ui_error);
        }
        Ok(current)
    }

    /// 确保当前 Session 只有一个事件驱动的队列 supervisor。
    pub(super) fn ensure_queue_supervisor(&self, session_id: &str) -> Result<(), NativeUiError> {
        self.open_session(session_id)?;
        let mut supervisors = self.inner.queue_supervisors.lock().map_err(ui_error)?;
        if let Some(current) = supervisors.get(session_id) {
            if !current.is_terminating() {
                return Ok(());
            }
            // 旧任务已经承诺退出，但清理 map 仍可能等待它的最后一次轮询。
            // 先替换状态，避免新的 Send 复用一个即将消失的 supervisor。
            supervisors.remove(session_id);
        }

        let (cancel, cancellation) = oneshot::channel();
        let state = Arc::new(QueueSupervisorState::new(cancel));
        supervisors.insert(session_id.to_owned(), Arc::clone(&state));
        drop(supervisors);

        let weak_host = Arc::downgrade(&self.inner);
        let session_id = session_id.to_owned();
        self.inner.executor.spawn(async move {
            run_queue_supervisor(weak_host, session_id, state, cancellation).await;
        });
        Ok(())
    }

    /// 显式发送一条已经进入 Journal 的队列项。
    pub(super) async fn send_queued(
        &self,
        session_id: &str,
        queue_item_id: &str,
        operation_id: &str,
    ) -> Result<(), NativeUiError> {
        self.ensure_queue_supervisor(session_id)?;
        self.send_queued_with_mode(session_id, queue_item_id, operation_id)
            .await
    }

    async fn send_queued_with_mode(
        &self,
        session_id: &str,
        queue_item_id: &str,
        operation_id: &str,
    ) -> Result<(), NativeUiError> {
        let session = self.open_session(session_id)?;
        if session.has_active_work().map_err(ui_error)? {
            return Err(ui_error("当前会话仍有活动 Turn"));
        }
        let (_, queue) = self
            .inner
            .runtime
            .session_input_queue(session_id)
            .map_err(ui_error)?;
        if queue.completions.iter().any(|completion| {
            completion.queue_item_id == queue_item_id
                && completion.completion_operation_id == operation_id
        }) {
            return Ok(());
        }
        let item = queue
            .items
            .into_iter()
            .find(|item| item.queue_item_id == queue_item_id)
            .ok_or_else(|| ui_error("队列项不存在或已经消费"))?;
        if item.dispatch != SessionInputDispatch::Queued {
            return Err(ui_error("队列项当前不可发送"));
        }

        let target_turn_id = queue_turn_id(operation_id);
        let reserve_operation_id = format!("{operation_id}:reserve");
        let release_operation_id = format!("{operation_id}:release");
        let reserved = match self.inner.runtime.reserve_session_input(
            session_id,
            &reserve_operation_id,
            queue_item_id,
            &target_turn_id,
        ) {
            Ok(item) => item,
            Err(error) => {
                // reserve 可能已经写入 Journal 后才丢失 ACK。只有确认当前项绑定的
                // 仍是本次 target turn 时才补偿 release，避免释放另一个消费者的租约。
                self.release_if_owned(
                    session_id,
                    queue_item_id,
                    &target_turn_id,
                    &release_operation_id,
                );
                return Err(ui_error(error));
            }
        };

        let result = async {
            match reserved.kind {
                SessionInputKind::Compact => {
                    let target = reserved
                        .promoted_turn_id
                        .as_ref()
                        .map(|turn_id| turn_id.as_str())
                        .ok_or_else(|| ui_error("压缩队列项缺少已持久化的 Turn 身份"))?;
                    self.inner
                        .runtime
                        .compact_session_context(session_id, operation_id, target)
                        .await
                        .map_err(ui_error)
                }
                SessionInputKind::SendText | SessionInputKind::SendGoalCommand => {
                    self.restore_frozen_options(session_id, operation_id, &reserved)?;
                    let attachments = reserved
                        .attachments
                        .iter()
                        .map(parse_attachment)
                        .collect::<Result<Vec<_>, _>>()?;
                    let plan = if reserved.plan_enabled {
                        PlanMode::ReadOnly
                    } else {
                        PlanMode::Off
                    };
                    let options = self.turn_options(session_id, &attachments, plan)?;
                    self.inner
                        .runtime
                        .start_root_turn(session_id, &target_turn_id, &reserved.text, options)
                        .await
                        .map(|_| ())
                        .map_err(ui_error)
                }
            }
        }
        .await;

        if let Err(error) = result {
            self.release_if_owned(
                session_id,
                queue_item_id,
                &target_turn_id,
                &release_operation_id,
            );
            return Err(error);
        }

        if let Err(error) =
            self.inner
                .runtime
                .complete_session_input(session_id, operation_id, queue_item_id)
        {
            // Turn 已经可能形成；complete 失败时仍尽力 release。若队列项已被
            // Runtime 冷恢复/重试收账，release 会自然成为幂等无操作。
            self.release_if_owned(
                session_id,
                queue_item_id,
                &target_turn_id,
                &release_operation_id,
            );
            return Err(ui_error(error));
        }
        Ok(())
    }

    fn restore_frozen_options(
        &self,
        session_id: &str,
        operation_id: &str,
        item: &SessionInputQueueItem,
    ) -> Result<(), NativeUiError> {
        if let Some(selection) = item.model_selection.as_ref() {
            let selection = parse_model(selection)?;
            self.inner
                .runtime
                .set_session_model(
                    session_id,
                    &format!("{operation_id}:model"),
                    &selection.provider_id,
                    &selection.model,
                )
                .map_err(ui_error)?;
            let effort = selection.reasoning_effort.as_deref().unwrap_or("none");
            self.inner
                .runtime
                .set_session_effort(session_id, &format!("{operation_id}:effort"), effort)
                .map_err(ui_error)?;
        }
        if let Some(mode) = item.mode.as_deref() {
            self.persist_permission(
                session_id,
                &format!("{operation_id}:permission"),
                parse_permission(mode)?,
            )?;
        }
        Ok(())
    }

    fn release_if_owned(
        &self,
        session_id: &str,
        queue_item_id: &str,
        target_turn_id: &str,
        operation_id: &str,
    ) {
        let owned = self
            .inner
            .runtime
            .session_input_queue(session_id)
            .ok()
            .and_then(|(_, queue)| {
                queue
                    .items
                    .into_iter()
                    .find(|item| item.queue_item_id == queue_item_id)
            })
            .is_some_and(|item| {
                item.dispatch != SessionInputDispatch::Queued
                    && item
                        .promoted_turn_id
                        .as_ref()
                        .is_some_and(|turn_id| turn_id.as_str() == target_turn_id)
            });
        if owned {
            let _ =
                self.inner
                    .runtime
                    .release_session_input(session_id, operation_id, queue_item_id);
        }
    }
}

async fn run_queue_supervisor(
    weak_host: Weak<super::NativeHostInner>,
    session_id: String,
    state: Arc<QueueSupervisorState>,
    mut cancellation: oneshot::Receiver<()>,
) {
    let mut source = {
        let Some(inner) = weak_host.upgrade() else {
            state.mark_terminating();
            return;
        };
        match inner.runtime.subscribe_session_events(&session_id) {
            Ok(source) => source,
            Err(_) => {
                state.mark_terminating();
                cleanup_supervisor(&inner, &session_id, &state);
                return;
            }
        }
    };
    if let Some(inner) = weak_host.upgrade() {
        NativeHost { inner }.initialize_hot(&session_id, source.last_observed_delivery_sequence());
    } else {
        state.mark_terminating();
        return;
    }
    // 冷恢复的唯一触发点：先读取权威 Journal，再尝试一次队首消费。
    if let Some(inner) = weak_host.upgrade() {
        let host = NativeHost { inner };
        let _ = drain_queue_once(&host, &session_id).await;
        if supervisor_can_exit(&host, &session_id) {
            state.mark_terminating();
            cleanup_supervisor_from_weak(&weak_host, &session_id, &state);
            return;
        }
    } else {
        state.mark_terminating();
        return;
    }

    loop {
        tokio::select! {
            _ = &mut cancellation => break,
            received = source.recv() => match received {
                Ok(delivery) => {
                    let Some(inner) = weak_host.upgrade() else {
                        state.mark_terminating();
                        break;
                    };
                    let host = NativeHost { inner };
                    host.observe_hot_event(&delivery);
                    if matches!(&delivery.payload, RuntimeEventPayload::Authoritative(record) if contains_completed(&record.event)) {
                        let _ = drain_queue_once(&host, &session_id).await;
                    }
                    if supervisor_can_exit(&host, &session_id) {
                        break;
                    }
                }
                Err(RuntimeEventReceiveError::Lagged(_)) => {
                    // Lag 只说明实时观察者必须由上层按 Snapshot/Journal 追赶；
                    // 这里不把丢失的事件猜成 idle，也不触发自动排放。
                }
                Err(RuntimeEventReceiveError::Closed) => break,
            }
        }
    }
    state.mark_terminating();
    cleanup_supervisor_from_weak(&weak_host, &session_id, &state);
}

async fn drain_queue_once(host: &NativeHost, session_id: &str) -> Result<(), NativeUiError> {
    let session = host.open_session(session_id)?;
    if session.has_active_work().map_err(ui_error)? {
        return Ok(());
    }
    let (_, queue) = host
        .inner
        .runtime
        .session_input_queue(session_id)
        .map_err(ui_error)?;
    if !queue.auto_drain {
        return Ok(());
    }
    let Some(item) = queue.items.first() else {
        return Ok(());
    };
    if item.dispatch != SessionInputDispatch::Queued
        || item.admitted_delivery != SessionInputDelivery::Queue
    {
        return Ok(());
    }
    let operation_id = format!("auto-drain-{}", digest_hex(&item.queue_item_id));
    host.send_queued_with_mode(session_id, &item.queue_item_id, &operation_id)
        .await
}

fn cleanup_supervisor(
    inner: &Arc<super::NativeHostInner>,
    session_id: &str,
    state: &Arc<QueueSupervisorState>,
) {
    if let Ok(mut supervisors) = inner.queue_supervisors.lock()
        && supervisors
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, state))
    {
        supervisors.remove(session_id);
    }
}

fn cleanup_supervisor_from_weak(
    weak_host: &Weak<super::NativeHostInner>,
    session_id: &str,
    state: &Arc<QueueSupervisorState>,
) {
    if let Some(inner) = weak_host.upgrade() {
        cleanup_supervisor(&inner, session_id, state);
    }
}

fn supervisor_can_exit(host: &NativeHost, session_id: &str) -> bool {
    // 有原生订阅者时必须继续持有 Runtime event source；否则空闲 Session 的
    // 首次订阅可能在 supervisor 释放后错过 hot watermark 初始化。
    let has_native_subscribers = host
        .inner
        .native_session_subscribers
        .lock()
        .map(|subscribers| subscribers.get(session_id).copied().unwrap_or_default() > 0)
        .unwrap_or(true);
    if has_native_subscribers {
        return false;
    }
    let Ok(session) = host.open_session(session_id) else {
        return true;
    };
    let Ok(active) = session.has_active_work() else {
        return false;
    };
    if active {
        return false;
    }
    host.inner
        .runtime
        .session_input_queue(session_id)
        .map(|(_, queue)| queue.items.is_empty())
        .unwrap_or(false)
}

fn contains_completed(event: &SessionEvent) -> bool {
    match event {
        SessionEvent::TurnCompleted { .. } => true,
        SessionEvent::AtomicBatch { events } => events.iter().any(contains_completed),
        _ => false,
    }
}

fn queue_turn_id(operation_id: &str) -> String {
    format!("native-queue-{}", digest_hex(operation_id))
}

fn digest_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn frozen_model_value(provider: &keencode_resources::ProviderSnapshot) -> Value {
    serde_json::json!({
        "providerId": provider.provider_id,
        "model": provider.model,
        "options": provider.reasoning_effort.map(|effort| serde_json::json!({
            "reasoningLevel": reasoning_effort_name(effort),
        })),
    })
}

fn reasoning_effort_name(effort: ReasoningEffortSnapshot) -> &'static str {
    match effort {
        ReasoningEffortSnapshot::Minimal => "minimal",
        ReasoningEffortSnapshot::Low => "low",
        ReasoningEffortSnapshot::Medium => "medium",
        ReasoningEffortSnapshot::High => "high",
        ReasoningEffortSnapshot::ExtraHigh => "xhigh",
        ReasoningEffortSnapshot::Maximum => "max",
    }
}

fn permission_name(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Build => "build",
        PermissionMode::Edit => "edit",
        PermissionMode::Plan => "plan",
        PermissionMode::Yolo => "yolo",
    }
}

fn parse_permission(value: &str) -> Result<PermissionMode, NativeUiError> {
    match value {
        "build" => Ok(PermissionMode::Build),
        "edit" => Ok(PermissionMode::Edit),
        "plan" => Ok(PermissionMode::Plan),
        "yolo" => Ok(PermissionMode::Yolo),
        _ => Err(ui_error("队列项权限模式无效")),
    }
}

fn freeze_attachment(attachment: AttachmentFact) -> Result<Value, NativeUiError> {
    let actual = inspect_attachment(&attachment.path)?;
    if actual != attachment {
        return Err(ui_error("附件状态已变化，请重新添加"));
    }
    Ok(attachment_value(attachment))
}

fn inspect_attachment(path: &str) -> Result<AttachmentFact, NativeUiError> {
    let path = std::fs::canonicalize(path).map_err(ui_error)?;
    let metadata = std::fs::metadata(&path).map_err(ui_error)?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
        return Err(ui_error("附件必须是 16 MiB 以内的文件"));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "text/plain",
    };
    Ok(AttachmentFact {
        attachment_id: format!("file-{}", stable_row_id(&path.to_string_lossy())),
        file_name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: path.to_string_lossy().into_owned(),
        media_type: media_type.to_owned(),
        bytes: metadata.len(),
        image: media_type.starts_with("image/"),
    })
}

fn stable_row_id(message_id: &str) -> u64 {
    let hash = Sha256::digest(message_id.as_bytes());
    u64::from_le_bytes(hash[..8].try_into().expect("SHA256 至少包含八字节")).max(1)
}

fn attachment_value(attachment: AttachmentFact) -> Value {
    serde_json::json!({
        "attachmentId": attachment.attachment_id,
        "path": attachment.path,
        "fileName": attachment.file_name,
        "mediaType": attachment.media_type,
        "bytes": attachment.bytes,
        "image": attachment.image,
    })
}

#[derive(Clone, Debug)]
struct FrozenModelSelection {
    provider_id: String,
    model: String,
    reasoning_effort: Option<String>,
}

fn parse_model(value: &Value) -> Result<FrozenModelSelection, NativeUiError> {
    let object = value
        .as_object()
        .ok_or_else(|| ui_error("队列项模型选择格式无效"))?;
    let provider_id = object
        .get("providerId")
        .or_else(|| object.get("provider"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ui_error("队列项模型缺少 Provider"))?
        .to_owned();
    let model = object
        .get("model")
        .or_else(|| object.get("modelId"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ui_error("队列项模型缺少模型标识"))?
        .to_owned();
    let reasoning_effort = object
        .get("options")
        .and_then(Value::as_object)
        .and_then(|options| options.get("reasoningLevel"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if let Some(effort) = reasoning_effort.as_deref()
        && !matches!(
            effort,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        )
    {
        return Err(ui_error("队列项推理强度无效"));
    }
    Ok(FrozenModelSelection {
        provider_id,
        model,
        reasoning_effort,
    })
}

fn parse_attachment(value: &Value) -> Result<AttachmentFact, NativeUiError> {
    let object = value
        .as_object()
        .ok_or_else(|| ui_error("队列附件格式无效"))?;
    let attachment = AttachmentFact {
        attachment_id: required_string(object, "attachmentId", "队列附件缺少 ID")?,
        path: required_string(object, "path", "队列附件缺少路径")?,
        file_name: required_string(object, "fileName", "队列附件缺少文件名")?,
        media_type: required_string(object, "mediaType", "队列附件缺少媒体类型")?,
        bytes: object
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| ui_error("队列附件缺少大小"))?,
        image: object
            .get("image")
            .and_then(Value::as_bool)
            .ok_or_else(|| ui_error("队列附件缺少类型"))?,
    };
    // 发送前再次核验路径、大小、媒体类型和稳定 ID，避免 Journal 中的引用
    // 被外部文件替换后直接交给 Provider。
    let actual = inspect_attachment(&attachment.path)?;
    if actual != attachment {
        return Err(ui_error("队列附件状态已变化，请重新添加"));
    }
    Ok(attachment)
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
    message: &str,
) -> Result<String, NativeUiError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| ui_error(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_supervisor_marks_it_terminating_and_is_idempotent() {
        let (sender, _receiver) = oneshot::channel();
        let state = QueueSupervisorState::new(sender);

        assert!(!state.is_terminating());
        state.cancel();
        assert!(state.is_terminating());
        state.cancel();
        assert!(state.is_terminating());
    }
}
