//! 文件工具、权威快照与标准 ACP Diff 的桌面装配，不读取工作区来重建历史。

use std::fmt;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use keencode_acp::{FileChangeSide, ReadFileChangeRequest, ReadFileChangeResponse};
use keencode_agent::{ToolContext, ToolError};
use keencode_resources::{RequestId, ToolEffect, existing_file_readonly};
use keencode_runtime::RuntimeSession;
use keencode_tools::{FileMutationRecorder, PreparedFileMutation};

use super::{AgentRuntime, AgentRuntimeError};

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
        let before_readonly =
            existing_file_readonly(path.as_ref()).map_err(|_| recording_error())?;
        #[cfg(windows)]
        let after_readonly = Some(before_readonly.unwrap_or(false));
        #[cfg(not(windows))]
        let after_readonly = None;
        self.session
            .prepare_file_change_with_readonly(
                &request_id,
                path,
                before,
                after,
                before_readonly,
                after_readonly,
            )
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

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use keencode_runtime::change_content;
