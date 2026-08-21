//! Prompt 入口：参数转换 + tokio::spawn 调度。

use std::sync::Arc;

use agent_client_protocol::{
    schema::v1::{PromptRequest, PromptResponse, StopReason},
    Client, ConnectionTo, Responder,
};
use peri_acp_types::messages::{ContentBlock as PeriContentBlock, MessageContent};
use tokio_util::sync::CancellationToken;

use super::super::context::StdioContext;
use super::prompt_exec::{self, PromptExecParams};

/// session/prompt 处理器（薄入口）。
///
/// 内容转换、捕获会话数据、设置取消令牌、提取 AgentPool 后，
/// 通过 `tokio::spawn` 将重活转交 `prompt_exec::run()`，
/// 保持事件循环对 session/cancel 的响应性。
///
/// 接收 `&Arc<StdioContext>` 以便 `Arc::clone` 进入后台任务。
pub(crate) async fn handle_prompt(
    ctx: &Arc<StdioContext>,
    req: PromptRequest,
    responder: Responder<PromptResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), agent_client_protocol::Error> {
    let sid = req.session_id.0.to_string();
    // Convert ACP SDK ContentBlocks to peri-agent MessageContent
    let content = if req.prompt.is_empty() {
        MessageContent::text("")
    } else {
        let blocks: Vec<PeriContentBlock> = req
            .prompt
            .iter()
            .filter_map(|b| match b {
                agent_client_protocol::schema::v1::ContentBlock::Text(t) => {
                    Some(PeriContentBlock::text(&t.text))
                }
                agent_client_protocol::schema::v1::ContentBlock::Image(img) => {
                    Some(PeriContentBlock::image_base64(&img.mime_type, &img.data))
                }
                _ => None, // Audio/ResourceLink/Resource not supported yet
            })
            .collect();
        if blocks.is_empty() {
            MessageContent::text("")
        } else {
            MessageContent::Blocks(blocks)
        }
    };

    // Install cancellation before queueing behind another prompt.  The
    // The capability/frozen snapshot itself is intentionally delayed until after
    // the per-session prompt lock is acquired below so queued prompts cannot
    // observe stale session state.
    let cancel = CancellationToken::new();
    {
        let mut sessions = ctx.sessions.write();
        let Some(session) = sessions.get_mut(&sid) else {
            let _ = responder.respond(PromptResponse::new(StopReason::EndTurn));
            return Ok(());
        };
        session.cancel_token = Some(cancel.clone());
    }

    let prompt_lock = {
        let mut locks = ctx.prompt_locks.lock().await;
        locks
            .entry(sid.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };

    // Capture all mutable session state only after the same lock used by
    // session/set_config_option. This keeps frozen capability and history from
    // becoming stale while the prompt is queued.
    let ctx_for_task = Arc::clone(ctx);
    let cx_for_task = cx.clone();
    let session_id = req.session_id.clone();

    // Spawn the heavy work to keep the event loop free for session/cancel.
    tokio::spawn(async move {
        let _prompt_guard = prompt_lock.lock_owned().await;

        let (agent_cwd, history, session_start_source, thread_id, frozen, pool, peri_caps) = {
            let mut sessions = ctx_for_task.sessions.write();
            let Some(session) = sessions.get_mut(&sid) else {
                let _ = responder.respond(PromptResponse::new(StopReason::EndTurn));
                return;
            };
            let history = session.history.clone();
            let session_start_source = if history.is_empty() {
                Some("startup".to_string())
            } else {
                None
            };
            let pool = std::mem::take(&mut session.agent_pool);
            (
                session.cwd.clone(),
                history,
                session_start_source,
                session.thread_id.clone(),
                session.frozen.clone(),
                pool,
                ctx_for_task.session_manager.get_caps(&sid),
            )
        };
        let history_len = history.len();
        let pool_arc = Arc::new(parking_lot::Mutex::new(pool));

        let params = PromptExecParams {
            ctx: ctx_for_task,
            cx: cx_for_task,
            session_id,
            sid,
            agent_cwd,
            content,
            frozen,
            history,
            session_start_source,
            history_len,
            cancel,
            pool: pool_arc,
            thread_id,
            responder,
            peri_caps,
        };
        prompt_exec::run(params).await;
    });

    // Return immediately: the event loop remains responsive while this
    // prompt waits for the per-session lock or runs the model turn.
    Ok(())
}
