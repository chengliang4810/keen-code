//! ACP Host 的 KeenCode 扩展请求路由。
//!
//! 该模块只处理已经由 [`keencode_acp::AcpRequestDecoder`] 验证过的 DTO，所有
//! 成功值仍通过父 Host 的封闭响应编码器输出。它不为尚未存在 Runtime 业务
//! 实现的能力制造“已接受”或“已完成”状态。

use super::{AcpHost, HostFailure, map_runtime_failure};
use crate::agent_runtime::{BackgroundTaskCancellationOutcome, RuntimeMcpServerSnapshot};
use crate::session_commands::{
    authorized_metadata, close_session_for_mutation, open_authorized_session,
    restore_session_after_mutation, retry_session_mutation,
};
use keencode_acp::schema;
use keencode_acp::{
    AcpRequest, CancelBackgroundTaskResponse, GoalClearResponse, GoalGetResponse,
    GoalMutationResponse, McpConnectionStatus, McpListResponse, McpOAuthCallbackRequest,
    McpOAuthStatus, McpRuntimePhase, McpServerStatus, McpTransportKind, RenameSessionResponse,
    ReplaySessionResponse, RewindCandidate, RewindCandidatesResponse, RewindSessionResponse,
    SteerSessionResponse, ValidateAcpParams,
};
use keencode_resources::{
    GoalDocument, GoalFileStore, GoalRecord as ResourceGoalRecord,
    GoalStatus as ResourceGoalStatus, MessagePart, MessageRole, ROOT_AGENT_ID, ScopeId,
    SessionEvent, SessionId, SessionMessage, project_scope_id,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

/// 单次回退候选读取使用的有界 Journal 页大小。
const REPLAY_PAGE_SIZE: usize = 1_000;
/// 回退候选响应的协议上限；与 ACP Schema 当前版本保持一致。
const MAX_REWIND_CANDIDATES: usize = 1_000;
/// 候选预览的本地展示上限，避免把整段用户正文复制进响应。
const MAX_CANDIDATE_PREVIEW_CHARS: usize = 1_024;
/// MCP 配置只读查看的文件上限；该入口不允许把配置当成无限制输入。
const MAX_MCP_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

/// Goal upsert 幂等收据绑定的完整请求载荷；期望 revision 不能被省略。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalUpsertOperation {
    /// 操作格式版本。
    operation: &'static str,
    /// 调用方声明的 CAS 期望 revision。
    expected_revision: u64,
    /// 用户提交的完整 Goal 字段。
    goal: keencode_acp::GoalInput,
}

/// Goal transition 幂等收据绑定的完整请求载荷。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalTransitionOperation<'a> {
    /// 操作格式版本。
    operation: &'static str,
    /// 调用方声明的 CAS 期望 revision。
    expected_revision: u64,
    /// 当前 Goal 的稳定标识。
    goal_id: &'a str,
    /// 请求的不可逆目标状态。
    status: ResourceGoalStatus,
    /// blocked 状态原因；completed 时为 null。
    reason: Option<&'a str>,
    /// completed 状态证据；blocked 时为 null。
    completion_evidence: Option<&'a str>,
}

/// Goal clear 幂等收据绑定的实际被清除 Goal 与 CAS 期望 revision。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GoalClearOperation<'a> {
    /// 操作格式版本。
    operation: &'static str,
    /// 调用方声明的 CAS 期望 revision。
    expected_revision: u64,
    /// 本次实际清除的 Goal 标识；ACP 请求未携带此字段，由当前文档解析。
    cleared_goal_id: &'a str,
}

/// 分发一个已经严格解码的 KeenCode 扩展请求。
pub(super) async fn dispatch(
    host: &AcpHost,
    id: schema::RequestId,
    request: AcpRequest,
) -> Result<Value, HostFailure> {
    match request {
        AcpRequest::SteerSession(request) => dispatch_steer(host, id, request),
        AcpRequest::RenameSession(request) => dispatch_rename(host, id, request).await,
        AcpRequest::GenerateSessionTitle(request) => {
            dispatch_generate_title(host, id, request).await
        }
        AcpRequest::RewindCandidates(request) => dispatch_rewind_candidates(host, id, request),
        AcpRequest::RewindSession(request) => dispatch_rewind(host, id, request).await,
        AcpRequest::ReplaySession(request) => dispatch_replay(host, id, request).await,
        AcpRequest::CancelBackgroundTask(request) => dispatch_background_cancel(host, id, request),
        AcpRequest::ListBackgroundTasks(request) => dispatch_background_list(host, id, request),
        AcpRequest::GoalGet(request) => dispatch_goal_get(host, id, request),
        AcpRequest::GoalUpsert(request) => dispatch_goal_upsert(host, id, request),
        AcpRequest::GoalTransition(request) => dispatch_goal_transition(host, id, request),
        AcpRequest::GoalClear(request) => dispatch_goal_clear(host, id, request),
        AcpRequest::McpList(request) => dispatch_mcp_list(host, id, request).await,
        AcpRequest::McpOAuthStart(request) => dispatch_mcp_oauth_start(host, id, request).await,
        AcpRequest::McpOAuthCallback(request) => {
            dispatch_mcp_oauth_callback(host, id, request).await
        }
        AcpRequest::McpOAuthCancel(request) => dispatch_mcp_oauth_cancel(host, id, request).await,
        _ => Err(HostFailure::MethodNotFound),
    }
}

