//! 生产中间件链装配（ARC-MIDDLEWARE-001）。
//!
//! 3.0 归位（L2）：链装配实现自 `peri-acp/src/agent/builder.rs` 迁入本模块。
//! 链序事实源位于 Agent 层 session 工厂
//! （`peri-agent/src/session/factory.rs` 的 `production_blueprint`），
//! 本模块按蓝本构造中间件实例——顺序是行为契约，禁止重排。
//!
//! 依赖方向说明（L5）：装配上下文（[`AssemblyContext`] / [`ChainAssembly`] /
//! [`OnBgCompleteFn`] / [`SystemPromptBuilder`]）随 L5 stage 装配迁入 Agent 层
//! session 工厂（事实源），middlewares 具体类型经 `peri-acp-types` 端口
//! （`McpPoolPort` / `ToolSearchPort` / `CronSchedulerPort`）接入，本模块装配时
//! downcast 还原具体实例。

use std::{collections::HashMap, path::Path, sync::Arc};

use parking_lot::RwLock;
use peri_acp_types::ports::LspPoolPort;
use peri_agent::{
    agent::{events::AgentEventHandler, react::ReactLLM},
    messages::BaseMessage,
    middleware::chain::MiddlewareChain,
    session::factory::{ChainSlot, MiddlewareChainAssembler, SubAgentMiddlewarePort},
    tools::BaseTool,
};
use peri_resources::lsp::config::{resolve_lsp_config_for_session, LspConfigFile};
use peri_resources::lsp::pool::LspServerPool;

use crate::{
    cron::{CronMiddleware, CronScheduler, CronSchedulerPortHandle},
    error_suggest,
    hooks::HookMiddleware,
    mcp::{build_tool_bridges, McpClientPool, McpMiddleware, McpResourceTool},
    middleware::{FilesystemMiddleware, TerminalMiddleware, TodoMiddleware, WebMiddleware},
    plugin::PluginMiddleware,
    skills::SkillsMiddleware,
    subagent::{SkillPreloadMiddleware, SubAgentMiddleware},
    tool_search::{ToolSearchIndex, ToolSearchMiddleware},
    tools::AskUserTool,
    AgentDefineMiddleware, AgentsMdMiddleware, AtMentionMiddleware, GitAttributionMiddleware,
    GoalMiddleware, ImageMiddleware, LspMiddleware,
};

/// 后台任务完成回调类型（事实源 peri-agent::session::factory，L5 迁入）
pub use peri_agent::session::factory::OnBgCompleteFn;
/// System prompt 构建器类型（事实源 peri-agent::session::factory，L5 迁入）
pub use peri_agent::session::factory::SystemPromptBuilder;

/// 链装配上下文（事实源 peri-agent::session::factory，L5 迁入）。
///
/// 由 stage 装配（Agent 层 `session::exec::stage_builder`）从会话输入投影构造；
/// middlewares 具体类型经 `peri-acp-types` 端口接入，本模块装配时
/// downcast 还原（见 [`ProductionChainAssembler::assemble`]）。
pub use peri_agent::session::factory::AssemblyContext;

/// 链装配产物（事实源 peri-agent::session::factory，L5 迁入）。
pub use peri_agent::session::factory::ChainAssembly;

/// 生产链装配器（当前唯一装配实现，见模块文档）。
pub struct ProductionChainAssembler;

impl MiddlewareChainAssembler for ProductionChainAssembler {
    type Context = AssemblyContext;
    type Output = ChainAssembly;

