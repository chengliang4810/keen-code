//! 子 Agent 创建统一入口（3.0 L3 迁移）。
//!
//! L3 归位：subagent 创建逻辑（建 thread / 建 session / 运行 + 收尾）自
//! `peri-middlewares/src/subagent/`（spawner / execute_fork / execute_bg /
//! build_agent 四条路径）收敛至 [`spawn_subagent`]。
//! Middleware 只声明工具与发起意图（组装 [`SubagentSpawnConfig`]），
//! 不持有创建实现。
//!
//! 依赖方向：Agent 层不反向依赖 middlewares。子链装配经
//! [`SubagentChainAssembler`] trait 依赖反转（中间件层提供实现，
//! 链序 AgentsMd→Skills→[SkillPreload]→Todo 由实现方保持，ARC-MIDDLEWARE-001）；
//! 生命周期 hook 触发经 [`SubagentLifecycleStart`]/[`SubagentLifecycleStop`]
//! 闭包注入（middlewares 构造闭包，内部触发其 RegisteredHook）。
//!
//! 验收语义：
//! - subagent 必有持久化 thread（parent_thread_id 父子链；transcript 绑定
//!   `with_persistence`，thread_id = agent_id）；
//! - frozen data 从父 session copy（parent 为 Some 时 claude_md / skill_summary /
//!   date 取自 `parent.store().frozen`，不重新读取磁盘）；
//! - agent_status 收尾语义与迁移前一致：done / cancelled / error。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::RwLock;
use peri_acp_types::identity::AgentId;
use peri_acp_types::skills::SkillRoot;
use peri_acp_types::thread::CancelPolicy;
use tokio_util::sync::CancellationToken;

use crate::agent::async_tasks::{
    AgentFollowupHandle, BackgroundTask, BackgroundTaskStatus, BgCancelHandle, BgTaskKind,
    TaskManager,
};
use crate::agent::events::{AgentEventHandler, ExecutorEvent};
use crate::agent::events_v2::{
    observe_event_to_executor, EventBus, EventBusConfig, EventHandles, ObserveEvent,
};
use crate::agent::react::{AgentOutput, ReactLLM};
use crate::agent::stages::{run_react_loop, LoopResult, SharedToolMap, StageContext};
use crate::agent::subagent_event_forwarder::spawn_subagent_event_forwarder;
use crate::agent::{CompactConfig, ContextBudget};
use crate::error_suggest::{ErrorSuggestRegistry, ToolRegistrySnapshot};
use crate::messages::BaseMessage;
use crate::middleware::chain::MiddlewareChain;
use crate::session::factory::{DeregisterRuntimeFn, RegisterRuntimeFn};
use crate::session::queue::{MessageKind, MessageSource, QueuedMessage};
use crate::session::turn::TurnId;
use crate::session::{FrozenContext, MessageQueue, MessageTranscript, Session};
use crate::thread::{AgentNickname, ThreadMeta, ThreadStore};
use crate::tools::DirectToolInvocationResolver;
use crate::tools::{BaseTool, ToolInvocationResolver};

// ─── 意图类型 ────────────────────────────────────────────────────────────────

/// Fork 指令类型，决定 fork agent 使用的 system directive 模板
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkDirectiveKind {
    /// 使用 [`build_fork_directive`]（英文，Agent 工具路径）
    Fork,
}

/// 子 agent 生命周期 hook 触发闭包（middlewares 构造，内部触发 RegisteredHook）。
/// 参数：(agent_name, cwd)。
pub type SubagentLifecycleStart = Arc<dyn Fn(&str, &str) + Send + Sync>;
/// 参数：(agent_name, cwd, result, is_error)。
pub type SubagentLifecycleStop = Arc<dyn Fn(&str, &str, &str, bool) + Send + Sync>;

// ─── 子链装配（依赖反转，ARC-MIDDLEWARE-001） ───────────────────────────────

/// 子 agent 链装配上下文：frozen 数据由 [`spawn_subagent`] 从父 session copy 后注入。
#[derive(Debug, Clone, Default)]
pub struct SubagentChainContext {
    /// 工作目录（解析 skill 文件路径）
    pub cwd: String,
    /// 需要预加载的 skill 名称（空 = 跳过 SkillPreloadMiddleware）
    pub skill_names: Vec<String>,
    /// Frozen CLAUDE.md/AGENTS.md main content（父 session copy；None = 从磁盘读取）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（上层注入的冻结数据）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（父 session copy）
    pub frozen_skill_summary: Option<String>,
    /// 父 session 捕获的插件 Skill 根（与主 Agent 使用同一快照）
    pub plugin_skill_roots: Vec<SkillRoot>,
}

/// 子 agent 中间件链装配器：由中间件层提供实现。
///
/// 链序（AgentsMd→Skills→[SkillPreload]→Todo）是行为契约，实现方必须保持
/// `peri-middlewares/src/subagent/tool/mod.rs` 的 `build_subagent_middlewares`
/// 顺序（ARC-MIDDLEWARE-001）。
pub trait SubagentChainAssembler: Send + Sync {
    fn assemble(&self, ctx: &SubagentChainContext) -> MiddlewareChain;
}

// ─── 父侧运行时宿主 ──────────────────────────────────────────────────────────

/// 父侧运行时通道聚合（L3）：executor/builder 在主 session 创建后注入，
/// subagent 创建所需的运行时通道统一经此读取，不再逐字段透传
/// SubAgentMiddleware。
#[derive(Clone, Default)]
#[allow(clippy::type_complexity)]
pub struct SubagentHost {
    /// 线程持久化存储（生产路径非 None；None 仅测试/遗留路径，跳过落库）
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// 后台任务管理器（per-session 聚合）
    pub task_manager: Option<Arc<TaskManager>>,
    /// 会话级 Inbox；WaitAgent 只监听其中的用户 Prompt。
    pub idle_inbox: Option<Arc<crate::agent::session::SessionInbox>>,
    /// WaitAgent 等待期间置位，使新用户输入直接注入 Inbox。
    pub idle_suspended_flag: Option<Arc<AtomicBool>>,
    /// 后台任务完成事件通道（bg pump，独立于主 event pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// bg 完成同步回调（registry.complete 之前调用，推送 Defer 到主 agent MQ）
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// 子 agent 启动注册回调（active_agents）
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// Frozen CLAUDE.local.md（父 session 冻结数据中唯一不在 FrozenContext 的字段）
    pub frozen_claude_local_md: Option<Arc<String>>,
    /// Frozen system prompt（fork 路径复用以避免重建；父 session 冻结的 subagent 版本）
    pub frozen_system_prompt: Option<Arc<String>>,
    /// 父线程 ID 回退值（生产路径由 spawn_subagent 从 parent session 读取）
    pub parent_thread_id: Option<String>,
    /// Frozen CLAUDE.md 回退值（生产路径由 spawn_subagent 从 parent session copy）
    pub frozen_claude_md: Option<Arc<String>>,
    /// Frozen skills summary 回退值（生产路径由 spawn_subagent 从 parent session copy）
    pub frozen_skill_summary: Option<Arc<String>>,
    /// 插件 Skill 根快照（生产路径由主 session 装配时注入）
    pub plugin_skill_roots: Vec<SkillRoot>,
}

// ─── spawn 配置与产物 ────────────────────────────────────────────────────────

/// 子 agent 创建意图 + 装配产物 + 运行时通道（统一入口 [`spawn_subagent`] 的输入）。
///
/// 父侧数据（cwd / parent_thread_id / frozen claude_md / skill_summary / date）
/// 在 `parent` 存在时从 parent Session 读取，config 中
/// 对应字段仅作 parent 缺失（测试或降级路径）时的回退。
#[allow(clippy::type_complexity)]
pub struct SubagentSpawnConfig {
    // ── 意图 ──
    /// 子 agent 名（事件 agent_name / thread title / task agent_name）
    pub agent_name: String,
    /// 派发给子 Agent 的任务描述（fork 路径经 fork directive 包装后入队）
    pub prompt: String,
    /// 父会话消息历史（fork 路径注入 transcript 让子 agent 理解上下文）
    pub parent_messages: Vec<BaseMessage>,
    /// 最大 ReAct 迭代次数
    pub max_iterations: usize,
    /// fork directive 模板（None = 不包装，直接 push prompt——agent 定义路径）
    pub fork_directive_kind: Option<ForkDirectiveKind>,
    /// agent 定义声明的 skills（SkillPreload 装配输入）
    pub skill_names: Vec<String>,
    // ── 装配产物 ──
    /// SubAgent LLM（ReactLLM 实现/装饰器）
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    /// 子 agent 中间件链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub chain_assembler: Arc<dyn SubagentChainAssembler>,
    /// 过滤后的工具集（agent 定义路径按 tools/disallowed_tools 过滤）
    pub tools: Vec<Arc<dyn BaseTool>>,
    /// SubAgent system prompt（注入 transcript 起始处）
    pub system_prompt: Option<String>,
    /// 错误感知建议注册表（可选）
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（None 用 default）
    pub tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    /// deferred 工具解析器（None = DirectToolInvocationResolver；middlewares 传
    /// ExecuteExtraToolResolver 保持包装层语义）
    pub tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    /// auto-compact 阈值配置（None = 不启用）
    pub compact_config: Option<CompactConfig>,
    /// 上下文预算（None = 不追踪 token 使用率）
    pub context_budget: Option<ContextBudget>,
    /// Full Compact 专用 LLM（None 时 Full Compact 跳过）
    pub compact_llm: Option<Arc<dyn peri_model::Model>>,
    // ── 运行时通道 ──
    /// 线程持久化存储（None = 不落库，仅测试/遗留路径）
    pub thread_store: Option<Arc<dyn ThreadStore>>,
    /// bg 任务完成事件发送通道（bg pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// 后台任务管理器（所有 Agent 调用必填）
    pub task_manager: Option<Arc<TaskManager>>,
    /// bg 完成同步回调
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_start: Option<SubagentLifecycleStart>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_stop: Option<SubagentLifecycleStop>,
    /// 子 agent 启动注册回调
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// 父 agent 事件侧 AgentId（v2 SubagentStart/Stop 的 agent_id 字段）
    pub parent_agent_id: Option<AgentId>,
    // ── 父侧数据回退（parent 为 None 时使用；parent 存在时被覆盖） ──
    /// 工作目录
    pub cwd: Option<String>,
    /// 父线程 ID
    pub parent_thread_id: Option<String>,
    /// Frozen CLAUDE.md main content（回退值）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（父 session 无此字段，恒由上层注入）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（回退值）
    pub frozen_skill_summary: Option<String>,
    /// Frozen 日期 YYYY-MM-DD（回退值）
    pub frozen_date: Option<String>,
}

