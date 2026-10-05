//! Agent 调试与目录的只读投影；事实始终来自授权 Session 的 Journal 和实际请求记录。

use super::dispatch::{GatewayContext, RpcError};
use super::session::projection::{
    parse_virtual_agent_view_id, resolve_conversation_scope, virtual_agent_view_id,
};
use crate::analytics::RequestRecord;
use keencode_resources::{AgentId, ModelRoundState, SessionState, SubAgentStatus, TurnStatus};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use tauri::Manager;

pub(crate) fn is_method(method: &str) -> bool {
    matches!(
        method,
        "readSessionDebug" | "listSessionSubagents" | "getSkillReferenceCatalog"
    )
}

pub(crate) async fn call(
    ctx: GatewayContext,
    method: &str,
    args: Value,
) -> Result<Value, RpcError> {
    if method == "getSkillReferenceCatalog" {
        return skill_catalog(&ctx, &args).await;
    }
    let session_id = args
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| RpcError::new("agent.invalid_params", "sessionId 不能为空"))?;
    let runtime = crate::require_owned_runtime(&ctx.app).map_err(failure)?;
    let parent_id = parse_virtual_agent_view_id(session_id)
        .map_err(failure)?
        .map(|(parent_id, _)| parent_id)
        .unwrap_or_else(|| session_id.to_owned());
    let (metadata, root) =
        crate::session_commands::authorized_metadata(&runtime, &ctx.app, &parent_id)
            .map_err(failure)?;
    if let Some(requested) = args.get("workspacePath").and_then(Value::as_str) {
        let canonical = crate::workspace::canonical_session_root(requested).map_err(failure)?;
        if canonical != root {
            return Err(RpcError::new(
                "agent.scope_mismatch",
                "Session 不属于请求的工作区",
            ));
        }
    }
    let scope = resolve_conversation_scope(&runtime, &ctx.app, session_id, &root.to_string_lossy())
        .map_err(failure)?;
    // 子 Agent 没有自己的子目录；拒绝递归读取，而不是伪造第二层空目录。
    if method == "listSessionSubagents" && scope.agent_id.is_some() {
        return Err(RpcError::new(
            "agent.invalid_tree",
            "子 Agent 视图不允许继续读取子 Agent 目录",
        ));
    }
    let session = crate::session_commands::open_authorized_session(
        &runtime,
        &ctx.app,
        metadata.session_id.as_str(),
    )
    .map_err(failure)?;
    let state = session.snapshot().map_err(failure)?.state;
    match method {
        "readSessionDebug" => {
            let app = ctx.app.clone();
            let records = tauri::async_runtime::spawn_blocking(move || {
                // Flush 屏障确保已经观测的请求先落盘；不把缺失日志伪装成空调试结果。
                if let Some(recorder) =
                    app.try_state::<std::sync::Arc<crate::analytics::AnalyticsRecorder>>()
                {
                    recorder.flush()?;
                }
                crate::analytics::read_records(&app)
            })
            .await
            .map_err(failure)?
            .map_err(failure)?;
            Ok(debug_snapshot(
                &state,
                &records,
                &scope.requested_session_id,
                scope.agent_id.as_ref(),
            ))
        }
        "listSessionSubagents" => {
            let mut actors = BTreeMap::new();
            for events in state.workflow_events.values() {
                for event in events
                    .iter()
                    .filter(|event| event.event_type == "actor-started")
                {
                    let Some(actor_id) = &event.actor_session_id else {
                        continue;
                    };
                    let actor = crate::session_commands::open_authorized_session(
                        &runtime, &ctx.app, actor_id,
                    )
                    .map_err(failure)?;
                    let actor_state = actor.snapshot().map_err(failure)?.state;
                    let actor_root =
                        crate::workspace::canonical_session_root(&actor_state.project_root)
                            .map_err(failure)?;
                    let binding =
                        actor_state
                            .workflow_events
                            .get(&event.run_id)
                            .and_then(|events| {
                                events
                                    .iter()
                                    .find(|event| event.event_type == "actor-bound")
                            });
                    if actor_root != root
                        || !actor.is_workflow_actor().map_err(failure)?
                        || binding.is_none_or(|binding| {
                            binding
                                .payload
                                .get("parentSessionId")
                                .and_then(Value::as_str)
                                != Some(state.session_id.as_str())
                                || binding.payload.get("nodeAddress")
                                    != event.payload.get("nodeAddress")
                        })
                    {
                        return Err(RpcError::new(
                            "agent.scope_mismatch",
                            "工作流 actor 绑定不属于父会话",
                        ));
                    }
                    actors.insert(actor_id.clone(), actor_state);
                }
            }
            subagents_view(&state, &actors, &args)
        }
        _ => Err(RpcError::new(
            "agent.method_not_found",
            "未知 Agent 视图方法",
        )),
    }
}