/// 接通用户 steer；只有 Runtime 实际写入动态输入后才返回 `accepted=true`。
fn dispatch_steer(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::SteerSessionRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let session_id = request.session_id;
    let operation_id = request_operation_id(request.meta.as_ref())?;
    let _session = open_authorized_session(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    host.runtime
        .steer_root_turn(&session_id, &operation_id, &request.text)
        .map_err(map_runtime_failure)?;
    let response = SteerSessionResponse::new(session_id);
    host.result_value(id, &response)
}

/// 接通标题变更，并把 Runtime 实际提交的标题与 Journal 水位编码返回。
async fn dispatch_rename(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::RenameSessionRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let _control = host.control_gate.lock().await;
    let session_id = request.session_id;
    let operation_id = request_operation_id(request.meta.as_ref())?;
    let session = open_authorized_session(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let response = rename_session_with_receipt(&session, session_id, &operation_id, request.title)?;
    host.result_value(id, &response)
}

/// 通过 Session 绑定的 Provider 生成候选，稳定操作身份复用 Runtime 持久缓存。
async fn dispatch_generate_title(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::GenerateSessionTitleRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let operation_id = request_operation_id(request.meta.as_ref())?;
    let _session = open_authorized_session(&host.runtime, &host.app, &request.session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let title = host
        .runtime
        .generate_title(&request.session_id, &operation_id, &request.user_message)
        .await
        .map_err(map_runtime_failure)?;
    host.result_value(id, &keencode_acp::GenerateSessionTitleResponse { title })
}

/// 提交或恢复一次标题变更的权威响应；重试只复用正文完全匹配的 Journal 收据。
fn rename_session_with_receipt(
    session: &keencode_runtime::RuntimeSession,
    session_id: String,
    operation_id: &str,
    requested_title: String,
) -> Result<RenameSessionResponse, HostFailure> {
    if let Some(record) = session
        .committed_control_event(operation_id)
        .map_err(|_| HostFailure::Internal)?
    {
        let same_request = matches!(
            &record.event,
            SessionEvent::SessionRenamed { title } if title == &requested_title
        );
        if !same_request {
            return Err(HostFailure::InvalidParams);
        }
        return Ok(RenameSessionResponse {
            session_id,
            title: requested_title,
            journal_sequence: record.sequence,
        });
    }

    let state = session
        .rename(operation_id, requested_title)
        .map_err(|_| HostFailure::Internal)?;
    Ok(RenameSessionResponse {
        session_id,
        title: state.title,
        journal_sequence: state.last_sequence,
    })
}

/// 查询可用于真实回退的用户消息锚点；该查询只读 Journal，不改变投递世代。
fn dispatch_rewind_candidates(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::RewindCandidatesRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let session_id = request.session_id;
    let session = open_authorized_session(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let candidates = collect_rewind_candidates(&session)?;
    let response = RewindCandidatesResponse {
        session_id,
        candidates,
    };
    host.result_value(id, &response)
}

/// 将当前 Session 回退到指定的根用户消息锚点之前。
///
/// 目标正文必须在关闭 Runtime 前从同一条权威 AtomicBatch 读取并绑定到资源层
/// 事务；这样重试时既不会依赖“最后一条消息”，也不会把动态 User 段误当作可编辑
/// 的根输入。
async fn dispatch_rewind(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::RewindSessionRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let _control = host.control_gate.lock().await;
    let session_id = request.session_id;
    let operation_id = request_operation_id(request.meta.as_ref())?;
    let _session = open_authorized_session(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;

    let context = close_session_for_mutation(&host.runtime, &host.app, &session_id)
        .await
        .map_err(|_| HostFailure::InvalidParams)?;
    let source_session_id =
        SessionId::new(session_id.clone()).map_err(|_| HostFailure::InvalidParams)?;
    let mutation = keencode_resources::SessionEditUserRequest {
        source_session_id,
        target_message_id: request.target_message_id,
        expected_text: request.expected_text,
        operation_id,
    };
    let mutation_result = retry_session_mutation(|| {
        host.runtime
            .runtime_manager()
            .prepare_edit_user_closed_session(mutation.clone())
    })
    .await;
    let restored = restore_session_after_mutation(&host.runtime, &session_id, &context);
    let mutation_result = match (mutation_result, restored) {
        (Ok(result), Ok(())) => result,
        (Err(_), Ok(())) => return Err(HostFailure::Internal),
        (Ok(_), Err(_)) | (Err(_), Err(_)) => return Err(HostFailure::Internal),
    };

    let snapshot = host
        .runtime
        .session_snapshot(&session_id)
        .map_err(map_runtime_failure)?;
    if snapshot.state.last_sequence == 0 {
        return Err(HostFailure::Internal);
    }
    let response = RewindSessionResponse {
        session_id,
        archived_session_id: mutation_result.archived_session_id.as_str().to_owned(),
        through_journal_sequence: snapshot.state.last_sequence,
        reverted_files: false,
    };
    host.result_value(id, &response)
}

/// 接通 Runtime 已实现的分页重放，并确保按需建立当前 Session 投递世代。
async fn dispatch_replay(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::ReplaySessionRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let _control = host.control_gate.lock().await;
    let session_id = request.session_id;
    let _session = open_authorized_session(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    host.runtime
        .ensure_session_delivery(&session_id)
        .map_err(map_runtime_failure)?;
    let response: ReplaySessionResponse = host
        .runtime
        .replay_session(&session_id, request.after, request.limit as usize)
        .await
        .map_err(map_runtime_failure)?;
    host.result_value(id, &response)
}

/// 只查询明确授权的 Session，避免焦点变化或其他 Session 账本影响当前列表。
fn dispatch_background_list(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::ListBackgroundTasksRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let _session = open_authorized_session(&host.runtime, &host.app, &request.session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let tasks = host
        .runtime
        .background_tasks_list(&request.session_id)
        .map_err(map_runtime_failure)?;
    host.result_value(
        id,
        &keencode_acp::ListBackgroundTasksResponse {
            session_id: request.session_id,
            tasks,
        },
    )
}

/// 精确授权并取消一个后台任务；没有任务时返回明确的 `cancelled=false`。
fn dispatch_background_cancel(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::CancelBackgroundTaskRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let session_id = request.session_id;
    let task_id = request.task_id;
    let _session = open_authorized_session(&host.runtime, &host.app, &session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let outcome = host
        .runtime
        .background_task_cancel_outcome(&session_id, &task_id)
        .map_err(map_runtime_failure)?;
    let response = CancelBackgroundTaskResponse {
        session_id,
        task_id,
        cancelled: cancellation_was_requested(outcome),
    };
    host.result_value(id, &response)
}

/// 只有底层本次首次发出取消信号时才报告 `cancelled=true`。
///
/// 不先读取后台任务列表，避免任务在“列表读取”和实际取消之间结束而产生
/// 虚假的成功；`AlreadyRequested` 和 `NotRunning` 都保留 Runtime 的真实结果。
fn cancellation_was_requested(outcome: BackgroundTaskCancellationOutcome) -> bool {
    matches!(outcome, BackgroundTaskCancellationOutcome::Requested)
}

/// 查询项目 Goal；Session 只作为项目授权锚点，不把 Goal 复制进 Session Journal。
fn dispatch_goal_get(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::GoalGetRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let session_id = request.session_id;
    let (store, scope) = goal_store_and_scope(host, &session_id)?;
    let document = store.read(&scope).map_err(|_| HostFailure::Internal)?;
    let response = GoalGetResponse {
        session_id,
        revision: document.as_ref().map_or(0, |value| value.revision),
        goal: document.and_then(|value| value.goal).map(acp_goal_record),
    };
    host.result_value(id, &response)
}

/// 创建或更新项目当前唯一 active Goal，并保留资源层 CAS 与幂等收据。
fn dispatch_goal_upsert(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::GoalUpsertRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let session_id = request.session_id;
    let request_nonce = request.request_nonce;
    let goal_input = request.goal;
    let expected_revision = request.expected_revision;
    let (store, scope) = goal_store_and_scope(host, &session_id)?;
    let current = store.read(&scope).map_err(|_| HostFailure::Internal)?;
    let operation = goal_upsert_operation(expected_revision, &goal_input);
    if let Some(document) = current.as_ref()
        && let Some(result_revision) = document
            .applied_operation_revision(&request_nonce, &operation)
            .map_err(|_| HostFailure::InvalidParams)?
    {
        if result_revision != document.revision {
            // 资源层当前收据只保存 result_revision，不保存完整结果快照。后续
            // Goal 变更后不能拿“当前 Goal”冒充原操作事实，必须 fail-closed，
            // 由上层补齐结果快照后才能安全恢复该旧响应。
            return Err(HostFailure::Internal);
        }
        let existing = document.goal.as_ref().ok_or(HostFailure::InvalidParams)?;
        let response = GoalMutationResponse {
            session_id,
            revision: result_revision,
            goal: acp_goal_record(existing.clone()),
            deduplicated: true,
        };
        return host.result_value(id, &response);
    }
    if current.as_ref().map_or(0, |value| value.revision) != expected_revision {
        return Err(HostFailure::InvalidParams);
    }

    let timestamp = crate::analytics::now_ms();
    let next_goal = match current.as_ref().and_then(|value| value.goal.as_ref()) {
        Some(existing) => {
            let fields_changed = existing.title != goal_input.title
                || existing.objective != goal_input.objective
                || existing.description != goal_input.description
                || existing.progress_percent != goal_input.progress_percent
                || existing.token_budget != goal_input.token_budget;
            ResourceGoalRecord {
                id: existing.id.clone(),
                title: goal_input.title,
                scope: existing.scope.clone(),
                status: ResourceGoalStatus::Active,
                description: goal_input.description,
                progress_percent: goal_input.progress_percent,
                objective: goal_input.objective,
                token_budget: goal_input.token_budget,
                tokens_used: existing.tokens_used,
                time_used_seconds: existing.time_used_seconds,
                blocked_reason: None,
                completion_evidence: None,
                created_at_unix_ms: existing.created_at_unix_ms,
                updated_at_unix_ms: if fields_changed {
                    timestamp.max(existing.updated_at_unix_ms)
                } else {
                    existing.updated_at_unix_ms
                },
            }
        }
        None => ResourceGoalRecord {
            id: deterministic_goal_id(&scope, &request_nonce),
            title: goal_input.title,
            scope: "project".to_owned(),
            status: ResourceGoalStatus::Active,
            description: goal_input.description,
            progress_percent: goal_input.progress_percent,
            objective: goal_input.objective,
            token_budget: goal_input.token_budget,
            tokens_used: 0,
            time_used_seconds: 0,
            blocked_reason: None,
            completion_evidence: None,
            created_at_unix_ms: timestamp,
            updated_at_unix_ms: timestamp,
        },
    };
    let document = GoalDocument::from_snapshot(
        scope,
        keencode_resources::GoalSnapshot {
            revision: expected_revision,
            goal: Some(next_goal),
            retired_goal_ids: current
                .as_ref()
                .map_or_else(Vec::new, |value| value.retired_goal_ids.clone()),
        },
    );
    let outcome = store
        .compare_and_swap(&request_nonce, &operation, expected_revision, document)
        .map_err(map_goal_write_failure)?;
    let deduplicated = outcome.deduplicated();
    let saved = outcome.into_document();
    let revision = saved.revision;
    let saved_goal = saved.goal.ok_or(HostFailure::Internal)?;
    if !deduplicated {
        host.runtime.publish_goal_changed(
            &session_id,
            Some(saved_goal.id.clone()),
            revision,
            Some(goal_status_name(saved_goal.status).to_owned()),
        );
    }
    let response = GoalMutationResponse {
        session_id,
        revision,
        goal: acp_goal_record(saved_goal),
        deduplicated,
    };
    host.result_value(id, &response)
}

/// 把 active Goal 迁移到不可逆的 completed 或 blocked 终态。
fn dispatch_goal_transition(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::GoalTransitionRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let session_id = request.session_id;
    let goal_id = request.goal_id;
    let status = request.status;
    let request_nonce = request.request_nonce;
    let expected_revision = request.expected_revision;
    let (store, scope) = goal_store_and_scope(host, &session_id)?;
    let current = store
        .read(&scope)
        .map_err(|_| HostFailure::Internal)?
        .ok_or(HostFailure::InvalidParams)?;
    let target_status = match status {
        keencode_acp::GoalTransitionStatus::Completed => ResourceGoalStatus::Completed,
        keencode_acp::GoalTransitionStatus::Blocked => ResourceGoalStatus::Blocked,
    };
    let (blocked_reason, completion_evidence) = match target_status {
        ResourceGoalStatus::Blocked => (request.reason, None),
        ResourceGoalStatus::Completed => (None, request.completion_evidence),
        ResourceGoalStatus::Active => unreachable!("Goal transition 不接受 active"),
    };
    let operation = goal_transition_operation(
        expected_revision,
        goal_id.as_str(),
        target_status,
        blocked_reason.as_deref(),
        completion_evidence.as_deref(),
    );
    if let Some(result_revision) = current
        .applied_operation_revision(&request_nonce, &operation)
        .map_err(|_| HostFailure::InvalidParams)?
    {
        if result_revision != current.revision {
            // 资源层收据没有保存历史 Goal 快照；禁止用后续状态冒充本次
            // transition 的原始结果，避免重试返回伪造的生命周期事实。
            return Err(HostFailure::Internal);
        }
        let existing = current.goal.as_ref().ok_or(HostFailure::InvalidParams)?;
        let response = GoalMutationResponse {
            session_id,
            revision: result_revision,
            goal: acp_goal_record(existing.clone()),
            deduplicated: true,
        };
        return host.result_value(id, &response);
    }
    let existing = current.goal.as_ref().ok_or(HostFailure::InvalidParams)?;
    if existing.id != goal_id || current.revision != expected_revision {
        return Err(HostFailure::InvalidParams);
    }
    let mut next_goal = existing.clone();
    next_goal.status = target_status;
    next_goal.blocked_reason = blocked_reason.clone();
    next_goal.completion_evidence = completion_evidence.clone();
    next_goal.updated_at_unix_ms = crate::analytics::now_ms().max(existing.updated_at_unix_ms);
    let document = GoalDocument::from_snapshot(
        scope,
        keencode_resources::GoalSnapshot {
            revision: expected_revision,
            goal: Some(next_goal),
            retired_goal_ids: current.retired_goal_ids,
        },
    );
    let outcome = store
        .compare_and_swap(&request_nonce, &operation, expected_revision, document)
        .map_err(map_goal_write_failure)?;
    let deduplicated = outcome.deduplicated();
    let saved = outcome.into_document();
    let revision = saved.revision;
    let saved_goal = saved.goal.ok_or(HostFailure::Internal)?;
    if !deduplicated {
        host.runtime.publish_goal_changed(
            &session_id,
            Some(saved_goal.id.clone()),
            revision,
            Some(goal_status_name(saved_goal.status).to_owned()),
        );
    }
    let response = GoalMutationResponse {
        session_id,
        revision,
        goal: acp_goal_record(saved_goal),
        deduplicated,
    };
    host.result_value(id, &response)
}

/// 清除已经进入终态的 Goal，并返回持久化的墓碑标识。
fn dispatch_goal_clear(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::GoalClearRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let session_id = request.session_id;
    let request_nonce = request.request_nonce;
    let expected_revision = request.expected_revision;
    let (store, scope) = goal_store_and_scope(host, &session_id)?;
    let current = store
        .read(&scope)
        .map_err(|_| HostFailure::Internal)?
        .ok_or(HostFailure::InvalidParams)?;
    if let Some((cleared_goal_id, result_revision)) =
        matching_goal_clear_receipt(&current, &request_nonce, expected_revision)?
    {
        let response = GoalClearResponse {
            session_id,
            revision: result_revision,
            cleared_goal_id,
            deduplicated: true,
        };
        return host.result_value(id, &response);
    }
    if current.revision != expected_revision {
        return Err(HostFailure::InvalidParams);
    }
    let cleared_goal_id = current
        .goal
        .as_ref()
        .ok_or(HostFailure::InvalidParams)?
        .id
        .clone();
    let operation = goal_clear_operation(expected_revision, &cleared_goal_id);
    let outcome = store
        .compare_and_swap(
            &request_nonce,
            &operation,
            expected_revision,
            GoalDocument::from_snapshot(
                scope,
                keencode_resources::GoalSnapshot {
                    revision: expected_revision,
                    goal: None,
                    retired_goal_ids: current.retired_goal_ids,
                },
            ),
        )
        .map_err(map_goal_write_failure)?;
    let deduplicated = outcome.deduplicated();
    let saved = outcome.into_document();
    if !deduplicated {
        host.runtime
            .publish_goal_changed(&session_id, None, saved.revision, None);
    }
    let response = GoalClearResponse {
        session_id,
        revision: saved.revision,
        cleared_goal_id,
        deduplicated,
    };
    host.result_value(id, &response)
}

/// 返回不启动连接、不暴露配置正文的 MCP Server 运行态列表。
async fn dispatch_mcp_list(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::McpListRequest,
) -> Result<Value, HostFailure> {
    let mut servers = BTreeMap::new();
    let mut runtime_servers = BTreeMap::new();
    let mut runtime_candidate_ready = false;
    let data_root = crate::storage::root_dir(&host.app).map_err(|_| HostFailure::Internal)?;
    let user_path = data_root.join("mcp.json");
    let user_servers = read_user_mcp_servers(&user_path)?;
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let project_root = request
        .project_path
        .as_deref()
        .map(|path| {
            crate::workspace::registered_project_root(&host.app, path)
                .map_err(|_| HostFailure::ResourceNotFound)
        })
        .transpose()?;

    // 只读取当前聚焦项目已经原子发布的扩展候选；查询 MCP 状态不得触发
    // 新的连接、工具发现、OAuth 或子进程初始化。
    if let Some(project_root) = project_root.as_deref()
        && let Some(snapshot) = host
            .runtime
            .mcp_runtime_snapshot(project_root)
            .map_err(map_runtime_failure)?
    {
        runtime_candidate_ready = true;
        for server in snapshot {
            runtime_servers.insert(server.name.clone(), server);
        }
    }

    for (name, config) in user_servers {
        let mut status = mcp_server_status(&name, &config, true)?;
        if let Some(runtime) = runtime_servers.remove(&name) {
            apply_mcp_runtime_status(&mut status, runtime);
        }
        servers.insert(name, status);
    }

    if let Some(project_root) = project_root.as_deref() {
        let snapshot = crate::extensions::plugin_runtime_snapshot(&host.app, project_root)
            .map_err(|_| HostFailure::Internal)?;
        for plugin in snapshot.plugins {
            let plugin_namespace = plugin
                .id
                .runtime_namespace()
                .map_err(|_| HostFailure::Internal)?;
            for (name, config) in plugin.mcp_servers {
                let name = format!("{plugin_namespace}:{name}");
                let mut status = mcp_server_status(&name, &config, false)?;
                if let Some(runtime) = runtime_servers.remove(&name) {
                    apply_mcp_runtime_status(&mut status, runtime);
                }
                servers.insert(name, status);
            }
        }
    }

    if let Some(project_root) = project_root.as_deref() {
        let registry = mcp_oauth_registry(host)?;
        for server in servers
            .values_mut()
            .filter(|server| server.enabled && server.oauth_status != McpOAuthStatus::NotRequired)
        {
            if let Ok(snapshot) = registry.status(project_root, &server.name).await {
                server.oauth_status = crate::extensions::mcp_oauth_status(snapshot.status);
            }
        }
    }
    let response = McpListResponse {
        init_phase: if runtime_candidate_ready {
            McpRuntimePhase::Ready
        } else {
            McpRuntimePhase::Pending
        },
        servers: servers.into_values().collect(),
        error: None,
    };
    host.result_value(id, &response)
}

/// 用已发布候选的实际 MCP 运行态覆盖配置推导的初始状态。
fn apply_mcp_runtime_status(configured: &mut McpServerStatus, runtime: RuntimeMcpServerSnapshot) {
    // `enabled` 只能来自当前配置；候选只包含构建时启用的 Server，不能用旧
    // 候选快照覆盖用户刚刚切换后的开关状态。
    if !configured.enabled {
        return;
    }
    configured.transport = runtime.transport;
    configured.connection_status = runtime.connection_status;
    configured.tools_count = runtime.tools_count;
    configured.oauth_status = runtime.oauth_status;
    configured.error = runtime.error;
}

/// 核验显式项目和当前 Server 配置后启动真实 PKCE/回环授权流程。
async fn dispatch_mcp_oauth_start(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::McpOAuthServerRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let project_root = crate::workspace::registered_project_root(&host.app, &request.project_path)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let server = crate::extensions::runtime_mcp_server_for_project(
        &host.app,
        &project_root,
        &request.server_name,
    )
    .map_err(|_| HostFailure::ResourceNotFound)?;
    server.oauth.ok_or(HostFailure::InvalidParams)?;
    // 先把没有聊天的设置页项目也登记到候选生命周期；后续禁用/删除配置
    // 才能找到并停用该项目的 pending OAuth，且无需重复执行元数据发现。
    host.ensure_extensions(&project_root).await?;
    let registry = mcp_oauth_registry(host)?;
    registry
        .start(&project_root, &request.server_name, oauth_now_seconds()?)
        .await
        .map_err(|_| HostFailure::Internal)?;
    host.result_value(id, &keencode_acp::McpOAuthStartResponse::new())
}

/// 手工回调只交给原项目中现存的 PKCE 操作；code/state 永不进入日志或响应。
async fn dispatch_mcp_oauth_callback(
    host: &AcpHost,
    id: schema::RequestId,
    request: McpOAuthCallbackRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let project_root = crate::workspace::registered_project_root(&host.app, &request.project_path)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    // 手工回调先对账最新配置，已停用或改绑的旧 PKCE 不能重新授权旧 Server。
    host.ensure_extensions(&project_root).await?;
    mcp_oauth_registry(host)?
        .callback(
            &project_root,
            &request.server_name,
            request.code,
            request.state,
            oauth_now_seconds()?,
        )
        .await
        .map_err(|_| HostFailure::InvalidParams)?;
    host.result_value(id, &keencode_acp::McpOAuthCallbackResponse::new())
}

/// 即使配置已移除也允许取消原项目的待决操作，不以当前焦点重定向取消。
async fn dispatch_mcp_oauth_cancel(
    host: &AcpHost,
    id: schema::RequestId,
    request: keencode_acp::McpOAuthServerRequest,
) -> Result<Value, HostFailure> {
    request.validate().map_err(|_| HostFailure::InvalidParams)?;
    let project_root = crate::workspace::registered_project_root(&host.app, &request.project_path)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let cancelled = match mcp_oauth_registry(host)?
        .cancel(&project_root, &request.server_name)
        .await
    {
        Ok(cancelled) => cancelled,
        Err(crate::mcp_oauth::McpOAuthServiceError::NotRegistered) => false,
        Err(_) => return Err(HostFailure::Internal),
    };
    host.result_value(id, &keencode_acp::McpOAuthCancelResponse::new(cancelled))
}

/// 读取应用级唯一 Registry；Host 不创建临时认证状态或第二个令牌库。
fn mcp_oauth_registry(
    host: &AcpHost,
) -> Result<std::sync::Arc<crate::mcp_oauth::McpOAuthRegistry>, HostFailure> {
    use tauri::Manager;
    host.app
        .try_state::<std::sync::Arc<crate::mcp_oauth::McpOAuthRegistry>>()
        .map(|state| state.inner().clone())
        .ok_or(HostFailure::Internal)
}

/// 为授权过期与令牌校验提供真实 UTC 秒数，不使用可被请求覆盖的时间。
fn oauth_now_seconds() -> Result<u64, HostFailure> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| HostFailure::Internal)
}

/// 读取扩展请求显式 operationId；缺失时为本次请求生成一次性随机身份。
///
/// JSON-RPC ID 只用于响应关联，不能在跨连接场景中充当持久控制操作身份。
fn request_operation_id(meta: Option<&schema::Meta>) -> Result<String, HostFailure> {
    super::operation_id(meta)
}

/// 从有界 Journal 页面收集根 Turn 起点的用户消息锚点。
fn collect_rewind_candidates(
    session: &keencode_runtime::RuntimeSession,
) -> Result<Vec<RewindCandidate>, HostFailure> {
    let mut candidates = Vec::new();
    let mut identifiers = HashSet::new();
    let mut after = None;
    loop {
        let page = session
            .replay(after, REPLAY_PAGE_SIZE)
            .map_err(|_| HostFailure::Internal)?;
        for record in page.records {
            collect_event_candidates(
                &record.event,
                record.time_unix_ms,
                &mut identifiers,
                &mut candidates,
            );
            if candidates.len() >= MAX_REWIND_CANDIDATES {
                return Ok(candidates);
            }
        }
        if !page.has_more {
            break;
        }
        after = page.next_after;
        if after.is_none() {
            return Err(HostFailure::Internal);
        }
    }
    Ok(candidates)
}

/// 读取一个根 Turn 起点 AtomicBatch 内的真实用户消息，不读取动态 Transcript 段。
fn collect_event_candidates(
    event: &SessionEvent,
    created_at_ms: u64,
    identifiers: &mut HashSet<String>,
    candidates: &mut Vec<RewindCandidate>,
) {
    if matches!(event, SessionEvent::AtomicBatch { .. }) {
        for message in root_user_messages(event) {
            collect_message_candidate(message, created_at_ms, identifiers, candidates);
        }
    }
}

/// 返回一个物理 AtomicBatch 中与根 Turn 起点配对的全部用户消息。
fn root_user_messages(event: &SessionEvent) -> Vec<&SessionMessage> {
    let SessionEvent::AtomicBatch { events } = event else {
        return Vec::new();
    };
    let Some(root_turn_id) = events.iter().find_map(|event| match event {
        SessionEvent::TurnStarted {
            turn_id,
            source_agent_id,
            root_turn_id,
            parent_turn_id,
            ..
        } if source_agent_id.as_str() == ROOT_AGENT_ID
            && root_turn_id == turn_id
            && parent_turn_id.is_none() =>
        {
            Some(turn_id)
        }
        _ => None,
    }) else {
        return Vec::new();
    };
    events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::MessageAdded { message }
                if message.role == MessageRole::User
                    && message.agent_id.is_none()
                    && message.turn_id.as_ref() == Some(root_turn_id) =>
            {
                Some(message)
            }
            _ => None,
        })
        .collect()
}

/// 将一条用户消息转换为有界、唯一的回退候选。
fn collect_message_candidate(
    message: &SessionMessage,
    created_at_ms: u64,
    identifiers: &mut HashSet<String>,
    candidates: &mut Vec<RewindCandidate>,
) {
    if message.role != MessageRole::User || !identifiers.insert(message.message_id.clone()) {
        return;
    }
    let preview = message
        .content
        .iter()
        .filter_map(|part| match part {
            MessagePart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if preview.trim().is_empty() || created_at_ms == 0 {
        return;
    }
    let preview = preview
        .chars()
        .take(MAX_CANDIDATE_PREVIEW_CHARS)
        .collect::<String>();
    if preview.is_empty() {
        return;
    }
    candidates.push(RewindCandidate {
        message_id: message.message_id.clone(),
        preview,
        created_at_ms,
    });
}

/// 打开当前 Session 对应的项目 Goal 存储并完成项目范围授权。
fn goal_store_and_scope(
    host: &AcpHost,
    session_id: &str,
) -> Result<(GoalFileStore, ScopeId), HostFailure> {
    let (_, project_root) = authorized_metadata(&host.runtime, &host.app, session_id)
        .map_err(|_| HostFailure::ResourceNotFound)?;
    let storage_root = crate::storage::root_dir(&host.app).map_err(|_| HostFailure::Internal)?;
    let store = GoalFileStore::open(storage_root).map_err(|_| HostFailure::Internal)?;
    let scope = project_scope_id(&project_root).map_err(|_| HostFailure::Internal)?;
    Ok((store, scope))
}

/// 把资源层 Goal 转换成 ACP 封闭 DTO。
fn acp_goal_record(goal: ResourceGoalRecord) -> keencode_acp::GoalRecord {
    keencode_acp::GoalRecord {
        id: goal.id,
        title: goal.title,
        scope: keencode_acp::GoalScope::Project,
        status: match goal.status {
            ResourceGoalStatus::Active => keencode_acp::GoalStatus::Active,
            ResourceGoalStatus::Completed => keencode_acp::GoalStatus::Completed,
            ResourceGoalStatus::Blocked => keencode_acp::GoalStatus::Blocked,
        },
        description: goal.description,
        progress_percent: goal.progress_percent,
        objective: goal.objective,
        token_budget: goal.token_budget,
        tokens_used: goal.tokens_used,
        time_used_seconds: goal.time_used_seconds,
        blocked_reason: goal.blocked_reason,
        completion_evidence: goal.completion_evidence,
        created_at_ms: goal.created_at_unix_ms,
        updated_at_ms: goal.updated_at_unix_ms,
    }
}

/// 返回 Goal 事件使用的稳定状态名称。
fn goal_status_name(status: ResourceGoalStatus) -> &'static str {
    match status {
        ResourceGoalStatus::Active => "active",
        ResourceGoalStatus::Completed => "completed",
        ResourceGoalStatus::Blocked => "blocked",
    }
}

/// 构造绑定期望 revision 和完整 Goal 输入的 upsert 收据载荷。
fn goal_upsert_operation(
    expected_revision: u64,
    goal: &keencode_acp::GoalInput,
) -> GoalUpsertOperation {
    GoalUpsertOperation {
        operation: "goal_upsert_v2",
        expected_revision,
        goal: goal.clone(),
    }
}

/// 构造绑定期望 revision、目标 Goal 和状态条件的 transition 收据载荷。
fn goal_transition_operation<'a>(
    expected_revision: u64,
    goal_id: &'a str,
    status: ResourceGoalStatus,
    reason: Option<&'a str>,
    completion_evidence: Option<&'a str>,
) -> GoalTransitionOperation<'a> {
    GoalTransitionOperation {
        operation: "goal_transition_v2",
        expected_revision,
        goal_id,
        status,
        reason,
        completion_evidence,
    }
}

/// 构造绑定期望 revision 和实际被清除 Goal 的 clear 收据载荷。
fn goal_clear_operation<'a>(
    expected_revision: u64,
    cleared_goal_id: &'a str,
) -> GoalClearOperation<'a> {
    GoalClearOperation {
        operation: "goal_clear_v2",
        expected_revision,
        cleared_goal_id,
    }
}

/// 从持久收据反解 clear 操作实际清除的 Goal，不依赖墓碑列表末项位置。
///
/// ACP clear 请求没有携带 Goal ID，因此重试时必须以收据载荷逐一核对保留的
/// 墓碑标识。收据中的 result revision 同时是原操作真实返回的 revision。
fn matching_goal_clear_receipt(
    document: &GoalDocument,
    request_nonce: &str,
    expected_revision: u64,
) -> Result<Option<(String, u64)>, HostFailure> {
    let candidates = document.retired_goal_ids.iter();
    if let Some(goal) = document.goal.as_ref()
        && let Some(result_revision) = document
            .applied_operation_revision(
                request_nonce,
                &goal_clear_operation(expected_revision, &goal.id),
            )
            .map_err(|_| HostFailure::InvalidParams)?
    {
        return Ok(Some((goal.id.clone(), result_revision)));
    }
    for goal_id in candidates {
        if let Some(result_revision) = document
            .applied_operation_revision(
                request_nonce,
                &goal_clear_operation(expected_revision, goal_id),
            )
            .map_err(|_| HostFailure::InvalidParams)?
        {
            return Ok(Some((goal_id.clone(), result_revision)));
        }
    }
    Ok(None)
}

/// 将 Goal CAS 的业务冲突映射为参数错误，其余持久化故障保持内部错误。
fn map_goal_write_failure(error: keencode_resources::ResourceError) -> HostFailure {
    match error {
        keencode_resources::ResourceError::InvalidId(_)
        | keencode_resources::ResourceError::RevisionConflict { .. }
        | keencode_resources::ResourceError::InvalidGoalTransition(_)
        | keencode_resources::ResourceError::OperationConflict => HostFailure::InvalidParams,
        _ => HostFailure::Internal,
    }
}

/// 读取用户 MCP 配置的 Server 映射；该函数只读且拒绝符号链接与超大文件。
fn read_user_mcp_servers(path: &Path) -> Result<BTreeMap<String, Value>, HostFailure> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(_) => return Err(HostFailure::Internal),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_MCP_CONFIG_BYTES
    {
        return Err(HostFailure::Internal);
    }
    let text = fs::read_to_string(path).map_err(|_| HostFailure::Internal)?;
    let root = serde_json::from_str::<Value>(&text).map_err(|_| HostFailure::Internal)?;
    let servers = root
        .as_object()
        .and_then(|root| root.get("mcpServers"))
        .and_then(Value::as_object)
        .ok_or(HostFailure::Internal)?;
    servers
        .iter()
        .map(|(name, config)| Ok((name.clone(), config.clone())))
        .collect()
}

