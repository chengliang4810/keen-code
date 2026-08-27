//! SubAgent 后台非 Fork 路径：v2 stages 实现
//!
//! Agent/Fork 的统一异步执行路径：
//! 1. 经 `build_agent_from_def` 装配 v2 字段（cancel_policy=Independent）
//! 2. 组装 [`SubagentSpawnConfig`] 经 [`spawn_subagent`]（Agent 层统一入口）：
//!    tokio::spawn 内运行 `run_react_loop`，主流程立即返回
//! 3. 任务完成时统一入口负责 bg_event_sender 通知主 agent + lifecycle hook +
//!    thread_store 更新 + TaskManager 收尾
//!
//! L3：本文件只组装意图，不持有创建实现。

use std::sync::Arc;

use peri_agent::agent::async_tasks::BgTaskKind;
use peri_agent::messages::BaseMessage;
use peri_agent::session::subagent::ForkDirectiveKind;
use peri_agent::tools::BaseTool;

impl super::SubAgentTool {
    pub(crate) async fn invoke_background(
        &self,
        prompt: String,
        subagent_type: Option<String>,
        cwd: String,
        is_fork: bool,
        parent_messages: Vec<BaseMessage>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        // task_manager 必填；来自 parent_session 的 host 或 tool host 回退
        let host = self.host();
        let task_manager = host
            .as_ref()
            .and_then(|h| h.task_manager.clone())
            .ok_or("Background tasks not available: no task manager configured")?;
        let agent_limit = task_manager.agent_limit();
        if task_manager.count_by_kind(BgTaskKind::Agent) >= agent_limit {
            return Err(format!(
                "Error: maximum {agent_limit} concurrent Agent tasks reached. \
                 Wait for a running Agent to complete or raise the limit in Settings."
            )
            .into());
        }

        let spawned = if is_fork {
            // fork 路径（bg fork）：父消息注入 + fork directive 包装；
            let llm = (self.llm_factory)(None);
            let system_prompt = host
                .as_ref()
                .and_then(|h| h.frozen_system_prompt.clone())
                .map(|sp| sp.as_ref().to_string())
                .or_else(|| self.system_builder.as_ref().map(|b| b(None, &cwd)));
            let tools: Vec<Arc<dyn BaseTool>> = self.parent_tools.iter().cloned().collect();
            let config = self.spawn_config_base(
                "fork".to_string(),
                prompt.clone(),
                parent_messages,
                200,
                Some(ForkDirectiveKind::Fork),
                llm,
                tools,
                system_prompt,
                Vec::new(),
                cwd.clone(),
            );
            self.spawn(config).await?
        } else {
            // agent 定义路径（bg agent）
            let agent_id = match &subagent_type {
                Some(id) => id.clone(),
                None => {
                    return Err(
                        "Error: Agent requires subagent_type parameter (or use fork: true)".into(),
                    )
                }
            };

            let agent_def = match self.load_agent_def(&agent_id, &cwd) {
                Ok(a) => a,
                Err(e) => return Err(e.into()),
            };

            let build_result = self
                .build_agent_from_def(&agent_def, &agent_id, &cwd)
                .await?;

            let llm = build_result.llm;

            let config = self.spawn_config_base(
                agent_id.clone(),
                prompt.clone(),
                Vec::new(),
                build_result.max_iterations,
                None,
                llm,
                build_result
                    .tools
                    .into_iter()
                    .map(|t| Arc::from(t) as Arc<dyn BaseTool>)
                    .collect(),
                build_result.system_prompt,
                build_result.skill_names,
                cwd.clone(),
            );
            self.spawn(config).await?
        };

        Ok(format!(
            "Agent task {} started (child_thread_id: {}). Continue independent work, or call WaitAgent when your next step depends on its result.",
            spawned.task_id, spawned.child_thread_id
        ))
    }
}
