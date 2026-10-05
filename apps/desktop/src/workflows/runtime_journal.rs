//! RuntimeSession 到 WorkflowJournalPort 的生产适配。
//!
//! 工作流运行事实只从父会话的 SessionState 读取；产物正文继续走 RuntimeSession 的
//! ArtifactStore 校验入口。这里没有 workflow run JSON 日志或第二份持久化状态。

use super::ports::{
    CommittedWorkflowEvent, WorkflowActorBinding, WorkflowActorTranscriptReader,
    WorkflowJournalPort, WorkflowLaunchRecord,
};
use super::types::*;
use base64::Engine as _;
use keencode_resources::{
    ArtifactCapacity, ArtifactId, ArtifactUse, MessagePart, SessionMessage, ToolResultPart,
    WorkflowJournalEvent,
};
use keencode_runtime::RuntimeSession;
use keencode_workflow::{Node, NodeAddress};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone)]
pub struct RuntimeWorkflowJournal {
    session: RuntimeSession,
    actor_transcript_reader: Option<Arc<dyn WorkflowActorTranscriptReader>>,
}

impl RuntimeWorkflowJournal {
    pub fn new(session: RuntimeSession) -> Self {
        Self {
            session,
            actor_transcript_reader: None,
        }
    }

    /// 构造带 actor Transcript 读取能力的生产 Journal 适配器。
    ///
    /// 父 Journal 只保存 actor-started 绑定，工具调用事实仍在 actor Session 自己的
    /// Resource Journal。读取器由 AgentRuntime 注入后，在线与冷恢复都复用同一条
    /// `session_transcript` 路径，不创建第二份 workflow 日志。
    pub fn new_with_actor_transcript_reader(
        session: RuntimeSession,
        actor_transcript_reader: Arc<dyn WorkflowActorTranscriptReader>,
    ) -> Self {
        Self {
            session,
            actor_transcript_reader: Some(actor_transcript_reader),
        }
    }

    fn events_for(&self, run_id: &str) -> Result<Vec<WorkflowJournalEvent>, WorkflowJournalError> {
        self.session
            .read_state(|state| {
                state
                    .workflow_events
                    .get(run_id)
                    .cloned()
                    .unwrap_or_default()
            })
            .map_err(|error| WorkflowJournalError(error.to_string()))
    }

    /// 接管冷会话时只结算“仍有启动事实但没有最新终态”的 run。
    ///
    /// `run-resumed` 写入和异步 Driver 启动之间可能发生进程退出。冷恢复不能猜测
    /// Driver 是否已经产生副作用，因此先把这段不确定窗口明确标记为 interrupted；
    /// 后续 `run_is_resumable` 再按节点事实决定是否允许 Resume，绝不会自动重放。
    pub fn reconcile_cold_runs_at_startup(&self) -> Result<(), WorkflowJournalError> {
        let pending = self
            .session
            .snapshot()
            .map_err(|error| WorkflowJournalError(error.to_string()))?
            .state
            .workflow_events
            .into_iter()
            .filter_map(|(run_id, events)| {
                let started = events
                    .iter()
                    .find(|event| event.event_type == "run-started")?
                    .clone();
                let parent = started
                    .payload
                    .get("parentSessionId")
                    .and_then(Value::as_str);
                if parent != Some(self.session.session_id().as_str())
                    || !run_needs_cold_recovery(&events)
                {
                    return None;
                }
                let updated_at = events
                    .iter()
                    .rev()
                    .find_map(|event| {
                        event
                            .payload
                            .get("updatedAt")
                            .or_else(|| event.payload.get("updated_at"))
                            .and_then(Value::as_u64)
                    })
                    .or_else(|| {
                        started
                            .payload
                            .get("createdAt")
                            .or_else(|| started.payload.get("created_at"))
                            .and_then(Value::as_u64)
                    })
                    .unwrap_or_else(unix_time_ms);
                Some((run_id, started, updated_at))
            })
            .collect::<Vec<_>>();

        for (run_id, started, updated_at) in pending {
            let event = WorkflowJournalEvent {
                run_id,
                tool_call_id: started.tool_call_id.clone(),
                sequence: 0,
                event_type: "run-settled".to_owned(),
                payload: json!({
                    "status": "stopped",
                    "stopReason": "interrupted",
                    "error": {
                        "code": "cold_recovery",
                        "message": "workflow execution was interrupted before its terminal fact was committed"
                    },
                    "updatedAt": updated_at,
                }),
                artifacts: Vec::new(),
                actor_session_id: None,
                launch_input_id: started.launch_input_id.clone(),
            };
            // 固定的时间戳来自已有事实，令重复接管使用同一 operationId；RuntimeSession
            // 会在控制锁内按完整正文去重，避免两个恢复入口追加两条终态。
            self.session
                .append_workflow_event(&super::workflow_event_operation_id(&event), event)
                .map_err(|error| WorkflowJournalError(error.to_string()))?;
        }
        Ok(())
    }

    fn summary_from_events(
        &self,
        run_id: &str,
        events: &[WorkflowJournalEvent],
    ) -> WorkflowRunSummary {
        let started = events
            .iter()
            .find(|event| event.event_type == "run-started");
        let terminal_index = events
            .iter()
            .rposition(|event| event.event_type == "run-settled");
        let terminal = terminal_index.and_then(|index| events.get(index));
        let started_payload = started.map(|event| &event.payload);
        let terminal_payload = terminal.map(|event| &event.payload);
        let indeterminate = terminal_payload
            .and_then(|payload| payload.get("status"))
            .and_then(Value::as_str)
            .is_some_and(|status| status == "indeterminate");
        let status = if resume_is_active(events) {
            WorkflowRunStatus::Running
        } else {
            terminal_payload
                .and_then(|payload| payload.get("status").and_then(Value::as_str))
                .map(parse_status)
                .unwrap_or(WorkflowRunStatus::Running)
        };
        let stop_reason = terminal_payload
            .and_then(|payload| {
                payload
                    .get("stopReason")
                    .or_else(|| payload.get("stop_reason"))
            })
            .and_then(Value::as_str)
            .and_then(parse_stop_reason);
        let created_at = started_payload
            .and_then(|payload| {
                payload
                    .get("createdAt")
                    .or_else(|| payload.get("created_at"))
            })
            .and_then(Value::as_u64)
            .unwrap_or_else(unix_time_ms);
        let updated_at = events
            .last()
            .and_then(|event| {
                event
                    .payload
                    .get("updatedAt")
                    .or_else(|| event.payload.get("updated_at"))
            })
            .and_then(Value::as_u64)
            .unwrap_or(created_at);
        let args = started_payload
            .and_then(|payload| payload.get("inputs").or_else(|| payload.get("args")))
            .cloned()
            .unwrap_or_else(|| json!({}));
        let artifacts = collect_user_artifacts(events)
            .into_iter()
            .filter_map(|artifact| serde_json::to_value(artifact).ok())
            .collect();
        let resumable = run_is_resumable(&status, indeterminate, events);
        WorkflowRunSummary {
            run_id: run_id.to_owned(),
            name: started_payload
                .and_then(|payload| payload.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            status,
            stop_reason,
            created_at,
            updated_at,
            spent_tokens: terminal_payload
                .and_then(|payload| {
                    payload
                        .get("spentTokens")
                        .or_else(|| payload.get("spent_tokens"))
                })
                .and_then(Value::as_u64)
                .unwrap_or(0),
            parent_session_id: started_payload
                .and_then(|payload| {
                    payload
                        .get("parentSessionId")
                        .or_else(|| payload.get("parent_session_id"))
                })
                .and_then(Value::as_str)
                .map(str::to_owned),
            tool_call_id: started.map(|event| event.tool_call_id.clone()),
            args,
            cwd: started_payload
                .and_then(|payload| payload.get("cwd"))
                .and_then(Value::as_str)
                .map(Into::into),
            canonical_hash: started_payload
                .and_then(|payload| {
                    payload
                        .get("canonicalHash")
                        .or_else(|| payload.get("canonical_hash"))
                })
                .and_then(Value::as_str)
                .map(str::to_owned),
            artifacts,
            resumable,
        }
    }

    fn owns_parent(&self, events: &[WorkflowJournalEvent]) -> bool {
        events.first().is_some_and(|event| {
            event
                .payload
                .get("parentSessionId")
                .and_then(Value::as_str)
                .is_some_and(|parent| parent == self.session.session_id().as_str())
        })
    }

    fn actor_workspace_projection(
        &self,
        run_id: &str,
        events: &[WorkflowJournalEvent],
    ) -> Result<Vec<ProjectedActorWorkspaceNode>, WorkflowJournalError> {
        let Some(reader) = self.actor_transcript_reader.as_ref() else {
            return Ok(Vec::new());
        };
        let bindings = actor_bindings(run_id, self.session.session_id().as_str(), events);
        let (created_at, updated_at) = run_times(events);
        bindings
            .into_iter()
            .try_fold(Vec::new(), |mut projected, binding| {
                let transcript = reader
                    .read_transcript(&binding.identity)?
                    .unwrap_or_default();
                projected.extend(project_actor_transcript(
                    &binding,
                    &transcript,
                    created_at,
                    updated_at,
                ));
                Ok(projected)
            })
    }
}

fn run_needs_cold_recovery(events: &[WorkflowJournalEvent]) -> bool {
    if !events.iter().any(|event| event.event_type == "run-started") {
        return false;
    }
    let terminal = events
        .iter()
        .rposition(|event| event.event_type == "run-settled");
    let resumed = events
        .iter()
        .rposition(|event| event.event_type == "run-resumed");
    terminal.is_none_or(|terminal| resumed.is_some_and(|resumed| resumed > terminal))
}

impl WorkflowJournalPort for RuntimeWorkflowJournal {
    fn artifact_capacity(&self) -> Result<ArtifactCapacity, WorkflowJournalError> {
        self.session
            .artifact_capacity()
            .map_err(|error| WorkflowJournalError(error.to_string()))
    }