    /// 按 Agent 层 `production_blueprint` 的槽位顺序构造中间件链。
    ///
    /// 链序由蓝本保证（ARC-MIDDLEWARE-001 事实源在 Agent 层工厂）；
    /// 本实现只负责逐槽位构造实例，条件注册（MCP/LSP/Goal）
    /// 与 Hook 组展开按上下文判断，行为与迁移前
    /// `peri-acp/src/agent/builder.rs` 完全一致。
    fn assemble(&self, blueprint: &[ChainSlot], ctx: &Self::Context) -> Self::Output {
        let AssemblyContext {
            cwd,
            cancel: _,
            broker,
            model_name,
            provider_name,
            supports_vision,
            auxiliary_model,
            claude_md_excludes,
            preload_skills,
            plugin_skill_roots,
            plugin_loaded,
            hook_groups,
            session_start_source,
            cron_scheduler,
            mcp_pool,
            channel_state: _,
            tool_search_index,
            shared_tools,
            lsp_servers,
            lsp_pool,
            event_handler,
            task_manager,
            bg_event_tx: _,
            on_bg_complete,
            thread_store: _,
            parent_thread_id: _,
            register_runtime: _,
            deregister_runtime: _,
            child_handler_factory,
            frozen_claude_md,
            frozen_claude_local_md,
            frozen_skill_summary,
            system_prompt_for_sub: _,
            llm_factory,
            system_builder,
            todo_tx,
            goal_controller,
        } = ctx;

        // L5：middlewares 具体类型经 peri-acp-types 端口接入，此处 downcast
        // 还原（端口实现方为本 crate，生产路径必成功；失败回退与原上层
        // 回退逻辑一致——临时实例 / None 降级）。

        // Cron 调度器：端口 → Arc<Mutex<CronScheduler>>（CronMiddleware 消费）。
        // 未注入或端口类型不匹配时保持禁用；不能构造无 tick 的临时调度器，
        // 否则模型会看到可以创建但永远不会触发的 Cron 工具。
        let cron_scheduler_concrete: Option<Arc<parking_lot::Mutex<CronScheduler>>> =
            cron_scheduler.as_ref().and_then(|port| {
                match Arc::clone(port).downcast_arc::<CronSchedulerPortHandle>() {
                    Ok(handle) => Some(handle.0.clone()),
                    Err(_) => {
                        tracing::warn!("Cron 端口类型不匹配，跳过 CronMiddleware 注册");
                        None
                    }
                }
            });

        // MCP 连接池：端口 → Arc<McpClientPool>。downcast 失败按未注入处理
        //（不注册 MCP 中间件/工具）。
        let mcp_pool_concrete: Option<Arc<McpClientPool>> = mcp_pool.as_ref().map(|p| {
            Arc::clone(p)
                .downcast_arc::<McpClientPool>()
                .unwrap_or_else(|_| Arc::new(McpClientPool::new_pending()))
        });

        // 工具搜索索引：端口 → Arc<ToolSearchIndex>（失败回退默认实例）。
        let tool_search_index_concrete: Arc<ToolSearchIndex> = Arc::clone(tool_search_index)
            .downcast_arc::<ToolSearchIndex>()
            .unwrap_or_else(|_| Arc::new(ToolSearchIndex::default()));

        // AskUser 工具使用宿主 broker，保留问答交互。
        let ask_user_tool = AskUserTool::new(broker.clone());

        // 父工具集（供子 agent 继承）
        let mut parent_tools: Vec<Box<dyn BaseTool>> = FilesystemMiddleware::build_tools(cwd);
        parent_tools.extend(TerminalMiddleware::build_tools(cwd));
        parent_tools.extend(WebMiddleware::build_tools());
        if let Some(ref pool) = mcp_pool_concrete {
            let mcp_tools = build_tool_bridges(pool);
            for tool in mcp_tools {
                parent_tools.push(tool);
            }
            if pool.has_resources() {
                parent_tools.push(Box::new(McpResourceTool::new(Arc::clone(pool))));
            }
        }

        // SubAgent middleware（L3 瘦身：只声明工具与发起意图）。
        // [TRAP] SubAgent 复用 main agent 在 session/new 时捕获的 frozen CLAUDE.md/Skills
        // （L3 起由 Agent 层 spawn_subagent 从父 session copy，此处不再透传）；
        // 运行时通道（thread_store / task_manager / bg_event_sender / register /
        // deregister / frozen 回退）统一经 SubagentHost 注入
        // 主 session（builder 侧构造），此处只留工具声明字段。
        let mut subagent = SubAgentMiddleware::new(
            parent_tools,
            Some(Arc::clone(event_handler) as Arc<dyn AgentEventHandler>),
            llm_factory.clone(),
        )
        .with_system_builder(system_builder.clone())
        .with_parent_messages(Arc::new(RwLock::new(Vec::<BaseMessage>::new())))
        .with_vision_agent_enabled(!supports_vision)
        .with_registered_hooks(vec![]);
        if let Some(factory) = child_handler_factory {
            subagent = subagent.with_child_handler_factory(Arc::clone(factory));
        }
        // 能力声明：task_manager 可用时注册 AgentResult 与 Agent 控制工具（collect_tools 阶段
        // 尚无 parent session，只能以布尔标记判定）
        // AssemblyContext.task_manager 为必填 Arc（上层已回退为临时实例），
        // 因此恒为可用——工具注册条件与迁移前（SubAgentMiddleware
        // 持 task_manager）生产路径一致。
        subagent.set_task_manager_available(true);

        // 直接构造 MiddlewareChain（顺序由 Agent 层 production_blueprint 保证）。
        // 中间件顺序是 [TRAP] 守护契约（禁止重排），详见 peri-middlewares/CLAUDE.md。
        let mut chain = MiddlewareChain::new();
        for slot in blueprint {
            match slot {
                // ── 第一组：上下文注入器（system prompt 段落 / agent 定义 / 插件 / skills） ──
                ChainSlot::AgentsMd => {
                    let mut mw =
                        AgentsMdMiddleware::new().with_excludes(claude_md_excludes.clone());
                    if frozen_claude_md.is_some() || frozen_claude_local_md.is_some() {
                        mw = mw.with_frozen_content(
                            frozen_claude_md.clone(),
                            frozen_claude_local_md.clone(),
                        );
                    }
                    chain.add(Box::new(mw));
                }
                ChainSlot::AgentDefine => {
                    chain.add(Box::new(AgentDefineMiddleware::new()));
                }
                ChainSlot::Plugin => {
                    chain.add(Box::new(PluginMiddleware::new(plugin_loaded.clone())));
                }
                // 构造 SkillsMiddleware：collect_tools 提供统一 skill 协议
                // （SkillTool(skill_name) + DiscoverSkillsTool）；旧 Skill(skill, args)
                // 双协议已按 D3 移除，不再单独注册 SkillToolMiddleware。
                ChainSlot::Skills => {
                    let mut skills_mw =
                        SkillsMiddleware::new().with_plugin_roots(plugin_skill_roots.clone());
                    if let Some(summary) = frozen_skill_summary {
                        skills_mw = skills_mw.with_frozen_summary(summary.clone());
                    }
                    chain.add(Box::new(skills_mw));
                }
                ChainSlot::SkillPreload => {
                    chain.add(Box::new(
                        SkillPreloadMiddleware::new(preload_skills.clone(), cwd)
                            .with_plugin_roots(plugin_skill_roots.clone()),
                    ));
                }
                ChainSlot::AtMention => {
                    chain.add(Box::new(AtMentionMiddleware::new(cwd.clone().into())));
                }
                // 新增：图片附件处理（在 @mention 之后，将 @image <path> 转换为 ContentBlock::Image）
                ChainSlot::Image => {
                    if *supports_vision {
                        chain.add(Box::new(ImageMiddleware::new()));
                    }
                }
                // ── 第二组：文件/终端/Web 工具提供器 ──
                ChainSlot::Filesystem => {
                    chain.add(Box::new(FilesystemMiddleware::new()));
                }
                ChainSlot::GitAttribution => {
                    chain.add(Box::new(GitAttributionMiddleware::new(model_name)));
                }
                ChainSlot::Terminal => {
                    let mut tm = TerminalMiddleware::new();
                    tm = tm.with_task_manager(
                        Arc::clone(task_manager) as Arc<dyn peri_acp_types::tasks::TaskManager>
                    );
                    if let Some(ref cb) = on_bg_complete {
                        tm = tm.with_on_bg_complete(Arc::clone(cb));
                    }
                    chain.add(Box::new(tm));
                }
                ChainSlot::Web => {
                    chain.add(Box::new(WebMiddleware::new()));
                }
                // ── 第三组：Todo / Cron ──
                ChainSlot::Todo => {
                    chain.add(Box::new(TodoMiddleware::new(todo_tx.clone())));
                }
                ChainSlot::Cron => {
                    if let Some(scheduler) = cron_scheduler_concrete.clone() {
                        chain.add(Box::new(CronMiddleware::new(scheduler)));
                    }
                }
                // ── 第四组：Hook 中间件（插件 hooks + 自定义 hooks） ──
                ChainSlot::Hook => {
                    tracing::info!(
                        groups = hook_groups.len(),
                        total_hooks = hook_groups.iter().map(|g| g.len()).sum::<usize>(),
                        session_start = session_start_source.is_some(),
                        "Builder: assembling HookMiddleware from groups"
                    );
                    if !hook_groups.is_empty() {
                        let hook_llm_factory: Arc<
                            dyn Fn() -> Box<dyn ReactLLM + Send + Sync> + Send + Sync,
                        > = Arc::new({
                            let factory = llm_factory.clone();
                            move || factory(None)
                        });
                        for (i, group) in hook_groups.iter().enumerate() {
                            if group.is_empty() {
                                continue;
                            }
                            let group_size = group.len();
                            let mw = HookMiddleware::with_session_start(
                                group.clone(),
                                hook_llm_factory.clone(),
                                cwd,
                                "",
                                "",
                                provider_name.clone(),
                                session_start_source.clone(),
                            );
                            tracing::info!(
                                group_index = i,
                                group_size,
                                "Builder: HookMiddleware group {} created with {} hooks",
                                i,
                                group_size
                            );
                            chain.add(Box::new(mw));
                        }
                    }
                }
                // chain 与上层各持一份 SubAgentMiddleware clone：
                // 链中实例负责 collect_tools 提供 SubAgentTool；原实例由上层
                // 注入主 agent 身份（共享 cell，见 set_parent_agent_id）。
                ChainSlot::SubAgent => {
                    let subagent_for_chain = subagent.clone();
                    chain.add(Box::new(subagent_for_chain));
                }
                // ── 第六组：MCP / ToolSearch（工具提供器） ──
                ChainSlot::Mcp => {
                    if let Some(pool) = mcp_pool_concrete.as_ref() {
                        let mw = McpMiddleware::new(Arc::clone(pool));
                        // 注入状态变化通知：经 session 事件通道发布
                        // system-notification（TUI 通知面显示）。pool 全局共享，
                        // 多 session 时以最后装配的 session 通道为准。
                        let tx = ctx.bg_event_tx.clone();
                        pool.set_notifier(Box::new(move |text: &str| {
                            let _ = tx.send(
                                peri_agent::agent::events::ExecutorEvent::SystemNotification {
                                    text: text.to_string(),
                                    level: "info".to_string(),
                                },
                            );
                        }));
                        chain.add(Box::new(mw));
                    }
                }
                // ToolSearch 中间件
                ChainSlot::ToolSearch => {
                    chain.add(Box::new(ToolSearchMiddleware::new(
                        Arc::clone(&tool_search_index_concrete),
                        Arc::clone(shared_tools),
                    )));
                }
                // ── 第七组：LSP / Goal（辅助诊断；Goal 链最后） ──
                ChainSlot::Lsp => {
                    if !lsp_servers.is_empty() {
                        // 会话级 pool 复用（H1）：
                        // Some → 复用跨 turn 存活的 pool（服务器进程/initialized/
                        // 诊断状态不丢）；None → 临时实例（print 模式等无 session 路径）。
                        let lsp_mw = if let Some(pool) = lsp_pool
                            .as_ref()
                            .and_then(|p| Arc::clone(p).downcast_arc::<LspServerPool>().ok())
                        {
                            LspMiddleware::from_pool(pool)
                        } else {
                            let lsp_config = LspConfigFile {
                                lsp_servers: lsp_servers
                                    .iter()
                                    .map(|s| (s.name.clone(), s.clone()))
                                    .collect(),
                            };
                            tracing::info!(
                                target: "lsp",
                                servers = lsp_config.lsp_servers.len(),
                                "LSP 中间件已注册（临时 pool）"
                            );
                            LspMiddleware::new(cwd.clone(), lsp_config)
                        };
                        chain.add(Box::new(lsp_mw));
                    }
                }
                ChainSlot::Goal => {
                    // goal active 时注入递增紧迫感 steering + 设 block_continue 让 agent 自驱续跑
                    if let Some(controller) = goal_controller {
                        let goal_mw =
                            GoalMiddleware::new(Arc::clone(controller), auxiliary_model.clone());
                        chain.add(Box::new(goal_mw));
                    }
                }
            }
        }

        // AskUserTool：v1 通过 register_tool 注册到 executor.self.tools（每轮 execute 合并）。
        // v2 stages 不调 execute()，改为一次性 insert 到 shared_tools。
        // 上层随后调 chain.collect_tools merge 时，本工具已存在不会覆盖。
        {
            let mut tools = shared_tools.write();
            tools.insert("AskUserQuestion".to_string(), Arc::new(ask_user_tool));
        }

        // 错误感知建议：从 shared_tools 构造 snapshot（所有工具都已注册）
        let all_tool_names: Vec<String> = shared_tools.read().keys().cloned().collect();
        let snapshot = error_suggest::build_tool_registry_snapshot(all_tool_names, Some(cwd));
        let registry = error_suggest::build_default_registry();

        ChainAssembly {
            chain,
            subagent_mw: Some(Arc::new(subagent) as Arc<dyn SubAgentMiddlewarePort>),
            error_suggest_registry: Some(registry),
            tool_registry_snapshot: Arc::new(snapshot),
        }
    }
}