/// 草稿按工作区当前发现目录展示；驻留 Session 只读首次完整 context 的冻结身份。
async fn skill_catalog(ctx: &GatewayContext, args: &Value) -> Result<Value, RpcError> {
    let requested = args
        .get("workspacePath")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| RpcError::new("agent.invalid_params", "workspacePath 不能为空"))?;
    let root =
        crate::session_commands::authorize_stored_root(&ctx.app, requested).map_err(failure)?;
    if let Some(session_id) = args.get("sessionId") {
        let session_id = session_id
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| RpcError::new("agent.invalid_params", "sessionId 必须为非空字符串"))?;
        let runtime = crate::require_owned_runtime(&ctx.app).map_err(failure)?;
        let (_, session_root) =
            crate::session_commands::authorized_metadata(&runtime, &ctx.app, session_id)
                .map_err(failure)?;
        if session_root != root {
            return Err(RpcError::new(
                "agent.scope_mismatch",
                "Skill 目录请求不属于当前 Session",
            ));
        }
        super::session::prepare_session_reference_catalog(&ctx.app, &runtime, session_id, &root)
            .await?;
        let catalog = runtime
            .session_extension_reference_catalog(session_id)
            .map_err(failure)?
            .ok_or_else(|| {
                RpcError::new(
                    "agent.catalog_not_ready",
                    "Session 首次完整上下文尚未冻结 Skill 目录",
                )
            })?;
        return Ok(json!({ "authority":"session", "skills":catalog.skills }));
    }
    // 工作区目录是设置/草稿的当前发现结果，不能冒充 Session 已冻结的能力目录。
    let result = crate::extensions::skills_list(
        Some(root.to_string_lossy().into_owned()),
        ctx.app.clone(),
        ctx.app.state(),
    )
    .map_err(failure)?;
    let skills: Vec<_> = result.skills.into_iter().filter(|skill| skill.enabled && skill.user_invocable)
        .map(|skill| {
            let scope = match skill.source.as_str() { "project" => "workspace", "user" => "user", _ => "plugin" };
            let mut entry = json!({ "id":format!("{scope}:{}", skill.path), "name":skill.name, "description":skill.description, "path":skill.path, "scope":scope, "enabled":true });
            if scope == "plugin" && let Some((plugin_name, _)) = skill.name.split_once(':') {
                entry["pluginName"] = plugin_name.into();
            }
            entry
        }).collect();
    Ok(json!({ "authority":"workspace", "skills":skills }))
}