    fn commit_event(
        &self,
        operation_id: &str,
        event: WorkflowJournalEvent,
    ) -> Result<CommittedWorkflowEvent, WorkflowJournalError> {
        if event.event_type == "run-started"
            && event.payload.get("parentSessionId").and_then(Value::as_str)
                != Some(self.session.session_id().as_str())
        {
            // RuntimeSession 按资源 Journal 隔离数据，但 append 本身不解释 workflow
            // payload；首事实必须在这里绑定当前父会话，避免留下不可见的跨父孤儿 run。
            return Err(WorkflowJournalError(
                "run-started parent session does not match the bound RuntimeSession".to_owned(),
            ));
        }
        // sequence=0 由 RuntimeSession 在控制提交锁内原子分配；Host 不能先读尾序号，
        // 否则 Native actor 与引擎并行提交时会产生相同 sequence。
        let event = self
            .session
            .append_workflow_event(operation_id, event)
            .map_err(|error| WorkflowJournalError(error.to_string()))?;
        Ok(CommittedWorkflowEvent { event })
    }

    fn list_events(
        &self,
        run_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<WorkflowEventPage, WorkflowJournalError> {
        let mut events = self.events_for(run_id)?;
        if !self.owns_parent(&events) {
            return Ok(WorkflowEventPage {
                events: Vec::new(),
                has_more: false,
            });
        }
        events.retain(|event| after_sequence.is_none_or(|after| event.sequence > after));
        let bounded = limit.clamp(1, MAX_WORKFLOW_EVENT_LIMIT);
        let has_more = events.len() > bounded;
        events.truncate(bounded);
        Ok(WorkflowEventPage { events, has_more })
    }

    fn all_events(&self, run_id: &str) -> Result<Vec<WorkflowJournalEvent>, WorkflowJournalError> {
        let events = self.events_for(run_id)?;
        if !self.owns_parent(&events) {
            return Ok(Vec::new());
        }
        Ok(events)
    }

    fn list_runs(
        &self,
        parent_session_id: Option<&str>,
        name: Option<&str>,
        scope: Option<WorkflowScope>,
        limit: usize,
    ) -> Result<WorkflowRunsResult, WorkflowJournalError> {
        let state = self
            .session
            .snapshot()
            .map_err(|error| WorkflowJournalError(error.to_string()))?
            .state;
        let parent = parent_session_id.unwrap_or(self.session.session_id().as_str());
        let mut runs = state
            .workflow_events
            .iter()
            .filter_map(|(run_id, events)| {
                let summary = self.summary_from_events(run_id, events);
                let Some(run_scope) = events
                    .iter()
                    .find(|event| event.event_type == "run-started")
                    .and_then(|event| event.payload.get("scope"))
                    .and_then(|value| serde_json::from_value::<WorkflowScope>(value.clone()).ok())
                else {
                    // scope 是 run-started 的冻结分区；缺失或未知值不能默认为
                    // project，否则损坏事实会出现在错误的 global/project 历史中。
                    return None;
                };
                (summary.parent_session_id.as_deref() == Some(parent)
                    && name.is_none_or(|wanted| summary.name.as_deref() == Some(wanted))
                    && scope.is_none_or(|wanted| run_scope == wanted))
                .then_some(summary)
            })
            .collect::<Vec<_>>();
        runs.sort_by_key(|run| std::cmp::Reverse(run.updated_at));
        let bounded = limit.clamp(1, 50);
        let truncated = runs.len() > bounded;
        runs.truncate(bounded);
        Ok(WorkflowRunsResult { runs, truncated })
    }

    fn find_launches(
        &self,
        parent_session_id: &str,
        launch_input_id: &str,
    ) -> Result<Vec<WorkflowLaunchRecord>, WorkflowJournalError> {
        let state = self
            .session
            .snapshot()
            .map_err(|error| WorkflowJournalError(error.to_string()))?
            .state;
        if parent_session_id != self.session.session_id().as_str() {
            return Ok(Vec::new());
        }
        let mut launches = Vec::new();
        for (run_id, events) in state.workflow_events {
            if !self.owns_parent(&events) {
                continue;
            }
            let Some(event) = events.iter().find(|event| {
                event.event_type == "run-started"
                    && event.launch_input_id.as_deref() == Some(launch_input_id)
                    && event.payload.get("parentSessionId").and_then(Value::as_str)
                        == Some(parent_session_id)
            }) else {
                continue;
            };
            launches.push(WorkflowLaunchRecord {
                run_id,
                event: event.clone(),
            });
        }
        launches.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        Ok(launches)
    }

    fn run_summary(
        &self,
        run_id: &str,
    ) -> Result<Option<WorkflowRunSummary>, WorkflowJournalError> {
        let events = self.events_for(run_id)?;
        if events.is_empty() || !self.owns_parent(&events) {
            return Ok(None);
        }
        Ok(Some(self.summary_from_events(run_id, &events)))
    }

    fn reconcile_cold_runs(&self) -> Result<(), WorkflowJournalError> {
        self.reconcile_cold_runs_at_startup()
    }

    fn list_workspace_nodes(
        &self,
        run_id: &str,
    ) -> Result<Option<WorkflowWorkspaceResult>, WorkflowJournalError> {
        let events = self.events_for(run_id)?;
        if events.is_empty() || !self.owns_parent(&events) {
            return Ok(None);
        }
        let mut nodes = Vec::new();
        for event in events
            .iter()
            .filter(|event| workspace_event_kind(&event.event_type).is_some())
        {
            let payload = &event.payload;
            let kind = workspace_event_kind(&event.event_type)
                .expect("workspace event filter guarantees a visible kind");
            let op = payload
                .get("op")
                .or_else(|| payload.get("toolName"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            nodes.push(WorkflowWorkspaceNode {
                site_id: payload
                    .get("siteId")
                    .or_else(|| payload.get("site_id"))
                    .or_else(|| payload.get("nodeId"))
                    .or_else(|| payload.get("node_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("world")
                    .to_owned(),
                ordinal: payload
                    .get("ordinal")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_else(|| u32::try_from(event.sequence).unwrap_or(u32::MAX)),
                kind: kind.to_owned(),
                op: op.clone(),
                args: frontend_workspace_args(
                    op.as_deref(),
                    payload.get("args").and_then(Value::as_array).cloned(),
                ),
                status: payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed")
                    .to_owned(),
                error: payload.get("error").cloned(),
                summary: payload.get("summary").cloned(),
                created_at: payload
                    .get("createdAt")
                    .or_else(|| payload.get("created_at"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                updated_at: payload
                    .get("updatedAt")
                    .or_else(|| payload.get("updated_at"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            });
        }
        // Agent 节点的 Read/Edit/Bash 事实落在 actor 自己的 Session Journal。只接受
        // 父 run 已提交的 actor-started 绑定，再把同一事实投影为既有 workspace 行。
        // 这样查询不会跨 run/跨父会话读取任意 actor，也不会把 Transcript 当成第二事实源。
        nodes.extend(
            self.actor_workspace_projection(run_id, &events)?
                .into_iter()
                .map(|projected| projected.node),
        );
        Ok(Some(WorkflowWorkspaceResult {
            truncated: nodes.len() > 2_000,
            nodes: nodes.into_iter().take(2_000).collect(),
        }))
    }

    fn read_node_result(
        &self,
        run_id: &str,
        site_id: &str,
        ordinal: u32,
        max_bytes: usize,
    ) -> Result<Option<WorkflowNodeResult>, WorkflowJournalError> {
        let events = self.events_for(run_id)?;
        if events.is_empty() || !self.owns_parent(&events) {
            return Ok(None);
        }
        let Some(event) = events.iter().rev().find(|event| {
            workspace_event_kind(&event.event_type).is_some()
                && event
                    .payload
                    .get("siteId")
                    .or_else(|| event.payload.get("site_id"))
                    .or_else(|| event.payload.get("nodeId"))
                    .or_else(|| event.payload.get("node_id"))
                    .and_then(Value::as_str)
                    == Some(site_id)
                && event
                    .payload
                    .get("ordinal")
                    .and_then(Value::as_u64)
                    .unwrap_or(event.sequence)
                    == u64::from(ordinal)
        }) else {
            let Some(projected) = self
                .actor_workspace_projection(run_id, &events)?
                .into_iter()
                .find(|projected| {
                    projected.node.site_id == site_id && projected.node.ordinal == ordinal
                })
            else {
                return Ok(None);
            };
            return Ok(Some(projected.result));
        };
        let status = event
            .payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("completed");
        // V4 nodeResult 的正文只属于已完成节点；失败/运行中的事件只能返回
        // status 与结构化 error，不能把包含控制字段的整条 Journal payload 当正文。
        let result = matches!(status, "completed" | "succeeded" | "applied").then(|| {
            event
                .payload
                .get("result")
                .cloned()
                .unwrap_or_else(|| event.payload.clone())
        });
        let total_bytes = result
            .as_ref()
            .and_then(|value| serde_json::to_vec(value).ok())
            .map_or(0, |bytes| bytes.len());
        let truncated = total_bytes > max_bytes;
        let result = result.map(|value| bound_json(value, max_bytes));
        Ok(Some(WorkflowNodeResult {
            status: status.to_owned(),
            result,
            error: event.payload.get("error").cloned(),
            truncated,
            total_bytes,
        }))
    }

    fn list_artifacts(
        &self,
        run_id: &str,
    ) -> Result<Option<Vec<WorkflowArtifact>>, WorkflowJournalError> {
        let events = self.events_for(run_id)?;
        if events.is_empty() || !self.owns_parent(&events) {
            return Ok(None);
        }
        Ok(Some(collect_user_artifacts(&events)))
    }

    fn list_artifact_items(
        &self,
        run_id: &str,
        artifact_id: &str,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<WorkflowArtifactPage, WorkflowJournalError> {
        let events = self.events_for(run_id)?;
        if events.is_empty() || !self.owns_parent(&events) {
            return Ok(WorkflowArtifactPage {
                items: Vec::new(),
                has_more: false,
            });
        }
        let mut items = events
            .iter()
            .filter(|event| event.event_type == "artifact-item" || event.event_type == "report")
            .filter(|event| after_sequence.is_none_or(|after| event.sequence > after))
            .filter(|event| {
                event
                    .payload
                    .get("artifactId")
                    .or_else(|| event.payload.get("artifact_id"))
                    .and_then(Value::as_str)
                    == Some(artifact_id)
            })
            .map(|event| {
                json!({
                    "sequence": event.sequence,
                    "siteId": event.payload.get("siteId").or_else(|| event.payload.get("site_id")).cloned().unwrap_or_else(|| json!("workflow")),
                    "ordinal": event.payload.get("ordinal").cloned().unwrap_or_else(|| json!(0)),
                    "item": event.payload.get("item").cloned().unwrap_or_else(|| event.payload.clone()),
                })
            })
            .collect::<Vec<_>>();
        let bounded = limit.clamp(1, MAX_WORKFLOW_EVENT_LIMIT);
        let has_more = items.len() > bounded;
        items.truncate(bounded);
        Ok(WorkflowArtifactPage { items, has_more })
    }

    fn read_artifact(
        &self,
        run_id: &str,
        artifact_id: &str,
        version: u32,
        offset: usize,
        limit: usize,
    ) -> Result<Option<WorkflowArtifactBytes>, WorkflowJournalError> {
        let events = self.events_for(run_id)?;
        if events.is_empty() || !self.owns_parent(&events) {
            return Ok(None);
        }
        let Some(reference) = events
            .iter()
            .filter(|event| is_user_artifact_event(event))
            .flat_map(|event| {
                event.artifacts.iter().filter(move |reference| {
                    reference.artifact_id.to_string() == artifact_id
                        && artifact_event_version(&event.payload) == Some(version)
                        && artifact_from_event(event, reference).is_some()
                })
            })
            .next()
            .cloned()
        else {
            return Ok(None);
        };
        let artifact_id = ArtifactId::new(reference.artifact_id.to_string())
            .map_err(|error| WorkflowJournalError(error.to_string()))?;
        let bytes = self
            .session
            .read_workflow_artifact(run_id, &artifact_id)
            .map_err(|error| WorkflowJournalError(error.to_string()))?;
        if offset >= bytes.len() {
            return Ok(Some(WorkflowArtifactBytes {
                data_base64: String::new(),
                media_type: reference
                    .media_type
                    .unwrap_or_else(|| "application/octet-stream".to_owned()),
                total_bytes: bytes.len(),
                next_offset: None,
            }));
        }
        let end = offset.saturating_add(limit.max(1)).min(bytes.len());
        Ok(Some(WorkflowArtifactBytes {
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end]),
            media_type: reference
                .media_type
                .unwrap_or_else(|| "application/octet-stream".to_owned()),
            total_bytes: bytes.len(),
            next_offset: (end < bytes.len()).then_some(end),
        }))
    }
}

const MAX_WORKFLOW_ARTIFACT_VERSIONS: u32 = 16;

fn is_user_artifact_event(event: &WorkflowJournalEvent) -> bool {
    matches!(
        event.event_type.as_str(),
        "artifact-committed" | "artifact-published"
    )
}

fn collect_user_artifacts(events: &[WorkflowJournalEvent]) -> Vec<WorkflowArtifact> {
    let mut by_id = BTreeMap::<String, WorkflowArtifact>::new();
    for event in events.iter().filter(|event| is_user_artifact_event(event)) {
        for reference in &event.artifacts {
            let Some(candidate) = artifact_from_event(event, reference) else {
                // Source artifact 版本/kind/publishedAt 不完整或超出封闭枚举时，
                // 直接丢弃该投影，不能用 file/v1 填充成看似有效的产物。
                continue;
            };
            let entry = by_id
                .entry(candidate.id.clone())
                .or_insert_with(|| candidate.clone());
            merge_artifact(entry, candidate);
        }
    }
    for event in events.iter().filter(|event| event.event_type == "report") {
        let Some(artifact_id) = event.payload.get("artifactId").and_then(Value::as_str) else {
            continue;
        };
        if let Some(artifact) = by_id.get_mut(artifact_id) {
            artifact.item_count = artifact.item_count.saturating_add(1);
        }
    }
    by_id.into_values().collect()
}

fn artifact_from_event(
    event: &WorkflowJournalEvent,
    reference: &ArtifactUse,
) -> Option<WorkflowArtifact> {
    let payload = &event.payload;
    let version = artifact_event_version(payload)?;
    let kind = artifact_event_kind(payload)?;
    let content_type = payload
        .get("contentType")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| reference.media_type.clone());
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(120).collect());
    let description = payload
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(500).collect());
    let source_path = payload
        .get("sourcePath")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(1_024).collect());
    let spec = payload
        .get("spec")
        .filter(|value| !value.is_null())
        .cloned();
    let published_at = payload.get("publishedAt").and_then(Value::as_u64)?;
    let mut version_record = Map::new();
    version_record.insert("version".to_owned(), json!(version));
    version_record.insert("bytes".to_owned(), json!(reference.size_bytes));
    if let Some(content_type) = content_type.clone() {
        version_record.insert("contentType".to_owned(), Value::String(content_type));
    }
    if let Some(title) = title.clone() {
        version_record.insert("title".to_owned(), Value::String(title));
    }
    if let Some(description) = description.clone() {
        version_record.insert("description".to_owned(), Value::String(description));
    }
    if let Some(source_path) = source_path.clone() {
        version_record.insert("sourcePath".to_owned(), Value::String(source_path));
    }
    if let Some(spec) = spec.clone() {
        version_record.insert("spec".to_owned(), spec);
    }
    version_record.insert("publishedAt".to_owned(), json!(published_at));
    if payload.get("primary").and_then(Value::as_bool) == Some(true) {
        version_record.insert("primary".to_owned(), Value::Bool(true));
    }
    Some(WorkflowArtifact {
        id: reference.artifact_id.to_string(),
        kind,
        title,
        description,
        content_type,
        source_path,
        spec,
        version,
        versions: vec![Value::Object(version_record)],
        item_count: 0,
        primary: payload.get("primary").and_then(Value::as_bool) == Some(true),
    })
}

fn merge_artifact(current: &mut WorkflowArtifact, candidate: WorkflowArtifact) {
    if candidate.version >= current.version {
        current.version = candidate.version;
        current.kind = candidate.kind;
        current.title = candidate.title;
        current.description = candidate.description;
        current.content_type = candidate.content_type;
        current.source_path = candidate.source_path;
        current.spec = candidate.spec;
    }
    current.primary |= candidate.primary;
    for version in candidate.versions {
        let Some(version_number) = version.get("version").and_then(Value::as_u64) else {
            continue;
        };
        if let Some(existing) = current.versions.iter_mut().find(|existing| {
            existing.get("version").and_then(Value::as_u64) == Some(version_number)
        }) {
            *existing = version;
        } else {
            current.versions.push(version);
        }
    }
    current.versions.sort_by_key(|version| {
        version
            .get("version")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    });
}

fn artifact_event_version(payload: &Value) -> Option<u32> {
    payload
        .get("version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| (1..=MAX_WORKFLOW_ARTIFACT_VERSIONS).contains(value))
}

fn artifact_event_kind(payload: &Value) -> Option<String> {
    let declared = payload.get("kind").and_then(Value::as_str)?;
    match declared {
        "markdown" => Some("markdown".to_owned()),
        "chart" => Some("chart".to_owned()),
        "table" => Some("table".to_owned()),
        "metrics" => Some("metrics".to_owned()),
        "board" => Some("board".to_owned()),
        "file" => Some("file".to_owned()),
        _ => None,
    }
}

/// 将父 Journal 中的世界读写证据投影到 Workflow workspace transcript。
///
/// `world-read` 是现有工作流节点直接产生的读操作；文件写入由 AgentRuntime
/// 先后提交 prepared/applied 事件，二者在对外 transcript 中都属于 `world-run`。
fn workspace_event_kind(event_type: &str) -> Option<&'static str> {
    match event_type {
        "world-read" => Some("world-read"),
        "world-run" | "file-change-prepared" | "file-change-applied" => Some("world-run"),
        _ => None,
    }
}

#[derive(Clone, Debug)]
struct ActorBinding {
    identity: WorkflowActorBinding,
    site_id: String,
}

#[derive(Clone, Debug)]
struct ProjectedActorWorkspaceNode {
    node: WorkflowWorkspaceNode,
    result: WorkflowNodeResult,
}

#[derive(Clone, Debug)]
struct ActorWorkspaceCall {
    kind: &'static str,
    op: &'static str,
    args: Vec<Value>,
}

/// 只规范化已知工具的路径参数；pattern、命令和 Journal 原始 arguments 保持不变。
/// `path_text_to_frontend` 同时处理普通盘符、UNC 和 Windows verbatim 前缀。
fn frontend_workspace_args(op: Option<&str>, args: Option<Vec<Value>>) -> Option<Vec<Value>> {
    let mut args = args?;
    let Some(path_index) = (match op {
        Some(value)
            if value.eq_ignore_ascii_case("read")
                || value.eq_ignore_ascii_case("edit")
                || value.eq_ignore_ascii_case("write") =>
        {
            Some(0)
        }
        Some(value) if value.eq_ignore_ascii_case("glob") || value.eq_ignore_ascii_case("grep") => {
            Some(1)
        }
        _ => None,
    }) else {
        return Some(args);
    };
    if let Some(Value::String(path)) = args.get_mut(path_index) {
        *path = crate::path_utils::path_text_to_frontend(path);
    }
    Some(args)
}

/// 运行时间只作为 transcript 的展示时刻，不参与事实判断。Actor Transcript 本身没有
/// 单独的 UI 时间字段，所以使用父 run 的 Journal 时间；工具正文/状态仍来自 actor 消息。
fn run_times(events: &[WorkflowJournalEvent]) -> (u64, u64) {
    let created_at = events
        .iter()
        .find(|event| event.event_type == "run-started")
        .and_then(|event| payload_u64(&event.payload, "createdAt"))
        .unwrap_or(0);
    let updated_at = events
        .iter()
        .rev()
        .find_map(|event| payload_u64(&event.payload, "updatedAt"))
        .unwrap_or(created_at);
    (created_at, updated_at.max(created_at))
}

fn payload_u64(payload: &Value, field: &str) -> Option<u64> {
    payload.get(field).and_then(Value::as_u64)
}

fn actor_bindings(
    run_id: &str,
    parent_session_id: &str,
    events: &[WorkflowJournalEvent],
) -> Vec<ActorBinding> {
    let Some(project_root) = events
        .iter()
        .find(|event| event.event_type == "run-started")
        .and_then(|event| event.payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Vec::new();
    };
    let mut seen_sessions = BTreeSet::new();
    let mut seen_sites = BTreeSet::new();
    let mut bindings = Vec::new();
    for event in events
        .iter()
        .filter(|event| event.event_type == "actor-started")
    {
        if event.run_id != run_id {
            continue;
        }
        let payload = &event.payload;
        if payload.get("parentSessionId").and_then(Value::as_str) != Some(parent_session_id)
            || payload.get("runId").and_then(Value::as_str) != Some(run_id)
        {
            continue;
        }
        let Some(actor_session_id) = payload
            .get("actorSessionId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .or(event.actor_session_id.as_deref())
        else {
            continue;
        };
        if event
            .actor_session_id
            .as_deref()
            .is_some_and(|bound| bound != actor_session_id)
        {
            continue;
        }
        let Some(node_id) = payload
            .get("nodeId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let Some(node_address) = payload.get("nodeAddress") else {
            continue;
        };
        let Some(actor_project_root) = payload
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        if actor_project_root != project_root {
            continue;
        }
        if !seen_sessions.insert(actor_session_id.to_owned()) {
            continue;
        }
        let mut site_id = format!("actor/{node_id}");
        if site_id.chars().count() > 64 {
            site_id = site_id.chars().take(64).collect();
        }
        if !seen_sites.insert(site_id.clone()) {
            let suffix = format!("/{:x}", event.sequence);
            let prefix_len = 64usize.saturating_sub(suffix.chars().count());
            site_id = format!(
                "{}{}",
                site_id.chars().take(prefix_len).collect::<String>(),
                suffix
            );
            if !seen_sites.insert(site_id.clone()) {
                continue;
            }
        }
        bindings.push(ActorBinding {
            identity: WorkflowActorBinding {
                actor_session_id: actor_session_id.to_owned(),
                parent_session_id: parent_session_id.to_owned(),
                run_id: run_id.to_owned(),
                node_id: node_id.to_owned(),
                node_address: node_address.clone(),
                project_root: project_root.to_owned(),
            },
            site_id,
        });
    }
    bindings
}

fn project_actor_transcript(
    binding: &ActorBinding,
    transcript: &[SessionMessage],
    created_at: u64,
    updated_at: u64,
) -> Vec<ProjectedActorWorkspaceNode> {
    let mut projected = Vec::new();
    let mut call_indexes = BTreeMap::<String, usize>::new();
    for message in transcript {
        for part in &message.content {
            match part {
                MessagePart::ToolCall {
                    tool_call_id,
                    tool_name,
                    arguments,
                } => {
                    let Some(call) = actor_workspace_call(tool_name, arguments) else {
                        continue;
                    };
                    let ordinal = u32::try_from(projected.len()).unwrap_or(u32::MAX);
                    let node = WorkflowWorkspaceNode {
                        site_id: binding.site_id.clone(),
                        ordinal,
                        kind: call.kind.to_owned(),
                        op: Some(call.op.to_owned()),
                        args: Some(call.args),
                        status: "running".to_owned(),
                        error: None,
                        summary: None,
                        created_at,
                        updated_at: created_at,
                    };
                    let result = WorkflowNodeResult {
                        status: "running".to_owned(),
                        result: None,
                        error: None,
                        truncated: false,
                        total_bytes: 0,
                    };
                    call_indexes.insert(tool_call_id.clone(), projected.len());
                    projected.push(ProjectedActorWorkspaceNode { node, result });
                }
                MessagePart::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    let Some(index) = call_indexes.get(tool_call_id).copied() else {
                        continue;
                    };
                    let Some(entry) = projected.get_mut(index) else {
                        continue;
                    };
                    entry.node.updated_at = updated_at;
                    if *is_error {
                        let message = bounded_text(&tool_result_value(content));
                        let error = json!({
                            "code": "tool_error",
                            "message": message,
                        });
                        entry.node.status = "failed".to_owned();
                        entry.node.error = Some(error.clone());
                        entry.node.summary = None;
                        entry.result.status = "failed".to_owned();
                        entry.result.error = Some(error);
                        entry.result.result = None;
                    } else {
                        let value = tool_result_value(content);
                        entry.node.status = "completed".to_owned();
                        entry.node.summary =
                            Some(workspace_summary(&value, entry.node.op.as_deref()));
                        entry.result.status = "completed".to_owned();
                        entry.result.total_bytes = result_bytes(&value);
                        entry.result.result = Some(value);
                    }
                }
                _ => {}
            }
        }
    }
    projected
}

fn actor_workspace_call(tool_name: &str, arguments: &Value) -> Option<ActorWorkspaceCall> {
    let field = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|value| Value::String(value.to_owned()))
    };
    let path_field = |name: &str| {
        field(name).map(|value| match value {
            Value::String(path) => Value::String(crate::path_utils::path_text_to_frontend(&path)),
            other => other,
        })
    };
    match tool_name {
        "Read" => Some(ActorWorkspaceCall {
            kind: "world-read",
            op: "read",
            args: vec![path_field("file_path")?],
        }),
        "Glob" => Some(ActorWorkspaceCall {
            kind: "world-read",
            op: "glob",
            args: vec![
                field("pattern")?,
                path_field("path").unwrap_or_else(|| Value::String(".".to_owned())),
            ],
        }),
        "Grep" => Some(ActorWorkspaceCall {
            kind: "world-read",
            op: "grep",
            args: vec![
                field("pattern")?,
                path_field("path").unwrap_or_else(|| Value::String(".".to_owned())),
            ],
        }),
        "Bash" | "PowerShell" => Some(ActorWorkspaceCall {
            kind: "world-run",
            op: "run",
            args: vec![field("command")?],
        }),
        "Edit" => Some(ActorWorkspaceCall {
            kind: "world-run",
            op: "edit",
            args: vec![path_field("file_path")?],
        }),
        "Write" => Some(ActorWorkspaceCall {
            kind: "world-run",
            op: "write",
            args: vec![path_field("file_path")?],
        }),
        _ => None,
    }
}

fn tool_result_value(parts: &[ToolResultPart]) -> Value {
    let mut text_parts = Vec::new();
    let mut all_text = true;
    let mut values = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            ToolResultPart::Text { text } => text_parts.push(text.clone()),
            _ => all_text = false,
        }
        values.push(serde_json::to_value(part).unwrap_or(Value::Null));
    }
    if all_text {
        Value::String(text_parts.join("\n"))
    } else {
        Value::Array(values)
    }
}

fn result_bytes(value: &Value) -> usize {
    value
        .as_str()
        .map(str::len)
        .or_else(|| serde_json::to_vec(value).ok().map(|bytes| bytes.len()))
        .unwrap_or(0)
}

fn workspace_summary(value: &Value, op: Option<&str>) -> Value {
    let mut summary = Map::new();
    summary.insert("resultBytes".to_owned(), json!(result_bytes(value)));
    if matches!(op, Some("glob" | "grep"))
        && let Some(items) = value.as_array()
    {
        summary.insert("resultCount".to_owned(), json!(items.len()));
    }
    Value::Object(summary)
}

fn bounded_text(value: &Value) -> String {
    let text = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    text.chars().take(2_000).collect()
}

fn resume_is_active(events: &[WorkflowJournalEvent]) -> bool {
    let Some(resume_index) = events
        .iter()
        .rposition(|event| event.event_type == "run-resumed")
    else {
        return false;
    };
    let terminal_index = events
        .iter()
        .rposition(|event| event.event_type == "run-settled");
    terminal_index.is_none_or(|terminal| resume_index > terminal)
}

fn event_identity(payload: &Value) -> Option<String> {
    [
        "address",
        "changeId",
        "change_id",
        "path",
        "filePath",
        "file_path",
        "siteId",
        "site_id",
        "nodeId",
        "node_id",
    ]
    .into_iter()
    .find_map(|field| payload.get(field))
    .filter(|value| !value.is_null())
    .and_then(|value| serde_json::to_string(value).ok())
}

/// 冷恢复与在线状态共用的 resume 门禁：成功结算的写入不会再次调用 driver，
/// 只有仍悬挂或以非成功状态结算的写入/未知副作用才禁止恢复；这让已完成写节点
/// 可以从缓存重用，同时把崩溃或取消落在副作用窗口内的 run 留在不可恢复态。
fn run_is_resumable(
    status: &WorkflowRunStatus,
    indeterminate: bool,
    events: &[WorkflowJournalEvent],
) -> bool {
    if indeterminate
        || !matches!(
            status,
            WorkflowRunStatus::Stopped | WorkflowRunStatus::Errored
        )
    {
        return false;
    }

    let definition = events
        .iter()
        .find(|event| event.event_type == "run-started")
        .and_then(|event| event.payload.get("definition"))
        .and_then(|value| serde_json::from_value::<WorkflowDefinition>(value.clone()).ok());
    let mut pending = BTreeMap::<String, PendingRecoveryNode>::new();
    for event in events {
        match event.event_type.as_str() {
            "node-started" => {
                let Some(effect) = event.payload.get("effect").and_then(Value::as_str) else {
                    return false;
                };
                let identity = event_identity(&event.payload)
                    .unwrap_or_else(|| format!("sequence:{}", event.sequence));
                let recoverable_read =
                    if effect == "read_only" {
                        match (
                            definition.as_ref(),
                            event.payload.get("address").cloned().and_then(|value| {
                                serde_json::from_value::<NodeAddress>(value).ok()
                            }),
                        ) {
                            (Some(definition), Some(address)) => {
                                is_recovery_safe_read_node(&definition.body, &address.node_id)
                                    .unwrap_or(false)
                            }
                            _ => false,
                        }
                    } else {
                        false
                    };
                pending.insert(
                    identity,
                    PendingRecoveryNode {
                        effect: effect.to_owned(),
                        recoverable_read,
                    },
                );
            }
            "node-settled" | "node-reused" => {
                if let Some(identity) = event_identity(&event.payload) {
                    if event.event_type == "node-settled" {
                        let succeeded = event
                            .payload
                            .get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|status| {
                                matches!(status, "succeeded" | "success" | "completed")
                            });
                        if !succeeded
                            && pending
                                .get(&identity)
                                .is_some_and(|node| !node.recoverable_read)
                        {
                            // Mutating nodes are recorded as started before the Driver call.
                            // Agent nodes are also not safe to replay merely because they said
                            // read_only: Native Runtime has no hard read-only actor recovery.
                            return false;
                        }
                    }
                    pending.remove(&identity);
                }
            }
            // prepared 表示写入窗口已经打开；只有 paired applied 事件能把它结算掉。
            "file-change-prepared" => {
                let identity = event_identity(&event.payload)
                    .unwrap_or_else(|| format!("sequence:{}", event.sequence));
                pending.insert(
                    identity,
                    PendingRecoveryNode {
                        effect: "write".to_owned(),
                        recoverable_read: false,
                    },
                );
            }
            "file-change-applied" => {
                if let Some(identity) = event_identity(&event.payload) {
                    pending.remove(&identity);
                }
            }
            // 这些事件已经明确告诉宿主无法证明外部副作用，不允许猜测恢复。
            "write-unknown" | "tool-indeterminate" => return false,
            "world-run" => {
                let state = event
                    .payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("running");
                if !matches!(state, "completed" | "succeeded") {
                    // world-run 代表工作区或其他外部写入；失败/取消只说明工具返回了
                    // 非成功结果，不能证明副作用没有发生，冷恢复必须停在人工处理。
                    return false;
                }
            }
            _ => {
                // 未知副作用只在没有明确结算事件时阻断；已结算 node 允许通过
                // engine 的 completed cache 重用，避免重新执行外部动作。
                if event
                    .payload
                    .get("effect")
                    .and_then(Value::as_str)
                    .is_some_and(|effect| effect == "unknown")
                {
                    return false;
                }
            }
        }
    }