/// spawn 产物
pub struct SubagentSpawned {
    /// 子线程 ID（= 子 session thread_id = 身份键来源）
    pub child_thread_id: String,
    /// 后台任务 ID（格式 bg-{uuid v7}）
    pub task_id: String,
}

// ─── resume 配置（统一恢复入口 [`resume_subagent`] 的输入） ─────────────────

/// 子 agent 恢复意图 + 装配产物 + 运行时通道（[`SessionFactory::resume_subagent`] 的输入）。
///
/// 与 [`SubagentSpawnConfig`] 的字段差异（恢复语义禁止项，不提供）：
/// - 无 `parent_messages` / `system_prompt` / `fork_directive_kind`（F4：已在旧
///   transcript 中，重复注入会重复）；
/// - 无 `skill_names`（R-H1：SkillPreload 重复注入——旧 transcript 已含首轮注入
///   的 skill 内容，恢复时恒传空）；
/// - `thread_store` 必填（恢复现场的唯一来源是磁盘 thread）。
///
/// 父侧数据（cwd / parent_thread_id / frozen 回退值）在 `parent` 存在时从 parent
/// Session 读取，config 中对应字段仅作 parent 缺失时的回退（与 spawn 一致）。
#[allow(clippy::type_complexity)]
pub struct SubagentResumeConfig {
    // ── 意图 ──
    /// 要恢复的子线程 ID（thread_id 不变，可无限次恢复重入）
    pub thread_id: String,
    /// 追加指令（None = 隐式 continue，slice 4 处理）
    pub prompt: Option<String>,
    /// 子 agent 名（None 时从 meta.title 取）
    pub agent_name: Option<String>,
    /// 最大 ReAct 迭代次数
    pub max_iterations: usize,
    // ── 装配产物 ──
    /// SubAgent LLM（ReactLLM 实现/装饰器）
    pub llm: Box<dyn ReactLLM + Send + Sync>,
    /// 子 agent 中间件链装配器（middlewares 实现，链序契约 ARC-MIDDLEWARE-001）
    pub chain_assembler: Arc<dyn SubagentChainAssembler>,
    /// 过滤后的工具集（恢复路径由 tool 层按 title 重新应用过滤）
    pub tools: Vec<Arc<dyn BaseTool>>,
    /// deferred 工具解析器（None = DirectToolInvocationResolver；middlewares 传
    /// ExecuteExtraToolResolver 保持包装层语义）
    pub tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    /// 错误感知建议注册表（可选）
    pub error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    /// 工具注册表快照（None 用 default）
    pub tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    /// auto-compact 阈值配置（None = 不启用）
    pub compact_config: Option<CompactConfig>,
    /// 上下文预算（None = 不追踪 token 使用率）
    pub context_budget: Option<ContextBudget>,
    /// Full Compact 专用 LLM（None 时 Full Compact 跳过）
    pub compact_llm: Option<Arc<dyn peri_model::Model>>,
    // ── 运行时通道 ──
    /// 线程持久化存储（必填：恢复现场来源）
    pub thread_store: Arc<dyn ThreadStore>,
    /// bg 任务完成事件发送通道（bg pump）
    pub bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    /// 后台任务管理器（所有 Agent 调用必填）
    pub task_manager: Option<Arc<TaskManager>>,
    /// bg 完成同步回调
    pub on_bg_complete:
        Option<Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_start: Option<SubagentLifecycleStart>,
    /// 生命周期 hook 触发闭包（middlewares 构造）
    pub on_subagent_stop: Option<SubagentLifecycleStop>,
    /// 子 agent 启动注册回调
    pub register_runtime: Option<RegisterRuntimeFn>,
    /// 子 agent 结束注销回调
    pub deregister_runtime: Option<DeregisterRuntimeFn>,
    /// 父 agent 事件侧 AgentId（v2 SubagentStart/Stop 的 agent_id 字段）
    pub parent_agent_id: Option<AgentId>,
    // ── 父侧数据回退（parent 为 None 时使用；parent 存在时被覆盖） ──
    /// 工作目录
    pub cwd: Option<String>,
    /// Frozen CLAUDE.md main content（回退值）
    pub frozen_claude_md: Option<String>,
    /// Frozen CLAUDE.local.md content（父 session 无此字段，恒由上层注入）
    pub frozen_claude_local_md: Option<String>,
    /// Frozen skills summary（回退值）
    pub frozen_skill_summary: Option<String>,
    /// Frozen 日期 YYYY-MM-DD（回退值）
    pub frozen_date: Option<String>,
}

// ─── 统一入口 ────────────────────────────────────────────────────────────────

/// Agent 层 session 工厂（L3）：subagent 创建统一入口命名空间。
///
/// 验收契约（子 issue L3）：`SessionFactory::spawn_subagent(parent, config)`
/// 为唯一 subagent 创建入口，位于 peri-agent。Middleware 只组装
/// [`SubagentSpawnConfig`] 发起意图，不持有创建实现。
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionFactory;

const AGENT_NICKNAME_COUNT: u16 = 128;

// ponytail: Agent 启动频率低，单锁让“并发预检 → 建 thread → 注册”原子化；
// 若未来跨进程共享 TaskManager，再改为持久层原子预留。
static SUBAGENT_START_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) fn allocate_agent_nickname(
    child_thread_id: &str,
    siblings: &[ThreadMeta],
) -> AgentNickname {
    let uuid = uuid::Uuid::parse_str(child_thread_id)
        .expect("child_thread_id 由 Uuid::now_v7() 生成，必为合法 UUID");
    let bytes = uuid.as_bytes();
    let start = u16::from_be_bytes([bytes[14], bytes[15]]) % AGENT_NICKNAME_COUNT;
    let used = siblings
        .iter()
        .filter_map(|meta| meta.agent_nickname)
        .collect::<std::collections::HashSet<_>>();

    for generation in 1_u32.. {
        for offset in 0..AGENT_NICKNAME_COUNT {
            let candidate = AgentNickname {
                index: (start + offset) % AGENT_NICKNAME_COUNT,
                generation,
            };
            if !used.contains(&candidate) {
                return candidate;
            }
        }
    }
    unreachable!("u32 generations are sufficient for any practical session")
}

impl SessionFactory {
    /// 启动子 agent（唯一创建入口，见 [`spawn_subagent_impl`] 的流程说明）。
    pub async fn spawn_subagent(
        parent: Option<&Arc<Session>>,
        config: SubagentSpawnConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        spawn_subagent_impl(parent, config).await
    }

    /// 恢复子 agent（唯一恢复入口，见 [`resume_subagent_impl`] 的校验流程说明）。
    ///
    /// 主 agent 凭中断、错误或后台通知文本携带的 `child_thread_id` 重新唤起被中断的
    /// subagent：从磁盘 thread_store 加载 meta 校验（存在 / 非 active / parent 链）
    /// 后重建现场继续执行。thread_id 不变，可无限次恢复。
    pub async fn resume_subagent(
        parent: Option<&Arc<Session>>,
        config: SubagentResumeConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        resume_subagent_impl(parent, config).await
    }
}