// 装配触发点收敛：不再提供本层便捷入口。装配一律经 Agent 层 session 工厂的
// `build_middleware_chain`（唯一触发点，ARC-MIDDLEWARE-001）触发，
// 本模块仅保留 trait 实现（`ProductionChainAssembler`）。

/// 加载全局 LSP 配置（settings.json 的 `config.lspServers`）并与插件 LSP
/// 服务器合并，返回装配用服务器列表。
///
/// 合并优先级对齐 MCP 三层合并（`crate::mcp::config::load_merged_config_full`）：
/// global < plugin——同名 key 插件覆盖全局（插件名带 `plugin:{name}:{server}`
/// 前缀，实际冲突面小，覆盖方向仍与 MCP 一致）。source 标记与静态 `${VAR}`
/// 展开由加载/构造侧完成；cwd 与 Session ID 保留到会话池工厂绑定。
/// 此处只做合并。无任何配置时返回空 Vec——装配处
/// `lsp_servers.is_empty()` 条件注册语义不变。
///
/// H5：宿主装配统一经 `assemble_server_config` 由此函数接入全局配置；此前宿主只取插件
/// lsp_servers，无插件时 LSP 整条产品线静默不可用。
pub fn load_merged_lsp_servers(
    settings_json_path: &Path,
    plugin_servers: Vec<peri_acp_types::lsp::LspServerConfig>,
) -> Vec<peri_acp_types::lsp::LspServerConfig> {
    let global = peri_resources::lsp::config::load_global_lsp_config(settings_json_path);
    let mut merged: HashMap<String, peri_acp_types::lsp::LspServerConfig> = global.lsp_servers;
    for server in plugin_servers {
        merged.insert(server.name.clone(), server);
    }
    merged.into_values().collect()
}

