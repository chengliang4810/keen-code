//! 状态变更工具的文件快照准备、应用确认与专用容量账本。

use std::collections::{BTreeMap, BTreeSet};

use keencode_resources::{
    ArtifactMaterialization, ArtifactUse, FileSnapshot, MessagePart, MessageRole, RequestId,
    ResourceError, SessionEvent, SessionEventId, SessionId, SessionMessage, SessionState,
    ToolEffect, ToolFileChange, ToolLifecycle, TranscriptRecord, TurnStatus,
};

use super::{
    ControlState, RuntimeError, RuntimeSession, RuntimeSessionInner, StateCollectionItems,
    encoded_record_len, find_committed_event, journal_len, mark_event_confirmed,
    mark_event_indeterminate, protected_runtime_terminal_capacity,
    protected_state_collection_items, recovery_gate_allows_event, refresh_recovery_required,
    state_collection_event_items, state_collection_items,
};

/// 文件变更两阶段事件尚未由 Journal 消费的独立容量 reservation。
pub(crate) struct FileChangeReservation {
    /// Prepared 事件的跨重启稳定身份。
    pub(crate) prepared_event_id: SessionEventId,
    /// Applied 事件的跨重启稳定身份。
    pub(crate) applied_event_id: SessionEventId,
    /// Prepared 事件保守编码后的 Journal 字节数。
    pub(crate) prepared_event_bytes: u64,
    /// Applied 事件保守编码后的 Journal 字节数。
    pub(crate) applied_event_bytes: u64,
    /// 尚未由已确认事件消费的 Journal 字节数。
    pub(crate) reserved_journal_bytes: u64,
    /// 尚未由已确认事件消费的 Journal 记录数。
    pub(crate) reserved_journal_records: u64,
    /// 尚未由已确认 Prepared 事件消费的状态集合预算。
    pub(crate) reserved_state_items: StateCollectionItems,
    /// 尚未形成权威快照引用的唯一 Artifact 身份。
    pub(crate) missing_artifact_ids: BTreeSet<String>,
    /// 尚未物化块的完整引用，用于在当前变更失败时精确区分其所有权。
    pub(crate) missing_artifact_uses: BTreeMap<String, ArtifactUse>,
    /// 已形成完整 Artifact pair 的唯一引用，用于失败清理时保护并发 reservation。
    pub(crate) materialized_artifact_uses: BTreeMap<String, ArtifactUse>,
    /// 已形成完整 Artifact pair 且已经从其他 reservation 移除的身份。
    pub(crate) materialized_artifact_ids: BTreeSet<String>,
    /// Prepared 事件是否已经由 Journal 明确确认。
    pub(crate) prepared_confirmed: bool,
    /// Applied 事件是否已经由 Journal 明确确认。
    pub(crate) applied_confirmed: bool,
}

impl FileChangeReservation {
    /// 创建同时保护 Prepared 与 Applied 两条事件的 reservation。
    fn new(
        prepared_event_id: SessionEventId,
        applied_event_id: SessionEventId,
        prepared_event_bytes: u64,
        applied_event_bytes: u64,
        reserved_state_items: StateCollectionItems,
        missing_artifact_uses: BTreeMap<String, ArtifactUse>,
    ) -> Result<Self, RuntimeError> {
        let missing_artifact_ids = missing_artifact_uses.keys().cloned().collect();
        Ok(Self {
            prepared_event_id,
            applied_event_id,
            prepared_event_bytes,
            applied_event_bytes,
            reserved_journal_bytes: prepared_event_bytes
                .checked_add(applied_event_bytes)
                .ok_or(RuntimeError::TurnUnpersistable)?,
            reserved_journal_records: 2,
            reserved_state_items,
            missing_artifact_ids,
            missing_artifact_uses,
            materialized_artifact_uses: BTreeMap::new(),
            materialized_artifact_ids: BTreeSet::new(),
            prepared_confirmed: false,
            applied_confirmed: false,
        })
    }

    /// 为已经确认 Prepared 的应用重试只保留 Applied 事件容量。
    fn applied_only(
        prepared_event_id: SessionEventId,
        applied_event_id: SessionEventId,
        applied_event_bytes: u64,
    ) -> Self {
        Self {
            prepared_event_id,
            applied_event_id,
            prepared_event_bytes: 0,
            applied_event_bytes,
            reserved_journal_bytes: applied_event_bytes,
            reserved_journal_records: 1,
            reserved_state_items: StateCollectionItems::default(),
            missing_artifact_ids: BTreeSet::new(),
            missing_artifact_uses: BTreeMap::new(),
            materialized_artifact_uses: BTreeMap::new(),
            materialized_artifact_ids: BTreeSet::new(),
            prepared_confirmed: true,
            applied_confirmed: false,
        }
    }

    /// 判断固定事件身份和预检字节仍与当前重试完全一致。
    fn matches_plan(
        &self,
        prepared_event_id: &SessionEventId,
        applied_event_id: &SessionEventId,
        prepared_event_bytes: u64,
        applied_event_bytes: u64,
    ) -> bool {
        self.prepared_event_id == *prepared_event_id
            && self.applied_event_id == *applied_event_id
            && self.prepared_event_bytes == prepared_event_bytes
            && self.applied_event_bytes == applied_event_bytes
    }
}

/// 文件变更事件的两阶段身份。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileChangePhase {
    /// 写入前后快照证据的 Prepared 事件。
    Prepared,
    /// 工作区写入成功后的 Applied 事件。
    Applied,
}

impl FileChangePhase {
    /// 返回稳定事件 ID 中使用的固定阶段文本。
    const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Applied => "applied",
        }
    }
}

/// 文件变更 Journal 故障注入点，仅用于 Runtime 单元回归测试。
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileChangeAppendFault {
    /// 在实际追加前返回明确失败，不形成 Journal 事件。
    BeforeAppend(FileChangePhase),
    /// 追加成功后模拟发布结果不确定，保留相同事件身份等待对账。
    AfterAppend(FileChangePhase),
}

#[cfg(test)]
thread_local! {
    /// 当前测试线程下一次文件变更追加要触发的故障。
    static FILE_CHANGE_APPEND_FAULT: std::cell::RefCell<Option<FileChangeAppendFault>> = const { std::cell::RefCell::new(None) };
}

/// 为当前测试线程设置一次性文件变更 Journal 故障。
#[cfg(test)]
fn inject_file_change_append_fault(fault: FileChangeAppendFault) {
    FILE_CHANGE_APPEND_FAULT.with(|current| {
        let previous = current.replace(Some(fault));
        assert!(
            previous.is_none(),
            "文件变更追加故障必须在设置下一个故障前被消费"
        );
    });
}