/// 普通子 Agent 保留在父 Journal 中，虚拟身份只用于只读投影，不能建立第二份 Session。
fn subagents_view(
    state: &SessionState,
    actors: &BTreeMap<String, SessionState>,
    args: &Value,
) -> Result<Value, RpcError> {
    let limit = match args.get("endedLimit") {
        None => 20,
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=100).contains(value))
            .ok_or_else(|| {
                RpcError::new("agent.invalid_params", "endedLimit 必须在 1 到 100 之间")
            })? as usize,
    };
    let offset = match args.get("endedCursor") {
        None => 0,
        Some(Value::String(cursor)) => {
            let (revision, offset) = cursor
                .split_once(':')
                .ok_or_else(|| RpcError::new("agent.invalid_cursor", "目录游标格式错误"))?;
            if revision.parse::<u64>().ok() != Some(state.last_sequence) {
                return Err(RpcError::new(
                    "agent.stale_cursor",
                    "子 Agent 目录已变化，请重新读取首页",
                ));
            }
            offset
                .parse::<usize>()
                .map_err(|_| RpcError::new("agent.invalid_cursor", "目录游标偏移错误"))?
        }
        Some(_) => {
            return Err(RpcError::new(
                "agent.invalid_cursor",
                "目录游标必须是字符串",
            ));
        }
    };
    let mut running = Vec::new();
    let mut ended = Vec::new();
    let mut child_ids = Vec::new();
    for agent in state.sub_agents.values() {
        if agent.parent_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID {
            return Err(RpcError::new(
                "agent.invalid_tree",
                "子 Agent 目录不允许递归层级",
            ));
        }
        let child_id = virtual_agent_view_id(state.session_id.as_str(), &agent.agent_id);
        let turn = agent
            .current_turn_id
            .as_ref()
            .and_then(|id| state.turns.get(id));
        let status = match agent.status {
            SubAgentStatus::Pending | SubAgentStatus::Waiting => "waiting",
            SubAgentStatus::Running => "running",
            SubAgentStatus::Completed => "success",
            SubAgentStatus::Failed => "failed",
            SubAgentStatus::Interrupted | SubAgentStatus::Stopped => "cancelled",
        };
        let mut item = json!({ "childSessionId": child_id, "agentId": agent.agent_id.as_str(), "subagentType": "agent", "title": agent.agent_path, "status": status });
        if let Some(summary) = &agent.result_summary {
            item["summary"] = summary.clone().into();
        }
        if let Some(turn) = turn {
            item["startedAt"] = turn.started_at_unix_ms.into();
            if let Some(completed) = turn.completed_at_unix_ms {
                item["endedAt"] = completed.into();
            }
        }
        child_ids.push(child_id);
        if matches!(status, "running" | "waiting" | "blocked") {
            running.push(item);
        } else {
            ended.push(item);
        }
    }
    for events in state.workflow_events.values() {
        for event in events
            .iter()
            .filter(|event| event.event_type == "actor-started")
        {
            let Some(actor_id) = &event.actor_session_id else {
                continue;
            };
            if child_ids.contains(actor_id) {
                continue;
            }
            let actor = actors.get(actor_id).ok_or_else(|| {
                RpcError::new("agent.actor_missing", "工作流 actor 的权威状态不可读取")
            })?;
            let turn = actor
                .turns
                .values()
                .filter(|turn| turn.source_agent_id.as_str() == keencode_resources::ROOT_AGENT_ID)
                .max_by_key(|turn| turn.started_at_unix_ms);
            let status = match turn.map(|turn| &turn.status) {
                None => "waiting",
                Some(TurnStatus::Running) => "running",
                Some(TurnStatus::Completed) => "success",
                Some(TurnStatus::Failed) => "failed",
                Some(TurnStatus::Cancelled) => "cancelled",
            };
            let node_id = event
                .payload
                .get("nodeId")
                .and_then(Value::as_str)
                .ok_or_else(|| RpcError::new("agent.invalid_actor", "工作流 actor 缺少节点身份"))?;
            let mut item = json!({ "childSessionId": actor_id, "subagentType": "workflow", "title": node_id, "toolCallId": event.tool_call_id, "status": status });
            if let Some(turn) = turn {
                item["startedAt"] = turn.started_at_unix_ms.into();
                if let Some(completed) = turn.completed_at_unix_ms {
                    item["endedAt"] = completed.into();
                }
                if let Some(summary) = &turn.outcome_message {
                    item["summary"] = summary.clone().into();
                }
            }
            child_ids.push(actor_id.clone());
            if matches!(status, "running" | "waiting") {
                running.push(item);
            } else {
                ended.push(item);
            }
        }
    }
    child_ids.sort();
    ended.sort_by(|left, right| {
        right
            .get("endedAt")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .cmp(&left.get("endedAt").and_then(Value::as_u64).unwrap_or(0))
            .then_with(|| {
                left["childSessionId"]
                    .as_str()
                    .cmp(&right["childSessionId"].as_str())
            })
    });
    if offset > ended.len() {
        return Err(RpcError::new(
            "agent.invalid_cursor",
            "目录游标超过结果范围",
        ));
    }
    let total = ended.len();
    let items: Vec<_> = ended.into_iter().skip(offset).take(limit).collect();
    let mut ended_page = json!({ "total": total, "items": items });
    if offset.saturating_add(limit) < total {
        ended_page["nextCursor"] = format!("{}:{}", state.last_sequence, offset + limit).into();
    }
    Ok(
        json!({ "revision": state.last_sequence, "childSessionIds": child_ids, "running": running, "ended": ended_page }),
    )
}