/// 把已冻结的 MCP 配置映射到不触发连接的运行态 DTO。
fn mcp_server_status(
    name: &str,
    config: &Value,
    _user_source: bool,
) -> Result<McpServerStatus, HostFailure> {
    let object = config.as_object().ok_or(HostFailure::Internal)?;
    let enabled = !object
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let transport = if object.get("command").and_then(Value::as_str).is_some() {
        McpTransportKind::Stdio
    } else if object.get("url").and_then(Value::as_str).is_some() {
        McpTransportKind::StreamableHttp
    } else {
        return Err(HostFailure::Internal);
    };
    Ok(McpServerStatus {
        name: name.to_owned(),
        enabled,
        transport,
        connection_status: if enabled {
            McpConnectionStatus::Uninitialized
        } else {
            McpConnectionStatus::Disabled
        },
        tools_count: 0,
        oauth_status: if transport == McpTransportKind::StreamableHttp
            && object.contains_key("oauth")
        {
            let settings: crate::mcp_oauth::McpOAuthSettings =
                serde_json::from_value(object["oauth"].clone())
                    .map_err(|_| HostFailure::Internal)?;
            settings.validate().map_err(|_| HostFailure::Internal)?;
            McpOAuthStatus::Idle
        } else {
            McpOAuthStatus::NotRequired
        },
        error: None,
    })
}