/// 仅在当前测试线程故障点匹配时消费文件变更 Journal 故障。
#[cfg(test)]
fn take_file_change_append_fault(fault: FileChangeAppendFault) -> bool {
    FILE_CHANGE_APPEND_FAULT.with(|current| {
        if *current.borrow() == Some(fault) {
            current.replace(None);
            true
        } else {
            false
        }
    })
}

impl RuntimeSession {
    /// 读取由当前 Session ArtifactStore 持有的完整文件快照，不读取工作区当前文件。
    ///
    /// 调用方应只传入从该 Session 权威工具生命周期取得的 [`FileSnapshot`]；这里
    /// 不接受磁盘路径，也不会通过快照中的路径字段访问用户工作区。
    pub fn read_file_snapshot(&self, snapshot: &FileSnapshot) -> Result<Vec<u8>, RuntimeError> {
        self.inner
            .artifacts
            .read_file_snapshot(snapshot)
            .map_err(Into::into)
    }

    /// 读取已提交文件快照中的有界原始字节区间，不读取工作区当前文件。
    ///
    /// `offset` 按原始字节计数，`length` 不得超过资源层单次读取上限；区间不需要
    /// 落在 UTF-8 边界。调用方应只传入从该 Session 权威工具生命周期取得的快照。
    pub fn read_file_snapshot_range(
        &self,
        snapshot: &FileSnapshot,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>, RuntimeError> {
        self.inner
            .artifacts
            .read_file_snapshot_range(snapshot, offset, length)
            .map_err(Into::into)
    }

    /// 读取指定工具当前已确认的文件变更证据，不读取工作区当前文件。
    pub fn current_tool_file_change(
        &self,
        request_id: &RequestId,
    ) -> Result<Option<ToolFileChange>, RuntimeError> {
        let _control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        let state = self.inner.journal.state()?;
        Ok(state
            .tools
            .get(request_id)
            .and_then(|tool| tool.file_change.clone()))
    }

    /// 在状态变更工具真正写入工作区前持久化前后快照证据。
    ///
    /// 所有快照块先写入当前 Session 的 ArtifactStore，再追加 Prepared 事件；只有
    /// Journal 明确确认事件且发布成功后才返回成功。该方法本身不写入用户文件。
    pub fn prepare_file_change(
        &self,
        request_id: &RequestId,
        path: String,
        before: Option<&[u8]>,
        after: &[u8],
    ) -> Result<(), RuntimeError> {
        let prepared_event_id = file_change_event_id(
            self.inner.artifacts.session_id(),
            request_id,
            FileChangePhase::Prepared,
        )?;
        let applied_event_id = file_change_event_id(
            self.inner.artifacts.session_id(),
            request_id,
            FileChangePhase::Applied,
        )?;
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != super::RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        if !recovery_gate_allows_event(&control, prepared_event_id.as_str()) {
            return Err(RuntimeError::RecoveryRequired);
        }
        let state = self.inner.journal.state()?;
        let tool = state
            .tools
            .get(request_id)
            .ok_or(RuntimeError::InvalidTurnRequest)?;
        validate_running_state_change_tool(&state, tool)?;
        if !valid_cross_platform_absolute_path(&path) {
            return Err(RuntimeError::InvalidTurnRequest);
        }

        let before_snapshot = before
            .map(|bytes| self.inner.artifacts.plan_file_snapshot(bytes))
            .transpose()?;
        let after_snapshot = self.inner.artifacts.plan_file_snapshot(after)?;
        let planned_change = ToolFileChange {
            path,
            before: before_snapshot,
            after: after_snapshot,
            applied: false,
        };

        if let Some(existing) = tool.file_change.as_ref() {
            return reconcile_existing_prepared(
                &self.inner,
                &mut control,
                request_id,
                existing,
                &planned_change,
                &prepared_event_id,
                &applied_event_id,
            );
        }

        if self.inner.journal.contains_event_id(&prepared_event_id)? {
            control.hard_recovery_required = true;
            refresh_recovery_required(&mut control);
            return Err(RuntimeError::RecoveryRequired);
        }

        let prepared_event = SessionEvent::ToolFileChangePrepared {
            request_id: request_id.clone(),
            change: planned_change.clone(),
        };
        let applied_event = SessionEvent::ToolFileChangeApplied {
            request_id: request_id.clone(),
        };
        let prepared_event_bytes =
            encoded_record_len(&state.session_id, &prepared_event_id, &prepared_event)?;
        let applied_event_bytes =
            encoded_record_len(&state.session_id, &applied_event_id, &applied_event)?;
        let prepared_state_items = state_collection_event_items(&prepared_event);
        let missing_artifact_uses = planned_missing_artifacts(&self.inner, &planned_change)?;
        let missing_artifact_ids = missing_artifact_uses
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();

        let reservation_exists = control.file_change_reservations.contains_key(request_id);
        if reservation_exists {
            let reservation = control
                .file_change_reservations
                .get(request_id)
                .ok_or(RuntimeError::StateUnavailable)?;
            if !control
                .pending_indeterminate
                .contains(prepared_event_id.as_str())
                || !reservation.matches_plan(
                    &prepared_event_id,
                    &applied_event_id,
                    prepared_event_bytes,
                    applied_event_bytes,
                )
            {
                control.hard_recovery_required = true;
                refresh_recovery_required(&mut control);
                return Err(RuntimeError::RecoveryRequired);
            }
        } else {
            ensure_file_change_capacity(
                &self.inner,
                &control,
                &state,
                prepared_event_bytes
                    .checked_add(applied_event_bytes)
                    .ok_or(RuntimeError::TurnUnpersistable)?,
                2,
                prepared_state_items,
                &missing_artifact_ids,
            )?;
            control.file_change_reservations.insert(
                request_id.clone(),
                FileChangeReservation::new(
                    prepared_event_id.clone(),
                    applied_event_id.clone(),
                    prepared_event_bytes,
                    applied_event_bytes,
                    prepared_state_items,
                    missing_artifact_uses,
                )?,
            );
        }

        if let Err(error) =
            persist_file_change_snapshots(&self.inner, &planned_change, before, after)
        {
            return abort_unjournaled_file_change(&self.inner, &mut control, request_id, error);
        }
        match append_file_change_event(
            &self.inner,
            &mut control,
            request_id,
            FileChangePhase::Prepared,
            prepared_event_id,
            prepared_event,
        ) {
            Ok(()) => Ok(()),
            Err(RuntimeError::RecoveryRequired) => Err(RuntimeError::RecoveryRequired),
            Err(error) => {
                abort_unjournaled_file_change(&self.inner, &mut control, request_id, error)
            }
        }
    }

    /// 在调用方确认已实际写入工作区后，幂等追加文件变更 Applied 事件。
    ///
    /// Runtime 只记录调用方已经完成的应用确认，不自行读取或修改用户文件；事件
    /// 追加或发布结果不确定时返回 `RecoveryRequired`，并保留 Prepared 证据。
    pub fn mark_file_change_applied(&self, request_id: &RequestId) -> Result<(), RuntimeError> {
        let prepared_event_id = file_change_event_id(
            self.inner.artifacts.session_id(),
            request_id,
            FileChangePhase::Prepared,
        )?;
        let applied_event_id = file_change_event_id(
            self.inner.artifacts.session_id(),
            request_id,
            FileChangePhase::Applied,
        )?;
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| RuntimeError::StateUnavailable)?;
        if control.lifecycle != super::RuntimeSessionLifecycle::Open {
            return Err(RuntimeError::SessionClosed);
        }
        // Applied 发布不确定时只会留下 Applied 事件身份；不能要求 Prepared 身份也
        // 仍处于 pending，否则同一调用方无法通过稳定 Applied 身份完成热对账。
        if !recovery_gate_allows_event(&control, prepared_event_id.as_str())
            && !recovery_gate_allows_event(&control, applied_event_id.as_str())
        {
            return Err(RuntimeError::RecoveryRequired);
        }
        let state = self.inner.journal.state()?;
        let tool = state
            .tools
            .get(request_id)
            .ok_or(RuntimeError::InvalidTurnRequest)?;
        validate_running_state_change_tool(&state, tool)?;
        let Some(change) = tool.file_change.as_ref() else {
            return Err(RuntimeError::InvalidTurnRequest);
        };
        confirm_prepared_record(
            &self.inner,
            &mut control,
            request_id,
            change,
            &prepared_event_id,
        )?;

        if change.applied {
            confirm_applied_record(&self.inner, &mut control, request_id, &applied_event_id)?;
            return Ok(());
        }
        if !recovery_gate_allows_event(&control, applied_event_id.as_str()) {
            return Err(RuntimeError::RecoveryRequired);
        }

        let applied_event = SessionEvent::ToolFileChangeApplied {
            request_id: request_id.clone(),
        };
        let applied_event_bytes =
            encoded_record_len(&state.session_id, &applied_event_id, &applied_event)?;
        if let Some(reservation) = control.file_change_reservations.get(request_id) {
            if !reservation.matches_plan(
                &prepared_event_id,
                &applied_event_id,
                reservation.prepared_event_bytes,
                applied_event_bytes,
            ) || !reservation.prepared_confirmed
            {
                control.hard_recovery_required = true;
                refresh_recovery_required(&mut control);
                return Err(RuntimeError::RecoveryRequired);
            }
        } else {
            ensure_file_change_capacity(
                &self.inner,
                &control,
                &state,
                applied_event_bytes,
                1,
                StateCollectionItems::default(),
                &BTreeSet::new(),
            )?;
            control.file_change_reservations.insert(
                request_id.clone(),
                FileChangeReservation::applied_only(
                    prepared_event_id.clone(),
                    applied_event_id.clone(),
                    applied_event_bytes,
                ),
            );
        }

        match append_file_change_event(
            &self.inner,
            &mut control,
            request_id,
            FileChangePhase::Applied,
            applied_event_id,
            applied_event,
        ) {
            Ok(()) => Ok(()),
            Err(RuntimeError::RecoveryRequired) => Err(RuntimeError::RecoveryRequired),
            Err(error) => {
                control.file_change_reservations.remove(request_id);
                refresh_recovery_required(&mut control);
                Err(error)
            }
        }
    }
}

