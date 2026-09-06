//! 文件工具、权威快照与标准 ACP Diff 的桌面装配，不读取工作区来重建历史。

use std::fmt;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use keencode_acp::{
    FILE_CHANGE_META_KEY, FileChangeReference, FileChangeSide, FileSnapshotInfo,
    ReadFileChangeRequest, ReadFileChangeResponse, schema,
};
use keencode_agent::{ToolContext, ToolError};
use keencode_resources::{RequestId, SessionEventRecord, SessionState, ToolEffect, ToolFileChange};
use keencode_runtime::RuntimeSession;
use keencode_tools::{FileMutationRecorder, PreparedFileMutation};

use super::{AgentRuntime, AgentRuntimeError, DeliveryDraft, session_update_draft, tool_request};

/// 内联快照的原始和编码后预算；较大或二进制快照仅通过资源引用按需读取。
const INLINE_FILE_CHANGE_BYTES: u64 = 32 * 1024;
/// 单次 Diff JSON 内容预算，为整个 ACP 投递及工具状态预留足够空间。
const INLINE_FILE_CHANGE_JSON_BYTES: usize = 64 * 1024;

/// 绑定单个 Session 的真实文件变更记录器。
pub(super) struct RuntimeFileMutationRecorder {
    /// 唯一权威 Session；不另建文件历史存储。
    session: RuntimeSession,
}

impl RuntimeFileMutationRecorder {
    /// 在生产工具环境中绑定当前 Session。
    pub(super) fn new(session: RuntimeSession) -> Self {
        Self { session }
    }
}

impl fmt::Debug for RuntimeFileMutationRecorder {
    /// 调试输出只包含 Session 标识，不打印文件正文。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeFileMutationRecorder")
            .field("session_id", &self.session.session_id())
            .finish()
    }
}

/// Prepared 已提交后的精确调用句柄；工具终态由 Runtime 释放剩余 reservation。
struct RuntimePreparedFileMutation {
    /// 保存 Prepared 的权威 Session。
    session: RuntimeSession,
    /// 精确到 Agent、Turn 和 Round 的资源层工具请求标识。
    request_id: RequestId,
}

impl PreparedFileMutation for RuntimePreparedFileMutation {
    /// 只有文件原子替换成功后，工具才能调用该入口提交 Applied。
    fn mark_applied(&self) -> Result<(), ToolError> {
        self.session
            .mark_file_change_applied(&self.request_id)
            .map_err(|_| recording_error())
    }
}

impl FileMutationRecorder for RuntimeFileMutationRecorder {
    /// 绑定可信工具上下文并在任何文件副作用前可靠保存原始前后快照。
    fn prepare(
        &self,
        context: &ToolContext,
        path: &Path,
        before: Option<&[u8]>,
        after: &[u8],
    ) -> Result<Box<dyn PreparedFileMutation>, ToolError> {
        if context.cancellation.is_cancelled()
            || context.session_id.as_str() != self.session.session_id().as_str()
        {
            return Err(recording_error());
        }
        let snapshot = self.session.snapshot().map_err(|_| recording_error())?;
        let mut candidates = snapshot.state.tools.values().filter(|tool| {
            let request = &tool.request;
            request.turn_id.as_str() == context.turn_id.as_str()
                && request.agent_id.as_str() == context.source_agent_id.as_str()
                && request.model_tool_call_id == context.tool_call_id.as_str()
                && matches!(request.tool_name.as_str(), "Write" | "Edit")
                && request.effect == ToolEffect::ChangesState
                && tool.execution_started
                && tool.outcome.is_none()
        });
        let request_id = candidates
            .next()
            .ok_or_else(recording_error)?
            .request
            .request_id
            .clone();
        if candidates.next().is_some() {
            // 模型调用 ID 可跨 Round 复用，但同一上下文不能同时对应两个 Started 请求。
            return Err(recording_error());
        }
        let path = path.to_str().ok_or_else(recording_error)?.to_owned();
        self.session
            .prepare_file_change(&request_id, path, before, after)
            .map_err(|_| recording_error())?;
        Ok(Box::new(RuntimePreparedFileMutation {
            session: self.session.clone(),
            request_id,
        }))
    }
}

/// 记录失败不可伪造文件写入成功，也不将路径或内容写进错误通知。
fn recording_error() -> ToolError {
    ToolError::permanent(
        "file_change_recording_failed",
        "文件变更证据无法可靠提交，请检查会话恢复状态",
    )
}

impl AgentRuntime {
    /// 读取 Host 已授权 Session 的持久快照页；参数不包含任意磁盘路径。
    pub fn read_file_change(
        &self,
        request: ReadFileChangeRequest,
    ) -> Result<ReadFileChangeResponse, AgentRuntimeError> {
        request
            .validate()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let session = self
            .runtime_manager
            .get(request.session_id.clone())
            .map_err(|_| AgentRuntimeError::SessionUnavailable)?;
        read_file_change_page(&session, request)
    }
}

