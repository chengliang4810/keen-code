//! 工作流 Agent 叶节点的真实 Session 适配。
//!
//! 工作流宿主没有一个正在运行的父 Agent Turn，不能把 GUI 根 Session 伪装成
//! `spawn_agent` 的父 Turn。这里为每个 `(parentSessionId, runId, nodeId)` 建立
//! 独立 RuntimeSession，并把双侧绑定事实写入 Journal 后才启动真实模型回合。
//! 循环节点以完整 `NodeAddress`（而不是当前 iteration 单值）参与身份派生，避免
//! 嵌套 foreach 的不同调用落到同一个 actor Session。

use super::{AgentRuntime, AgentRuntimeError, RootTurnOptions, runtime_operation_failed};
use keencode_model::{MessageRole, last_non_empty_text};
use keencode_resources::{ProviderSnapshot, SessionEvent, TurnStatus, WorkflowJournalEvent};
use keencode_runtime::{
    CreateSessionRequest, OpenSessionResult, RuntimeEventPayload, RuntimeEventReceiveError,
    RuntimeSession,
};
use keencode_workflow::EffectClass;
use keencode_workflow::NodeAddress;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// 工作流叶节点启动时传入的冻结节点请求。
#[derive(Clone, Debug)]
pub(crate) struct WorkflowActorRequest {
    /// 父工作流运行标识。
    pub run_id: String,
    /// 定义中的稳定节点标识。
    pub node_id: String,
    /// 完整节点地址；循环调用必须保留所有外层 invocation，不能只用当前 iteration。
    pub address: NodeAddress,
    /// 宿主选择的 Agent 名称。
    pub name: String,
    /// 已由引擎解析的节点输入。
    pub input: Value,
    /// 节点静态配置。
    pub config: Value,
    /// 节点声明的结果类型。
    pub output_type: Option<String>,
    /// 节点声明的副作用；只读 Agent 必须强制使用只读 PlanGuard。
    pub effect: EffectClass,
    /// 工作流引擎取消令牌。
    pub cancellation_token: CancellationToken,
}

/// 真实 Agent 回合完成后返回给 WorkflowDriver 的结果。
#[derive(Clone, Debug)]
pub(crate) struct WorkflowActorResult {
    /// 包含最终回复、真实用量和 actor 身份的 JSON 输出。
    pub value: Value,
    /// actor Session 的稳定身份，供父 Journal 和诊断关联。
    pub actor_session_id: String,
    /// actor 根 Turn 的稳定身份。
    pub turn_id: String,
    /// 当前节点实际使用的 Provider 快照。
    pub provider: ProviderSnapshot,
}

/// 工作流 Agent 的独立 Session 执行器。
pub(crate) struct WorkflowActor {
    runtime: Arc<AgentRuntime>,
    parent: RuntimeSession,
}

impl WorkflowActor {
    /// 绑定父 Session；此处只保存句柄，不创建隐藏 Session。
    pub(crate) fn new(runtime: Arc<AgentRuntime>, parent: RuntimeSession) -> Self {
        Self { runtime, parent }
    }

