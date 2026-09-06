//! ACP Host 的文件变更快照读取路由。
//!
//! 路由只接受已通过 ACP 边界验证的身份和范围，并让 Agent Runtime 从
//! Session 权威 Artifact 中读取正文；这里不根据客户端提供的路径访问工作区。

use super::{AcpHost, HostFailure, map_runtime_failure};
use crate::session_commands::{authorized_metadata, open_authorized_session};
use keencode_acp::{ReadFileChangeRequest, ReadFileChangeResponse};
use std::sync::Arc;

/// 在当前项目授权和 Session 作用域内读取一页持久文件变更快照。
pub(super) async fn read(
    host: &AcpHost,
    request: ReadFileChangeRequest,
) -> Result<ReadFileChangeResponse, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    // 先通过项目登记表确认 Session 所属根目录，再打开同一授权 Session；
    // Runtime 的读取 API 仍只接收 Session、工具请求和快照侧，不接收磁盘路径。
    let session_id = request.session_id.clone();
    let request_id = request.request_id.clone();
    let side = request.side;
    let offset = request.offset;
    let (_, _) = authorized_metadata(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let _session = open_authorized_session(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let runtime = Arc::clone(&host.runtime);
    let response = tokio::task::spawn_blocking(move || runtime.read_file_change(request))
        .await
        .map_err(|_| HostFailure::Internal)?
        .map_err(map_runtime_failure)?;
    // Runtime 返回的身份和页坐标必须仍与已授权请求一致，避免把其他快照
    // 的成功值包装成当前请求的响应。
    if response.session_id != session_id
        || response.request_id != request_id
        || response.side != side
        || response.offset != offset
    {
        return Err(HostFailure::Internal);
    }
    Ok(response)
}