/// 按项目作用域和请求 nonce 派生不可猜测且稳定的 Goal 标识。
fn deterministic_goal_id(scope: &ScopeId, request_nonce: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode/goal-id/v1\0");
    digest.update(scope.as_str().as_bytes());
    digest.update(b"\0");
    digest.update(request_nonce.as_bytes());
    let mut hex = String::with_capacity(64);
    for byte in digest.finalize() {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("String 写入不会失败");
    }
    format!("goal-{hex}")
}

#[cfg(test)]
mod tests {
    /// 未建立 Session 或 MCP 候选时，公开 OAuth 配置也必须展示待授权而非无需认证。
    #[test]
    fn configured_oauth_is_visible_without_a_runtime_session() {
        let status = super::mcp_server_status(
            "demo",
            &serde_json::json!({
                "url": "https://mcp.example.test/api",
                "oauth": {"clientId": "desktop", "resource": "https://mcp.example.test/api"}
            }),
            true,
        )
        .unwrap();
        assert_eq!(status.oauth_status, keencode_acp::McpOAuthStatus::Idle);
        assert_eq!(
            status.connection_status,
            keencode_acp::McpConnectionStatus::Uninitialized
        );
        assert_eq!(status.tools_count, 0);
    }

    use super::{
        MAX_CANDIDATE_PREVIEW_CHARS, cancellation_was_requested, collect_event_candidates,
        collect_message_candidate, deterministic_goal_id, goal_transition_operation,
        goal_upsert_operation, map_goal_write_failure, matching_goal_clear_receipt,
        rename_session_with_receipt, request_operation_id,
    };
    use keencode_acp::{AcpIncomingFrame, AcpRequest, AcpRequestDecoder, GoalInput};
    use keencode_resources::{
        AgentId, DocumentOperationReceipt, GoalDocument, MessagePart, MessageRole, ScopeId,
        SessionEvent, SessionMessage, TurnId,
    };
    use keencode_runtime::{CreateSessionRequest, RuntimeConfig, RuntimeSession};
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    /// 缺失元数据时每次请求都要生成新的身份；显式元数据才负责重试稳定性。
    #[test]
    fn request_operation_id_is_fresh_without_metadata_and_stable_with_it() {
        let first = request_operation_id(None).unwrap();
        let second = request_operation_id(None).unwrap();
        assert_ne!(first, second);
        assert!(first.len() <= 128);

        let meta = serde_json::Map::from_iter([(
            "keencode/operationId".to_owned(),
            serde_json::json!("operation-1"),
        )]);
        assert_eq!(
            request_operation_id(Some(&meta)).unwrap(),
            request_operation_id(Some(&meta)).unwrap()
        );
    }