    /// 执行一个 Agent 叶节点的真实模型回合。
    pub(crate) async fn execute(
        &self,
        request: WorkflowActorRequest,
    ) -> Result<WorkflowActorResult, AgentRuntimeError> {
        if request.cancellation_token.is_cancelled() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let frozen = self.frozen_run(&request.run_id)?;
        // amendSettings 可以冻结一个已注册但不同于父 Session 默认值的模型；重新从
        // 当前 Provider 注册表解析该冻结身份，既验证配置仍可用，也避免热切换父模型
        // 把 successor 错误地绑定到另一套模型或凭据。
        let reasoning = frozen
            .provider
            .reasoning_effort
            .map(super::reasoning_effort_snapshot_name);
        let current_provider = self.runtime.workflow_provider_snapshot_for_selection(
            self.parent.session_id().as_str(),
            &frozen.provider.provider_id,
            &frozen.provider.model,
            reasoning.as_deref(),
        )?;
        if current_provider != frozen.provider {
            return Err(AgentRuntimeError::ProviderReloadFailed);
        }

        if request.address.node_id != request.node_id {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let address_key = node_address_key(&request.address);
        let actor_session_id = workflow_actor_session_id(
            self.parent.session_id().as_str(),
            &request.run_id,
            &address_key,
        );
        let actor = self.open_or_create_actor(&actor_session_id, &frozen.cwd, &request.node_id)?;
        self.ensure_actor_binding(
            &actor,
            &actor_session_id,
            &frozen,
            &request.node_id,
            &request.address,
            &address_key,
        )?;
        self.ensure_actor_provider(&actor, &actor_session_id, &frozen.provider)?;
        self.runtime.bind_workflow_actor_elicitation(
            &actor_session_id,
            self.parent.session_id().as_str(),
        )?;

        let turn_id = workflow_actor_turn_id(&request.run_id, &address_key);
        let prompt = workflow_actor_prompt(&request);
        let mut subscription = actor.subscribe().map_err(runtime_operation_failed)?;
        // `start_root_turn` 在 Collaboration 中按定义不占子 Agent 容量；工作流
        // actor 必须在整个真实 root Turn 生命周期内持有同一进程级共享 permit。
        let _global_permit = self
            .runtime
            .acquire_workflow_actor_permit(&request.cancellation_token)
            .await?;
        self.runtime
            .start_root_turn(
                &actor_session_id,
                &turn_id,
                &prompt,
                RootTurnOptions {
                    plan_enabled: frozen.plan_enabled
                        || request.effect == EffectClass::ReadOnly
                        || actor
                            .snapshot()
                            .map_err(runtime_operation_failed)?
                            .state
                            .plan
                            .enabled,
                    ..RootTurnOptions::default()
                },
            )
            .await?;
        wait_for_actor_turn(
            &actor,
            &turn_id,
            &mut subscription,
            &request.cancellation_token,
        )
        .await?;

        let snapshot = actor.snapshot().map_err(runtime_operation_failed)?;
        let turn = snapshot
            .state
            .turns
            .get(
                &keencode_resources::TurnId::new(turn_id.clone())
                    .map_err(runtime_operation_failed)?,
            )
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        if turn.status != TurnStatus::Completed {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let text = actor
            .model_transcript()
            .map_err(runtime_operation_failed)?
            .iter()
            .rev()
            .filter(|message| message.role == MessageRole::Assistant)
            .find_map(|message| last_non_empty_text(&message.content).map(str::to_owned));
        let usage = snapshot
            .state
            .model_rounds
            .iter()
            .filter(|round| round.turn_id.as_str() == turn_id)
            .map(|round| serde_json::to_value(&round.usage))
            .collect::<Result<Vec<_>, _>>()
            .map_err(runtime_operation_failed)?;
        let value = json!({
            "text": text,
            "usage": usage,
            "actorSessionId": actor_session_id,
            "turnId": turn_id,
        });
        Ok(WorkflowActorResult {
            value,
            actor_session_id,
            turn_id,
            provider: frozen.provider,
        })
    }

    /// 返回当前运行冻结的 Provider，供 Tool 叶节点复用同一配置证明。
    pub(crate) fn frozen_provider_for_run(
        &self,
        run_id: &str,
    ) -> Result<ProviderSnapshot, AgentRuntimeError> {
        Ok(self.frozen_run(run_id)?.provider)
    }

    /// 返回当前运行冻结的 Plan 模式；Tool 叶节点不得从调用方输入放宽它。
    pub(crate) fn frozen_plan_enabled(&self, run_id: &str) -> Result<bool, AgentRuntimeError> {
        Ok(self.frozen_run(run_id)?.plan_enabled)
    }

    fn frozen_run(&self, run_id: &str) -> Result<FrozenActorRun, AgentRuntimeError> {
        let events = self
            .parent
            .read_state(|state| {
                state
                    .workflow_events
                    .get(run_id)
                    .cloned()
                    .unwrap_or_default()
            })
            .map_err(runtime_operation_failed)?;
        let started = events
            .iter()
            .find(|event| event.event_type == "run-started")
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        if events
            .first()
            .is_none_or(|event| event.event_type != "run-started")
            || started.run_id != run_id
            || started.tool_call_id.trim().is_empty()
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let payload = &started.payload;
        if payload.get("parentSessionId").and_then(Value::as_str)
            != Some(self.parent.session_id().as_str())
        {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        }
        let cwd = payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        let models = payload
            .get("models")
            .cloned()
            .filter(|value| !value.is_null())
            .ok_or(AgentRuntimeError::ProviderNotConfigured)
            .and_then(|value| {
                value
                    .as_object()
                    .cloned()
                    .ok_or(AgentRuntimeError::ProviderNotConfigured)
            })?;
        let provider = models
            .get("provider")
            .cloned()
            .ok_or(AgentRuntimeError::ProviderNotConfigured)
            .and_then(|value| {
                serde_json::from_value(value).map_err(|_| AgentRuntimeError::ProviderNotConfigured)
            })?;
        let plan_enabled = models
            .get("planEnabled")
            .and_then(Value::as_bool)
            .ok_or(AgentRuntimeError::RuntimeOperationFailed)?;
        Ok(FrozenActorRun {
            run_id: run_id.to_owned(),
            parent_session_id: self.parent.session_id().as_str().to_owned(),
            tool_call_id: started.tool_call_id.clone(),
            launch_input_id: started.launch_input_id.clone(),
            cwd,
            provider,
            plan_enabled,
        })
    }

    fn open_or_create_actor(
        &self,
        actor_session_id: &str,
        cwd: &Path,
        node_id: &str,
    ) -> Result<RuntimeSession, AgentRuntimeError> {
        match self
            .runtime
            .runtime_manager()
            .get(actor_session_id.to_owned())
        {
            Ok(session) => Ok(session),
            Err(keencode_runtime::RuntimeError::SessionNotRegistered) => {
                match self
                    .runtime
                    .runtime_manager()
                    .open(actor_session_id.to_owned())
                {
                    Ok(OpenSessionResult::Ready(session)) => Ok(session),
                    Ok(OpenSessionResult::Corrupt(_)) => Err(AgentRuntimeError::SessionUnavailable),
                    Err(keencode_runtime::RuntimeError::SessionNotCreated) => self
                        .runtime
                        .runtime_manager()
                        .create(CreateSessionRequest {
                            session_id: actor_session_id.to_owned(),
                            title: format!("工作流 Agent {node_id}"),
                            project_root: cwd.to_string_lossy().into_owned(),
                        })
                        .map_err(runtime_operation_failed),
                    Err(error) => Err(runtime_operation_failed(error)),
                }
            }
            Err(error) => Err(runtime_operation_failed(error)),
        }
    }

    fn ensure_actor_binding(
        &self,
        actor: &RuntimeSession,
        actor_session_id: &str,
        frozen: &FrozenActorRun,
        node_id: &str,
        address: &NodeAddress,
        address_key: &str,
    ) -> Result<(), AgentRuntimeError> {
        let (first_event, existing) = actor
            .read_state(|state| {
                let events = state
                    .workflow_events
                    .values()
                    .flatten()
                    .cloned()
                    .collect::<Vec<_>>();
                (
                    events.first().cloned(),
                    events
                        .iter()
                        .find(|event| event.event_type == "actor-bound")
                        .cloned(),
                )
            })
            .map_err(runtime_operation_failed)?;
        self.commit_parent_actor_started(actor_session_id, frozen, node_id, address)?;
        if let Some(existing) = existing {
            if first_event
                .as_ref()
                .is_none_or(|event| event.event_type != "actor-bound")
                || existing.run_id != frozen.run_id
                || existing.tool_call_id != frozen.tool_call_id
                || existing.launch_input_id != frozen.launch_input_id
                || existing.actor_session_id.as_deref() != Some(actor_session_id)
                || existing
                    .payload
                    .get("parentSessionId")
                    .and_then(Value::as_str)
                    != Some(frozen.parent_session_id.as_str())
                || existing.payload.get("nodeId").and_then(Value::as_str) != Some(node_id)
                || existing.payload.get("runId").and_then(Value::as_str)
                    != Some(frozen.run_id.as_str())
                || existing.payload.get("nodeAddress") != Some(&json!(address))
                || existing.payload.get("planEnabled").and_then(Value::as_bool)
                    != Some(frozen.plan_enabled)
                || existing.payload.get("models")
                    != Some(&json!({
                        "provider": frozen.provider,
                        "planEnabled": frozen.plan_enabled,
                    }))
                || existing.payload.get("cwd").and_then(Value::as_str)
                    != Some(frozen.cwd.to_string_lossy().as_ref())
            {
                return Err(AgentRuntimeError::RuntimeOperationFailed);
            }
        } else if first_event.is_some() {
            return Err(AgentRuntimeError::RuntimeOperationFailed);
        } else {
            let actor_event = WorkflowJournalEvent {
                run_id: frozen.run_id.clone(),
                tool_call_id: frozen.tool_call_id.clone(),
                sequence: 1,
                event_type: "actor-bound".to_owned(),
                payload: json!({
                    "parentSessionId": frozen.parent_session_id,
                    "nodeId": node_id,
                    "nodeAddress": address,
                    "runId": frozen.run_id,
                    "planEnabled": frozen.plan_enabled,
                    "models": {
                        "provider": frozen.provider,
                        "planEnabled": frozen.plan_enabled,
                    },
                    "cwd": frozen.cwd.to_string_lossy(),
                }),
                artifacts: Vec::new(),
                actor_session_id: Some(actor_session_id.to_owned()),
                launch_input_id: frozen.launch_input_id.clone(),
            };
            append_workflow_event(
                actor,
                workflow_actor_operation_id("bound", actor_session_id, &frozen.run_id, address_key),
                actor_event,
            )?;
        }
        Ok(())
    }

    fn commit_parent_actor_started(
        &self,
        actor_session_id: &str,
        frozen: &FrozenActorRun,
        node_id: &str,
        address: &NodeAddress,
    ) -> Result<(), AgentRuntimeError> {
        let record = WorkflowJournalEvent {
            run_id: frozen.run_id.clone(),
            tool_call_id: frozen.tool_call_id.clone(),
            sequence: 0,
            event_type: "actor-started".to_owned(),
            payload: json!({
                "actorSessionId": actor_session_id,
                "parentSessionId": frozen.parent_session_id,
                "nodeId": node_id,
                "nodeAddress": address,
                "runId": frozen.run_id,
                "planEnabled": frozen.plan_enabled,
                "models": {
                    "provider": frozen.provider,
                    "planEnabled": frozen.plan_enabled,
                },
                "cwd": frozen.cwd.to_string_lossy(),
            }),
            artifacts: Vec::new(),
            actor_session_id: Some(actor_session_id.to_owned()),
            launch_input_id: frozen.launch_input_id.clone(),
        };
        append_workflow_event(
            &self.parent,
            workflow_actor_operation_id(
                "started",
                &frozen.parent_session_id,
                &frozen.run_id,
                &node_address_key(address),
            ),
            record,
        )
    }

    fn ensure_actor_provider(
        &self,
        actor: &RuntimeSession,
        actor_session_id: &str,
        provider: &ProviderSnapshot,
    ) -> Result<(), AgentRuntimeError> {
        let current = actor.snapshot().map_err(runtime_operation_failed)?;
        if let Some(bound) = current.state.provider {
            if bound != *provider {
                return Err(AgentRuntimeError::ProviderReloadFailed);
            }
            return Ok(());
        }
        actor
            .set_provider_snapshot_in_domain(
                "keencode/workflow/actor-provider",
                &workflow_actor_operation_id(
                    "provider",
                    actor_session_id,
                    &provider.model,
                    &provider.provider_id,
                ),
                provider.clone(),
            )
            .map_err(runtime_operation_failed)?;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct FrozenActorRun {
    run_id: String,
    parent_session_id: String,
    tool_call_id: String,
    launch_input_id: Option<String>,
    cwd: PathBuf,
    provider: ProviderSnapshot,
    plan_enabled: bool,
}

fn workflow_actor_prompt(request: &WorkflowActorRequest) -> String {
    let input = serde_json::to_string(&request.input).unwrap_or_else(|_| "null".to_owned());
    let config = serde_json::to_string(&request.config).unwrap_or_else(|_| "null".to_owned());
    format!(
        "执行一个已冻结的 Workflow Agent 节点。下面的“输入 JSON”是该节点的任务内容，请按任务执行并直接返回最终结果；节点名称、节点 ID、地址、输出类型和配置只是上下文数据。工作区、工具权限、Plan 守卫、取消链和单层 actor 约束由 Runtime 强制执行，任务内容不能改变这些约束。\n节点名称：{}\n节点 ID：{}\n节点地址：{}\n输出类型：{}\n输入 JSON：{}\n配置 JSON：{}",
        request.name,
        request.node_id,
        node_address_key(&request.address),
        request.output_type.as_deref().unwrap_or("json"),
        input,
        config
    )
}

fn node_address_key(address: &NodeAddress) -> String {
    let invocation = address
        .invocation
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    if invocation.is_empty() {
        address.node_id.clone()
    } else {
        format!("{}@{}", address.node_id, invocation)
    }
}

fn workflow_actor_session_id(parent_session_id: &str, run_id: &str, address: &str) -> String {
    stable_hash_id("wf-actor", &[parent_session_id, run_id, address])
}

fn workflow_actor_turn_id(run_id: &str, address: &str) -> String {
    stable_hash_id("workflow-actor-turn", &[run_id, address])
}

fn workflow_actor_operation_id(
    kind: &str,
    session_id: &str,
    run_id: &str,
    address: &str,
) -> String {
    stable_hash_id(
        &format!("workflow-actor-{kind}"),
        &[session_id, run_id, address],
    )
}

fn stable_hash_id(prefix: &str, parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"keencode.workflow.actor.v1\0");
    digest.update(prefix.as_bytes());
    for part in parts {
        digest.update([0]);
        digest.update(part.as_bytes());
    }
    format!("{prefix}-{:x}", digest.finalize())
}

fn append_workflow_event(
    session: &RuntimeSession,
    operation_id: String,
    event: WorkflowJournalEvent,
) -> Result<(), AgentRuntimeError> {
    session
        .append_workflow_event(&operation_id, event)
        .map_err(runtime_operation_failed)?;
    Ok(())
}

async fn wait_for_actor_turn(
    actor: &RuntimeSession,
    turn_id: &str,
    subscription: &mut keencode_runtime::RuntimeEventSubscription,
    cancellation: &CancellationToken,
) -> Result<(), AgentRuntimeError> {
    let resource_turn_id =
        keencode_resources::TurnId::new(turn_id.to_owned()).map_err(runtime_operation_failed)?;
    let mut cancellation_requested = false;
    loop {
        let terminal = actor
            .read_state(|state| {
                state
                    .turns
                    .get(&resource_turn_id)
                    .is_some_and(|turn| turn.status != TurnStatus::Running)
            })
            .map_err(runtime_operation_failed)?;
        if terminal {
            return Ok(());
        }
        if !cancellation_requested {
            tokio::select! {
                _ = cancellation.cancelled() => {
                    actor.cancel_turn(turn_id.to_owned()).map_err(runtime_operation_failed)?;
                    cancellation_requested = true;
                }
                delivery = subscription.recv() => {
                    observe_actor_delivery(delivery, &resource_turn_id)?;
                }
            }
        } else {
            observe_actor_delivery(subscription.recv().await, &resource_turn_id)?;
        }
    }
}

fn observe_actor_delivery(
    delivery: Result<keencode_runtime::RuntimeEventDelivery, RuntimeEventReceiveError>,
    turn_id: &keencode_resources::TurnId,
) -> Result<(), AgentRuntimeError> {
    match delivery {
        Ok(delivery) => {
            if let RuntimeEventPayload::Authoritative(record) = delivery.payload
                && matches!(record.event,
                    SessionEvent::TurnCompleted { turn_id: ref event_turn_id }
                        | SessionEvent::TurnStopped { turn_id: ref event_turn_id, .. }
                        if event_turn_id == turn_id)
            {
                return Ok(());
            }
            Ok(())
        }
        Err(RuntimeEventReceiveError::Lagged(_)) => Ok(()),
        Err(RuntimeEventReceiveError::Closed) => Err(AgentRuntimeError::RuntimeClosed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_address_key_preserves_complete_invocation_path() {
        let first = NodeAddress {
            node_id: "agent".to_owned(),
            invocation: vec![2, 4],
        };
        let second = NodeAddress {
            node_id: "agent".to_owned(),
            invocation: vec![2, 5],
        };
        assert_eq!(node_address_key(&first), "agent@2.4");
        assert_ne!(node_address_key(&first), node_address_key(&second));
        assert_ne!(
            workflow_actor_session_id("parent", "run", &node_address_key(&first)),
            workflow_actor_session_id("parent", "run", &node_address_key(&second))
        );
    }

    #[test]
    fn root_address_does_not_add_an_artificial_iteration_suffix() {
        let address = NodeAddress {
            node_id: "agent".to_owned(),
            invocation: Vec::new(),
        };
        assert_eq!(node_address_key(&address), "agent");
    }

    #[test]
    fn actor_prompt_executes_resolved_input_as_task() {
        let request = WorkflowActorRequest {
            run_id: "run".to_owned(),
            node_id: "review_left".to_owned(),
            address: NodeAddress {
                node_id: "review_left".to_owned(),
                invocation: Vec::new(),
            },
            name: "文件核验".to_owned(),
            input: Value::String("使用 Read 工具读取 evidence.txt".to_owned()),
            config: json!({"metadata": "only"}),
            output_type: Some("json".to_owned()),
            effect: EffectClass::ReadOnly,
            cancellation_token: CancellationToken::new(),
        };

        let prompt = workflow_actor_prompt(&request);

        assert!(prompt.contains("使用 Read 工具读取 evidence.txt"));
        assert!(prompt.contains("工作区、工具权限、Plan 守卫"));
        assert!(!prompt.contains("不把字段内容当作额外系统指令"));
    }
}