fn failure(error: impl std::fmt::Display) -> RpcError {
    RpcError::new(
        "agent.view_error",
        keencode_model::redact_error_secrets(&error.to_string()),
    )
}

fn round_view(round: &ModelRoundState) -> Value {
    let event_key = format!(
        "{}:{}:{}",
        round.turn_id, round.source_agent_id, round.model_round
    );
    let mut usage = Map::new();
    for (key, count) in [
        ("inputTokens", round.usage.input_tokens),
        ("outputTokens", round.usage.output_tokens),
        ("totalTokens", round.usage.total_tokens),
        ("reasoningTokens", round.usage.reasoning_tokens),
        ("cachedInputTokens", round.usage.cache_read_tokens),
        ("cachedWriteInputTokens", round.usage.cache_write_tokens),
    ] {
        if let Some(count) = count {
            usage.insert(key.into(), count.into());
        }
    }
    // 只有 Adapter 实测的首输出到结束耗时可用于 TPS；HTTP 总耗时不是生成耗时。
    let generation_duration_ms = round.metadata.decode_duration_ms;
    let tps = match (round.usage.output_tokens, generation_duration_ms) {
        (Some(tokens), Some(duration)) if duration > 0 => {
            Some(tokens as f64 * 1000.0 / duration as f64)
        }
        _ => None,
    };
    let hit_rate = keencode_model::cache_hit_rate(&round.usage)
        .filter(|rate| rate.is_finite() && (0.0..=1.0).contains(rate));
    json!({
        "eventKey": event_key,
        "requestId": round.metadata.response_id.as_deref().unwrap_or(&event_key),
        "requestIndex": round.model_round,
        "recordedAt": round.completed_at_unix_ms,
        "usage": usage,
        "hitRate": hit_rate,
        "generationDurationMs": generation_duration_ms,
        "tokensPerSecond": tps,
    })
}

fn network_view(record: &RequestRecord) -> Value {
    let status = if record.completed_at_ms.is_none() {
        "model_request_started"
    } else if record.status == "success" {
        "model_request_completed"
    } else {
        "model_request_failed"
    };
    let mut result = json!({
        "eventKey": record.id,
        "traceId": record.logical_request_id,
        "requestId": record.provider_request_id.as_ref().unwrap_or(&record.logical_request_id),
        "recordedAt": record.completed_at_ms.unwrap_or(record.requested_at_ms),
        "statusType": status,
        "providerKind": record.protocol,
        "modelId": record.model,
        "transport": record.request_mode,
        "querySource": record.purpose,
        "attempt": record.attempt,
        "maxAttempts": record.max_attempts,
        "durationMs": record.duration_ms,
        // 请求记录明确不采集 Header；不从配置读取密钥来填充调试界面。
        "requestHeaders": {}, "responseHeaders": {},
        "requestHeaderCount": 0, "responseHeaderCount": 0,
    });
    if let Some(code) = record.http_status {
        result["statusCode"] = code.into();
    }
    if let Some(kind) = &record.error_kind {
        result["reason"] = kind.clone().into();
    }
    if let Some(message) = &record.error {
        result["message"] = keencode_model::redact_error_secrets_bounded(message, 512).into();
    }
    // 本地接口地址是私有配置；调试投影省略 baseURL/provider host，而保留实际协议和模型。
    result
}