/// 启动子 agent（统一创建入口实现，L3）。
///
/// 流程（与迁移前四条路径语义一致）：
/// 1. 生成 child_thread_id / task_id
/// 2. 解析父侧数据（parent 优先；frozen copy 自 parent session，不重读磁盘）
/// 3. 创建子线程（thread_store Some 时；parent_thread_id 挂父子链）
/// 4. 构造子 session（frozen copy + transcript with_persistence 绑定存储）
/// 5. 注入 parent_messages / system_prompt 到 transcript，push prompt 到 queue
/// 6. 经 chain_assembler 装配子链（frozen 注入链上下文），构造 StageContext
/// 7. tokio::spawn + TaskManager 注册，立即返回任务与线程 ID
/// 8. 收尾：update_thread_status（done/cancelled/error）+ 事件 + hook 闭包
///
/// 缺少 TaskManager 或并发已满时在创建 thread 前失败。
#[allow(clippy::too_many_arguments)]
async fn spawn_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentSpawnConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 解构 config：字段分散使用，避免部分 move 后整体借用冲突
    let SubagentSpawnConfig {
        agent_name,
        prompt,
        parent_messages,
        max_iterations,
        fork_directive_kind,
        skill_names,
        llm,
        chain_assembler,
        tools,
        system_prompt,
        error_suggest_registry,
        tool_registry_snapshot,
        tool_invocation_resolver,
        compact_config,
        context_budget,
        compact_llm,
        thread_store,
        bg_event_sender,
        task_manager,
        on_bg_complete,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        deregister_runtime,
        parent_agent_id,
        cwd: cwd_cfg,
        parent_thread_id: parent_thread_id_cfg,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    let task_manager =
        task_manager.ok_or("Agent tasks not available: no task manager configured")?;
    let _start_guard = SUBAGENT_START_LOCK.lock().await;
    let agent_limit = task_manager.agent_limit();
    if task_manager.count_by_kind(BgTaskKind::Agent) >= agent_limit {
        return Err(format!(
            "Maximum {agent_limit} concurrent Agent tasks reached; wait for a running Agent or stop one"
        )
        .into());
    }

    // 2. 生成标识符
    let child_thread_id = uuid::Uuid::now_v7().to_string();
    let task_id = format!("bg-{}", uuid::Uuid::now_v7());

    // 3. 父侧数据解析（parent 优先；frozen data 从父 session copy）
    let cwd = parent
        .map(|p| p.store().cwd.to_string())
        .or(cwd_cfg)
        .ok_or("spawn_subagent: cwd is missing (parent is absent and config.cwd is None)")?;
    let parent_thread_id = parent
        .and_then(|p| p.store().thread_id.clone())
        .or(parent_thread_id_cfg);
    let frozen_claude_md = parent
        .map(|p| p.store().frozen.claude_md.to_string())
        .or(frozen_claude_md_cfg);
    let frozen_skill_summary = parent
        .map(|p| p.store().frozen.skill_summary.to_string())
        .or(frozen_skill_summary_cfg);
    let plugin_skill_roots = parent
        .and_then(|p| p.subagent_host())
        .map(|host| host.plugin_skill_roots.clone())
        .unwrap_or_default();
    let frozen_date = parent
        .map(|p| p.store().frozen.date.to_string())
        .or(frozen_date_cfg);
    let frozen_claude_local_md = frozen_claude_local_md_cfg;

    let cancel_token = CancellationToken::new();

    // 4. 创建子线程并分配稳定展示昵称。锁覆盖“读取兄弟 → 创建 thread”，保证
    // 同一进程内并发创建的兄弟 Agent 不会拿到同一个昵称。
    let agent_nickname = if let Some(ref store) = thread_store {
        let siblings = match &parent_thread_id {
            Some(parent_id) => store
                .list_child_threads(parent_id)
                .await
                .map_err(|e| format!("Failed to list sibling threads: {}", e))?,
            None => Vec::new(),
        };
        let nickname = allocate_agent_nickname(&child_thread_id, &siblings);
        let snapshot_id = parent_messages.last().map(|m| m.id().as_uuid().to_string());
        let mut child_meta = ThreadMeta::new(&cwd);
        child_meta.id = child_thread_id.clone();
        child_meta.parent_thread_id = parent_thread_id.clone();
        child_meta.snapshot_at_message_id = snapshot_id;
        child_meta.hidden = true;
        child_meta.cancel_policy = CancelPolicy::Independent;
        child_meta.title = Some(agent_name.clone());
        child_meta.agent_nickname = Some(nickname);
        store
            .create_thread(child_meta)
            .await
            .map_err(|e| format!("Failed to create child thread: {}", e))?;
        nickname
    } else {
        allocate_agent_nickname(&child_thread_id, &[])
    };

    // 4b. 双轨寻址注册:/root/{agent_name} ↔ child_thread_id(冲突自动加后缀)。
    //     路径用于消息头 / 提示词 / 工具入参的友好寻址,内部身份仍为 UUID。
    //     条目 D 将把该路径写入 BackgroundTaskResult 的消息头。
    let _agent_path =
        task_manager.register_agent_path(&format!("/root/{agent_name}"), &child_thread_id);

    // 5. 构造子 session + 链装配 + v2_ctx（共享 helper [build_subagent_session_v2]：
    //    frozen 从父 copy 不重读磁盘，transcript 绑定存储，ancestor 为空）
    //    注入 parent_messages / system_prompt / prompt 留在本函数——spawn 与
    //    resume 的消息注入差异大，不进 helper（D1）
    let frozen = FrozenContext {
        system_prompt: parent
            .map(|p| Arc::clone(&p.store().frozen.system_prompt))
            .unwrap_or_default(),
        claude_md: frozen_claude_md
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        skill_summary: frozen_skill_summary
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        date: frozen_date
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        language: parent.and_then(|p| p.store().frozen.language.clone()),
    };
    let (session, v2_ctx) = build_subagent_session_v2(
        cwd.clone(),
        frozen,
        cancel_token.clone(),
        child_thread_id.clone(),
        thread_store.clone(),
        Vec::new(), // 无 ancestor（spawn 新建 thread，transcript 为空）
        llm,
        chain_assembler,
        tools,
        skill_names,
        plugin_skill_roots,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&child_thread_id)),
    );

    let transcript = session.transcript();

    // 6a. fork 路径：把 parent_messages 注入 transcript（让子 agent 看到父会话上下文）
    if !parent_messages.is_empty() {
        let mut tx = transcript.write();
        for msg in &parent_messages {
            tx.append(msg.clone());
        }
    }

    // 6b. SubAgent system_prompt（身份构建）注入到 transcript 开头位置：
    // - fork 路径：在 parent_messages 之后（让身份提示词位于对话上下文之后、
    //   prompt 之前——SubAgent 的 prompt 由下方 push 到 queue，Receive 阶段追加）
    // - 非 fork 路径：parent_messages 为空，直接 append 到 transcript 开头
    //
    // 注意：这是 session 起始身份构建（在 run_react_loop 调用前注入），不是中途纠正，
    // 用 BaseMessage::System 合法（CLAUDE.md TRAP 仅禁止中途纠正用 System）。
    if let Some(sp) = system_prompt {
        let mut tx = transcript.write();
        tx.append(BaseMessage::system(sp));
    }

    // 6c. push prompt 到 queue（fork 路径套 fork directive 模板）
    let prompt_message = match fork_directive_kind {
        Some(ForkDirectiveKind::Fork) => build_fork_directive(&prompt),
        None => prompt.clone(),
    };
    v2_ctx.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_message),
    ));

    if let Err(error) = spawn_background_subagent(
        task_id.clone(),
        child_thread_id.clone(),
        agent_name,
        agent_nickname,
        prompt,
        cwd,
        max_iterations,
        bg_event_sender,
        Arc::clone(&task_manager),
        on_bg_complete,
        thread_store.clone(),
        deregister_runtime,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        parent_agent_id,
        cancel_token,
        v2_ctx,
    )
    .await
    {
        if let Some(store) = thread_store {
            let _ = store.delete_thread(&child_thread_id).await;
        }
        return Err(error);
    }
    Ok(SubagentSpawned {
        child_thread_id,
        task_id,
    })
}

// ─── 共享 session 构造（spawn / resume 共用，D1） ───────────────────────────

/// 构造子 session + 链装配 + v2_ctx（[`spawn_subagent_impl`] 与
/// [`resume_subagent_impl`] 共用的装配块，纯 move 提取——spawn 行为不变）。
///
/// - session 以 `child_thread_id` 为 thread_id（subagent 必有持久化 thread；
///   thread_id = agent_id）；
/// - transcript 装载 `ancestor`（resume 的旧 transcript 重放；spawn 传空——
///   `with_ancestor(vec![])` 为 no-op），再 `with_persistence` 绑定存储
///   （**顺序不可反**：with_ancestor 只建 id_index、不触发持久化
///   transcript.rs:158-169，append 会 send_persist 二次落库 :430-438）；
/// - 链装配（skill_names / frozen 注入链上下文；链序由 assembler 实现方保持）；
/// - `build_v2_subagent_context` 构造 StageContext。
///
/// 父侧解析 / cancel token / 消息注入（parent_messages / system_prompt /
/// prompt）差异大，留在调用方（D1）。
#[allow(clippy::too_many_arguments)]
fn build_subagent_session_v2(
    cwd: String,
    frozen: FrozenContext,
    cancel_token: CancellationToken,
    child_thread_id: String,
    thread_store: Option<Arc<dyn ThreadStore>>,
    ancestor: Vec<BaseMessage>,
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain_assembler: Arc<dyn SubagentChainAssembler>,
    tools: Vec<Arc<dyn BaseTool>>,
    skill_names: Vec<String>,
    plugin_skill_roots: Vec<SkillRoot>,
    frozen_claude_md: Option<String>,
    frozen_claude_local_md: Option<String>,
    frozen_skill_summary: Option<String>,
    tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    agent_id: Option<AgentId>,
) -> (Arc<Session>, V2SubagentContext) {
    let cancel_arc: Arc<CancellationToken> = Arc::new(cancel_token.clone());
    // SubAgent 独立 MessageQueue（不与 main agent 共享）
    let queue = MessageQueue::new();
    let session = Session::new_with_cancel_and_queue(
        Arc::from(cwd.as_str()),
        frozen,
        Some(child_thread_id.clone()),
        cancel_arc,
        queue,
    );

    // transcript 绑定（ancestor 先于 with_persistence，顺序不可反）
    {
        let transcript_arc = session.transcript();
        let mut transcript = transcript_arc.write();
        let old = std::mem::take(&mut *transcript);
        let with_ancestor = old.with_ancestor(ancestor);
        *transcript = match thread_store {
            Some(ref store) => {
                with_ancestor.with_persistence(Arc::clone(store), child_thread_id.clone())
            }
            None => with_ancestor,
        };
    }

    // 子链装配（frozen 数据注入链上下文；链序由 assembler 实现方保持）
    let chain = chain_assembler.assemble(&SubagentChainContext {
        cwd: cwd.clone(),
        skill_names,
        frozen_claude_md,
        frozen_claude_local_md,
        frozen_skill_summary,
        plugin_skill_roots,
    });

    // StageContext 构造（v2_bridge 迁移；tool_invocation_resolver 参数化；
    // 复用上面预创建的 session——transcript 已装载 ancestor 并绑定持久化）
    let v2_ctx = build_v2_subagent_context(
        Some(session.clone()),
        llm,
        chain,
        tools,
        &cwd,
        cancel_token,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        agent_id,
    );

    (session, v2_ctx)
}