/// 为文件变更两阶段事件生成跨重启稳定的幂等身份。
fn file_change_event_id(
    session_id: &SessionId,
    request_id: &RequestId,
    phase: FileChangePhase,
) -> Result<SessionEventId, RuntimeError> {
    let digest = super::canonical_sha256(&(
        "keencode/runtime-file-change/v1",
        session_id.as_str(),
        request_id.as_str(),
        phase.as_str(),
    ))?;
    SessionEventId::new(format!("runtime-file-change-{}-{digest}", phase.as_str()))
        .map_err(RuntimeError::from)
}

/// 验证当前请求确实属于一个仍运行且已开始的状态变更工具。
fn validate_running_state_change_tool(
    state: &SessionState,
    tool: &ToolLifecycle,
) -> Result<(), RuntimeError> {
    if tool.request.effect != ToolEffect::ChangesState
        || !tool.execution_started
        || tool.outcome.is_some()
        || !state.turns.get(&tool.request.turn_id).is_some_and(|turn| {
            turn.status == TurnStatus::Running && turn.source_agent_id == tool.request.agent_id
        })
    {
        return Err(RuntimeError::InvalidTurnRequest);
    }
    Ok(())
}

/// 校验跨平台绝对路径的最小形状，保持与资源层归约规则一致。
fn valid_cross_platform_absolute_path(path: &str) -> bool {
    if path.is_empty() || path.chars().any(char::is_control) {
        return false;
    }
    if path.starts_with('/') {
        return true;
    }
    if let Some(unc_path) = path.strip_prefix("\\\\") {
        let components = unc_path
            .split(['\\', '/'])
            .filter(|component| !component.is_empty());
        return components.take(2).count() == 2;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// 统计两份快照中尚未存在的唯一 Artifact 槽位，不读取用户文件。
fn planned_missing_artifacts(
    inner: &RuntimeSessionInner,
    change: &ToolFileChange,
) -> Result<BTreeMap<String, ArtifactUse>, RuntimeError> {
    let mut missing = BTreeMap::new();
    let snapshots = change.before.iter().chain(std::iter::once(&change.after));
    for snapshot in snapshots {
        for chunk in &snapshot.chunks {
            match inner.artifacts.validate_use(chunk) {
                Ok(()) => {}
                Err(ResourceError::ArtifactNotFound) => {
                    missing
                        .entry(chunk.artifact_id.as_str().to_owned())
                        .or_insert_with(|| chunk.clone());
                }
                Err(error) => return Err(RuntimeError::Resource(error)),
            }
        }
    }
    Ok(missing)
}

/// 在所有快照块已经完成预检后按固定顺序写入 before 与 after Artifact。
fn persist_file_change_snapshots(
    inner: &RuntimeSessionInner,
    change: &ToolFileChange,
    before: Option<&[u8]>,
    after: &[u8],
) -> Result<(), RuntimeError> {
    if let (Some(snapshot), Some(bytes)) = (change.before.as_ref(), before) {
        inner.artifacts.persist_file_snapshot(snapshot, bytes)?;
    }
    inner
        .artifacts
        .persist_file_snapshot(&change.after, after)?;
    Ok(())
}

/// Prepared 事件已经存在时校验重试正文、完整 Artifact 和两阶段事件证据。
fn reconcile_existing_prepared(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
    request_id: &RequestId,
    existing: &ToolFileChange,
    planned: &ToolFileChange,
    prepared_event_id: &SessionEventId,
    applied_event_id: &SessionEventId,
) -> Result<(), RuntimeError> {
    if existing.path != planned.path
        || existing.before != planned.before
        || existing.after != planned.after
    {
        return Err(RuntimeError::ControlOperationConflict);
    }
    validate_file_change_artifacts(inner, existing)?;
    confirm_prepared_record(inner, control, request_id, existing, prepared_event_id)?;
    if existing.applied {
        confirm_applied_record(inner, control, request_id, applied_event_id)?;
    }
    Ok(())
}

/// 验证当前状态持有的 before/after 快照完整性。
fn validate_file_change_artifacts(
    inner: &RuntimeSessionInner,
    change: &ToolFileChange,
) -> Result<(), RuntimeError> {
    if let Some(snapshot) = &change.before {
        inner.artifacts.validate_file_snapshot(snapshot)?;
    }
    inner.artifacts.validate_file_snapshot(&change.after)?;
    Ok(())
}

/// 核对 Prepared 事件仍以相同正文存在，并确认当前 reservation 的 Prepared 预算。
fn confirm_prepared_record(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
    request_id: &RequestId,
    change: &ToolFileChange,
    prepared_event_id: &SessionEventId,
) -> Result<(), RuntimeError> {
    validate_file_change_artifacts(inner, change)?;
    let Some(record) = find_committed_event(&inner.journal, prepared_event_id)? else {
        control.hard_recovery_required = true;
        mark_event_indeterminate(control, prepared_event_id.as_str());
        return Err(RuntimeError::RecoveryRequired);
    };
    // Applied 事件只改变权威状态中的标志位；Prepared 事件正文始终保存
    // `applied: false`，重试对账不能把当前状态的 true 反写进 Prepared 比较值。
    let mut prepared_change = change.clone();
    prepared_change.applied = false;
    let expected = SessionEvent::ToolFileChangePrepared {
        request_id: request_id.clone(),
        change: prepared_change,
    };
    if record.event != expected {
        control.hard_recovery_required = true;
        mark_event_indeterminate(control, prepared_event_id.as_str());
        return Err(RuntimeError::RecoveryRequired);
    }
    let prepared_bytes = encoded_record_len(
        &inner.journal.state()?.session_id,
        prepared_event_id,
        &expected,
    )?;
    let state_items = state_collection_event_items(&expected);
    if charge_file_change_event(
        control,
        request_id,
        FileChangePhase::Prepared,
        prepared_bytes,
        state_items,
    )
    .is_err()
    {
        control.hard_recovery_required = true;
        mark_event_indeterminate(control, prepared_event_id.as_str());
        return Err(RuntimeError::RecoveryRequired);
    }
    mark_file_change_artifacts_materialized(control, request_id);
    mark_event_confirmed(control, prepared_event_id.as_str());
    Ok(())
}

/// 核对 Applied 事件已经存在；应用成功但发布未确认时只完成同身份对账。
fn confirm_applied_record(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
    request_id: &RequestId,
    applied_event_id: &SessionEventId,
) -> Result<(), RuntimeError> {
    let Some(record) = find_committed_event(&inner.journal, applied_event_id)? else {
        control.hard_recovery_required = true;
        mark_event_indeterminate(control, applied_event_id.as_str());
        return Err(RuntimeError::RecoveryRequired);
    };
    let expected = SessionEvent::ToolFileChangeApplied {
        request_id: request_id.clone(),
    };
    if record.event != expected {
        control.hard_recovery_required = true;
        mark_event_indeterminate(control, applied_event_id.as_str());
        return Err(RuntimeError::RecoveryRequired);
    }
    let state = inner.journal.state()?;
    let applied_bytes = encoded_record_len(&state.session_id, applied_event_id, &expected)?;
    if charge_file_change_event(
        control,
        request_id,
        FileChangePhase::Applied,
        applied_bytes,
        StateCollectionItems::default(),
    )
    .is_err()
    {
        control.hard_recovery_required = true;
        mark_event_indeterminate(control, applied_event_id.as_str());
        return Err(RuntimeError::RecoveryRequired);
    }
    mark_event_confirmed(control, applied_event_id.as_str());
    control.file_change_reservations.remove(request_id);
    refresh_recovery_required(control);
    Ok(())
}

/// 以已有 reservation 的阶段预算扣除一条明确确认的文件变更事件。
fn charge_file_change_event(
    control: &mut ControlState,
    request_id: &RequestId,
    phase: FileChangePhase,
    event_bytes: u64,
    event_state_items: StateCollectionItems,
) -> Result<(), ()> {
    let Some(entry) = control.file_change_reservations.get_mut(request_id) else {
        return Ok(());
    };
    match phase {
        FileChangePhase::Prepared => {
            if entry.prepared_confirmed {
                return Ok(());
            }
            if event_bytes != entry.prepared_event_bytes
                || entry.reserved_journal_records == 0
                || entry.reserved_journal_bytes < event_bytes
            {
                return Err(());
            }
            let remaining_state_items = {
                let mut remaining = entry.reserved_state_items;
                remaining.try_consume(event_state_items).map_err(|_| ())?;
                remaining
            };
            entry.reserved_journal_bytes -= event_bytes;
            entry.reserved_journal_records -= 1;
            entry.reserved_state_items = remaining_state_items;
            entry.prepared_confirmed = true;
        }
        FileChangePhase::Applied => {
            if entry.applied_confirmed {
                return Ok(());
            }
            if event_bytes != entry.applied_event_bytes
                || entry.reserved_journal_records == 0
                || entry.reserved_journal_bytes < event_bytes
            {
                return Err(());
            }
            entry.reserved_journal_bytes -= event_bytes;
            entry.reserved_journal_records -= 1;
            entry.applied_confirmed = true;
        }
    }
    Ok(())
}

/// Prepared Journal 确认后把已物化的块从其他 reservation 的待占用集合移除。
fn mark_file_change_artifacts_materialized(control: &mut ControlState, request_id: &RequestId) {
    let Some((materialized, materialized_uses)) = control
        .file_change_reservations
        .get(request_id)
        .map(|entry| {
            (
                entry.missing_artifact_ids.clone(),
                entry.missing_artifact_uses.clone(),
            )
        })
    else {
        return;
    };
    if materialized.is_empty() {
        return;
    }
    for entry in control.reservations.values_mut() {
        for artifact_id in &materialized {
            entry.missing_artifact_ids.remove(artifact_id);
        }
    }
    for (entry_request_id, entry) in control.file_change_reservations.iter_mut() {
        for artifact_id in &materialized {
            entry.missing_artifact_ids.remove(artifact_id);
            entry.missing_artifact_uses.remove(artifact_id);
            if entry_request_id == request_id {
                if let Some(artifact) = materialized_uses.get(artifact_id) {
                    entry
                        .materialized_artifact_uses
                        .insert(artifact_id.clone(), artifact.clone());
                }
                entry.materialized_artifact_ids.insert(artifact_id.clone());
            }
        }
    }
}

/// 工具形成终态后释放 Applied 备用容量，但不删除已经写入 Journal 的证据。
pub(crate) fn release_file_change_reservation(control: &mut ControlState, request_id: &RequestId) {
    control.file_change_reservations.remove(request_id);
    refresh_recovery_required(control);
}

/// 回收当前文件变更未形成权威引用的 Artifact，同时保留其他并发 reservation 的内容。
///
/// 资源层的通用 `recover_for_state` 只认识 Journal 状态；这里向临时状态投影其他
/// reservation 已经物化的完整引用，避免一次失败的文件准备误删另一轮尚未提交的内容。
pub(crate) fn recover_unjournaled_file_change_artifacts(
    inner: &RuntimeSessionInner,
    control: &ControlState,
    excluded_request_id: &RequestId,
) -> Result<(), RuntimeError> {
    let mut state = inner.journal.state()?;
    let preserved = preserved_materialized_artifacts(control, excluded_request_id);
    if !preserved.is_empty() {
        let content = preserved
            .into_values()
            .map(|artifact| MessagePart::Artifact {
                artifact,
                materialization: ArtifactMaterialization::Binary,
            })
            .collect();
        state
            .transcript
            .push(TranscriptRecord::MessageAdded(SessionMessage {
                message_id: "runtime-recovery-reservation-artifacts".to_owned(),
                turn_id: None,
                agent_id: None,
                role: MessageRole::System,
                content,
            }));
    }
    inner.artifacts.recover_for_state(&inner.lease, &state)?;
    Ok(())
}

/// 汇总其他 reservation 已经物化但尚未出现在权威状态中的完整 Artifact 引用。
fn preserved_materialized_artifacts(
    control: &ControlState,
    excluded_request_id: &RequestId,
) -> BTreeMap<String, ArtifactUse> {
    let mut preserved = BTreeMap::new();
    for entry in control.reservations.values() {
        for (artifact_id, artifact) in &entry.materialized_artifact_uses {
            preserved
                .entry(artifact_id.clone())
                .or_insert_with(|| artifact.clone());
        }
    }
    for (request_id, entry) in &control.file_change_reservations {
        if request_id == excluded_request_id {
            continue;
        }
        for (artifact_id, artifact) in &entry.materialized_artifact_uses {
            preserved
                .entry(artifact_id.clone())
                .or_insert_with(|| artifact.clone());
        }
    }
    preserved
}

/// 为尚未开始写入的文件变更同时检查两条事件、快照块和全部既有 reservation。
fn ensure_file_change_capacity(
    inner: &RuntimeSessionInner,
    control: &ControlState,
    state: &SessionState,
    event_bytes: u64,
    event_records: u64,
    event_state_items: StateCollectionItems,
    event_missing_artifacts: &BTreeSet<String>,
) -> Result<(), RuntimeError> {
    if event_bytes == 0 || event_records == 0 {
        return Err(RuntimeError::TurnUnpersistable);
    }
    let protected_tool_bytes = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_bytes)
        })
        .ok_or(RuntimeError::RecoveryRequired)?;
    let protected_tool_records = control
        .reservations
        .values()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(entry.reserved_journal_records)
        })
        .ok_or(RuntimeError::RecoveryRequired)?;
    let (protected_runtime_bytes, protected_runtime_records) =
        protected_runtime_terminal_capacity(control).ok_or(RuntimeError::RecoveryRequired)?;
    let (protected_file_bytes, protected_file_records) =
        protected_file_change_journal_capacity(control).ok_or(RuntimeError::RecoveryRequired)?;
    let protected_bytes = protected_tool_bytes
        .checked_add(protected_runtime_bytes)
        .and_then(|value| value.checked_add(protected_file_bytes))
        .ok_or(RuntimeError::RecoveryRequired)?;
    let protected_records = protected_tool_records
        .checked_add(protected_runtime_records)
        .and_then(|value| value.checked_add(protected_file_records))
        .ok_or(RuntimeError::RecoveryRequired)?;
    let log_len = journal_len(&inner.journal)?;
    if log_len
        .checked_add(protected_bytes)
        .and_then(|value| value.checked_add(event_bytes))
        .is_none_or(|value| value > inner.config.journal.max_log_bytes)
        || state
            .last_sequence
            .checked_add(protected_records)
            .and_then(|value| value.checked_add(event_records))
            .is_none_or(|value| value > inner.config.journal.max_records)
    {
        return Err(RuntimeError::TurnUnpersistable);
    }
    let protected_state = protected_state_collection_items(control)
        .saturating_add(protected_file_change_state_items(control));
    if !state_collection_items(state)
        .saturating_add(protected_state)
        .saturating_add(event_state_items)
        .fits_limit(inner.config.journal.max_state_collection_items)
    {
        return Err(RuntimeError::TurnUnpersistable);
    }
    let mut protected_artifacts = control
        .reservations
        .values()
        .flat_map(|entry| entry.missing_artifact_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    protected_artifacts.extend(protected_file_change_artifacts(control));
    protected_artifacts.extend(event_missing_artifacts.iter().cloned());
    let protected_unknown_artifacts = control
        .reservations
        .values()
        .try_fold(0_usize, |total, entry| {
            total.checked_add(entry.reserved_unknown_artifacts)
        })
        .ok_or(RuntimeError::RecoveryRequired)?;
    let remaining = inner.artifacts.capacity()?.remaining();
    if protected_artifacts
        .len()
        .checked_add(protected_unknown_artifacts)
        .is_none_or(|required| required > remaining)
    {
        return Err(RuntimeError::TurnUnpersistable);
    }
    Ok(())
}