/// 构造会话级 LSP 服务器池并 upcast 端口（装配面宿主 session/new /
/// load / resume / fork 调用；返回类型已锚定端口 trait，调用方无需引用
/// peri-lsp 类型路径）。
///
/// 工厂为每个 Session 先绑定其 cwd 与 ID，再创建独立服务器池。无服务器配置时
/// 返回 None（不注册 LSP 中间件，与装配面
/// `lsp_servers.is_empty()` 条件注册语义一致）。H1：会话级实例跨 turn
/// 复用（服务器进程 / initialized / 诊断状态不丢），宿主退出时经端口
/// `shutdown` 优雅关闭。
pub fn create_session_lsp_pool(
    cwd: &str,
    session_id: &str,
    configs: &[peri_acp_types::lsp::LspServerConfig],
) -> Option<Arc<dyn LspPoolPort>> {
    if configs.is_empty() {
        return None;
    }
    let lsp_config = build_session_lsp_config(cwd, session_id, configs);
    Some(Arc::new(LspServerPool::new(cwd, lsp_config)) as Arc<dyn LspPoolPort>)
}

/// 克隆 Host 级 LSP 模板，并为当前 Session 绑定 cwd 与 Session ID。
pub(crate) fn build_session_lsp_config(
    cwd: &str,
    session_id: &str,
    configs: &[peri_acp_types::lsp::LspServerConfig],
) -> LspConfigFile {
    LspConfigFile {
        lsp_servers: configs
            .iter()
            .map(|server| {
                let resolved = resolve_lsp_config_for_session(server, cwd, session_id);
                (resolved.name.clone(), resolved)
            })
            .collect(),
    }
}

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