// ─── 恢复（统一入口 resume_subagent） ───────────────────────────────────────

/// resume 校验段互斥锁（R2-LOW-2）：static 全局单锁（`SessionFactory` 为 unit
/// struct 无字段，锁表不能放 factory；放工具实例则多实例互斥失效）。
///
/// 锁内仅 load_meta + update_thread_status（无嵌套锁，tokio::sync::Mutex 跨 await
/// 安全，无死锁）。resume 为低频操作，单锁即可。跨进程双 resume 仍可能双执行，
/// 属已接受限制（缓解定位，与 issue 非目标「不自动恢复」一致）。
static RESUME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 恢复子 agent（统一入口实现，slice 4：重建 + 执行）。
///
/// 三层校验（不通过返回明确 Err，与 issue 验收一致）：
/// 1. 存在性：`load_meta` 失败/不存在 → `thread not found`
/// 2. status：`agent_status == Active`（可能未正常收尾）→ 拒绝恢复
/// 3. parent 链：`meta.parent_thread_id` 与 parent 的持久化 thread ID 比对
///    （parent 为 None 时仅校验存在性，不校验 parent 链——与 spawn 的 parent
///    回退语义一致）
///
/// 校验 → 置 active 段整体持锁（R-M1：防并发双 resume 双执行同一 thread）；
/// 锁内仅 load_meta + update_thread_status（无嵌套锁，不 await run_react_loop）。
/// 锁释放后重建：
/// - `load_messages` 重放 transcript；**仅当末条**含未配对 tool_calls 的 AI 时
///   pop（R2-MID-1：禁止从后往前找 AI；pop 后其后无消息，无孤儿 Tool 可清理）
/// - cwd 取 `meta.cwd`（thread 创建时固化，进程重启后不得改用父 cwd）
/// - frozen 从父 session copy（ARC-FROZEN-001；parent None 用 config 回退）
/// - cancel token：始终新建，恢复后的 Agent 仍保持独立取消语义
/// - **不注入** parent_messages / identity System / skill_names（F4 / R-H1：
///   旧 transcript 已含首轮注入内容，重复注入会重复）
/// - prompt 入队：`Some(p)` 原样追加（不套 fork directive）；`None` 注入隐式
///   continue 常量（issue 决策 9）
/// - 始终生成新 task_id，并经 `spawn_background_subagent` 异步启动
///
/// 重建/装配失败（load_messages 失败）时回滚 status 至原值（R-M1），防 thread
/// 永久停留 active（R-M4 崩溃遗留的镜像问题）。执行开始后的失败走
/// 后的失败走后台任务收尾路径（error / cancelled），不回滚。
async fn resume_subagent_impl(
    parent: Option<&Arc<Session>>,
    config: SubagentResumeConfig,
) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
    // 解构 config（cwd 不用于恢复——cwd 取 meta.cwd，thread 创建时固化）
    let SubagentResumeConfig {
        thread_id,
        prompt,
        agent_name: agent_name_cfg,
        max_iterations,
        llm,
        chain_assembler,
        tools,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        thread_store,
        bg_event_sender,
        task_manager,
        on_bg_complete,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        deregister_runtime,
        parent_agent_id,
        cwd: _,
        frozen_claude_md: frozen_claude_md_cfg,
        frozen_claude_local_md: frozen_claude_local_md_cfg,
        frozen_skill_summary: frozen_skill_summary_cfg,
        frozen_date: frozen_date_cfg,
    } = config;

    let task_manager =
        task_manager.ok_or("Agent tasks not available: no task manager configured")?;
    let _start_guard = SUBAGENT_START_LOCK.lock().await;
    let agent_limit = task_manager.agent_limit();
    if task_manager.count_by_kind(BgTaskKind::Agent) >= agent_limit {
        return Err(format!(
            "Maximum {agent_limit} concurrent Agent tasks reached; wait for a running Agent or stop one"
        )
        .into());
    }

    // 校验 → 置 active 段整体持锁（R-M1：防并发双 resume 双执行同一 thread）
    let guard = RESUME_LOCK.lock().await;

    // 0. thread_id 格式校验（review low-1）：重建阶段 `agent_id_from_child_thread`
    //    会对非 UUID 字符串 expect panic，入口统一拒绝
    if uuid::Uuid::parse_str(&thread_id).is_err() {
        return Err(format!("resume_subagent: invalid thread id: {}", thread_id).into());
    }

    // 1. 存在性校验（load_meta 失败/不存在统一映射为 not found）
    let meta = thread_store
        .load_meta(&thread_id)
        .await
        .map_err(|_| format!("resume_subagent: thread not found: {}", thread_id))?;

    // 2. status 校验（R-M4：active = 未正常收尾，崩溃遗留需手动处理）
    if meta.agent_status.is_active() {
        return Err(format!(
            "resume_subagent: thread {} is still active \
            (thread 仍处于运行态: 可能仍在执行, 或上次异常退出未收尾; \
            若确认无执行中任务, 可改用 Agent(subagent_type: ...) 新建)",
            thread_id
        )
        .into());
    }
    // 原值快照：重建失败时回滚（R-M1）
    let previous_status = meta.agent_status;

    // 3. parent 链校验（所有权校验；parent 为 None 时仅校验存在性）
    if let Some(p) = parent {
        let parent_thread_id = p.store().thread_id.clone().or_else(|| {
            p.subagent_host()
                .and_then(|host| host.parent_thread_id.clone())
        });
        if meta.parent_thread_id != parent_thread_id {
            return Err(format!(
                "resume_subagent: parent thread mismatch for {} \
                (该 thread 属于其他父 agent 的上下文, 当前会话无权恢复; \
                并行派发的兄弟 subagent 需由原父 agent 恢复, 或改传 subagent_type 新建)",
                thread_id
            )
            .into());
        }
    }

    // 校验通过 → 锁内立即置 active（R-M1：置 active 与校验原子化，第二个并发
    // resume 在锁内看到 active 被拒）；重建失败时下方回滚
    thread_store
        .update_thread_status(&thread_id, "active")
        .await
        .map_err(|e| {
            format!(
                "resume_subagent: failed to mark thread {} active: {}",
                thread_id, e
            )
        })?;

    // 双轨寻址:resume 重新注册 /root/{title} ↔ thread(同 pair 幂等;被占自动后缀)。
    if let Some(p) = parent {
        if let Some(tm) = p.subagent_host().and_then(|host| host.task_manager.clone()) {
            tm.register_agent_path(
                &format!("/root/{}", meta.title.as_deref().unwrap_or("agent")),
                &thread_id,
            );
        }
    }

    // 释放锁：重建/执行不持锁（load_messages 与 run_react_loop 不在互斥段内）
    drop(guard);

    // ── 重建（失败回滚 status 至原值，防 thread 永久停留 active）──

    // 1. 加载 transcript；末条含未配对 tool_calls 的 AI 则 pop（R2-MID-1：
    //    仅末条规则，幂等——磁盘旧消息不删除，每次 resume 重截）
    let mut loaded = match thread_store.load_messages(&thread_id).await {
        Ok(msgs) => msgs,
        Err(e) => {
            // R-M1 回滚：重建失败 → status 回滚至原值（不置 active 卡死）
            let _ = thread_store
                .update_thread_status(&thread_id, previous_status.as_str())
                .await;
            return Err(format!(
                "resume_subagent: failed to load messages for {}: {}",
                thread_id, e
            )
            .into());
        }
    };
    if loaded.last().is_some_and(|m| m.has_tool_calls()) {
        loaded.pop();
    }

    // 2. cwd 取 meta.cwd（thread 创建时固化的；进程重启后不得改用父 cwd）
    let cwd = meta.cwd.clone();

    // 3. frozen 从父 session copy（ARC-FROZEN-001：不重读磁盘；parent None 用
    //    config 回退，与 spawn 的父侧解析一致）
    let frozen_claude_md = parent
        .map(|p| p.store().frozen.claude_md.to_string())
        .or(frozen_claude_md_cfg);
    let frozen_skill_summary = parent
        .map(|p| p.store().frozen.skill_summary.to_string())
        .or(frozen_skill_summary_cfg);
    let plugin_skill_roots = parent
        .and_then(|p| p.subagent_host())
        .map(|host| host.plugin_skill_roots.clone())
        .unwrap_or_default();
    let frozen_date = parent
        .map(|p| p.store().frozen.date.to_string())
        .or(frozen_date_cfg);
    let frozen = FrozenContext {
        system_prompt: parent
            .map(|p| Arc::clone(&p.store().frozen.system_prompt))
            .unwrap_or_default(),
        claude_md: frozen_claude_md
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        skill_summary: frozen_skill_summary
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        date: frozen_date
            .as_ref()
            .map(|s| Arc::from(s.as_str()))
            .unwrap_or_default(),
        language: parent.and_then(|p| p.store().frozen.language.clone()),
    };

    let cancel_token = CancellationToken::new();

    // 5. agent_name：config 优先，回退 meta.title，最后兜底 "subagent"
    let agent_name = agent_name_cfg
        .or_else(|| meta.title.clone())
        .unwrap_or_else(|| "subagent".to_string());
    let agent_nickname = meta.agent_nickname.ok_or_else(|| {
        format!(
            "resume_subagent: thread {} has no agent nickname",
            thread_id
        )
    })?;

    // 6. 重建 session（thread_id 固定 = config.thread_id；with_ancestor 装载
    //    旧 transcript 重放 + with_persistence 绑定，顺序不可反——helper 内）
    //    不注入 parent_messages / identity System / skill_names（F4 / R-H1）
    let (_session, v2_ctx) = build_subagent_session_v2(
        cwd.clone(),
        frozen,
        cancel_token.clone(),
        thread_id.clone(),
        Some(Arc::clone(&thread_store)),
        loaded,
        llm,
        chain_assembler,
        tools,
        Vec::new(), // skill_names 恒空（R-H1：恢复不重复注入 SkillPreload）
        plugin_skill_roots,
        frozen_claude_md,
        frozen_claude_local_md_cfg,
        frozen_skill_summary,
        tool_invocation_resolver,
        error_suggest_registry,
        tool_registry_snapshot,
        compact_config,
        context_budget,
        compact_llm,
        Some(agent_id_from_child_thread(&thread_id)),
    );

    // 7. prompt 入队：Some(p) 原样追加（不套 fork directive——恢复目标仍是原
    //    任务，直接追加指令）；None 注入隐式 continue 常量（issue 决策 9）
    let prompt_text = prompt.unwrap_or_else(|| IMPLICIT_CONTINUE_PROMPT.to_string());
    v2_ctx.context.session.queue.push(QueuedMessage::new(
        MessageKind::Prompt,
        MessageSource::UserInput,
        BaseMessage::human(prompt_text.clone()),
    ));

    let task_id = format!("bg-{}", uuid::Uuid::now_v7());
    if let Err(error) = spawn_background_subagent(
        task_id.clone(),
        thread_id.clone(),
        agent_name,
        agent_nickname,
        prompt_text,
        cwd,
        max_iterations,
        bg_event_sender,
        Arc::clone(&task_manager),
        on_bg_complete,
        Some(Arc::clone(&thread_store)),
        deregister_runtime,
        on_subagent_start,
        on_subagent_stop,
        register_runtime,
        parent_agent_id,
        cancel_token,
        v2_ctx,
    )
    .await
    {
        let _ = thread_store
            .update_thread_status(&thread_id, previous_status.as_str())
            .await;
        return Err(format!("resume_subagent: thread {}: {}", thread_id, error).into());
    }
    Ok(SubagentSpawned {
        child_thread_id: thread_id,
        task_id,
    })
}