    /// 从真实 JSON 解码到扩展路由，再用 Runtime SessionStore 核对 A→B→重试 A 的收据。
    #[test]
    fn decoded_rename_reuses_the_original_persisted_receipt() {
        let decoder = AcpRequestDecoder::new();
        let raw = br#"{"jsonrpc":"2.0","id":"rpc-a","method":"keencode/session/rename","params":{"sessionId":"session-json","title":"A","_meta":{"keencode/operationId":"rename-a"}}}"#;
        let frame = decoder.decode_raw(raw).expect("扩展 JSON 应严格解码");
        let request = match frame {
            AcpIncomingFrame::Request(frame) => {
                let (_, request) = frame.into_parts();
                match request {
                    AcpRequest::RenameSession(request) => request,
                    _ => panic!("应路由到 rename 扩展"),
                }
            }
            AcpIncomingFrame::Notification(_) => panic!("带 ID 的请求不能解码为通知"),
        };
        let operation_id = request_operation_id(request.meta.as_ref()).unwrap();
        assert_eq!(operation_id, "rename-a");

        let storage = tempdir().expect("测试 Runtime 存储根应创建");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: request.session_id.clone(),
                title: "初始标题".to_owned(),
                project_root: storage.path().to_string_lossy().into_owned(),
            },
        )
        .expect("测试 Session 应创建");

        let first = rename_session_with_receipt(
            &session,
            request.session_id.clone(),
            &operation_id,
            request.title,
        )
        .expect("首次标题变更应提交");
        let _second = rename_session_with_receipt(
            &session,
            "session-json".to_owned(),
            "rename-b",
            "B".to_owned(),
        )
        .expect("后续标题变更应提交");
        let retry = rename_session_with_receipt(
            &session,
            "session-json".to_owned(),
            &operation_id,
            "A".to_owned(),
        )
        .expect("相同 operationId 应恢复原收据");

        assert_eq!(retry.title, "A");
        assert_eq!(retry.journal_sequence, first.journal_sequence);
        assert_eq!(
            session.snapshot().unwrap().state.title,
            "B",
            "重试不得把当前 B 快照误报为原始 A 响应"
        );
    }

    /// Goal 标识必须同时绑定项目作用域与请求 nonce。
    #[test]
    fn goal_id_is_scope_and_nonce_stable() {
        let scope = ScopeId::new("project-a").unwrap();
        assert_eq!(
            deterministic_goal_id(&scope, "nonce-1"),
            deterministic_goal_id(&scope, "nonce-1")
        );
        assert_ne!(
            deterministic_goal_id(&scope, "nonce-1"),
            deterministic_goal_id(&scope, "nonce-2")
        );
    }

    /// 候选预览必须是有界文本，并拒绝没有文本正文的用户消息。
    #[test]
    fn candidate_preview_is_bounded_and_text_only() {
        let message = SessionMessage {
            message_id: "message-1".to_owned(),
            turn_id: None,
            agent_id: None,
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: "x".repeat(MAX_CANDIDATE_PREVIEW_CHARS + 20),
            }],
        };
        let mut ids = std::collections::HashSet::new();
        let mut candidates = Vec::new();
        collect_message_candidate(&message, 1, &mut ids, &mut candidates);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].preview.chars().count(),
            MAX_CANDIDATE_PREVIEW_CHARS
        );

        let image_only = SessionMessage {
            message_id: "message-2".to_owned(),
            turn_id: None,
            agent_id: None,
            role: MessageRole::User,
            content: Vec::new(),
        };
        collect_message_candidate(&image_only, 1, &mut ids, &mut candidates);
        assert_eq!(candidates.len(), 1);
    }

    /// 回退候选只包含根 Turn 起点 AtomicBatch，动态 User 段不得混入候选。
    #[test]
    fn rewind_candidates_exclude_dynamic_user_segments() {
        let turn_id = TurnId::new("root-turn").unwrap();
        let root_message = SessionMessage {
            message_id: "root-message".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: None,
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: "早期锚点".to_owned(),
            }],
        };
        let dynamic_message = SessionMessage {
            message_id: "dynamic-message".to_owned(),
            turn_id: Some(turn_id.clone()),
            agent_id: Some(AgentId::new("root").unwrap()),
            role: MessageRole::User,
            content: vec![MessagePart::Text {
                text: "动态 steer".to_owned(),
            }],
        };
        let root_event = SessionEvent::AtomicBatch {
            events: vec![
                SessionEvent::TurnStarted {
                    turn_id: turn_id.clone(),
                    source_agent_id: AgentId::new("root").unwrap(),
                    root_turn_id: turn_id.clone(),
                    parent_turn_id: None,
                    prompt_summary: "早期锚点".to_owned(),
                },
                SessionEvent::MessageAdded {
                    message: root_message,
                },
            ],
        };
        let dynamic_event = SessionEvent::TranscriptSegmentCommitted {
            segment: keencode_resources::TranscriptSegment {
                turn_id,
                source_agent_id: AgentId::new("root").unwrap(),
                model_round: 1,
                segment_index: 0,
                expected_transcript_revision: 1,
                messages: vec![dynamic_message],
            },
        };
        let mut ids = std::collections::HashSet::new();
        let mut candidates = Vec::new();
        collect_event_candidates(&root_event, 1, &mut ids, &mut candidates);
        collect_event_candidates(&dynamic_event, 2, &mut ids, &mut candidates);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["root-message"]
        );
    }

    /// 后台取消响应只把 Runtime 的首次 Requested 映射为 true。
    #[test]
    fn cancellation_response_uses_atomic_runtime_outcome() {
        use super::BackgroundTaskCancellationOutcome;

        assert!(cancellation_was_requested(
            BackgroundTaskCancellationOutcome::Requested
        ));
        assert!(!cancellation_was_requested(
            BackgroundTaskCancellationOutcome::AlreadyRequested
        ));
        assert!(!cancellation_was_requested(
            BackgroundTaskCancellationOutcome::NotRunning
        ));
    }

    /// 资源层拒绝用户操作标识时，Host 必须报告参数错误而不是内部故障。
    #[test]
    fn goal_write_rejects_invalid_operation_id_as_invalid_params() {
        assert_eq!(
            map_goal_write_failure(keencode_resources::ResourceError::InvalidId(
                "operation id too long".to_owned()
            )),
            super::HostFailure::InvalidParams
        );
        assert_eq!(
            map_goal_write_failure(keencode_resources::ResourceError::Json(
                "invalid goal document".to_owned()
            )),
            super::HostFailure::Internal
        );
    }

    /// Goal 操作收据必须把期望 revision 和完整请求条件纳入身份摘要输入。
    #[test]
    fn goal_operation_payloads_bind_expected_revision() {
        let goal = GoalInput {
            title: "标题".to_owned(),
            objective: "目标".to_owned(),
            description: None,
            progress_percent: Some(10),
            token_budget: Some(100),
        };
        assert_ne!(
            serde_json::to_value(goal_upsert_operation(1, &goal)).unwrap(),
            serde_json::to_value(goal_upsert_operation(2, &goal)).unwrap()
        );
        assert_ne!(
            serde_json::to_value(goal_transition_operation(
                1,
                "goal-1",
                super::ResourceGoalStatus::Completed,
                None,
                Some("证据"),
            ))
            .unwrap(),
            serde_json::to_value(goal_transition_operation(
                2,
                "goal-1",
                super::ResourceGoalStatus::Completed,
                None,
                Some("证据"),
            ))
            .unwrap()
        );
    }

    /// clear 重试必须按收据载荷还原原始 Goal，而不能取当前墓碑列表的最后一项。
    #[test]
    fn clear_receipt_recovers_original_goal_after_later_clear() {
        let canonical = serde_json::json!({
            "clearedGoalId": "goal-old",
            "expectedRevision": 2,
            "operation": "goal_clear_v2",
        });
        let digest = Sha256::digest(serde_json::to_vec(&canonical).unwrap());
        let payload_sha256 = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let document = GoalDocument {
            schema: "keencode/goal".to_owned(),
            version: 2,
            scope: ScopeId::new("project-a").unwrap(),
            revision: 6,
            goal: None,
            retired_goal_ids: vec!["goal-old".to_owned(), "goal-new".to_owned()],
            operation_receipts: vec![DocumentOperationReceipt {
                operation_id: "clear-op".to_owned(),
                payload_sha256,
                result_revision: 3,
            }],
        };
        assert_eq!(
            matching_goal_clear_receipt(&document, "clear-op", 2).unwrap(),
            Some(("goal-old".to_owned(), 3))
        );
    }
}