/// 返回文件变更 reservation 尚未确认的 Journal 字节和记录数。
pub(crate) fn protected_file_change_journal_capacity(control: &ControlState) -> Option<(u64, u64)> {
    control
        .file_change_reservations
        .values()
        .try_fold((0_u64, 0_u64), |(bytes, records), entry| {
            Some((
                bytes.checked_add(entry.reserved_journal_bytes)?,
                records.checked_add(entry.reserved_journal_records)?,
            ))
        })
}

/// 聚合文件变更 reservation 尚未确认的状态集合预算。
pub(crate) fn protected_file_change_state_items(control: &ControlState) -> StateCollectionItems {
    control
        .file_change_reservations
        .values()
        .fold(StateCollectionItems::default(), |total, entry| {
            total.saturating_add(entry.reserved_state_items)
        })
}

/// 聚合文件变更 reservation 尚未确认的唯一 Artifact 身份。
pub(crate) fn protected_file_change_artifacts(control: &ControlState) -> BTreeSet<String> {
    control
        .file_change_reservations
        .values()
        .flat_map(|entry| entry.missing_artifact_ids.iter().cloned())
        .collect()
}

/// 在文件变更 reservation 下追加一条已预检的幂等事件。
fn append_file_change_event(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
    request_id: &RequestId,
    phase: FileChangePhase,
    event_id: SessionEventId,
    event: SessionEvent,
) -> Result<(), RuntimeError> {
    let event_key = event_id.as_str().to_owned();
    if !recovery_gate_allows_event(control, &event_key) {
        return Err(RuntimeError::RecoveryRequired);
    }
    #[cfg(test)]
    if take_file_change_append_fault(FileChangeAppendFault::BeforeAppend(phase)) {
        return Err(ResourceError::Json("测试注入文件变更 Journal 明确失败".to_owned()).into());
    }
    let state = inner.journal.state()?;
    let event_bytes = encoded_record_len(&state.session_id, &event_id, &event)?;
    let event_state_items = state_collection_event_items(&event);
    let mut expected_sequence = state.last_sequence;
    for _ in 0..2 {
        match inner
            .journal
            .append_idempotent(event_id.clone(), expected_sequence, event.clone())
        {
            Ok(super::IdempotentAppendOutcome::Appended(receipt)) => {
                #[cfg(test)]
                if take_file_change_append_fault(FileChangeAppendFault::AfterAppend(phase)) {
                    mark_event_indeterminate(control, &event_key);
                    return Err(RuntimeError::RecoveryRequired);
                }
                if charge_file_change_event(
                    control,
                    request_id,
                    phase,
                    event_bytes,
                    event_state_items,
                )
                .is_err()
                {
                    control.hard_recovery_required = true;
                    mark_event_indeterminate(control, &event_key);
                    return Err(RuntimeError::RecoveryRequired);
                }
                if matches!(phase, FileChangePhase::Prepared) {
                    mark_file_change_artifacts_materialized(control, request_id);
                }
                if inner
                    .publisher
                    .publish_authoritative(receipt.record)
                    .is_err()
                {
                    control.hard_recovery_required = true;
                    mark_event_indeterminate(control, &event_key);
                    return Err(RuntimeError::RecoveryRequired);
                }
                mark_event_confirmed(control, &event_key);
                if matches!(phase, FileChangePhase::Applied) {
                    control.file_change_reservations.remove(request_id);
                }
                refresh_recovery_required(control);
                return Ok(());
            }
            Ok(super::IdempotentAppendOutcome::AlreadyCommitted { record }) => {
                if record.event != event {
                    control.hard_recovery_required = true;
                    mark_event_indeterminate(control, &event_key);
                    return Err(RuntimeError::RecoveryRequired);
                }
                if charge_file_change_event(
                    control,
                    request_id,
                    phase,
                    event_bytes,
                    event_state_items,
                )
                .is_err()
                {
                    control.hard_recovery_required = true;
                    mark_event_indeterminate(control, &event_key);
                    return Err(RuntimeError::RecoveryRequired);
                }
                if matches!(phase, FileChangePhase::Prepared) {
                    mark_file_change_artifacts_materialized(control, request_id);
                }
                mark_event_confirmed(control, &event_key);
                if matches!(phase, FileChangePhase::Applied) {
                    control.file_change_reservations.remove(request_id);
                }
                refresh_recovery_required(control);
                return Ok(());
            }
            Ok(super::IdempotentAppendOutcome::SequenceConflict {
                actual_sequence, ..
            }) => {
                expected_sequence = actual_sequence;
            }
            Ok(super::IdempotentAppendOutcome::EventIdConflict { .. }) => {
                control.hard_recovery_required = true;
                mark_event_indeterminate(control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
            Ok(super::IdempotentAppendOutcome::Indeterminate { .. }) => {
                mark_event_indeterminate(control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
            Err(ResourceError::CorruptReadOnly) => {
                control.hard_recovery_required = true;
                mark_event_indeterminate(control, &event_key);
                return Err(RuntimeError::RecoveryRequired);
            }
            Err(error) => return Err(RuntimeError::Resource(error)),
        }
    }
    control.hard_recovery_required = true;
    mark_event_indeterminate(control, &event_key);
    Err(RuntimeError::RecoveryRequired)
}

/// 清理尚未形成 Journal 证据的 Artifact，并释放对应 reservation。
fn abort_unjournaled_file_change(
    inner: &RuntimeSessionInner,
    control: &mut ControlState,
    request_id: &RequestId,
    error: RuntimeError,
) -> Result<(), RuntimeError> {
    match recover_unjournaled_file_change_artifacts(inner, control, request_id) {
        Ok(()) => {
            control.file_change_reservations.remove(request_id);
            refresh_recovery_required(control);
            Err(error)
        }
        Err(_) => {
            control.hard_recovery_required = true;
            refresh_recovery_required(control);
            Err(RuntimeError::RecoveryRequired)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use keencode_resources::{
        AgentId, FILE_SNAPSHOT_CHUNK_BYTES, SessionEvent, SessionEventId, SessionEventRecord,
        ToolRequest, TurnId,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::{
        CreateSessionRequest, OpenSessionResult, ReservationEntry, RoundKey, RuntimeConfig,
    };

    /// 创建最小 Runtime Session。
    fn create(root: &TempDir, session_id: &str) -> RuntimeSession {
        RuntimeSession::create_session(
            RuntimeConfig::new(root.path()),
            CreateSessionRequest {
                session_id: session_id.to_owned(),
                title: "文件变更测试".to_owned(),
                project_root: root.path().display().to_string(),
            },
        )
        .expect("Session 应创建")
    }

    /// 追加一条最小资源事件。
    fn append(session: &RuntimeSession, event_id: &str, event: SessionEvent) {
        crate::append_resource_event(
            &session.inner.journal,
            SessionEventId::new(event_id).expect("事件 ID 应有效"),
            event,
        )
        .expect("测试事件应提交");
    }

    /// 读取当前 Session Journal 中的全部完整记录，仅用于核对幂等追加次数。
    fn records(session: &RuntimeSession) -> Vec<SessionEventRecord> {
        std::fs::read_to_string(session.inner.journal.log_path())
            .expect("Journal 应读取")
            .lines()
            .map(serde_json::from_str::<SessionEventRecord>)
            .collect::<Result<Vec<_>, _>>()
            .expect("Journal 记录应解码")
    }

    /// 构造仍在运行且已经越过执行起点的 ChangesState 工具。
    fn start_tool(session: &RuntimeSession, turn_id: &str) -> RequestId {
        let turn_id = TurnId::new(turn_id).expect("Turn ID 应有效");
        let agent_id = AgentId::new("root").expect("Agent ID 应有效");
        append(
            session,
            "file-change-turn",
            SessionEvent::TurnStarted {
                turn_id: turn_id.clone(),
                source_agent_id: agent_id.clone(),
                root_turn_id: turn_id.clone(),
                parent_turn_id: None,
                prompt_summary: "文件变更测试".to_owned(),
            },
        );
        let request_id = RequestId::derive_model_tool_call(
            session.session_id(),
            &turn_id,
            &agent_id,
            1,
            "call-file-change",
        )
        .expect("工具请求 ID 应派生");
        append(
            session,
            "file-change-request",
            SessionEvent::ToolRequested {
                request: ToolRequest {
                    request_id: request_id.clone(),
                    turn_id: turn_id.clone(),
                    agent_id: agent_id.clone(),
                    model_round: 1,
                    request_index: 0,
                    model_tool_call_id: "call-file-change".to_owned(),
                    tool_name: "Write".to_owned(),
                    arguments: serde_json::json!({"path": "file.txt"}),
                    effect: ToolEffect::ChangesState,
                },
            },
        );
        append(
            session,
            "file-change-started",
            SessionEvent::ToolExecutionStarted {
                request_id: request_id.clone(),
            },
        );
        request_id
    }

    /// 失败预检不得追加 Journal、写入 Artifact 或建立文件变更 reservation。
    #[test]
    fn prepare_failure_does_not_consume_journal_or_artifact_capacity() {
        let root = TempDir::new().expect("临时目录应创建");
        let mut session = create(&root, "file-change-capacity-failure");
        let request_id = start_tool(&session, "file-change-capacity-turn");
        let before_sequence = session.snapshot().expect("状态应读取").state.last_sequence;
        let before_artifacts = session
            .inner
            .artifacts
            .capacity()
            .expect("Artifact 容量应读取")
            .committed_unique_artifacts;
        Arc::get_mut(&mut session.inner)
            .expect("Session 应无其他强引用")
            .config
            .journal
            .max_state_collection_items = 1;
        let path = root.path().join("not-written.txt").display().to_string();
        let after = vec![b'x'; FILE_SNAPSHOT_CHUNK_BYTES + 1];
        let error = session
            .prepare_file_change(&request_id, path, None, &after)
            .expect_err("状态集合容量不足时必须拒绝准备");
        assert!(matches!(error, RuntimeError::TurnUnpersistable));
        assert_eq!(
            session
                .snapshot()
                .expect("失败后状态应读取")
                .state
                .last_sequence,
            before_sequence
        );
        assert_eq!(
            session
                .inner
                .artifacts
                .capacity()
                .expect("失败后 Artifact 容量应读取")
                .committed_unique_artifacts,
            before_artifacts
        );
        assert!(
            session
                .inner
                .control
                .lock()
                .expect("控制面锁应可用")
                .file_change_reservations
                .is_empty()
        );
    }

    /// Prepared 与 Applied 的确认都应幂等，且每个事件只占用一条 Journal 记录。
    #[test]
    fn applied_confirmation_is_idempotent() {
        let root = TempDir::new().expect("临时目录应创建");
        let session = create(&root, "file-change-applied-idempotent");
        let request_id = start_tool(&session, "file-change-applied-turn");
        let path = root.path().join("not-written.txt").display().to_string();
        session
            .prepare_file_change(&request_id, path.clone(), None, b"new")
            .expect("Prepared 应提交");
        let prepared_sequence = session
            .snapshot()
            .expect("Prepared 状态应读取")
            .state
            .last_sequence;
        session
            .prepare_file_change(&request_id, path, None, b"new")
            .expect("重复 Prepared 应幂等");
        assert_eq!(
            session
                .snapshot()
                .expect("重复 Prepared 状态应读取")
                .state
                .last_sequence,
            prepared_sequence
        );
        session
            .mark_file_change_applied(&request_id)
            .expect("Applied 应提交");
        let applied_sequence = session
            .snapshot()
            .expect("Applied 状态应读取")
            .state
            .last_sequence;
        session
            .mark_file_change_applied(&request_id)
            .expect("重复 Applied 应幂等");
        assert_eq!(
            session
                .snapshot()
                .expect("重复 Applied 状态应读取")
                .state
                .last_sequence,
            applied_sequence
        );
        let evidence = session
            .current_tool_file_change(&request_id)
            .expect("文件证据应读取")
            .expect("文件证据应存在");
        assert!(evidence.applied);
        let records = std::fs::read_to_string(session.inner.journal.log_path())
            .expect("Journal 应读取")
            .lines()
            .map(serde_json::from_str::<keencode_resources::SessionEventRecord>)
            .collect::<Result<Vec<_>, _>>()
            .expect("Journal 记录应解码");
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record.event,
                    SessionEvent::ToolFileChangePrepared { .. }
                ))
                .count(),
            1
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(record.event, SessionEvent::ToolFileChangeApplied { .. }))
                .count(),
            1
        );
    }

    /// 工具终态冷恢复后仍保留 Prepared 快照证据，且不尝试写入用户文件。
    #[test]
    fn unknown_cold_reopen_retains_prepared_file_evidence() {
        let root = TempDir::new().expect("临时目录应创建");
        let session_id = "file-change-cold-unknown";
        let session = create(&root, session_id);
        let request_id = start_tool(&session, "file-change-cold-turn");
        let path = root.path().join("not-written.txt").display().to_string();
        session
            .prepare_file_change(&request_id, path, None, b"new")
            .expect("Prepared 应提交");
        drop(session);
        let reopened =
            match RuntimeSession::open_session(RuntimeConfig::new(root.path()), session_id)
                .expect("Session 应冷恢复")
            {
                OpenSessionResult::Ready(session) => session,
                OpenSessionResult::Corrupt(report) => {
                    panic!("Prepared 证据不应损坏：{:?}", report.issues)
                }
            };
        let state = reopened.snapshot().expect("冷状态应读取").state;
        let tool = state.tools.get(&request_id).expect("工具应保留");
        assert_eq!(
            tool.outcome.as_ref().map(|outcome| outcome.status),
            Some(keencode_resources::ToolCompletionStatus::SideEffectUnknown)
        );
        let evidence = reopened
            .current_tool_file_change(&request_id)
            .expect("冷恢复证据应读取")
            .expect("冷恢复应保留 Prepared 证据");
        assert!(!evidence.applied);
        assert_eq!(evidence.after.size_bytes, 3);
        assert!(!root.path().join("not-written.txt").exists());
    }

    /// Prepared 追加结果不确定时，相同正文重试只能对账已有事件，不能产生第二条记录。
    #[test]
    fn prepared_append_indeterminate_reconciles_once() {
        let root = TempDir::new().expect("临时目录应创建");
        let session = create(&root, "file-change-prepared-indeterminate");
        let request_id = start_tool(&session, "file-change-prepared-turn");
        let path = root.path().join("prepared-retry.txt").display().to_string();
        inject_file_change_append_fault(FileChangeAppendFault::AfterAppend(
            FileChangePhase::Prepared,
        ));
        assert!(matches!(
            session.prepare_file_change(&request_id, path.clone(), None, b"prepared"),
            Err(RuntimeError::RecoveryRequired)
        ));
        assert!(
            session
                .snapshot()
                .expect("待对账状态应读取")
                .recovery_required
        );
        session
            .prepare_file_change(&request_id, path, None, b"prepared")
            .expect("Prepared 重试应完成同事件对账");
        let snapshot = session.snapshot().expect("对账后状态应读取");
        assert!(!snapshot.recovery_required);
        let journal = records(&session);
        assert_eq!(
            journal
                .iter()
                .filter(|record| matches!(
                    record.event,
                    SessionEvent::ToolFileChangePrepared { .. }
                ))
                .count(),
            1
        );
    }

    /// Applied 追加结果不确定时，必须保留 Prepared 证据并允许按 Applied 身份热对账。
    #[test]
    fn applied_append_indeterminate_preserves_prepared_and_reconciles_once() {
        let root = TempDir::new().expect("临时目录应创建");
        let session = create(&root, "file-change-applied-indeterminate");
        let request_id = start_tool(&session, "file-change-applied-indeterminate-turn");
        let path = root.path().join("applied-retry.txt").display().to_string();
        session
            .prepare_file_change(&request_id, path, None, b"applied")
            .expect("Prepared 应提交");
        inject_file_change_append_fault(FileChangeAppendFault::AfterAppend(
            FileChangePhase::Applied,
        ));
        assert!(matches!(
            session.mark_file_change_applied(&request_id),
            Err(RuntimeError::RecoveryRequired)
        ));
        let pending = session.snapshot().expect("Applied 待对账状态应读取");
        assert!(pending.recovery_required);
        assert_eq!(pending.pending_indeterminate_events, 1);
        assert!(
            session
                .current_tool_file_change(&request_id)
                .expect("待对账证据应读取")
                .expect("文件变更证据应存在")
                .applied
        );
        session
            .mark_file_change_applied(&request_id)
            .expect("Applied 重试应按同一身份完成对账");
        let snapshot = session.snapshot().expect("Applied 对账后状态应读取");
        assert!(!snapshot.recovery_required);
        let journal = records(&session);
        assert_eq!(
            journal
                .iter()
                .filter(|record| matches!(
                    record.event,
                    SessionEvent::ToolFileChangePrepared { .. }
                ))
                .count(),
            1
        );
        assert_eq!(
            journal
                .iter()
                .filter(|record| matches!(record.event, SessionEvent::ToolFileChangeApplied { .. }))
                .count(),
            1
        );
    }

    /// Prepared 明确失败时只回收本次文件变更孤儿，不得误删其他 Round reservation 的 Artifact。
    #[test]
    fn prepared_failure_reclaims_only_own_orphan_artifact() {
        let root = TempDir::new().expect("临时目录应创建");
        let session = create(&root, "file-change-scoped-cleanup");
        let request_id = start_tool(&session, "file-change-scoped-cleanup-turn");
        let other = session
            .put_artifact(b"other-round-artifact", None)
            .expect("其他 Round Artifact 应先物化")
            .as_event_use();
        let other_key = RoundKey {
            session_id: session.session_id().as_str().to_owned(),
            turn_id: "other-round".to_owned(),
            agent_id: "root".to_owned(),
            model: "test".to_owned(),
            model_round: 1,
            segment_index: 0,
        };
        {
            let mut control = session.inner.control.lock().expect("控制面锁应可用");
            control.reservations.insert(
                other_key,
                ReservationEntry {
                    token: 1,
                    known_content_sha256: "other".to_owned(),
                    pre_tool_context_count: 0,
                    reserved_journal_bytes: 0,
                    reserved_journal_records: 0,
                    tool_request_sha256: BTreeMap::new(),
                    missing_artifact_ids: BTreeSet::new(),
                    materialized_artifact_uses: BTreeMap::from([(
                        other.artifact_id.as_str().to_owned(),
                        other.clone(),
                    )]),
                    reserved_unknown_artifacts: 0,
                    materialized_artifact_ids: BTreeSet::from([other
                        .artifact_id
                        .as_str()
                        .to_owned()]),
                    committed_event_ids: BTreeSet::new(),
                    reserved_state_items: StateCollectionItems::default(),
                    retained_event: None,
                    abandoned_after_progress: false,
                },
            );
        }
        inject_file_change_append_fault(FileChangeAppendFault::BeforeAppend(
            FileChangePhase::Prepared,
        ));
        let path = root.path().join("scoped-cleanup.txt").display().to_string();
        assert!(matches!(
            session.prepare_file_change(&request_id, path, None, b"own-orphan"),
            Err(RuntimeError::Resource(_))
        ));
        assert_eq!(
            session
                .inner
                .artifacts
                .capacity()
                .expect("清理后 Artifact 容量应读取")
                .committed_unique_artifacts,
            1
        );
        assert_eq!(
            session
                .inner
                .artifacts
                .read_use(&other)
                .expect("其他 Round Artifact 不得被清理"),
            b"other-round-artifact"
        );
        assert!(
            session
                .inner
                .control
                .lock()
                .expect("控制面锁应可用")
                .file_change_reservations
                .is_empty()
        );
    }

    /// 工具取消形成终态时释放 Applied 备用容量，但 Prepared 快照仍保持可读取证据。
    #[test]
    fn terminal_release_keeps_prepared_evidence() {
        let root = TempDir::new().expect("临时目录应创建");
        let session = create(&root, "file-change-terminal-release");
        let request_id = start_tool(&session, "file-change-terminal-release-turn");
        let path = root
            .path()
            .join("terminal-release.txt")
            .display()
            .to_string();
        session
            .prepare_file_change(&request_id, path, None, b"retained")
            .expect("Prepared 应提交");
        {
            let mut control = session.inner.control.lock().expect("控制面锁应可用");
            release_file_change_reservation(&mut control, &request_id);
            assert!(control.file_change_reservations.is_empty());
        }
        let evidence = session
            .current_tool_file_change(&request_id)
            .expect("终态后的证据应读取")
            .expect("Prepared 证据应保留");
        assert!(!evidence.applied);
        assert_eq!(
            session
                .read_file_snapshot(&evidence.after)
                .expect("Prepared 快照应可读取"),
            b"retained"
        );
    }
}