/// 隐式 continue 指令（prompt 缺省时注入，issue 决策 9）
const IMPLICIT_CONTINUE_PROMPT: &str = "Continue your previous task where you left off.";

// ─── 后台运行 ────────────────────────────────────────────────────────────────

/// 后台子 agent：tokio::spawn 包装运行 + TaskManager 注册（S3.1 gate）+ 收尾。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
async fn spawn_background_subagent(
    task_id: String,
    child_thread_id: String,
    agent_name: String,
    agent_nickname: AgentNickname,
    prompt: String,
    cwd: String,
    max_iterations: usize,
    bg_event_sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
    task_manager: Arc<TaskManager>,
    on_bg_complete: Option<
        Arc<dyn Fn(&crate::agent::events::BackgroundTaskResult, BgTaskKind) + Send + Sync>,
    >,
    thread_store: Option<Arc<dyn ThreadStore>>,
    deregister_runtime: Option<DeregisterRuntimeFn>,
    on_subagent_start: Option<SubagentLifecycleStart>,
    on_subagent_stop: Option<SubagentLifecycleStop>,
    register_runtime: Option<RegisterRuntimeFn>,
    parent_agent_id: Option<AgentId>,
    cancel_token: CancellationToken,
    v2_ctx: V2SubagentContext,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let task_manager_spawn = Arc::clone(&task_manager);
    let followup = AgentFollowupHandle::new(v2_ctx.session.queue().clone());
    let followup_for_task = followup.clone();

    let prompt_summary: String = prompt.chars().take(100).collect();

    // S3.1 注册门控：spawn 包装任务，闭包第一步 await 注册结果 oneshot。
    let (reg_tx, reg_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();

    let task_id_for_task = task_id.clone();
    let child_thread_id_for_task = child_thread_id.clone();
    let agent_name_for_task = agent_name.clone();
    let prompt_summary_for_task = prompt_summary.clone();
    let cwd_for_task = cwd.clone();

    let join_handle = tokio::spawn(async move {
        // S3.1 门控：注册结果（失败时调用方已发 Err；sender 被 drop 同样返回）
        match reg_rx.await {
            Ok(Ok(())) => {}
            _ => return,
        }

        let started_at = std::time::Instant::now();
        // context 将被 move 进 run_react_loop，turn_id 提前提取（Start/Stop emit 用）
        let subagent_turn_id = v2_ctx.context.turn_id();
        let context = v2_ctx.context;
        let session = v2_ctx.session;
        // Start/Stop emit 需要 event_bus（partial move 后仍可用）+ 统一身份键
        let event_bus_for_emit = v2_ctx.event_bus.clone();
        let subagent_agent_id = v2_ctx.agent_id;

        // S3.2 同步收尾 guard：abort/panic 时 deregister_runtime + 补发
        // v2 SubagentStop（含 v1 协议化直发，与 SubagentStarted 配对）。
        // 必须在本段事件 emit 之前构造。
        let mut cleanup_guard = BgCleanupGuard {
            thread_id: child_thread_id_for_task.clone(),
            deregister: deregister_runtime.clone(),
            stop: Some(BgStopEmitV2 {
                event_bus: event_bus_for_emit.clone(),
                turn_id: subagent_turn_id,
                parent_agent_id,
                child_agent_id: subagent_agent_id,
                agent_name: agent_name_for_task.clone(),
                // v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
                sender: bg_event_sender.clone(),
            }),
        };

        // v1 协议化发射目标（bg 泵）：BG pump 独立于主 pump，主 turn 结束后仍存活。
        // 构造提前到 Started 直发之前（start 借用、stop 直发 clone、forwarder move）。
        let bg_forwarder_handler: Option<Arc<dyn AgentEventHandler>> =
            bg_event_sender.clone().map(|tx| {
                Arc::new(crate::agent::events::FnEventHandler(
                    move |ev: ExecutorEvent| {
                        let _ = tx.send(ev);
                    },
                )) as Arc<dyn AgentEventHandler>
            });
        let bg_stop_handler = bg_forwarder_handler.clone();

        // lifecycle hook（SubagentStart）
        if let Some(ref on_start) = on_subagent_start {
            on_start(&agent_name_for_task, &cwd_for_task);
        }

        // v2 SubagentStart（C2）：与 lifecycle hook 同点、同通道（child EventBus）。
        emit_subagent_start_v2(
            &event_bus_for_emit,
            subagent_turn_id,
            parent_agent_id,
            subagent_agent_id,
            &agent_name_for_task,
            agent_nickname,
            true,
        );
        // v1 协议化载体直发（SubagentStarted）：发射语义单一事实源为 v2 事件构造
        // （ObserveEvent 身份透传：child_agent_id → instance_id），经
        // `observe_event_to_executor` 同步映射后直发 bg_event_sender——同步保证
        // Started 恒先于任何 SubagentStopped / BackgroundTaskCompleted
        // （正常/取消/abort 三路，P2 顺序契约）。
        if bg_event_sender.is_some() {
            forward_subagent_start_v1(
                bg_forwarder_handler.as_ref(),
                build_subagent_start_v2(
                    subagent_turn_id,
                    parent_agent_id,
                    subagent_agent_id,
                    &agent_name_for_task,
                    agent_nickname,
                    true,
                ),
            );
        } else {
            tracing::warn!(
                agent = %agent_name_for_task,
                instance_id = %child_thread_id_for_task,
                "bg_event_sender unavailable, SubagentStarted event dropped"
            );
        }

        // 启动 v2 事件转发器：消费 SubAgent EventBus 的事件，注入 source_agent_id
        // 后转发到 bg_event_sender（BG pump 独立于主 pump，主 turn 结束后仍存活）。
        // SubagentStart/Stop 不在此转发（发射侧已同步协议化直发，防双发——
        // 见 `forward_subagent_start_v1` / `forward_subagent_stop_v1`）。
        let _forwarder_handle = spawn_subagent_event_forwarder(
            v2_ctx.event_handles,
            bg_forwarder_handler,
            child_thread_id_for_task.clone(),
        );

        let loop_result = loop {
            match run_react_loop(context.clone(), max_iterations).await {
                LoopResult::Completed if followup_for_task.continue_after_completion() => {
                    continue;
                }
                LoopResult::Completed => break LoopResult::Completed,
                other => {
                    followup_for_task.close();
                    break other;
                }
            }
        };

        // followup_task 可在当前任务完成后立即恢复同一线程；先把 transcript
        // 写入持久层，避免恢复方读到半写 meta 或缺失最后一轮消息。
        let flush_tx = session.transcript().read().persist_tx_handle();
        if let Some(tx) = flush_tx {
            if let Err(error) = MessageTranscript::flush_via_tx(&tx).await {
                tracing::warn!(
                    child_thread_id = %child_thread_id_for_task,
                    error = %error,
                    "subagent transcript flush failed"
                );
            }
        }

        // 补发 v2 SubagentStop（C3）：一个 emit 点覆盖 Completed / Interrupted / Error。
        let (stop_result, stop_is_error) = match &loop_result {
            LoopResult::Completed => (
                extract_last_ai_text(&session)
                    .chars()
                    .take(500)
                    .collect::<String>(),
                false,
            ),
            LoopResult::Interrupted => ("interrupted".to_string(), true),
            LoopResult::Error(e) => (
                format!("Background sub-agent failed: {}", e)
                    .chars()
                    .take(500)
                    .collect::<String>(),
                true,
            ),
        };
        emit_subagent_stop_v2(
            &event_bus_for_emit,
            subagent_turn_id,
            parent_agent_id,
            subagent_agent_id,
            &agent_name_for_task,
            &stop_result,
            stop_is_error,
        );
        // v1 协议化直发（SubagentStopped）在下方各分支显式执行（Error 分支 / 正常
        // 分支），此处仅闭合 v2 发射：guard drop 时不得重复（P1 防双发）。
        cleanup_guard.disarm_stop();

        let (final_text, interrupted) = match loop_result {
            LoopResult::Completed => (extract_last_ai_text(&session), false),
            LoopResult::Interrupted => (String::new(), true),
            LoopResult::Error(e) => {
                let output = format!("Background sub-agent failed: {}", e);
                // 错误路径：lifecycle hook + thread_store + registry notification
                if let Some(ref on_stop) = on_subagent_stop {
                    on_stop(&agent_name_for_task, &cwd_for_task, &output, true);
                }
                if let Some(ref store) = thread_store {
                    let _ = store
                        .update_thread_status(&child_thread_id_for_task, "error")
                        .await;
                }
                // 错误分支也必须发射 SubagentStopped（is_error=true），保证 depth 配对减 1。
                // v1 协议化直发从 v2 事件构造同步映射（发射语义单一事实源 = v2；
                // ObserveEvent 身份透传：child_agent_id → instance_id）。
                // 必须在 BackgroundTaskResult 构造之前发射——后者会 move output。
                forward_subagent_stop_v1(
                    bg_stop_handler.as_ref(),
                    build_subagent_stop_v2(
                        subagent_turn_id,
                        parent_agent_id,
                        subagent_agent_id,
                        &agent_name_for_task,
                        &output,
                        true,
                    ),
                );
                let result = crate::agent::events::BackgroundTaskResult {
                    agent_path: task_manager_spawn.agent_path(&child_thread_id_for_task),
                    task_id: task_id_for_task.clone(),
                    agent_name: agent_name_for_task.clone(),
                    prompt_summary: prompt_summary_for_task.clone(),
                    success: false,
                    output,
                    tool_calls_count: count_tool_calls_from_session(&session),
                    duration_ms: started_at.elapsed().as_millis() as u64,
                    child_thread_id: Some(child_thread_id_for_task.clone()),
                    timed_out: false,
                };
                task_manager_spawn.complete_with(&task_id_for_task, result, |result| {
                    if let Some(ref sender) = bg_event_sender {
                        let _ = sender.send(ExecutorEvent::BackgroundTaskCompleted(result.clone()));
                    }
                    if let Some(ref on_complete) = on_bg_complete {
                        on_complete(result, BgTaskKind::Agent);
                    }
                });
                // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
                return;
            }
        };

        let output_summary: String = if interrupted {
            "interrupted".to_string()
        } else {
            final_text.chars().take(500).collect()
        };

        // SubagentStopped v1 协议化直发 + lifecycle hook（经 bg_event_sender，
        // 与 spawner 对齐）。v1 从 v2 事件构造同步映射，保证 Stopped 先于
        // BackgroundTaskCompleted 到达 bg 泵（顺序契约）。
        forward_subagent_stop_v1(
            bg_stop_handler.as_ref(),
            build_subagent_stop_v2(
                subagent_turn_id,
                parent_agent_id,
                subagent_agent_id,
                &agent_name_for_task,
                &output_summary,
                interrupted,
            ),
        );
        if let Some(ref on_stop) = on_subagent_stop {
            on_stop(
                &agent_name_for_task,
                &cwd_for_task,
                &output_summary,
                interrupted,
            );
        }

        // thread_store 状态
        if let Some(ref store) = thread_store {
            let status = if interrupted { "cancelled" } else { "done" };
            let _ = store
                .update_thread_status(&child_thread_id_for_task, status)
                .await;
        }

        // 后台任务完成通知（注入到主 agent 消息流）
        let result = crate::agent::events::BackgroundTaskResult {
            agent_path: task_manager_spawn.agent_path(&child_thread_id_for_task),
            task_id: task_id_for_task.clone(),
            agent_name: agent_name_for_task.clone(),
            prompt_summary: prompt_summary_for_task.clone(),
            success: !interrupted,
            output: if interrupted {
                "Background sub-agent was interrupted".to_string()
            } else {
                final_text
            },
            tool_calls_count: count_tool_calls_from_session(&session),
            duration_ms: started_at.elapsed().as_millis() as u64,
            child_thread_id: Some(child_thread_id_for_task.clone()),
            timed_out: false,
        };
        task_manager_spawn.complete_with(&task_id_for_task, result, |result| {
            if let Some(ref sender) = bg_event_sender {
                let _ = sender.send(ExecutorEvent::BackgroundTaskCompleted(result.clone()));
            } else {
                tracing::warn!(
                    task_id = %task_id_for_task,
                    "bg_event_sender unavailable, BackgroundTaskCompleted event dropped"
                );
            }
            if let Some(ref on_complete) = on_bg_complete {
                on_complete(result, BgTaskKind::Agent);
            }
        });
        // deregister 由 cleanup_guard drop 统一执行（正常/abort/panic 三路）
    });

    // 注册到 BackgroundTaskRegistry
    let bg_task = BackgroundTask {
        id: task_id.clone(),
        agent_name: agent_name.clone(),
        prompt_summary,
        status: BackgroundTaskStatus::Running,
        started_at: std::time::Instant::now(),
        chrono_started_at: chrono::Utc::now(),
        kind: BgTaskKind::Agent,
        child_thread_id: Some(child_thread_id.clone()),
        cancel_handle: BgCancelHandle::Abort(join_handle),
        cancel_token: Some(cancel_token.clone()),
        agent_followup: Some(followup),
        pid: None,
        output_preview: None,
    };
    if let Err(e) = task_manager.register_with_kind(bg_task) {
        // S3.1：注册失败（并发撞 kind 上限）——通知包装任务直接 return（不执行
        // run_react_loop、不 emit 任何事件），再如实返回错误。任务零事件零注册，
        // 无幽灵执行 / 无泄漏。
        let _ = reg_tx.send(Err(e.to_string()));
        return Err(format!("Failed to register background task: {}", e).into());
    }
    // 注册成功：先注册运行时（active_agents，与任务内 guard 的 deregister 配对），
    // 再放行包装任务继续执行。
    if let Some(register) = &register_runtime {
        register(child_thread_id.clone(), cancel_token);
    }
    let _ = reg_tx.send(Ok(()));

    Ok(())
}

// ─── v2 桥接（自 peri-middlewares/src/subagent/v2_bridge.rs 迁移） ──────────

/// SubAgent v2 上下文产物
pub struct V2SubagentContext {
    /// v2 StageContext（传给 run_react_loop）
    pub context: StageContext,
    /// v2 Session（调用方持有以读取 transcript）
    pub session: Arc<Session>,
    /// EventBus 消费端（调用方 spawn forwarder 用）
    pub event_handles: EventHandles,
    /// 统一后的 subagent 身份键（= child_thread_id 的 AgentId 形式）
    pub agent_id: AgentId,
    /// EventBus 生产端（补发 SubagentStart/Stop 等 ObserveEvent 用）
    pub event_bus: Arc<EventBus>,
}

/// 从 `child_thread_id`（UUID v7 字符串）解析统一身份键 `AgentId`（C1）。
///
/// 身份契约：`child_thread_id`、subagent session `AgentId`、`instance_id`、
/// forwarder `source_agent_id`、事件 `agent_id` 收敛为同一 UUID。
pub fn agent_id_from_child_thread(child_thread_id: &str) -> AgentId {
    AgentId::from_uuid(
        uuid::Uuid::parse_str(child_thread_id)
            .expect("child_thread_id 由 Uuid::now_v7() 生成，必为合法 UUID"),
    )
}

/// 构造 v2 `SubagentStart` 事件（发射语义单一事实源）。
///
/// `agent_id` 为父视角归属身份：`parent_agent_id` 未注入（测试或降级路径）时以
/// `child_agent_id` 占位——v1 协议化映射（`observe_event_to_executor`）不消费
/// 该字段，仅 v2 emit 的观察归属需要真实父身份。
pub(crate) fn build_subagent_start_v2(
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    agent_nickname: AgentNickname,
    is_background: bool,
) -> ObserveEvent {
    ObserveEvent::SubagentStart {
        turn_id,
        agent_id: parent_agent_id.unwrap_or(child_agent_id),
        child_agent_id,
        agent_name: agent_name.to_string(),
        agent_nickname,
        is_background,
    }
}

/// 经 child EventBus 发射 v2 `SubagentStart`（C2）。
///
/// `parent_agent_id` 为 None（未注入/测试路径）时不 emit，仅 tracing warn——
/// 防脏数据：缺父身份的事件无法正确归属，宁可走 incomplete 分支。
/// （v1 协议化直发不依赖本函数：`forward_subagent_start_v1` 独立于父身份。）
pub(crate) fn emit_subagent_start_v2(
    event_bus: &Arc<EventBus>,
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    agent_nickname: AgentNickname,
    is_background: bool,
) {
    if parent_agent_id.is_none() {
        tracing::warn!(
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStart emit（防脏数据）"
        );
        return;
    }
    event_bus.emit_observe(build_subagent_start_v2(
        turn_id,
        parent_agent_id,
        child_agent_id,
        agent_name,
        agent_nickname,
        is_background,
    ));
}

/// 构造 v2 `SubagentStop` 事件（发射语义单一事实源）。占位规则同
/// [`build_subagent_start_v2`]。
pub(crate) fn build_subagent_stop_v2(
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    result: &str,
    is_error: bool,
) -> ObserveEvent {
    ObserveEvent::SubagentStop {
        turn_id,
        agent_id: parent_agent_id.unwrap_or(child_agent_id),
        child_agent_id,
        agent_name: agent_name.to_string(),
        result: result.to_string(),
        is_error,
    }
}

/// 经 child EventBus 发射 v2 `SubagentStop`（C3）。
///
/// 与 [`emit_subagent_start_v2`] 同一通道；parent_agent_id 为 None 时同样跳过。
pub(crate) fn emit_subagent_stop_v2(
    event_bus: &Arc<EventBus>,
    turn_id: TurnId,
    parent_agent_id: Option<AgentId>,
    child_agent_id: AgentId,
    agent_name: &str,
    result: &str,
    is_error: bool,
) {
    if parent_agent_id.is_none() {
        tracing::warn!(
            child_agent_id = %child_agent_id,
            agent_name,
            "parent_agent_id 未注入，跳过 v2 SubagentStop emit（防脏数据）"
        );
        return;
    }
    event_bus.emit_observe(build_subagent_stop_v2(
        turn_id,
        parent_agent_id,
        child_agent_id,
        agent_name,
        result,
        is_error,
    ));
}

// ─── v1 协议化载体直发（发射侧同步映射） ───────────────────────────────────
//
// v1 `ExecutorEvent` 中间态已退役（`2026-07-18-executor-event-retirement.md`）：
// SubagentStart/Stop 的发射语义单一事实源为 v2 事件构造，v1 仅作 ACP 协议化
// 载体——经 `peri-acp-types::event_v2::observe_event_to_executor`（协议序列化面
// 保留的最小映射）同步映射后直发父 handler / bg 泵。同步直发（非 forwarder
// 异步转发）保证 Started/Stopped 与 BackgroundTaskCompleted 的顺序契约；
// 转发器（`subagent_event_forwarder`）对 v2 SubagentStart/Stop 保持过滤（防双发）。

/// v1 协议化直发 `SubagentStarted`（从 v2 事件同步映射）。
///
/// `handler` 为 None（无父 handler / 无 bg 通道）时静默跳过。
fn forward_subagent_start_v1(handler: Option<&Arc<dyn AgentEventHandler>>, ev: ObserveEvent) {
    let Some(h) = handler else { return };
    // SubagentStarted 无 source_agent_id 字段（TUI 按 instance_id 配对），
    // 无需 set_source_agent_id；instance_id 由 child_agent_id 身份透传（C1）。
    if let Some(exec_ev) = observe_event_to_executor(ev) {
        h.on_event(exec_ev);
    }
}

/// v1 协议化直发 `SubagentStopped`（从 v2 事件同步映射）。语义同
/// [`forward_subagent_start_v1`]。
fn forward_subagent_stop_v1(handler: Option<&Arc<dyn AgentEventHandler>>, ev: ObserveEvent) {
    let Some(h) = handler else { return };
    if let Some(exec_ev) = observe_event_to_executor(ev) {
        h.on_event(exec_ev);
    }
}

/// 构造 SubAgent v2 上下文（自 `build_v2_subagent_context` 迁移；
/// `tool_invocation_resolver` 参数化避免 Agent 层反向依赖 middlewares）。
///
/// `session` 为调用方预创建的子 session（transcript 已绑定持久化、已注入
/// parent_messages / system_prompt）；None 时内部自建（测试/工具直调路径兜底，
/// 无持久化）。
#[allow(clippy::too_many_arguments)]
pub fn build_v2_subagent_context(
    session: Option<Arc<Session>>,
    llm: Box<dyn ReactLLM + Send + Sync>,
    chain: MiddlewareChain,
    tools: Vec<Arc<dyn BaseTool>>,
    cwd: &str,
    cancel_token: CancellationToken,
    tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
    error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
    tool_registry_snapshot: Option<ToolRegistrySnapshot>,
    compact_config: Option<CompactConfig>,
    context_budget: Option<ContextBudget>,
    compact_llm: Option<Arc<dyn peri_model::Model>>,
    agent_id: Option<AgentId>,
) -> V2SubagentContext {
    let session = match session {
        Some(s) => s,
        None => {
            let cwd_arc: Arc<str> = Arc::from(cwd);
            let frozen = FrozenContext::builder().build();
            let cancel_arc = Arc::new(cancel_token);
            // 自建兜底：独立 MessageQueue，无持久化
            let queue = MessageQueue::new();
            Session::new_with_cancel_and_queue(cwd_arc, frozen, None, cancel_arc, queue)
        }
    };

    let turn = session.start_turn();
    let transcript = session.transcript();
    let queue_clone = session.queue().clone();

    // tools → SharedToolMap（本地 tools 全部进 map）
    let mut tools_map: std::collections::BTreeMap<String, Arc<dyn BaseTool>> =
        std::collections::BTreeMap::new();
    for tool in tools {
        tools_map.insert(tool.name().to_string(), tool);
    }
    let combined_shared_tools: SharedToolMap = Arc::new(RwLock::new(tools_map));

    let (event_bus, event_handles) = EventBus::new(EventBusConfig::default());
    let event_bus_arc: Arc<EventBus> = Arc::new(event_bus);

    // 身份键统一（C1）：child_thread_id → AgentId；None（测试路径）内部生成。
    let resolved_agent_id = agent_id.unwrap_or_default();

    let session_context = Arc::new(RwLock::new(std::collections::HashMap::new()));
    let v2_llm: Arc<dyn ReactLLM + Send + Sync> = Arc::from(llm);

    let snapshot = tool_registry_snapshot.unwrap_or_default();

    let mut builder = StageContext::builder(turn, transcript, queue_clone)
        .with_agent_id(resolved_agent_id)
        .with_llm(v2_llm)
        .with_tools(combined_shared_tools)
        .with_tool_invocation_resolver(tool_invocation_resolver.unwrap_or_else(|| {
            Arc::new(DirectToolInvocationResolver) as Arc<dyn ToolInvocationResolver>
        }))
        .with_middleware_chain(Arc::new(chain))
        .with_event_bus(Arc::clone(&event_bus_arc))
        .with_session_context(session_context)
        .with_tool_registry_snapshot(snapshot);

    if let Some(reg) = error_suggest_registry {
        builder = builder.with_error_suggest_registry(reg);
    }
    if let Some(budget) = context_budget {
        builder = builder.with_context_budget(budget);
    }
    if let Some(cc) = compact_config {
        builder = builder.with_compact_config(cc);
    }
    if let Some(llm) = compact_llm {
        builder = builder.with_compact_llm(llm);
    }
    // system_prompt 由 spawn_subagent 以 BaseMessage::System 注入 transcript
    //（StageContext.system_prompt 为死字段，不写入）。

    let context = builder.build();

    V2SubagentContext {
        context,
        session,
        event_handles,
        agent_id: resolved_agent_id,
        event_bus: event_bus_arc,
    }
}

/// SubAgent v2 上下文构建器（3.0 批 2 注入面）。
///
/// `build_v2_subagent_context` 的 trait 封装：协议面
/// 经装配注入本 trait 调用（不直接引用本层实现），默认实现即委托
/// [`build_v2_subagent_context`]（[`DefaultSubagentV2ContextBuilder`]）。
#[allow(clippy::too_many_arguments)]
pub trait SubagentV2ContextBuilder: Send + Sync {
    /// 构造 SubAgent v2 上下文（参数与 [`build_v2_subagent_context`] 一致）。
    fn build(
        &self,
        session: Option<Arc<Session>>,
        llm: Box<dyn ReactLLM + Send + Sync>,
        chain: MiddlewareChain,
        tools: Vec<Arc<dyn BaseTool>>,
        cwd: &str,
        cancel_token: CancellationToken,
        tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
        error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
        tool_registry_snapshot: Option<ToolRegistrySnapshot>,
        compact_config: Option<CompactConfig>,
        context_budget: Option<ContextBudget>,
        compact_llm: Option<Arc<dyn peri_model::Model>>,
        agent_id: Option<AgentId>,
    ) -> V2SubagentContext;
}

/// [`SubagentV2ContextBuilder`] 的默认实现：委托 [`build_v2_subagent_context`]。
pub struct DefaultSubagentV2ContextBuilder;

#[allow(clippy::too_many_arguments)]
impl SubagentV2ContextBuilder for DefaultSubagentV2ContextBuilder {
    fn build(
        &self,
        session: Option<Arc<Session>>,
        llm: Box<dyn ReactLLM + Send + Sync>,
        chain: MiddlewareChain,
        tools: Vec<Arc<dyn BaseTool>>,
        cwd: &str,
        cancel_token: CancellationToken,
        tool_invocation_resolver: Option<Arc<dyn ToolInvocationResolver>>,
        error_suggest_registry: Option<Arc<ErrorSuggestRegistry>>,
        tool_registry_snapshot: Option<ToolRegistrySnapshot>,
        compact_config: Option<CompactConfig>,
        context_budget: Option<ContextBudget>,
        compact_llm: Option<Arc<dyn peri_model::Model>>,
        agent_id: Option<AgentId>,
    ) -> V2SubagentContext {
        build_v2_subagent_context(
            session,
            llm,
            chain,
            tools,
            cwd,
            cancel_token,
            tool_invocation_resolver,
            error_suggest_registry,
            tool_registry_snapshot,
            compact_config,
            context_budget,
            compact_llm,
            agent_id,
        )
    }
}

// ─── 生命周期工具（自 tool/lifecycle.rs 迁移；hook 触发闭包化） ────────────

/// v2 SubagentStop 补发参数（BgCleanupGuard 取消兜底路径使用）。
///
/// 字段与 [`build_subagent_stop_v2`] 参数一一对应（C3 配对契约）：
/// abort 兜底路径下 v2 Start 已 emit 而 v2 Stop 永不 emit → 观察者中的 AGENT span
/// 悬挂，Drop 时经 child EventBus 补发；同时 v1 协议化直发（`sender` 存在时）
/// 补发 SubagentStopped——两者共用同一 v2 事件构造（发射语义单一事实源）。
pub(crate) struct BgStopEmitV2 {
    pub(crate) event_bus: Arc<EventBus>,
    pub(crate) turn_id: TurnId,
    pub(crate) parent_agent_id: Option<AgentId>,
    pub(crate) child_agent_id: AgentId,
    pub(crate) agent_name: String,
    /// v1 协议化直发目标（bg 泵；None = 无 bg 通道，仅 v2 补发）
    pub(crate) sender: Option<tokio::sync::mpsc::UnboundedSender<ExecutorEvent>>,
}

/// bg 任务同步收尾 guard（S3.2）：Drop 时（任务被 abort / panic / 正常结束）执行：
/// - `deregister_runtime`（active_agents 清理，防泄漏）
/// - 补发 v2 `SubagentStop`（若未显式 emit——正常路径 emit 后需 `disarm_stop`）
///   + v1 协议化直发 `SubagentStopped`（sender 存在时，同一事件构造）
pub(crate) struct BgCleanupGuard {
    pub(crate) thread_id: String,
    pub(crate) deregister: Option<DeregisterRuntimeFn>,
    /// 未显式 emit v2 SubagentStop 时补发（取消/abort 兜底路径）
    pub(crate) stop: Option<BgStopEmitV2>,
}

impl BgCleanupGuard {
    /// 正常路径已显式 emit v2 SubagentStop + v1 协议化直发后调用，
    /// 防止 drop 时重复发射。
    pub(crate) fn disarm_stop(&mut self) {
        self.stop = None;
    }
}

impl Drop for BgCleanupGuard {
    fn drop(&mut self) {
        if let Some(ref deregister) = self.deregister {
            deregister(&self.thread_id);
        }
        if let Some(stop) = &self.stop {
            // 单一 v2 事件构造：v2 发射（parent 身份存在时）+ v1 协议化直发
            // （sender 存在时）。ObserveEvent 身份透传：child_agent_id → instance_id。
            let ev = build_subagent_stop_v2(
                stop.turn_id,
                stop.parent_agent_id,
                stop.child_agent_id,
                &stop.agent_name,
                "Background sub-agent was cancelled",
                true,
            );
            if stop.parent_agent_id.is_some() {
                stop.event_bus.emit_observe(ev.clone());
            }
            if let Some(sender) = &stop.sender {
                if let Some(exec_ev) = observe_event_to_executor(ev) {
                    let _ = sender.send(exec_ev);
                }
            }
        }
    }
}

// ─── 工具函数（自 tool/mod.rs / mod.rs 迁移） ──────────────────────────────

/// 从 session transcript 提取最后一条非空 AI 消息文本（P1-11: 各执行路径共用）。
pub fn extract_last_ai_text(session: &Arc<Session>) -> String {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .rev()
        .find_map(|m| {
            if matches!(m, BaseMessage::Ai { .. }) {
                let t = m.content();
                let trimmed = t.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// 从 session transcript 统计 subagent 实际执行的工具调用次数。
///
/// 遍历 `visible_messages()` 中所有 `BaseMessage::Tool` 条目——每条对应一次
/// 工具执行（含成功和失败）。
pub fn count_tool_calls_from_session(session: &Arc<Session>) -> usize {
    let transcript = session.transcript();
    let tx = transcript.read();
    tx.visible_messages()
        .iter()
        .filter(|m| matches!(m, BaseMessage::Tool { .. }))
        .count()
}

/// Format sub-agent execution result as a summary string returned to the parent agent.
pub fn format_subagent_result(output: &AgentOutput) -> String {
    if output.tool_calls.is_empty() {
        return output.text.clone();
    }

    let mut tool_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (call, _) in &output.tool_calls {
        *tool_counts.entry(call.name.as_str()).or_insert(0) += 1;
    }

    let mut tools: Vec<_> = tool_counts.into_iter().collect();
    tools.sort_by_key(|b| std::cmp::Reverse(b.1));

    let tool_summary = tools
        .into_iter()
        .map(|(name, count)| format!("{} {} times", name, count))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "[Sub-agent executed {} tool calls: {}]\n\n{}",
        output.tool_calls.len(),
        tool_summary,
        output.text
    )
}

// ─── Fork 指令模板（自 fork.rs 迁移，纯字符串函数） ────────────────────────

/// Build fork directive message for fork mode.
pub fn build_fork_directive(prompt: &str) -> String {
    format!(
        "<fork_directive>\n\
         You are a forked agent continuing from the parent conversation.\n\
         You have full access to the conversation history above.\n\
         \n\
         RULES:\n\
         1. Do NOT spawn sub-agents — execute directly using your tools\n\
         2. Do NOT ask questions — act on the directive below\n\
         3. Stay strictly within your assigned scope\n\
         4. Report structured facts, then stop\n\
         5. Keep your response under 500 words unless specified otherwise\n\
         \n\
         Output format:\n\
           Scope: <your assigned scope in one sentence>\n\
           Result: <the answer or key findings>\n\
           Key files: <relevant file paths>\n\
           Files changed: <list if you modified files>\n\
         </fork_directive>\n\n\
         {prompt}"
    )
}

/// 构建 Prediction 指令模板（英文固定文案）。
/// 用于 agent 完成后预测用户下一步输入。
///
/// `current_title` 为会话当前标题（`None` 表示尚无标题）。注入后模型才能判断
/// 现有标题是否需要更新——不传则模型无从得知标题现状，会默认不输出 title 标记。
pub fn build_prediction_directive(current_title: Option<&str>) -> String {
    // 防御性 XML 注入防护（标题可能含闭合标签文本）
    let title_ctx = match current_title {
        Some(t) => {
            let sanitized = t.replace("</prediction_directive>", "<\u{200b}/prediction_directive>");
            format!("Current conversation title: \"{sanitized}\"")
        }
        None => "Current conversation title: (none)".to_string(),
    };
    format!(
        "<prediction_directive>\n\
         You are an input prediction assistant. Based on the conversation context, predict what the user is most likely to type next,\n\
         and keep the conversation metadata up to date.\n\
         \n\
         {title_ctx}\n\
         \n\
         Rules:\n\
         1. By default, output one predicted input as the placeholder text, without explanation\n\
         2. Write the prediction naturally in the user's language, as the user would type it\n\
         3. Do not add quotation marks, prefixes, or formatting\n\
         4. Keep it between 5 and 30 characters\n\
         5. If you cannot make a reasonable prediction, output an empty string\n\
         \n\
         Structured markers (emit only when the information is useful; you may emit multiple markers):\n\
         - <peri:title>new title</peri:title>: when the title is missing, stale, or no longer matches the current task, update it to a concise title for the current task; update it immediately when the topic changes\n\
         - <peri:tag>tag</peri:tag>: add one tag when a clear topic is detected, such as bugfix or refactor\n\
         - <peri:summary>one-sentence summary</peri:summary>: write a short one-sentence summary of the entire conversation\n\
         Example: Continue investigating the memory leak <peri:title>Investigate memory leak</peri:title><peri:tag>bugfix</peri:tag>\n\
         Example (topic changed, so update the title immediately): <peri:title>Performance optimization</peri:title>\n\
         </prediction_directive>"
    )
}

#[cfg(test)]
#[path = "subagent_test.rs"]
mod tests;