    !pending
        .values()
        .any(|node| !node.recoverable_read || matches!(node.effect.as_str(), "write" | "unknown"))
}

#[derive(Clone, Debug)]
struct PendingRecoveryNode {
    effect: String,
    recoverable_read: bool,
}

/// 判断 Journal 地址对应的定义节点是否允许只读恢复。
///
/// 控制节点只负责重算分支/循环结构，本身没有外部副作用；取消时它们可以和
/// 已确认的 Read 叶节点一起重建。Agent、Artifact 和除 Read 外的工具仍保持拒绝。
fn is_recovery_safe_read_node(body: &[Node], node_id: &str) -> Option<bool> {
    body.iter().find_map(|node| match node {
        Node::Sequence { node_id: id, .. } | Node::Parallel { node_id: id, .. }
            if id == node_id =>
        {
            Some(true)
        }
        Node::If { node_id: id, .. } if id == node_id => Some(true),
        Node::Foreach { node_id: id, .. } | Node::Repeat { node_id: id, .. } if id == node_id => {
            Some(true)
        }
        Node::Sequence { nodes, .. } | Node::Parallel { nodes, .. } => {
            is_recovery_safe_read_node(nodes, node_id)
        }
        Node::If {
            then_body,
            else_body,
            ..
        } => is_recovery_safe_read_node(then_body, node_id)
            .or_else(|| is_recovery_safe_read_node(else_body, node_id)),
        Node::Foreach { body, .. } | Node::Repeat { body, .. } => {
            is_recovery_safe_read_node(body, node_id)
        }
        Node::Agent(node) if node.node_id == node_id => Some(false),
        Node::Tool(node) if node.node_id == node_id => Some(node.name == "Read"),
        Node::Artifact(node) if node.node_id == node_id => Some(false),
        _ => None,
    })
}