fn debug_snapshot(
    state: &SessionState,
    records: &[RequestRecord],
    requested_session_id: &str,
    agent_id: Option<&AgentId>,
) -> Value {
    let mut network: Vec<_> = records
        .iter()
        .filter(|record| record.session_id.as_deref() == Some(state.session_id.as_str()))
        .filter(|record| {
            agent_id.is_none_or(|agent| record.agent_id.as_deref() == Some(agent.as_str()))
        })
        .collect();
    network.sort_by_key(|record| (record.requested_at_ms, record.id.as_str()));
    let network_entries: Vec<_> = network
        .iter()
        .rev()
        .take(100)
        .rev()
        .map(|record| network_view(record))
        .collect();
    let mut rounds: Vec<_> = state
        .model_rounds
        .iter()
        .filter(|round| agent_id.is_none_or(|agent| &round.source_agent_id == agent))
        .rev()
        .take(200)
        .map(round_view)
        .collect();
    rounds.reverse();
    let mut cache_count = 0_u64;
    let mut input = 0_u64;
    let mut cache_read = 0_u64;
    let mut complete = true;
    for round in &state.model_rounds {
        if agent_id.map_or_else(
            || round.source_agent_id.as_str() != keencode_resources::ROOT_AGENT_ID,
            |agent| &round.source_agent_id != agent,
        ) {
            continue;
        }
        match (round.usage.input_tokens, round.usage.cache_read_tokens) {
            (Some(tokens), Some(cached)) if cached <= tokens => {
                cache_count += 1;
                input = input.saturating_add(tokens);
                cache_read = cache_read.saturating_add(cached);
            }
            _ => complete = false,
        }
    }
    let cache = (complete && cache_count > 0).then(|| {
        json!({
            "hitRateRequestCount": cache_count,
            "totalInputTokens": input,
            "totalCacheReadTokens": cache_read,
            "hitRate": (input > 0).then_some(cache_read as f64 / input as f64),
        })
    });
    json!({ "sessionId": requested_session_id, "rounds": rounds, "networkEntries": network_entries, "cache": cache })
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_model::{ResponseMetadata, StopReason, TokenUsage};
    use keencode_resources::{AgentId, SessionId, SubAgentState, TurnId};

    fn round(index: u32, usage: TokenUsage, duration: Option<u64>) -> ModelRoundState {
        ModelRoundState {
            turn_id: TurnId::new("view-turn").unwrap(),
            source_agent_id: AgentId::new(keencode_resources::ROOT_AGENT_ID).unwrap(),
            model_round: index,
            requested_model: "contract-model".into(),
            metadata: ResponseMetadata {
                decode_duration_ms: duration,
                ..Default::default()
            },
            usage,
            stop_reason: StopReason::Completed,
            completed_at_unix_ms: u64::from(index),
        }
    }

    #[test]
    fn debug_unknown_usage_and_duration_stay_unknown() {
        let view = round_view(&round(1, TokenUsage::unknown(), None));
        assert_eq!(view["usage"], json!({}));
        assert_eq!(view["hitRate"], Value::Null);
        assert_eq!(view["tokensPerSecond"], Value::Null);
        assert_eq!(view["generationDurationMs"], Value::Null);
    }

    #[test]
    fn debug_tps_and_cache_use_actual_adapter_measurements() {
        let usage = TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(40),
            cache_read_tokens: Some(25),
            ..Default::default()
        };
        let view = round_view(&round(1, usage, Some(2000)));
        assert_eq!(view["tokensPerSecond"], 20.0);
        assert_eq!(view["hitRate"], 0.25);
    }

    #[test]
    fn debug_rounds_are_bounded_and_partial_cache_is_not_presented_as_zero() {
        let mut state = SessionState::empty(SessionId::new("view-session").unwrap());
        state.model_rounds = (1..=220)
            .map(|index| round(index, TokenUsage::unknown(), None))
            .collect();
        let view = debug_snapshot(&state, &[], state.session_id.as_str(), None);
        assert_eq!(view["rounds"].as_array().unwrap().len(), 200);
        assert_eq!(view["rounds"][0]["requestIndex"], 21);
        assert_eq!(view["cache"], Value::Null);
    }

    fn request_record(index: u32, session_id: &str) -> RequestRecord {
        serde_json::from_value(json!({
            "id":format!("request-{index}"), "logicalRequestId":format!("logical-{index}"),
            "attempt":1, "maxAttempts":2, "sessionId":session_id, "purpose":"user_turn",
            "model":"contract-model", "provider":"private.invalid", "protocol":"chat_completions",
            "endpoint":"https://private.invalid", "requestMode":"stream", "status":"success",
            "requestedAtMs":index, "completedAtMs":index + 1, "durationMs":1,
            "usageReported":true, "inputTokens":100, "outputTokens":40, "estimated":false,
        }))
        .unwrap()
    }

    #[test]
    fn debug_network_is_session_scoped_bounded_and_omits_private_endpoint() {
        let state = SessionState::empty(SessionId::new("view-session").unwrap());
        let mut records: Vec<_> = (0..120)
            .map(|index| request_record(index, "view-session"))
            .collect();
        records.push(request_record(999, "other-session"));
        let view = debug_snapshot(&state, &records, state.session_id.as_str(), None);
        let network = view["networkEntries"].as_array().unwrap();
        assert_eq!(network.len(), 100);
        assert_eq!(network[0]["traceId"], "logical-20");
        assert_eq!(network[99]["traceId"], "logical-119");
        assert_eq!(network[0]["statusType"], "model_request_completed");
        assert!(
            !serde_json::to_string(&view)
                .unwrap()
                .contains("private.invalid")
        );
        assert_eq!(network[0]["requestHeaders"], json!({}));
    }

    #[test]
    fn virtual_agent_debug_never_leaks_parent_or_sibling_requests() {
        let mut state = SessionState::empty(SessionId::new("view-session").unwrap());
        let agent_id = AgentId::new("child-reader").unwrap();
        let mut child_round = round(
            2,
            TokenUsage {
                input_tokens: Some(80),
                cache_read_tokens: Some(20),
                ..Default::default()
            },
            None,
        );
        child_round.source_agent_id = agent_id.clone();
        state.model_rounds = vec![round(1, TokenUsage::unknown(), None), child_round];
        let mut records = vec![
            request_record(1, "view-session"),
            request_record(2, "view-session"),
            request_record(3, "view-session"),
            request_record(4, "other-session"),
        ];
        records[0].agent_id = Some(keencode_resources::ROOT_AGENT_ID.into());
        records[1].agent_id = Some(agent_id.to_string());
        records[2].agent_id = Some("child-sibling".into());
        records[3].agent_id = Some(agent_id.to_string());
        let requested_id = virtual_agent_view_id(state.session_id.as_str(), &agent_id);
        let view = debug_snapshot(&state, &records, &requested_id, Some(&agent_id));
        assert_eq!(view["sessionId"], requested_id);
        assert_eq!(view["rounds"].as_array().unwrap().len(), 1);
        assert_eq!(view["rounds"][0]["requestIndex"], 2);
        assert_eq!(view["networkEntries"].as_array().unwrap().len(), 1);
        assert_eq!(view["networkEntries"][0]["traceId"], "logical-2");
        assert_eq!(view["cache"]["hitRate"], 0.25);
    }

    fn child(index: u32, status: SubAgentStatus) -> SubAgentState {
        SubAgentState {
            agent_id: AgentId::new(format!("child-{index:02}")).unwrap(),
            parent_agent_id: AgentId::new(keencode_resources::ROOT_AGENT_ID).unwrap(),
            agent_path: format!("/root/child_{index}"),
            task: "fixture task".into(),
            status,
            current_turn_id: None,
            result_summary: None,
        }
    }

    #[test]
    fn subagent_pages_preserve_identity_and_reject_stale_cursors() {
        let mut state = SessionState::empty(SessionId::new("view-session").unwrap());
        state.last_sequence = 88;
        for index in 0..23 {
            let child = child(index, SubAgentStatus::Completed);
            state.sub_agents.insert(child.agent_id.clone(), child);
        }
        let first = subagents_view(&state, &BTreeMap::new(), &json!({"endedLimit":20})).unwrap();
        assert_eq!(first["ended"]["total"], 23);
        assert_eq!(first["ended"]["items"].as_array().unwrap().len(), 20);
        let second = subagents_view(
            &state,
            &BTreeMap::new(),
            &json!({"endedLimit":20, "endedCursor":first["ended"]["nextCursor"]}),
        )
        .unwrap();
        assert_eq!(second["ended"]["items"].as_array().unwrap().len(), 3);
        assert!(second["ended"].get("nextCursor").is_none());
        assert_eq!(
            first["ended"]["items"][0]["childSessionId"],
            "agent:view-session:child-00"
        );
        state.last_sequence += 1;
        assert!(subagents_view(&state, &BTreeMap::new(), &json!({"endedCursor":"88:20"})).is_err());
    }

    #[test]
    fn subagent_directory_separates_active_terminal_states_and_rejects_recursion() {
        let mut state = SessionState::empty(SessionId::new("view-session").unwrap());
        for (index, status) in [
            SubAgentStatus::Running,
            SubAgentStatus::Waiting,
            SubAgentStatus::Failed,
            SubAgentStatus::Interrupted,
        ]
        .into_iter()
        .enumerate()
        {
            let child = child(index as u32, status);
            state.sub_agents.insert(child.agent_id.clone(), child);
        }
        let view = subagents_view(&state, &BTreeMap::new(), &json!({})).unwrap();
        assert_eq!(view["running"].as_array().unwrap().len(), 2);
        assert_eq!(view["ended"]["items"].as_array().unwrap().len(), 2);
        assert!(
            view["ended"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["status"] == "cancelled")
        );
        state
            .sub_agents
            .values_mut()
            .next()
            .unwrap()
            .parent_agent_id = AgentId::new("other-parent").unwrap();
        assert!(subagents_view(&state, &BTreeMap::new(), &json!({})).is_err());
    }

    #[test]
    fn export_agent_views_source_contract_fixture_when_requested() {
        let Ok(directory) = std::env::var("KEENCODE_RPC_CONTRACT_FIXTURES") else {
            return;
        };
        let mut state = SessionState::empty(SessionId::new("view-session").unwrap());
        state.model_rounds.push(round(
            1,
            TokenUsage {
                input_tokens: Some(100),
                output_tokens: Some(40),
                cache_read_tokens: Some(25),
                ..Default::default()
            },
            Some(2000),
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let child = child(1, SubAgentStatus::Completed);
        state.sub_agents.insert(child.agent_id.clone(), child);
        let records = vec![request_record(1, "view-session")];
        std::fs::write(
            std::path::Path::new(&directory).join("agent_views_contract.json"),
            serde_json::to_vec_pretty(&json!({ "debug":debug_snapshot(&state, &records, state.session_id.as_str(), None), "subagents":subagents_view(&state, &BTreeMap::new(), &json!({})).unwrap() })).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn export_native_journal_contract_fixture_when_requested() {
        use keencode_resources::{
            SessionEvent, SessionEventId, SessionEventRecord, WorkflowJournalEvent,
        };
        let Ok(directory) = std::env::var("KEENCODE_RPC_CONTRACT_FIXTURES") else {
            return;
        };
        let workflow_event = |actor: &str, sequence| SessionEvent::WorkflowEventCommitted {
            record: WorkflowJournalEvent {
                run_id: "contract-run".into(),
                tool_call_id: "contract-tool".into(),
                sequence,
                event_type: "actor-started".into(),
                payload: json!({}),
                artifacts: Vec::new(),
                actor_session_id: Some(actor.into()),
                launch_input_id: None,
            },
        };
        let start = |turn: &str| SessionEvent::TurnStarted {
            turn_id: TurnId::new(turn).unwrap(),
            source_agent_id: AgentId::new(keencode_resources::ROOT_AGENT_ID).unwrap(),
            root_turn_id: TurnId::new(turn).unwrap(),
            parent_turn_id: None,
            prompt_summary: "fixture".into(),
        };
        let events = [
            (
                "parent",
                1,
                SessionEvent::AtomicBatch {
                    events: vec![
                        workflow_event("actor-left", 1),
                        workflow_event("actor-right", 2),
                    ],
                },
            ),
            (
                "actor-left",
                1,
                SessionEvent::AtomicBatch {
                    events: vec![start("left-turn")],
                },
            ),
            ("actor-right", 1, start("right-turn")),
            (
                "actor-left",
                2,
                SessionEvent::TurnCompleted {
                    turn_id: TurnId::new("left-turn").unwrap(),
                },
            ),
        ];
        let records: Vec<_> = events
            .into_iter()
            .enumerate()
            .map(|(index, (session, sequence, event))| SessionEventRecord {
                schema: keencode_resources::SESSION_EVENT_SCHEMA.into(),
                version: keencode_resources::SESSION_EVENT_VERSION,
                event_id: SessionEventId::new(format!("contract-{index}")).unwrap(),
                session: SessionId::new(session).unwrap(),
                sequence,
                time_unix_ms: 1_000 + index as u64,
                event,
            })
            .collect();
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join("native_journal_contract.json"),
            serde_json::to_vec_pretty(&records).unwrap(),
        )
        .unwrap();
    }
}