/// 先从权威工具生命周期选择快照，随后按原始字节区间读取并编码。
fn read_file_change_page(
    session: &RuntimeSession,
    request: ReadFileChangeRequest,
) -> Result<ReadFileChangeResponse, AgentRuntimeError> {
    request
        .validate()
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    if request.session_id != session.session_id().as_str() {
        return Err(AgentRuntimeError::SessionUnavailable);
    }
    let request_id = RequestId::new(request.request_id.clone())
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    let change = session
        .current_tool_file_change(&request_id)
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
        .ok_or(AgentRuntimeError::SessionUnavailable)?;
    let snapshot = match request.side {
        FileChangeSide::Before => change
            .before
            .as_ref()
            .ok_or(AgentRuntimeError::SessionUnavailable)?,
        FileChangeSide::After => &change.after,
    };
    let bytes = session
        .read_file_snapshot_range(snapshot, request.offset, request.length as usize)
        .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
    let end = request
        .offset
        .checked_add(bytes.len() as u64)
        .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
    Ok(ReadFileChangeResponse {
        session_id: request.session_id,
        request_id: request.request_id,
        side: request.side,
        offset: request.offset,
        total_bytes: snapshot.size_bytes,
        sha256: snapshot.sha256.clone(),
        data: STANDARD.encode(bytes),
        eof: end == snapshot.size_bytes,
    })
}

/// 构造不含正文或内部 Artifact 路径的持久文件变更引用。
fn change_reference(
    session: &RuntimeSession,
    request_id: &RequestId,
    change: &ToolFileChange,
) -> FileChangeReference {
    let info = |snapshot: &keencode_resources::FileSnapshot| FileSnapshotInfo {
        size_bytes: snapshot.size_bytes,
        sha256: snapshot.sha256.clone(),
    };
    FileChangeReference {
        session_id: session.session_id().as_str().to_owned(),
        request_id: request_id.as_str().to_owned(),
        path: change.path.clone(),
        before: change.before.as_ref().map(info),
        after: info(&change.after),
        applied: change.applied,
    }
}

/// 小型已应用 UTF-8 变更使用标准 Diff，其余使用标准 ResourceLink 和命名空间元数据。
fn change_content(
    session: &RuntimeSession,
    request_id: &RequestId,
    change: &ToolFileChange,
) -> Result<Vec<schema::ToolCallContent>, AgentRuntimeError> {
    let total_bytes = change
        .before
        .as_ref()
        .map_or(0, |before| before.size_bytes)
        .saturating_add(change.after.size_bytes);
    if change.applied && total_bytes <= INLINE_FILE_CHANGE_BYTES {
        let before = change
            .before
            .as_ref()
            .map(|before| session.read_file_snapshot(before))
            .transpose()
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let after = session
            .read_file_snapshot(&change.after)
            .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?;
        let before_text = before.map(String::from_utf8).transpose();
        let after_text = String::from_utf8(after);
        if let (Ok(before_text), Ok(after_text)) = (before_text, after_text)
            && !before_text
                .as_ref()
                .is_some_and(|value| value.contains('\0'))
            && !after_text.contains('\0')
        {
            let content = vec![schema::ToolCallContent::Diff(
                schema::Diff::new(&change.path, after_text).old_text(before_text),
            )];
            if serde_json::to_vec(&content)
                .map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?
                .len()
                <= INLINE_FILE_CHANGE_JSON_BYTES
            {
                return Ok(content);
            }
        }
    }
    let reference = change_reference(session, request_id, change);
    // Session 和 Request ID 已由资源层约束为单个安全 ASCII 路径段。
    let uri = format!(
        "keencode://sessions/{}/file-changes/{}",
        reference.session_id, reference.request_id
    );
    let mut meta = schema::Meta::new();
    meta.insert(
        FILE_CHANGE_META_KEY.to_owned(),
        serde_json::to_value(reference).map_err(|_| AgentRuntimeError::RuntimeOperationFailed)?,
    );
    Ok(vec![schema::ToolCallContent::Content(
        schema::Content::new(schema::ContentBlock::ResourceLink(
            schema::ResourceLink::new("文件变更快照", uri)
                .meta(meta)
                .description(if change.applied {
                    "已应用的持久文件快照"
                } else {
                    "已准备快照，尚未确认文件应用结果"
                }),
        )),
    )])
}

/// 工具终态更新始终携带权威快照，确保 live、在途恢复和 Transcript 冷重放语义一致。
pub(super) fn with_change_content(
    session: &RuntimeSession,
    state: &SessionState,
    request_id: &RequestId,
    mut fields: schema::ToolCallUpdateFields,
) -> Result<schema::ToolCallUpdateFields, AgentRuntimeError> {
    match state
        .tools
        .get(request_id)
        .and_then(|tool| tool.file_change.as_ref())
    {
        Some(change) => {
            let mut content = fields.content.take().unwrap_or_default();
            content.extend(change_content(session, request_id, change)?);
            Ok(fields.content(content))
        }
        None => Ok(fields),
    }
}

/// 以标准工具更新投递 Prepared/Applied，不把准备阶段误报成实际文件变更。
pub(super) fn change_update_drafts(
    session: &RuntimeSession,
    state: &SessionState,
    record: &SessionEventRecord,
    request_id: &RequestId,
    change: &ToolFileChange,
) -> Result<Vec<DeliveryDraft>, AgentRuntimeError> {
    let request = tool_request(state, request_id.as_str())?;
    Ok(vec![session_update_draft(
        record,
        Some(request.turn_id.as_str()),
        Some(request.agent_id.as_str()),
        schema::SessionUpdate::ToolCallUpdate(schema::ToolCallUpdate::new(
            request.model_tool_call_id.clone(),
            schema::ToolCallUpdateFields::new()
                .content(change_content(session, request_id, change)?),
        )),
    )])
}

#[cfg(test)]
mod tests;