fn parse_status(value: &str) -> WorkflowRunStatus {
    match value {
        "completed" | "succeeded" => WorkflowRunStatus::Completed,
        // Engine 的 indeterminate 表示副作用结果无法证明，必须进入不可恢复的错误态；
        // 将它误归为 running 会让 UI 永久显示运行中并错误开放 resume。
        "errored" | "failed" | "indeterminate" => WorkflowRunStatus::Errored,
        "stopped" | "cancelled" => WorkflowRunStatus::Stopped,
        "pending" => WorkflowRunStatus::Pending,
        _ => WorkflowRunStatus::Running,
    }
}

fn parse_stop_reason(value: &str) -> Option<WorkflowRunStopReason> {
    Some(match value {
        "user" => WorkflowRunStopReason::User,
        "model" => WorkflowRunStopReason::Model,
        "provider" => WorkflowRunStopReason::Provider,
        "interrupted" => WorkflowRunStopReason::Interrupted,
        "superseded" => WorkflowRunStopReason::Superseded,
        _ => return None,
    })
}

fn bound_json(value: Value, max_bytes: usize) -> Value {
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    if bytes.len() <= max_bytes {
        return value;
    }
    Value::String(String::from_utf8_lossy(&bytes[..max_bytes.min(bytes.len())]).into_owned())
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keencode_runtime::{CreateSessionRequest, OpenSessionResult, RuntimeConfig};
    use keencode_workflow::{ToolNode, ValueExpr};

    fn event(event_type: &str, payload: Value) -> WorkflowJournalEvent {
        WorkflowJournalEvent {
            run_id: "run-1".to_owned(),
            tool_call_id: "tool-1".to_owned(),
            sequence: 1,
            event_type: event_type.to_owned(),
            payload,
            artifacts: Vec::new(),
            actor_session_id: None,
            launch_input_id: None,
        }
    }

    fn actor_event(sequence: u64, event_type: &str, payload: Value) -> WorkflowJournalEvent {
        WorkflowJournalEvent {
            run_id: "run-1".to_owned(),
            tool_call_id: "tool-1".to_owned(),
            sequence,
            event_type: event_type.to_owned(),
            payload,
            artifacts: Vec::new(),
            actor_session_id: Some("actor-1".to_owned()),
            launch_input_id: None,
        }
    }

    fn actor_message(role: &str, content: Value) -> SessionMessage {
        serde_json::from_value(json!({
            "messageId": format!("message-{}", role),
            "turnId": null,
            "agentId": null,
            "role": role,
            "content": content,
        }))
        .expect("测试 Transcript 消息应符合资源层 schema")
    }

    fn read_tool_definition() -> Value {
        serde_json::to_value(WorkflowDefinition {
            version: keencode_workflow::WORKFLOW_DEFINITION_VERSION,
            meta: WorkflowMeta {
                id: None,
                name: "read-recovery".to_owned(),
                description: None,
                when_to_use: None,
                tags: Vec::new(),
            },
            inputs: BTreeMap::new(),
            body: vec![Node::Tool(ToolNode {
                node_id: "read".to_owned(),
                name: "Read".to_owned(),
                input: ValueExpr::literal(json!({"file_path": "missing.txt"})),
                config: Value::Null,
                output_type: None,
                effect: EffectClass::ReadOnly,
            })],
        })
        .expect("只读 Read 定义应可序列化")
    }

    #[test]
    fn actor_read_transcript_projects_existing_workspace_contract() {
        let events = vec![
            event(
                "run-started",
                json!({
                    "parentSessionId": "parent-1",
                    "createdAt": 10,
                    "cwd": "/workspace",
                    "inputs": {}
                }),
            ),
            actor_event(
                2,
                "actor-started",
                json!({
                    "actorSessionId": "actor-1",
                    "parentSessionId": "parent-1",
                    "runId": "run-1",
                    "nodeId": "review_left",
                    "nodeAddress": {"node_id": "review_left", "invocation": []},
                    "cwd": "/workspace"
                }),
            ),
        ];
        let binding = actor_bindings("run-1", "parent-1", &events)
            .into_iter()
            .next()
            .expect("父 Journal 应产生 actor 绑定");
        assert_eq!(binding.identity.actor_session_id, "actor-1");
        assert_eq!(binding.identity.project_root, "/workspace");
        assert_eq!(binding.identity.node_address["node_id"], "review_left");
        let transcript = vec![
            actor_message(
                "assistant",
                json!([{
                    "type": "tool_call",
                    "tool_call_id": "read-1",
                    "tool_name": "Read",
                    "arguments": {"file_path": "evidence.txt"}
                }]),
            ),
            actor_message(
                "tool",
                json!([{
                    "type": "tool_result",
                    "tool_call_id": "read-1",
                    "content": [{"type": "text", "text": "NATIVE_FILE_OK_41"}],
                    "is_error": false
                }]),
            ),
        ];
        let projected = project_actor_transcript(&binding, &transcript, 10, 20);
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].node.kind, "world-read");
        assert_eq!(projected[0].node.op.as_deref(), Some("read"));
        assert_eq!(projected[0].node.args, Some(vec![json!("evidence.txt")]));
        assert_eq!(projected[0].node.status, "completed");
        assert_eq!(projected[0].result.result, Some(json!("NATIVE_FILE_OK_41")));
        assert_eq!(projected[0].node.summary, Some(json!({"resultBytes": 17})));
    }

    #[test]
    fn workspace_projection_normalizes_known_paths_without_rewriting_other_arguments() {
        let read_args = frontend_workspace_args(
            Some("Read"),
            Some(vec![
                json!(r"\\?\D:\project\evidence.txt"),
                json!(r"pattern\with\slashes"),
            ]),
        )
        .expect("Read 应保留参数数组");
        assert_eq!(read_args[0], json!("D:/project/evidence.txt"));
        assert_eq!(read_args[1], json!(r"pattern\with\slashes"));

        let grep_args = frontend_workspace_args(
            Some("grep"),
            Some(vec![
                json!(r"literal\pattern"),
                json!(r"\\?\UNC\server\share\project\evidence.txt"),
            ]),
        )
        .expect("Grep 应保留参数数组");
        assert_eq!(grep_args[0], json!(r"literal\pattern"));
        assert_eq!(grep_args[1], json!("//server/share/project/evidence.txt"));

        let run_args =
            frontend_workspace_args(Some("run"), Some(vec![json!(r"\\?\D:\project\script.cmd")]))
                .expect("run 应保留参数数组");
        assert_eq!(run_args[0], json!(r"\\?\D:\project\script.cmd"));

        let unknown_args = frontend_workspace_args(
            Some("future-op"),
            Some(vec![json!(r"\\?\D:\project\opaque-value")]),
        )
        .expect("未知操作也应保留原始参数数组");
        assert_eq!(unknown_args[0], json!(r"\\?\D:\project\opaque-value"));
    }

    #[test]
    fn actor_workspace_call_normalizes_file_paths_but_not_commands_or_patterns() {
        let read = actor_workspace_call(
            "Read",
            &json!({"file_path": r"\\?\D:\project\evidence.txt"}),
        )
        .expect("Read 应产生 workspace 节点");
        assert_eq!(read.args, vec![json!("D:/project/evidence.txt")]);

        let grep = actor_workspace_call(
            "Grep",
            &json!({
                "pattern": r"literal\pattern",
                "path": r"\\?\UNC\server\share\project"
            }),
        )
        .expect("Grep 应产生 workspace 节点");
        assert_eq!(grep.args[0], json!(r"literal\pattern"));
        assert_eq!(grep.args[1], json!("//server/share/project"));

        let bash = actor_workspace_call(
            "Bash",
            &json!({"command": r"type \\?\D:\project\evidence.txt"}),
        )
        .expect("Bash 应产生 workspace 节点");
        assert_eq!(bash.args, vec![json!(r"type \\?\D:\project\evidence.txt")]);
    }

    #[test]
    fn actor_workspace_projection_rejects_unbound_parent_and_keeps_failed_tool() {
        let run_started = event(
            "run-started",
            json!({
                "parentSessionId": "parent-1",
                "runId": "run-1",
                "cwd": "/workspace",
                "createdAt": 10
            }),
        );
        let events = [actor_event(
            1,
            "actor-started",
            json!({
                "actorSessionId": "actor-1",
                "parentSessionId": "other-parent",
                "runId": "run-1",
                "nodeId": "review_left",
                "nodeAddress": {"node_id": "review_left", "invocation": []},
                "cwd": "/workspace"
            }),
        )];
        assert!(
            actor_bindings(
                "run-1",
                "parent-1",
                &[run_started.clone(), events[0].clone()]
            )
            .is_empty()
        );
        let wrong_run = vec![
            run_started.clone(),
            actor_event(
                1,
                "actor-started",
                json!({
                    "actorSessionId": "actor-1",
                    "parentSessionId": "parent-1",
                    "runId": "run-2",
                    "nodeId": "review_left",
                "nodeAddress": {"node_id": "review_left", "invocation": []},
                    "cwd": "/workspace"
                }),
            ),
        ];
        assert!(actor_bindings("run-1", "parent-1", &wrong_run).is_empty());
        let wrong_project = vec![
            run_started.clone(),
            actor_event(
                1,
                "actor-started",
                json!({
                    "actorSessionId": "actor-1",
                    "parentSessionId": "parent-1",
                    "runId": "run-1",
                    "nodeId": "review_left",
                "nodeAddress": {"node_id": "review_left", "invocation": []},
                    "cwd": "/other-workspace"
                }),
            ),
        ];
        assert!(actor_bindings("run-1", "parent-1", &wrong_project).is_empty());
        let legacy_field_names = vec![
            run_started,
            actor_event(
                1,
                "actor-started",
                json!({
                    "actor_session_id": "actor-1",
                    "parent_session_id": "parent-1",
                    "run_id": "run-1",
                    "node_id": "review_left"
                }),
            ),
        ];
        assert!(actor_bindings("run-1", "parent-1", &legacy_field_names).is_empty());

        let binding = ActorBinding {
            identity: WorkflowActorBinding {
                actor_session_id: "actor-1".to_owned(),
                parent_session_id: "parent-1".to_owned(),
                run_id: "run-1".to_owned(),
                node_id: "review_left".to_owned(),
                node_address: json!({"node_id": "review_left", "invocation": []}),
                project_root: "/workspace".to_owned(),
            },
            site_id: "actor/review_left".to_owned(),
        };
        let transcript = vec![
            actor_message(
                "assistant",
                json!([{
                    "type": "tool_call",
                    "tool_call_id": "read-2",
                    "tool_name": "Read",
                    "arguments": {"file_path": "missing.txt"}
                }]),
            ),
            actor_message(
                "tool",
                json!([{
                    "type": "tool_result",
                    "tool_call_id": "read-2",
                    "content": [{"type": "text", "text": "read_failed"}],
                    "is_error": true
                }]),
            ),
        ];
        let projected = project_actor_transcript(&binding, &transcript, 10, 20);
        assert_eq!(projected[0].node.status, "failed");
        assert_eq!(projected[0].result.status, "failed");
        assert_eq!(projected[0].result.result, None);
        assert_eq!(
            projected[0].result.error.as_ref().unwrap()["code"],
            "tool_error"
        );
    }

    #[test]
    fn resumable_cold_run_only_accepts_read_only_failure() {
        let read_only = vec![event("node-settled", json!({"effect": "read_only"}))];
        assert!(run_is_resumable(
            &WorkflowRunStatus::Stopped,
            false,
            &read_only
        ));
        assert!(!run_is_resumable(
            &WorkflowRunStatus::Completed,
            false,
            &read_only
        ));
        assert!(!run_is_resumable(
            &WorkflowRunStatus::Errored,
            true,
            &read_only
        ));
    }

    #[test]
    fn resumable_cold_run_rejects_write_and_unknown_effects() {
        for events in [
            vec![event(
                "node-started",
                json!({"address": {"node_id": "write"}, "effect": "write"}),
            )],
            vec![event(
                "node-started",
                json!({"address": {"node_id": "unknown"}, "effect": "unknown"}),
            )],
            vec![event("file-change-prepared", json!({"path": "out.txt"}))],
            vec![event("write-unknown", json!({}))],
        ] {
            assert!(!run_is_resumable(
                &WorkflowRunStatus::Errored,
                false,
                &events
            ));
        }
    }

    #[test]
    fn resumable_cold_run_rejects_non_successful_mutating_settlement() {
        for effect in ["write", "unknown"] {
            for status in ["cancelled", "failed", "indeterminate"] {
                let events = vec![
                    event(
                        "node-started",
                        json!({
                            "address": {"node_id": "mutating"},
                            "effect": effect
                        }),
                    ),
                    event(
                        "node-settled",
                        json!({
                            "address": {"node_id": "mutating"},
                            "status": status
                        }),
                    ),
                ];
                assert!(!run_is_resumable(
                    &WorkflowRunStatus::Errored,
                    false,
                    &events
                ));
            }
        }
    }

    #[test]
    fn resumable_cold_run_keeps_cancelled_read_only_recovery_available() {
        let events = vec![
            event("run-started", json!({"definition": read_tool_definition()})),
            event(
                "node-started",
                json!({
                    "address": {"node_id": "read"},
                    "effect": "read_only"
                }),
            ),
            event(
                "node-settled",
                json!({
                    "address": {"node_id": "read"},
                    "status": "cancelled"
                }),
            ),
        ];
        assert!(run_is_resumable(
            &WorkflowRunStatus::Stopped,
            false,
            &events
        ));
    }

    #[test]
    fn resumable_cold_run_ignores_cancelled_read_only_control_node() {
        let definition = json!({
            "version": keencode_workflow::WORKFLOW_DEFINITION_VERSION,
            "meta": {"name": "repeat-read-recovery"},
            "body": [{
                "type": "repeat",
                "node_id": "queued_reads",
                "max_iterations": 2,
                "body": [{
                    "type": "tool",
                    "node_id": "read_once",
                    "name": "Read",
                    "input": {
                        "type": "literal",
                        "value": {"file_path": "missing.txt"}
                    },
                    "effect": "read_only"
                }]
            }]
        });
        let events = vec![
            event("run-started", json!({"definition": definition})),
            event(
                "node-started",
                json!({
                    "address": {"node_id": "queued_reads", "invocation": []},
                    "effect": "read_only"
                }),
            ),
            event(
                "node-started",
                json!({
                    "address": {"node_id": "read_once", "invocation": [0]},
                    "effect": "read_only"
                }),
            ),
            event(
                "node-settled",
                json!({
                    "address": {"node_id": "read_once", "invocation": [0]},
                    "status": "succeeded"
                }),
            ),
            event(
                "node-settled",
                json!({
                    "address": {"node_id": "queued_reads", "invocation": []},
                    "status": "cancelled"
                }),
            ),
        ];
        assert!(run_is_resumable(
            &WorkflowRunStatus::Stopped,
            false,
            &events
        ));
    }

    #[test]
    fn resumable_cold_run_rejects_failed_read_only_agent() {
        let mut definition = read_tool_definition();
        definition["body"][0]["type"] = Value::String("agent".to_owned());
        definition["body"][0]["name"] = Value::String("review".to_owned());
        let events = vec![
            event("run-started", json!({"definition": definition})),
            event(
                "node-started",
                json!({
                    "address": {"node_id": "read"},
                    "effect": "read_only"
                }),
            ),
            event(
                "node-settled",
                json!({
                    "address": {"node_id": "read"},
                    "status": "failed"
                }),
            ),
        ];
        assert!(!run_is_resumable(
            &WorkflowRunStatus::Errored,
            false,
            &events
        ));
    }

    #[test]
    fn resumable_cold_run_reuses_completed_write_without_replaying_it() {
        let events = vec![
            event(
                "node-started",
                json!({"address": {"node_id": "write"}, "effect": "write"}),
            ),
            event(
                "node-settled",
                json!({"address": {"node_id": "write"}, "status": "succeeded"}),
            ),
            event("file-change-prepared", json!({"path": "out.txt"})),
            event("file-change-applied", json!({"path": "out.txt"})),
        ];
        assert!(run_is_resumable(
            &WorkflowRunStatus::Stopped,
            false,
            &events
        ));
    }

    #[test]
    fn resumable_cold_run_rejects_failed_or_cancelled_world_write() {
        for status in ["failed", "cancelled", "running"] {
            let events = vec![event(
                "world-run",
                json!({"path": "out.txt", "status": status}),
            )];
            assert!(!run_is_resumable(
                &WorkflowRunStatus::Stopped,
                false,
                &events
            ));
        }
        assert!(run_is_resumable(
            &WorkflowRunStatus::Stopped,
            false,
            &[event(
                "world-run",
                json!({"path": "out.txt", "status": "completed"}),
            )]
        ));
    }

    #[test]
    fn cold_running_run_is_never_marked_resumable_or_active() {
        let events = vec![event("run-started", json!({"status": "running"}))];
        assert!(!resume_is_active(&events));
        assert!(!run_is_resumable(
            &WorkflowRunStatus::Running,
            false,
            &events
        ));
    }

    #[test]
    fn resumed_run_stays_running_until_a_new_terminal_event() {
        let events = vec![
            event("run-started", json!({})),
            event("run-settled", json!({"status": "stopped"})),
            event("run-resumed", json!({})),
            event("node-started", json!({"effect": "read_only"})),
        ];
        assert!(resume_is_active(&events));

        let mut settled = events.clone();
        settled.push(event("run-settled", json!({"status": "completed"})));
        assert!(!resume_is_active(&settled));
    }

    #[test]
    fn cold_recovery_settles_only_unfinished_resume_and_keeps_read_gate() {
        let active = vec![
            event(
                "run-started",
                json!({"definition": read_tool_definition(), "createdAt": 1}),
            ),
            event("run-resumed", json!({"status": "running"})),
        ];
        assert!(run_needs_cold_recovery(&active));
        let interrupted = {
            let mut events = active.clone();
            events.push(event(
                "run-settled",
                json!({"status": "stopped", "stopReason": "interrupted"}),
            ));
            events
        };
        assert!(!run_needs_cold_recovery(&interrupted));
        assert!(run_is_resumable(
            &parse_status("stopped"),
            false,
            &interrupted
        ));

        let mut write = active;
        write[0].payload["definition"] = json!({
            "version": keencode_workflow::WORKFLOW_DEFINITION_VERSION,
            "meta": {"name": "write-recovery"},
            "body": [{
                "type": "tool",
                "node_id": "write",
                "name": "Edit",
                "input": {"type": "literal", "value": {}},
                "effect": "write"
            }]
        });
        write.push(event(
            "node-started",
            json!({"address": {"node_id": "write"}, "effect": "write"}),
        ));
        write.push(event(
            "run-settled",
            json!({"status": "stopped", "stopReason": "interrupted"}),
        ));
        assert!(!run_is_resumable(&parse_status("stopped"), false, &write));
    }

    #[test]
    fn runtime_cold_reconcile_persists_interrupted_terminal_without_replaying() {
        let storage = tempfile::tempdir().expect("测试存储目录应创建");
        let session = RuntimeSession::create_session(
            RuntimeConfig::new(storage.path()),
            CreateSessionRequest {
                session_id: "workflow-cold-reconcile".to_owned(),
                title: "工作流冷恢复测试".to_owned(),
                project_root: storage.path().display().to_string(),
            },
        )
        .expect("测试 Session 应创建");
        let journal = RuntimeWorkflowJournal::new(session.clone());
        let started = event(
            "run-started",
            json!({
                "parentSessionId": session.session_id().as_str(),
                "definition": read_tool_definition(),
                "scope": "project",
                "inputs": {},
                "createdAt": 1,
                "updatedAt": 1,
            }),
        );
        journal
            .commit_event("workflow-cold-start", started)
            .expect("run-started 应写入父 Journal");
        journal
            .commit_event(
                "workflow-cold-resume",
                event("run-resumed", json!({"status": "running"})),
            )
            .expect("run-resumed 应写入父 Journal");

        let session_id = session.session_id().as_str().to_owned();
        drop(journal);
        drop(session);
        let reopened =
            match RuntimeSession::open_session(RuntimeConfig::new(storage.path()), session_id) {
                Ok(OpenSessionResult::Ready(session)) => session,
                Ok(OpenSessionResult::Corrupt(report)) => {
                    panic!("冷恢复测试 Session 不应损坏: {report:?}")
                }
                Err(error) => panic!("冷恢复测试 Session 应可重新打开: {error}"),
            };
        let reopened_journal = RuntimeWorkflowJournal::new(reopened);
        reopened_journal
            .reconcile_cold_runs_at_startup()
            .expect("冷接管应提交中断终态");
        reopened_journal
            .reconcile_cold_runs_at_startup()
            .expect("重复冷接管应保持幂等");
        let events = reopened_journal
            .all_events("run-1")
            .expect("事件流应可读取");
        assert_eq!(events.len(), 3, "冷接管只能追加一条中断终态");
        assert_eq!(
            events.last().map(|event| event.event_type.as_str()),
            Some("run-settled")
        );
        assert_eq!(
            events
                .last()
                .and_then(|event| event.payload.get("status"))
                .and_then(Value::as_str),
            Some("stopped")
        );
        let summary = reopened_journal
            .run_summary("run-1")
            .expect("摘要应可读取")
            .expect("run 应存在");
        assert_eq!(summary.status, WorkflowRunStatus::Stopped);
        assert!(summary.resumable, "没有副作用事实的 run 应可人工 Resume");
        assert!(!run_needs_cold_recovery(&events));
    }

    #[test]
    fn artifact_projection_keeps_user_metadata_and_hides_tool_evidence() {
        let artifact_id = "a".repeat(64);
        let reference = ArtifactUse {
            artifact_id: ArtifactId::new(artifact_id.clone()).unwrap(),
            sha256: artifact_id.clone(),
            size_bytes: 12,
            media_type: Some("text/markdown".to_owned()),
        };
        let mut published = event(
            "artifact-committed",
            json!({
                "kind": "markdown",
                "title": "Report",
                "version": 1,
                "publishedAt": 42,
                "primary": true
            }),
        );
        published.artifacts = vec![reference.clone()];
        let mut tool_evidence = event("tool-settled", json!({"kind": "file"}));
        tool_evidence.artifacts = vec![reference];
        let report = event(
            "report",
            json!({"artifactId": artifact_id, "item": {"value": 1}}),
        );

        let artifacts = collect_user_artifacts(&[published, tool_evidence, report]);
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "markdown");
        assert_eq!(artifacts[0].title.as_deref(), Some("Report"));
        assert_eq!(artifacts[0].item_count, 1);
        assert_eq!(artifacts[0].versions[0]["publishedAt"], json!(42));
    }

    #[test]
    fn artifact_projection_drops_invalid_kind_or_version() {
        let artifact_id = "b".repeat(64);
        let reference = ArtifactUse {
            artifact_id: ArtifactId::new(artifact_id).unwrap(),
            sha256: "b".repeat(64),
            size_bytes: 1,
            media_type: Some("text/plain".to_owned()),
        };
        for payload in [
            json!({"kind": "custom", "version": 1, "publishedAt": 1}),
            json!({"kind": "file", "version": 17, "publishedAt": 1}),
            json!({"kind": "file", "version": 1}),
        ] {
            let mut event = event("artifact-committed", payload);
            event.artifacts = vec![reference.clone()];
            assert!(artifact_from_event(&event, &reference).is_none());
        }
    }
}
