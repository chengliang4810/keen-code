//! 项目级扩展候选的原子构建、MCP 预连接与 Runtime 贡献器。

use super::agent_catalog::{AgentCatalog, AgentCatalogEntry, AgentTools, build_agent_catalog};
use super::*;
use crate::agent_runtime::{
    AgentRuntime, RuntimeAgentTemplate, RuntimeAgentTemplateContext, RuntimeExtensionCandidate,
    RuntimeExtensionContributor, RuntimeExtensionDiagnostic, RuntimeMcpServerSnapshot,
    RuntimeToolContext,
};
use keencode_agent::{
    AgentHook, HookCallbackError, HookContextAddition, HookFuture, HookLimits, HookPhase,
    HookRegistry, HookRuntime, PlanGuard, PostToolUseContext, PostToolUseFailureContext,
    PreToolUseAction, PreToolUseContext, PreToolUseOutput, StopHookAction, StopHookContext,
    StopHookOutput, ToolEffect, ToolHookOutput, ToolRegistry,
};
use keencode_mcp::McpClientOptions;
use keencode_tools::{
    BoundedCommandError, BoundedCommandRequest, DeferredToolCatalog, LspDiagnostic, LspRuntime,
    LspServerConfig, McpDiagnosticCode, McpToolBuildReport, McpToolDiagnostic, SkillTool,
    prepare_mcp_server_tools, register_deferred_tools, register_lsp_tool, run_bounded_command,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;

/// 同一项目已经成功发布的候选指纹与代次。
#[derive(Debug, Default)]
pub(super) struct ProjectRuntimeCache {
    /// 最近成功发布的完整输入指纹。
    fingerprint: Option<String>,
    /// 与指纹同时成功发布的 Runtime 候选代次。
    generation: Option<u64>,
}

/// 构建期间允许检测外部配置变化并重新开始的最大次数。
const MAX_STALE_BUILD_RETRIES: usize = 3;
/// 单个 Hook 命令允许产生的标准输出或错误输出字节数。
const MAX_HOOK_OUTPUT_BYTES: usize = 1024 * 1024;
/// Hook 命令自身的硬超时，短于 Agent Hook 外层超时以便主动清理子进程。
const HOOK_COMMAND_TIMEOUT: Duration = Duration::from_secs(25);
/// 元数据发现有总时限，MCP 尚未授权时不得等待浏览器交互或无限阻塞 Session。
const MCP_OAUTH_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
/// 单个 OAuth 发现文档的最大响应大小。
const MCP_OAUTH_METADATA_BYTES: usize = 64 * 1024;
/// 已经同步冻结且可进入异步 MCP 准备阶段的完整输入。
struct PreparedExtensionInputs {
    /// 该候选唯一适用的规范项目根。
    project_root: PathBuf,
    /// 所有影响工具、Hook、Skill 与 Agent 的输入摘要。
    fingerprint: String,
    /// 当前项目的安全 Skill 目录。
    skills: Arc<keencode_skills::SkillCatalog>,
    /// 当前项目完成优先级归约的 Agent 定义。
    agents: AgentCatalog,
    /// 当前项目启用插件的 Slash command 目录。
    commands: Arc<crate::plugins::PluginCommandCatalog>,
    /// 已合并并转换的 MCP Server 配置。
    mcp_servers: Vec<RuntimeMcpServer>,
    /// 插件 MCP 配置转换期间收集的安全诊断。
    diagnostics: Vec<RuntimeExtensionDiagnostic>,
    /// 用户 MCP 配置是否损坏；为 true 时本次构建必须先保持 fail-closed。
    mcp_config_invalid: bool,
    /// 已通过结构检查的 Hook 声明。
    hooks: Vec<HookSpec>,
    /// 已完成静态插值且启用的原生 LSP Server 配置。
    lsp_servers: Vec<LspServerConfig>,
}

/// 当前项目完整扩展能力的不可变贡献器。
struct NativeExtensionContributor {
    /// 该贡献器唯一适用的规范项目根。
    project_root: PathBuf,
    /// 按需读取正文的 Skill 目录。
    skills: Arc<keencode_skills::SkillCatalog>,
    /// MCP 工具非空时使用的冻结延迟目录。
    deferred_tools: Option<Arc<DeferredToolCatalog>>,
    /// 当前候选构建阶段得到的 MCP Server 运行态快照。
    mcp_servers: Vec<RuntimeMcpServerSnapshot>,
    /// 每个 Turn 重新实例化的 Hook 规范。
    hooks: Vec<HookSpec>,
    /// 已冻结的 Agent 模板目录。
    agents: AgentCatalog,
    /// 已冻结的插件 Slash command 目录。
    commands: Arc<crate::plugins::PluginCommandCatalog>,
    /// 候选释放时自动终止进程树的项目级原生 LSP 生命周期。
    lsp_runtime: Option<Arc<LspRuntime>>,
    /// 候选构建期间收集、在首个根 Turn 中送达 ACP 的安全诊断。
    diagnostics: Vec<RuntimeExtensionDiagnostic>,
}

/// 插件 Hook 声明归一化后的生命周期实现。
#[derive(Clone, Debug)]
enum HookSpec {
    /// 不执行进程、只产生声明式动作与上下文的 Hook。
    Context(ContextHookSpec),
    /// 受 Plan 和输出限制保护的命令 Hook。
    Command(CommandHookSpec),
}

impl HookSpec {
    /// 返回当前 Hook 的稳定唯一名称。
    fn name(&self) -> &str {
        match self {
            Self::Context(spec) => &spec.name,
            Self::Command(spec) => &spec.name,
        }
    }
}

/// 声明式上下文 Hook 的冻结配置。
#[derive(Clone, Debug)]
struct ContextHookSpec {
    /// 当前候选内唯一且可安全进入日志的名称。
    name: String,
    /// Hook 参与的唯一生命周期阶段。
    phase: HookPhase,
    /// 工具阶段使用的可选名称匹配表达式。
    matcher: Option<String>,
    /// 追加到下一模型轮的静态上下文。
    context: Option<String>,
    /// PreToolUse 使用的可选阻止原因。
    block_message: Option<String>,
    /// PreToolUse 使用的可选完整替换输入。
    modified_input: Option<Value>,
    /// Stop 阶段是否要求继续模型循环。
    continue_turn: bool,
}

/// 命令 Hook 的冻结配置。
#[derive(Clone, Debug)]
struct CommandHookSpec {
    /// 当前候选内唯一且可安全进入日志的名称。
    name: String,
    /// Hook 参与的唯一生命周期阶段。
    phase: HookPhase,
    /// 工具阶段使用的可选名称匹配表达式。
    matcher: Option<String>,
    /// 通过系统 shell 执行的非空命令。
    command: String,
    /// 插件根工作目录。
    current_dir: PathBuf,
}

/// 已绑定单个 Turn 计划守卫的命令 Hook。
struct NativeCommandHook {
    /// 不再引用可变插件状态的命令声明。
    spec: CommandHookSpec,
    /// 当前 Turn 生效的计划只读守卫。
    plan: PlanGuard,
}

/// 已绑定声明式配置的上下文 Hook。
struct NativeContextHook {
    /// 不再引用可变插件状态的声明。
    spec: ContextHookSpec,
}

impl RuntimeExtensionContributor for NativeExtensionContributor {
    /// 注册 Skill 入口与预连接 MCP 的延迟搜索/执行入口。
    fn register_tools(
        &self,
        registry: &mut ToolRegistry,
        context: &RuntimeToolContext,
    ) -> Result<(), String> {
        self.validate_project(context)?;
        if !self.skills.entries().is_empty() {
            registry
                .register(Arc::new(SkillTool::new(Arc::clone(&self.skills))))
                .map_err(|error| format!("注册 Skill 工具失败：{error}"))?;
        }
        if !self.commands.is_empty() {
            registry
                .register(Arc::new(crate::plugins::PluginCommandTool::new(
                    Arc::clone(&self.commands),
                )))
                .map_err(|error| format!("注册插件 command 工具失败：{error}"))?;
        }
        if let Some(catalog) = &self.deferred_tools {
            register_deferred_tools(registry, Arc::clone(catalog))
                .map_err(|error| format!("注册 MCP 延迟工具失败：{error}"))?;
        }
        if let Some(runtime) = &self.lsp_runtime {
            register_lsp_tool(registry, Arc::clone(runtime))
                .map_err(|error| format!("注册 LSP 工具失败：{error}"))?;
        }
        Ok(())
    }

    /// 为当前 Turn 构建独立 Hook Registry，并冻结计划只读守卫。
    fn build_hook_runtime(&self, context: &RuntimeToolContext) -> Result<HookRuntime, String> {
        self.validate_project(context)?;
        let plan = context.plan_guard();
        let mut registry = HookRegistry::new();
        for spec in &self.hooks {
            let hook: Arc<dyn AgentHook> = match spec {
                HookSpec::Context(spec) => Arc::new(NativeContextHook { spec: spec.clone() }),
                HookSpec::Command(spec) => Arc::new(NativeCommandHook {
                    spec: spec.clone(),
                    plan,
                }),
            };
            registry
                .register(hook)
                .map_err(|error| format!("注册 Hook {} 失败：{error}", spec.name()))?;
        }
        HookRuntime::new(registry, HookLimits::default())
            .map_err(|error| format!("构建 Hook Runtime 失败：{error}"))
    }

    /// 验证当前 Session 项目与已经冻结的 LSP 生命周期属于同一项目。
    fn prepare_lsp_runtime(&self, context: &RuntimeToolContext) -> Result<(), String> {
        self.validate_project(context)?;
        if let Some(runtime) = &self.lsp_runtime
            && runtime.project_root() != self.project_root
        {
            return Err("LSP Runtime 与扩展候选项目根不一致".to_owned());
        }
        Ok(())
    }

    /// 返回候选构建时已经清理过的 MCP/LSP 诊断快照。
    fn diagnostics(&self) -> &[RuntimeExtensionDiagnostic] {
        &self.diagnostics
    }

    /// 返回候选构建时已经完成连接和工具发现的 MCP 运行态。
    fn mcp_runtime_snapshot(&self) -> Vec<RuntimeMcpServerSnapshot> {
        self.mcp_servers.clone()
    }

    /// 清空当前候选共享的延迟 MCP 目录，使已经开始的 Turn 也无法再解析旧工具。
    fn revoke_mcp_tools(&self) -> Result<(), String> {
        let Some(catalog) = &self.deferred_tools else {
            return Ok(());
        };
        catalog
            .replace_all(Vec::new())
            .map(|_| ())
            .map_err(|error| format!("撤销 MCP 工具目录失败：{error}"))
    }

    /// 从当前项目冻结目录解析一个 Agent 模板。
    fn resolve_agent(
        &self,
        name: &str,
        _parent: &RuntimeAgentTemplateContext,
    ) -> Result<Option<RuntimeAgentTemplate>, String> {
        let Some(entry) = self.agents.get(name) else {
            return Ok(None);
        };
        let tool_names = match &entry.document.tools {
            AgentTools::Inherit => None,
            AgentTools::None => Some(Vec::new()),
            AgentTools::List(tools) => Some(tools.clone()),
        };
        Ok(Some(RuntimeAgentTemplate {
            name: entry.name.clone(),
            system_prompt: entry.document.system_prompt.clone(),
            model: entry.document.model.clone(),
            tool_names,
            disallowed_tool_names: entry.document.disallowed_tools.clone(),
            max_turns: entry.document.max_turns,
            allowed_write_dirs: entry
                .document
                .allowed_write_dirs
                .iter()
                .map(PathBuf::from)
                .collect(),
        }))
    }
}

impl NativeExtensionContributor {
    /// 拒绝把某个项目构建的候选用于其他项目 Session。
    fn validate_project(&self, context: &RuntimeToolContext) -> Result<(), String> {
        if context.project_root() != self.project_root {
            return Err("扩展候选与当前 Session 项目根不一致".to_owned());
        }
        Ok(())
    }
}

/// 单飞确保指定项目拥有最新完整候选；构建失败时保留旧代次。
pub(crate) async fn ensure_runtime_extension_candidate(
    app: &AppHandle,
    project_root: &Path,
    runtime: &Arc<AgentRuntime>,
    force: bool,
) -> Result<u64, String> {
    let project_root = canonical_project_root(project_root)?;
    let state = app
        .try_state::<ExtensionsState>()
        .ok_or_else(|| "扩展状态尚未初始化".to_owned())?;
    let project_lock = state.project_runtime_lock(&project_root)?;
    let mut cache = project_lock.lock().await;
    for attempt in 0..=MAX_STALE_BUILD_RETRIES {
        let inputs = {
            let _guard = state.lock_io()?;
            prepare_extension_inputs(app, &project_root)?
        };
        let current_generation = runtime
            .extension_generation(&project_root)
            .map_err(|error| format!("读取扩展候选代次失败：{error}"))?;
        if !force
            && cache.fingerprint.as_deref() == Some(inputs.fingerprint.as_str())
            && cache.generation == current_generation
            && !runtime
                .extension_candidate_needs_refresh(&project_root)
                .map_err(|_| "无法读取扩展候选失效状态".to_owned())?
            && let Some(generation) = current_generation
        {
            return Ok(generation);
        }
        let fingerprint = inputs.fingerprint.clone();
        if inputs.mcp_config_invalid {
            runtime
                .revoke_mcp_extension_tools()
                .map_err(|error| format!("撤销无效 MCP 配置对应的旧工具失败：{error}"))?;
        }
        let oauth = app
            .try_state::<Arc<crate::mcp_oauth::McpOAuthRegistry>>()
            .ok_or_else(|| "MCP OAuth 服务尚未初始化".to_owned())?
            .inner()
            .clone();
        let previous_servers = runtime
            .mcp_runtime_snapshot(&project_root)
            .map_err(|_| "无法读取待停用 OAuth 的旧项目快照".to_owned())?
            .unwrap_or_default();
        for server_name in removed_oauth_servers(&previous_servers, &inputs.mcp_servers) {
            // 停用只关闭进程内绑定与 pending，不删除用户已保存的凭据；再次启用
            // 必须重新发现并匹配原签发方后才允许恢复这些凭据。
            oauth
                .deactivate(&project_root, server_name)
                .await
                .map_err(|_| "无法安全停用旧 MCP OAuth 绑定".to_owned())?;
        }
        let contributor = build_contributor(inputs, oauth).await?;
        let current_fingerprint = {
            let _guard = state.lock_io()?;
            prepare_extension_inputs(app, &project_root)?.fingerprint
        };
        if current_fingerprint != fingerprint {
            if attempt == MAX_STALE_BUILD_RETRIES {
                return Err("扩展配置持续变化，未发布不一致候选".to_owned());
            }
            continue;
        }
        let generation = state.reserve_runtime_generation()?;
        let candidate = RuntimeExtensionCandidate::new(generation, Arc::new(contributor))
            .map_err(|error| format!("创建扩展候选失败：{error}"))?;
        let published = runtime
            .publish_extension_candidate(&project_root, candidate)
            .map_err(|error| format!("发布扩展候选失败：{error}"))?;
        cache.fingerprint = Some(fingerprint);
        cache.generation = Some(published);
        return Ok(published);
    }
    Err("扩展候选构建未收敛".to_owned())
}

/// 找出配置已移除、禁用或取消 OAuth 的旧 Server，不影响其他仍启用的绑定。
fn removed_oauth_servers<'a>(
    previous: &'a [RuntimeMcpServerSnapshot],
    current: &[RuntimeMcpServer],
) -> Vec<&'a str> {
    previous
        .iter()
        .filter(|server| {
            server.oauth_status != keencode_acp::McpOAuthStatus::NotRequired
                && !current
                    .iter()
                    .any(|candidate| candidate.id == server.name && candidate.oauth.is_some())
        })
        .map(|server| server.name.as_str())
        .collect()
}

/// 同步冻结所有本地文件输入，并计算不泄露正文的完整摘要。
fn prepare_extension_inputs(
    app: &AppHandle,
    project_root: &Path,
) -> Result<PreparedExtensionInputs, String> {
    let data_root =
        crate::storage::root_dir(app).map_err(|error| format!("无法确定扩展数据目录：{error}"))?;
    let plugins = plugin_runtime_snapshot(app, project_root)?;
    let lsp_servers = runtime_lsp_servers(&plugins);
    let skill_config = runtime_skill_config_from_snapshot(
        data_root.clone(),
        project_root.to_path_buf(),
        plugins.clone(),
    );
    let skills = Arc::new(
        keencode_skills::discover_skills(&skill_config)
            .map_err(|error| format!("无法建立 Skill 目录：{error}"))?,
    );
    let overrides = read_agent_model_overrides(app)?;
    let agents = build_agent_catalog(&data_root, project_root, &plugins, &overrides)?;
    let commands = Arc::new(
        crate::plugins::PluginCommandCatalog::from_snapshot(&plugins)
            .map_err(|error| format!("无法建立插件 command 目录：{error}"))?,
    );
    let hooks = parse_plugin_hooks(&plugins)?;
    let user_path = mcp_user_config_path(app)?;
    let (mcp_document, user_diagnostic) = match load_mcp_document(&user_path) {
        Ok(document) => (document.unwrap_or_else(empty_mcp_document), None),
        Err(error) => {
            tracing::warn!(
                path = %user_path.display(),
                %error,
                "用户 MCP 配置无效，本次扩展候选使用空用户配置"
            );
            (
                empty_mcp_document(),
                Some(RuntimeExtensionDiagnostic {
                    source: "mcp".to_owned(),
                    server: "<user-config>".to_owned(),
                    code: "mcp_user_config_invalid".to_owned(),
                    message: "用户 MCP 配置无效，已禁用本次运行中的用户 MCP Server".to_owned(),
                    tool: None,
                }),
            )
        }
    };
    let mcp_config_invalid = user_diagnostic.is_some();
    let (mcp_servers, mut diagnostics) =
        runtime_mcp_servers_from_sources(&mcp_document, plugins.clone(), project_root)?;
    if let Some(diagnostic) = user_diagnostic {
        diagnostics.push(diagnostic);
    }
    let fingerprint = extension_fingerprint(
        project_root,
        &data_root,
        &mcp_document,
        &plugins,
        &skills,
        &agents,
        &hooks,
    )?;
    Ok(PreparedExtensionInputs {
        project_root: project_root.to_path_buf(),
        fingerprint,
        skills,
        agents,
        commands,
        mcp_servers,
        diagnostics,
        mcp_config_invalid,
        hooks,
        lsp_servers,
    })
}

/// 异步连接全部 MCP Server，成功后构造不可变贡献器。
async fn build_contributor(
    inputs: PreparedExtensionInputs,
    oauth: Arc<crate::mcp_oauth::McpOAuthRegistry>,
) -> Result<NativeExtensionContributor, String> {
    let initial_diagnostics = inputs.diagnostics;
    let (deferred_tools, tool_diagnostics, mcp_servers) =
        prepare_mcp_tools(inputs.mcp_servers, &inputs.project_root, oauth).await?;
    let mut diagnostics = initial_diagnostics;
    diagnostics.extend(tool_diagnostics);
    let lsp_runtime = if inputs.lsp_servers.is_empty() {
        None
    } else {
        let (runtime, report) =
            LspRuntime::new_best_effort(&inputs.project_root, inputs.lsp_servers)
                .map_err(|error| format!("构建原生 LSP Runtime 失败：{error}"))?;
        log_lsp_diagnostics(&report);
        diagnostics.extend(report.diagnostics().iter().map(lsp_runtime_diagnostic));
        if runtime.is_empty() {
            None
        } else {
            let startup_report = runtime.start_available().await;
            log_lsp_diagnostics(&startup_report);
            diagnostics.extend(
                startup_report
                    .diagnostics()
                    .iter()
                    .map(lsp_runtime_diagnostic),
            );
            Some(Arc::new(runtime))
        }
    };
    sort_extension_diagnostics(&mut diagnostics);
    Ok(NativeExtensionContributor {
        project_root: inputs.project_root,
        skills: inputs.skills,
        deferred_tools,
        mcp_servers,
        hooks: inputs.hooks,
        agents: inputs.agents,
        commands: inputs.commands,
        lsp_runtime,
        diagnostics,
    })
}

/// 并行预连接 MCP、冻结 tools/list，并构造全新延迟工具目录。
async fn prepare_mcp_tools(
    servers: Vec<RuntimeMcpServer>,
    project_root: &Path,
    oauth: Arc<crate::mcp_oauth::McpOAuthRegistry>,
) -> Result<
    (
        Option<Arc<DeferredToolCatalog>>,
        Vec<RuntimeExtensionDiagnostic>,
        Vec<RuntimeMcpServerSnapshot>,
    ),
    String,
> {
    if servers.is_empty() {
        return Ok((None, Vec::new(), Vec::new()));
    }
    let mut tasks = JoinSet::new();
    // JoinError 不会携带任务闭包的返回值，因此按 Tokio Task ID 保存
    // Server 身份，确保异常退出也能生成对应的运行态快照。
    let mut task_servers = HashMap::new();
    for mut server in servers {
        let id = server.id.clone();
        let transport = mcp_transport_kind(&server.config);
        let task_server_id = id.clone();
        let task_transport = transport;
        let project_root = project_root.to_path_buf();
        let oauth = Arc::clone(&oauth);
        let configured_oauth = server.oauth.is_some();
        let task = tasks.spawn(async move {
            let oauth_status = match prepare_mcp_oauth(&oauth, &project_root, &mut server).await {
                Ok(status) => status,
                Err(()) => {
                    let status = oauth
                        .status(&project_root, &id)
                        .await
                        .map(|snapshot| mcp_oauth_status(snapshot.status))
                        .unwrap_or(keencode_acp::McpOAuthStatus::Idle);
                    return (
                        id,
                        transport,
                        None,
                        status,
                        Some("MCP OAuth 准备失败，请检查授权配置后重试".to_owned()),
                    );
                }
            };
            if !matches!(
                oauth_status,
                keencode_acp::McpOAuthStatus::NotRequired
                    | keencode_acp::McpOAuthStatus::Authorized
            ) {
                return (id, transport, None, oauth_status, None);
            }
            let report =
                prepare_mcp_server_tools(id.clone(), server.config, McpClientOptions::default())
                    .await;
            (id, transport, Some(report), oauth_status, None)
        });
        task_servers.insert(
            task.id(),
            (task_server_id, task_transport, configured_oauth),
        );
    }
    let mut prepared = Vec::new();
    let mut diagnostics = Vec::new();
    let mut snapshots = Vec::new();
    while let Some(result) = tasks.join_next_with_id().await {
        match result {
            Ok((task_id, (id, transport, report, oauth_status, error))) => {
                task_servers.remove(&task_id);
                let Some(report) = report else {
                    snapshots.push(RuntimeMcpServerSnapshot {
                        name: id,
                        transport,
                        connection_status: if error.is_some() {
                            keencode_acp::McpConnectionStatus::Failed
                        } else {
                            keencode_acp::McpConnectionStatus::Disconnected
                        },
                        tools_count: 0,
                        oauth_status,
                        error,
                    });
                    continue;
                };
                log_mcp_diagnostics(&report);
                diagnostics.extend(report.diagnostics().iter().map(mcp_runtime_diagnostic));
                let mut snapshot = mcp_runtime_snapshot(&id, transport, &report);
                snapshot.oauth_status = oauth_status;
                snapshots.push(snapshot);
                prepared.push((id, report.into_tools()));
            }
            Err(error) => {
                let task_id = error.id();
                let (id, transport, configured_oauth) =
                    task_servers.remove(&task_id).unwrap_or_else(|| {
                        tracing::error!(
                            target: "extensions.mcp",
                            %error,
                            "MCP Server 初始化任务缺少身份映射"
                        );
                        (
                            "<unknown>".to_owned(),
                            keencode_acp::McpTransportKind::Stdio,
                            false,
                        )
                    });
                let message = "MCP Server 初始化任务异常退出，已跳过该 Server";
                tracing::warn!(target: "extensions.mcp", server = %id, %error, "{message}");
                diagnostics.push(RuntimeExtensionDiagnostic {
                    source: "mcp".to_owned(),
                    server: id.clone(),
                    code: "mcp_initialization_task_failed".to_owned(),
                    message: message.to_owned(),
                    tool: None,
                });
                snapshots.push(RuntimeMcpServerSnapshot {
                    name: id,
                    transport,
                    connection_status: keencode_acp::McpConnectionStatus::Failed,
                    tools_count: 0,
                    // 任务异常不能伪装成无需认证，更不能报告已授权。
                    oauth_status: if configured_oauth {
                        keencode_acp::McpOAuthStatus::Idle
                    } else {
                        keencode_acp::McpOAuthStatus::NotRequired
                    },
                    error: Some(message.to_owned()),
                });
            }
        }
    }
    prepared.sort_by(|left, right| left.0.cmp(&right.0));
    let tools = prepared
        .into_iter()
        .flat_map(|(_, tools)| tools)
        .collect::<Vec<_>>();
    if tools.is_empty() {
        snapshots.sort_by(|left, right| left.name.cmp(&right.name));
        return Ok((None, diagnostics, snapshots));
    }
    let catalog = Arc::new(DeferredToolCatalog::new());
    catalog
        .replace_all(tools)
        .map_err(|error| format!("冻结 MCP 工具目录失败：{error}"))?;
    snapshots.sort_by(|left, right| left.name.cmp(&right.name));
    Ok((Some(catalog), diagnostics, snapshots))
}

/// 为 HTTP MCP 绑定动态认证；没有有效令牌时只展示授权状态，不进行工具发现。
async fn prepare_mcp_oauth(
    registry: &crate::mcp_oauth::McpOAuthRegistry,
    project_root: &Path,
    server: &mut RuntimeMcpServer,
) -> Result<keencode_acp::McpOAuthStatus, ()> {
    let Some(settings) = server.oauth.clone() else {
        return Ok(keencode_acp::McpOAuthStatus::NotRequired);
    };
    let keencode_mcp::McpServerConfig::StreamableHttp(config) = &mut server.config else {
        return Err(());
    };
    registry
        .register(project_root, server.id.clone(), settings.clone())
        .await
        .map_err(|_| ())?;
    configure_mcp_oauth(registry, project_root, &server.id, &settings).await?;
    let provider = registry
        .auth_provider(project_root, &server.id)
        .map_err(|_| ())?;
    // 主动读取一次认证状态，令牌到期时由 Registry single-flight 刷新；不得把
    // 仅有持久化记录但已到期的令牌当作可调用的 Authorized 状态。
    let has_token = provider.access_token().await.map_err(|_| ())?.is_some();
    config.auth_provider = Some(provider);
    let status = registry
        .status(project_root, &server.id)
        .await
        .map_err(|_| ())?;
    if has_token {
        Ok(keencode_acp::McpOAuthStatus::Authorized)
    } else {
        Ok(mcp_oauth_status(status.status))
    }
}

/// 发现并核验签发方端点；占位回调不打开端口，真实随机回环端口由 start 分配。
async fn configure_mcp_oauth(
    registry: &crate::mcp_oauth::McpOAuthRegistry,
    project_root: &Path,
    server_name: &str,
    settings: &crate::mcp_oauth::McpOAuthSettings,
) -> Result<(), ()> {
    tokio::time::timeout(MCP_OAUTH_DISCOVERY_TIMEOUT, async {
        let fetcher = keencode_mcp::ReqwestOAuthMetadataFetcher::new(
            MCP_OAUTH_DISCOVERY_TIMEOUT,
            MCP_OAUTH_METADATA_BYTES,
        )
        .map_err(|_| ())?;
        let mut config = keencode_mcp::discover_oauth_config(
            &fetcher,
            &settings.resource,
            &settings.client_id,
            "http://127.0.0.1/oauth/callback",
            None,
        )
        .await
        .map_err(|_| ())?;
        if !settings.scopes.is_empty() {
            config.scopes = settings.scopes.clone();
        }
        registry
            .configure(project_root, server_name, config)
            .await
            .map_err(|_| ())
    })
    .await
    .map_err(|_| ())?
}

/// 将 OAuth 核心的明确生命周期逐项映射到 ACP，不用字符串推测或兼容别名。
pub(crate) fn mcp_oauth_status(status: keencode_mcp::OAuthStatus) -> keencode_acp::McpOAuthStatus {
    use keencode_acp::McpOAuthStatus as Acp;
    use keencode_mcp::OAuthStatus as Core;
    match status {
        Core::Idle => Acp::Idle,
        Core::AwaitingAuthorization => Acp::AwaitingAuthorization,
        Core::ExchangingCode => Acp::ExchangingCode,
        Core::Authorized => Acp::Authorized,
        Core::Refreshing => Acp::Refreshing,
        Core::Denied => Acp::Denied,
        Core::Expired => Acp::Expired,
    }
}

/// 将 MCP 配置的 Provider 中立传输映射为 ACP 传输枚举。
fn mcp_transport_kind(config: &keencode_mcp::McpServerConfig) -> keencode_acp::McpTransportKind {
    match config {
        keencode_mcp::McpServerConfig::Stdio(_) => keencode_acp::McpTransportKind::Stdio,
        keencode_mcp::McpServerConfig::StreamableHttp(_) => {
            keencode_acp::McpTransportKind::StreamableHttp
        }
    }
}

/// 将一次 MCP 连接/发现报告转换为不启动连接的 Runtime 状态快照。
fn mcp_runtime_snapshot(
    name: &str,
    transport: keencode_acp::McpTransportKind,
    report: &McpToolBuildReport,
) -> RuntimeMcpServerSnapshot {
    let server_unavailable_error = report
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code == McpDiagnosticCode::ServerUnavailable)
        .map(|diagnostic| diagnostic.message.as_str());
    let tool_discovery_error = report
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.code == McpDiagnosticCode::ToolDiscoveryFailed)
        .map(|diagnostic| diagnostic.message.as_str());
    let (connection_status, error) = mcp_runtime_connection_state(
        report.tool_count(),
        server_unavailable_error,
        tool_discovery_error,
    );
    RuntimeMcpServerSnapshot {
        name: name.to_owned(),
        transport,
        connection_status,
        // 工具数量是当前延迟目录中实际可调用入口数，包含 MCP resources 的只读包装器；不代表远端原始 tools 数。
        tools_count: u32::try_from(report.tool_count()).unwrap_or(u32::MAX),
        // 传输报告不包含凭据事实；调用方仅用 Registry 的实际状态覆盖此字段。
        oauth_status: keencode_acp::McpOAuthStatus::NotRequired,
        error,
    }
}

/// 根据可调用入口数和关键诊断映射 Runtime 连接状态，保留工具发现失败说明。
fn mcp_runtime_connection_state(
    tool_count: usize,
    server_unavailable_error: Option<&str>,
    tool_discovery_error: Option<&str>,
) -> (keencode_acp::McpConnectionStatus, Option<String>) {
    if let Some(error) = server_unavailable_error {
        return (
            keencode_acp::McpConnectionStatus::Failed,
            Some(error.to_owned()),
        );
    }
    if let Some(error) = tool_discovery_error {
        return (
            if tool_count > 0 {
                keencode_acp::McpConnectionStatus::Connected
            } else {
                keencode_acp::McpConnectionStatus::Failed
            },
            Some(error.to_owned()),
        );
    }
    (keencode_acp::McpConnectionStatus::Connected, None)
}

/// 把工具层 MCP 诊断复制到 Runtime 通用的安全 ACP 诊断形状。
fn mcp_runtime_diagnostic(diagnostic: &McpToolDiagnostic) -> RuntimeExtensionDiagnostic {
    RuntimeExtensionDiagnostic {
        source: "mcp".to_owned(),
        server: diagnostic.server_id.clone(),
        code: diagnostic.code.to_string(),
        message: diagnostic.message.clone(),
        tool: diagnostic.tool_name.clone(),
    }
}

/// 把工具层 LSP 诊断复制到 Runtime 通用的安全 ACP 诊断形状。
fn lsp_runtime_diagnostic(diagnostic: &LspDiagnostic) -> RuntimeExtensionDiagnostic {
    RuntimeExtensionDiagnostic {
        source: "lsp".to_owned(),
        server: diagnostic.server.clone(),
        code: diagnostic.code.to_string(),
        message: diagnostic.message.clone(),
        tool: None,
    }
}

/// 按稳定字段排序候选诊断，避免并行 MCP 初始化完成顺序改变 ACP 时间线。
fn sort_extension_diagnostics(diagnostics: &mut [RuntimeExtensionDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.server.cmp(&right.server))
            .then_with(|| left.tool.cmp(&right.tool))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
}

/// 记录 MCP best-effort 初始化诊断；诊断正文已经由工具层做有界清理。
fn log_mcp_diagnostics(report: &McpToolBuildReport) {
    for diagnostic in report.diagnostics() {
        tracing::warn!(
            target: "extensions.mcp",
            server = %diagnostic.server_id,
            tool = diagnostic.tool_name.as_deref().unwrap_or("<server>"),
            code = %diagnostic.code,
            message = %diagnostic.message,
            "MCP 扩展已降级，跳过不可用条目"
        );
    }
}

/// 记录 LSP best-effort 配置或启动诊断；不阻断其他 Server 与核心 Session。
fn log_lsp_diagnostics(report: &keencode_tools::LspPreparationReport) {
    for diagnostic in report.diagnostics() {
        tracing::warn!(
            target: "extensions.lsp",
            server = %diagnostic.server,
            code = %diagnostic.code,
            message = %diagnostic.message,
            "LSP 扩展已降级，跳过不可用 Server"
        );
    }
}

/// 将启用的插件 LSP 声明转换为不携带插件私有类型的工具层配置。
fn runtime_lsp_servers(snapshot: &PluginRuntimeSnapshot) -> Vec<LspServerConfig> {
    snapshot
        .plugins
        .iter()
        .flat_map(|plugin| plugin.lsp_servers.iter())
        .filter(|server| !server.disabled)
        .map(|server| LspServerConfig {
            name: server.name.clone(),
            command: server.command.clone(),
            args: server.args.clone(),
            current_dir: server.current_dir.clone(),
            environment: server.environment.clone(),
            extension_to_language: server.extension_to_language.clone(),
            initialization_options: server.initialization_options.clone(),
            max_restarts: server.max_restarts,
            startup_timeout_ms: server.startup_timeout_ms,
        })
        .collect()
}

/// 将插件 Hook JSON 严格转换为当前 Runtime 的声明式或命令规范。
fn parse_plugin_hooks(snapshot: &PluginRuntimeSnapshot) -> Result<Vec<HookSpec>, String> {
    let mut hooks = Vec::new();
    for plugin in &snapshot.plugins {
        let plugin_namespace = plugin
            .id
            .runtime_namespace()
            .map_err(|error| error.to_string())?;
        let Some(Value::Object(events)) = plugin.hooks.as_ref() else {
            if plugin.hooks.is_some() {
                return Err(format!("插件 {} 的 hooks 必须是对象", plugin.id));
            }
            continue;
        };
        for (event_name, groups) in events {
            let phase = parse_hook_phase(event_name)
                .ok_or_else(|| format!("插件 {} 声明了未知 Hook 事件 {event_name}", plugin.id))?;
            let groups = normalize_hook_items(groups.clone());
            for (group_index, group) in groups.into_iter().enumerate() {
                let (matcher, values) = parse_hook_group(group)?;
                for (hook_index, value) in normalize_hook_items(values).into_iter().enumerate() {
                    let name = format!(
                        "{}:{}:{}:{}",
                        plugin_namespace,
                        hook_phase_name(phase),
                        group_index,
                        hook_index
                    );
                    hooks.push(parse_hook_spec(
                        name,
                        phase,
                        matcher.clone(),
                        value,
                        &plugin.root,
                    )?);
                }
            }
        }
    }
    hooks.sort_by(|left, right| left.name().cmp(right.name()));
    Ok(hooks)
}

/// 将 Hook 事件别名归一为 Provider 中立生命周期阶段。
fn parse_hook_phase(value: &str) -> Option<HookPhase> {
    let normalized = value
        .chars()
        .filter(|character| !matches!(character, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect::<String>();
    match normalized.as_str() {
        "pretooluse" => Some(HookPhase::PreToolUse),
        "posttooluse" => Some(HookPhase::PostToolUse),
        "posttoolusefailure" => Some(HookPhase::PostToolUseFailure),
        "stop" => Some(HookPhase::Stop),
        _ => None,
    }
}

/// 返回 Hook 阶段组成稳定名称时使用的 ASCII 片段。
fn hook_phase_name(phase: HookPhase) -> &'static str {
    match phase {
        HookPhase::PreToolUse => "pre",
        HookPhase::PostToolUse => "post",
        HookPhase::PostToolUseFailure => "failure",
        HookPhase::Stop => "stop",
    }
}

/// 将单个对象或数组统一成有序 Hook 项。
fn normalize_hook_items(value: Value) -> Vec<Value> {
    match value {
        Value::Array(values) => values,
        value => vec![value],
    }
}

/// 解析一组可选 matcher 与内部 hooks 列表。
fn parse_hook_group(value: Value) -> Result<(Option<String>, Value), String> {
    let Value::Object(mut object) = value else {
        return Ok((None, value));
    };
    if !object.contains_key("hooks") {
        return Ok((None, Value::Object(object)));
    }
    let matcher = object
        .remove("matcher")
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| "Hook matcher 必须是非空字符串".to_owned())
        })
        .transpose()?;
    let hooks = object
        .remove("hooks")
        .ok_or_else(|| "Hook 组缺少 hooks".to_owned())?;
    if !object.is_empty() {
        return Err("Hook 组包含未知字段".to_owned());
    }
    Ok((matcher, hooks))
}

/// 解析单个声明式或命令 Hook。
fn parse_hook_spec(
    name: String,
    phase: HookPhase,
    matcher: Option<String>,
    value: Value,
    plugin_root: &Path,
) -> Result<HookSpec, String> {
    let mut object = match value {
        Value::String(command) => {
            return parse_command_hook(name, phase, matcher, command, plugin_root);
        }
        Value::Object(object) => object,
        _ => return Err("Hook 必须是命令字符串或对象".to_owned()),
    };
    let kind = object
        .remove("type")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or_else(|| "Hook 对象缺少 string type".to_owned())?;
    match kind.as_str() {
        "command" => {
            let command = object
                .remove("command")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .ok_or_else(|| "command Hook 缺少 string command".to_owned())?;
            if !object.is_empty() {
                return Err("command Hook 包含未知字段".to_owned());
            }
            parse_command_hook(name, phase, matcher, command, plugin_root)
        }
        "context" => parse_context_hook(name, phase, matcher, object),
        _ => Err(format!("Hook {name} 使用了未实现类型 {kind}")),
    }
}

/// 校验并创建命令 Hook 规范。
fn parse_command_hook(
    name: String,
    phase: HookPhase,
    matcher: Option<String>,
    command: String,
    plugin_root: &Path,
) -> Result<HookSpec, String> {
    if command.trim().is_empty() || command.len() > 16 * 1024 || command.contains('\0') {
        return Err(format!("Hook {name} 的 command 无效"));
    }
    if phase == HookPhase::Stop && matcher.is_some() {
        return Err(format!("Stop Hook {name} 不能声明 matcher"));
    }
    Ok(HookSpec::Command(CommandHookSpec {
        name,
        phase,
        matcher,
        command,
        current_dir: plugin_root.to_path_buf(),
    }))
}

/// 校验并创建声明式上下文 Hook 规范。
fn parse_context_hook(
    name: String,
    phase: HookPhase,
    matcher: Option<String>,
    mut object: serde_json::Map<String, Value>,
) -> Result<HookSpec, String> {
    let context = object
        .remove("context")
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| "context Hook 的 context 必须是非空字符串".to_owned())
        })
        .transpose()?;
    let block_message = object
        .remove("block")
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| "context Hook 的 block 必须是非空字符串".to_owned())
        })
        .transpose()?;
    let modified_input = object.remove("input");
    let continue_turn = object
        .remove("continue")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| "context Hook 的 continue 必须是布尔值".to_owned())
        })
        .transpose()?
        .unwrap_or(false);
    if !object.is_empty() {
        return Err(format!("context Hook {name} 包含未知字段"));
    }
    if phase != HookPhase::PreToolUse && (block_message.is_some() || modified_input.is_some()) {
        return Err(format!(
            "只有 PreToolUse Hook {name} 可以修改或阻止工具输入"
        ));
    }
    if phase != HookPhase::Stop && continue_turn {
        return Err(format!("只有 Stop Hook {name} 可以声明 continue"));
    }
    if phase == HookPhase::Stop && matcher.is_some() {
        return Err(format!("Stop Hook {name} 不能声明 matcher"));
    }
    if block_message.is_some() && modified_input.is_some() {
        return Err(format!("Hook {name} 不能同时修改并阻止工具输入"));
    }
    if continue_turn && context.is_none() {
        return Err(format!("Stop Hook {name} 要求继续时必须提供 context"));
    }
    Ok(HookSpec::Context(ContextHookSpec {
        name,
        phase,
        matcher,
        context,
        block_message,
        modified_input,
        continue_turn,
    }))
}

impl AgentHook for NativeContextHook {
    /// 返回目录构建时冻结的稳定名称。
    fn name(&self) -> &str {
        &self.spec.name
    }

    /// 在阶段和 matcher 同时匹配时应用静态输入动作与上下文。
    fn pre_tool_use(
        &self,
        context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        Box::pin(async move {
            if spec.phase != HookPhase::PreToolUse
                || !matches_tool(&spec.matcher, &context.tool_name)
            {
                return Ok(PreToolUseOutput::allow());
            }
            let action = if let Some(message) = spec.block_message {
                PreToolUseAction::Block { message }
            } else if let Some(input) = spec.modified_input {
                PreToolUseAction::ModifyInput { input }
            } else {
                PreToolUseAction::Allow
            };
            Ok(PreToolUseOutput {
                action,
                context: context_additions(spec.context),
            })
        })
    }

    /// 在成功工具阶段匹配时追加静态上下文。
    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        Box::pin(async move {
            Ok(ToolHookOutput {
                context: if spec.phase == HookPhase::PostToolUse
                    && matches_tool(&spec.matcher, &context.tool_name)
                {
                    context_additions(spec.context)
                } else {
                    Vec::new()
                },
            })
        })
    }

    /// 在失败工具阶段匹配时追加静态上下文。
    fn post_tool_use_failure(
        &self,
        context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        Box::pin(async move {
            Ok(ToolHookOutput {
                context: if spec.phase == HookPhase::PostToolUseFailure
                    && matches_tool(&spec.matcher, &context.tool_name)
                {
                    context_additions(spec.context)
                } else {
                    Vec::new()
                },
            })
        })
    }

    /// 在 Stop 阶段返回冻结的停止或继续决策。
    fn stop(
        &self,
        _context: StopHookContext,
    ) -> HookFuture<'_, Result<StopHookOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        Box::pin(async move {
            if spec.phase != HookPhase::Stop {
                return Ok(StopHookOutput::stop());
            }
            Ok(StopHookOutput {
                action: if spec.continue_turn {
                    StopHookAction::Continue
                } else {
                    StopHookAction::Stop
                },
                context: context_additions(spec.context),
            })
        })
    }
}

impl AgentHook for NativeCommandHook {
    /// 返回目录构建时冻结的稳定名称。
    fn name(&self) -> &str {
        &self.spec.name
    }

    /// 执行匹配的 PreToolUse 命令，再解析其输出动作。
    fn pre_tool_use(
        &self,
        context: PreToolUseContext,
    ) -> HookFuture<'_, Result<PreToolUseOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        let plan = self.plan;
        Box::pin(async move {
            if spec.phase != HookPhase::PreToolUse
                || !matches_tool(&spec.matcher, &context.tool_name)
            {
                return Ok(PreToolUseOutput::allow());
            }
            let payload = json!({
                "phase": "pre_tool_use",
                "sessionId": context.invocation.session_id.as_str(),
                "turnId": context.invocation.turn_id.as_str(),
                "sourceAgentId": context.invocation.source_agent_id.as_str(),
                "toolCallId": context.tool_call_id,
                "toolName": context.tool_name,
                "input": context.input,
            });
            let output = run_command_hook(&spec, plan, &payload).await?;
            parse_pre_hook_output(output)
        })
    }

    /// 执行匹配的 PostToolUse 命令，再解析追加上下文。
    fn post_tool_use(
        &self,
        context: PostToolUseContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        let plan = self.plan;
        Box::pin(async move {
            if spec.phase != HookPhase::PostToolUse
                || !matches_tool(&spec.matcher, &context.tool_name)
            {
                return Ok(ToolHookOutput::default());
            }
            let payload = json!({
                "phase": "post_tool_use",
                "sessionId": context.invocation.session_id.as_str(),
                "turnId": context.invocation.turn_id.as_str(),
                "sourceAgentId": context.invocation.source_agent_id.as_str(),
                "toolCallId": context.tool_call_id,
                "toolName": context.tool_name,
                "input": context.input,
                "result": context.result,
            });
            let output = run_command_hook(&spec, plan, &payload).await?;
            parse_tool_hook_output(output)
        })
    }

    /// 执行匹配的失败 Hook 命令，再解析追加上下文。
    fn post_tool_use_failure(
        &self,
        context: PostToolUseFailureContext,
    ) -> HookFuture<'_, Result<ToolHookOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        let plan = self.plan;
        Box::pin(async move {
            if spec.phase != HookPhase::PostToolUseFailure
                || !matches_tool(&spec.matcher, &context.tool_name)
            {
                return Ok(ToolHookOutput::default());
            }
            let payload = json!({
                "phase": "post_tool_use_failure",
                "sessionId": context.invocation.session_id.as_str(),
                "turnId": context.invocation.turn_id.as_str(),
                "sourceAgentId": context.invocation.source_agent_id.as_str(),
                "toolCallId": context.tool_call_id,
                "toolName": context.tool_name,
                "input": context.input,
                "result": context.result,
                "failure": context.failure,
            });
            let output = run_command_hook(&spec, plan, &payload).await?;
            parse_tool_hook_output(output)
        })
    }

    /// 执行 Stop Hook 命令，再解析停止或继续动作。
    fn stop(
        &self,
        context: StopHookContext,
    ) -> HookFuture<'_, Result<StopHookOutput, HookCallbackError>> {
        let spec = self.spec.clone();
        let plan = self.plan;
        Box::pin(async move {
            if spec.phase != HookPhase::Stop {
                return Ok(StopHookOutput::stop());
            }
            let payload = json!({
                "phase": "stop",
                "sessionId": context.invocation.session_id.as_str(),
                "turnId": context.invocation.turn_id.as_str(),
                "sourceAgentId": context.invocation.source_agent_id.as_str(),
                "modelRound": context.model_round,
                "stopHookRound": context.stop_hook_round,
                "response": context.response,
            });
            let output = run_command_hook(&spec, plan, &payload).await?;
            parse_stop_hook_output(output)
        })
    }
}

/// 先执行 Plan 只读守卫，再启动命令子进程。
async fn run_command_hook(
    spec: &CommandHookSpec,
    plan: PlanGuard,
    payload: &Value,
) -> Result<String, HookCallbackError> {
    plan.authorize(ToolEffect::ChangesState)
        .map_err(|_| HookCallbackError::new("hook_plan_denied", "计划模式禁止执行命令 Hook"))?;
    execute_hook_command(spec, payload).await
}

/// 执行一个有界、可超时并在 Future 丢弃时终止的系统 shell 命令。
async fn execute_hook_command(
    spec: &CommandHookSpec,
    payload: &Value,
) -> Result<String, HookCallbackError> {
    execute_hook_command_with_limits(spec, payload, HOOK_COMMAND_TIMEOUT, MAX_HOOK_OUTPUT_BYTES)
        .await
}

/// 使用明确资源边界执行 Hook；独立入口允许测试真实超时与输出超限路径。
async fn execute_hook_command_with_limits(
    spec: &CommandHookSpec,
    payload: &Value,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<String, HookCallbackError> {
    let payload = serde_json::to_vec(payload)
        .map_err(|_| HookCallbackError::new("hook_payload_invalid", "Hook 输入无法编码"))?;
    let request =
        BoundedCommandRequest::shell(&spec.command, &spec.current_dir, timeout, max_output_bytes)
            .with_stdin(payload)
            .with_environment(vec![
                (
                    OsString::from("KEENCODE_HOOK_NAME"),
                    OsString::from(&spec.name),
                ),
                (
                    OsString::from("KEENCODE_HOOK_PHASE"),
                    OsString::from(hook_phase_name(spec.phase)),
                ),
            ]);
    let output = run_bounded_command(request)
        .await
        .map_err(map_hook_command_error)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HookCallbackError::new(
            "hook_command_failed",
            if stderr.trim().is_empty() {
                "Hook 命令执行失败".to_owned()
            } else {
                format!("Hook 命令执行失败：{}", bounded_error_text(&stderr))
            },
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| HookCallbackError::new("hook_output_invalid", "Hook 输出不是有效 UTF-8"))
}

/// 将系统能力层的细分错误归一到既有 Hook 稳定错误码。
fn map_hook_command_error(error: BoundedCommandError) -> HookCallbackError {
    match error.code() {
        "command_spawn_failed"
        | "invalid_command_program"
        | "invalid_command_cwd"
        | "invalid_command_timeout"
        | "invalid_command_output_limit" => {
            HookCallbackError::new("hook_spawn_failed", "无法启动 Hook 命令")
        }
        "command_stdin_unavailable" | "command_stdin_failed" | "invalid_command_stdin_limit" => {
            HookCallbackError::new("hook_stdin_failed", "无法写入 Hook 输入")
        }
        "command_output_too_large" => {
            HookCallbackError::new("hook_output_too_large", "Hook 命令输出超过容量上限")
        }
        "command_timed_out" => HookCallbackError::new("hook_command_timeout", "Hook 命令执行超时"),
        "command_stdout_unavailable"
        | "command_stderr_unavailable"
        | "command_stdout_failed"
        | "command_stderr_failed" => {
            HookCallbackError::new("hook_output_unavailable", "无法读取 Hook 输出")
        }
        "command_wait_failed" | "command_termination_failed" => {
            HookCallbackError::new("hook_wait_failed", "无法读取 Hook 命令状态")
        }
        _ => HookCallbackError::new("hook_wait_failed", "Hook 命令监督失败"),
    }
}

/// 将命令输出解析为 PreToolUse 动作与上下文。
fn parse_pre_hook_output(output: String) -> Result<PreToolUseOutput, HookCallbackError> {
    if output.trim().is_empty() {
        return Ok(PreToolUseOutput::allow());
    }
    let value = parse_hook_output_value(&output)?;
    if let Value::String(text) = value {
        return Ok(PreToolUseOutput {
            action: PreToolUseAction::Allow,
            context: context_additions(Some(text)),
        });
    }
    let object = value.as_object().ok_or_else(|| {
        HookCallbackError::new("hook_output_invalid", "PreToolUse Hook 输出必须是对象")
    })?;
    let context = output_context(object)?;
    let action = match object
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("allow")
    {
        "allow" => PreToolUseAction::Allow,
        "block" => PreToolUseAction::Block {
            message: object
                .get("message")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    HookCallbackError::new("hook_output_invalid", "block 动作缺少 message")
                })?,
        },
        "modify" => PreToolUseAction::ModifyInput {
            input: object.get("input").cloned().ok_or_else(|| {
                HookCallbackError::new("hook_output_invalid", "modify 动作缺少 input")
            })?,
        },
        _ => {
            return Err(HookCallbackError::new(
                "hook_output_invalid",
                "PreToolUse Hook action 无效",
            ));
        }
    };
    Ok(PreToolUseOutput { action, context })
}

/// 将命令输出解析为成功或失败工具 Hook 上下文。
fn parse_tool_hook_output(output: String) -> Result<ToolHookOutput, HookCallbackError> {
    if output.trim().is_empty() {
        return Ok(ToolHookOutput::default());
    }
    let value = parse_hook_output_value(&output)?;
    let context = match value {
        Value::String(text) => context_additions(Some(text)),
        Value::Object(object) => output_context(&object)?,
        _ => {
            return Err(HookCallbackError::new(
                "hook_output_invalid",
                "工具 Hook 输出必须是字符串或对象",
            ));
        }
    };
    Ok(ToolHookOutput { context })
}

/// 将命令输出解析为 Stop Hook 决策。
fn parse_stop_hook_output(output: String) -> Result<StopHookOutput, HookCallbackError> {
    if output.trim().is_empty() {
        return Ok(StopHookOutput::stop());
    }
    let value = parse_hook_output_value(&output)?;
    let Value::Object(object) = value else {
        return Err(HookCallbackError::new(
            "hook_output_invalid",
            "Stop Hook 输出必须是对象",
        ));
    };
    let context = output_context(&object)?;
    match object
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("stop")
    {
        "stop" if context.is_empty() => Ok(StopHookOutput::stop()),
        "continue" if !context.is_empty() => Ok(StopHookOutput {
            action: StopHookAction::Continue,
            context,
        }),
        "stop" => Err(HookCallbackError::new(
            "hook_output_invalid",
            "stop 动作不能同时追加 context",
        )),
        "continue" => Err(HookCallbackError::new(
            "hook_output_invalid",
            "continue 动作必须提供 context",
        )),
        _ => Err(HookCallbackError::new(
            "hook_output_invalid",
            "Stop Hook action 无效",
        )),
    }
}

/// 空白外的非 JSON 输出按普通上下文字符串处理。
fn parse_hook_output_value(output: &str) -> Result<Value, HookCallbackError> {
    let trimmed = output.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.starts_with('"') {
        serde_json::from_str(trimmed)
            .map_err(|_| HookCallbackError::new("hook_output_invalid", "Hook 输出 JSON 无效"))
    } else {
        Ok(Value::String(trimmed.to_owned()))
    }
}

/// 从 Hook JSON 对象提取可选非空上下文。
fn output_context(
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<HookContextAddition>, HookCallbackError> {
    match object.get("context") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(text)) if !text.trim().is_empty() => {
            Ok(context_additions(Some(text.clone())))
        }
        Some(_) => Err(HookCallbackError::new(
            "hook_output_invalid",
            "Hook context 必须是非空字符串",
        )),
    }
}

/// 将可选文本转换为零个或一个 Hook 上下文项。
fn context_additions(context: Option<String>) -> Vec<HookContextAddition> {
    context.map(HookContextAddition::new).into_iter().collect()
}

/// 判断工具名称是否匹配空、星号或竖线分隔的精确表达式。
fn matches_tool(matcher: &Option<String>, tool_name: &str) -> bool {
    matcher.as_deref().is_none_or(|matcher| {
        matcher
            .split('|')
            .map(str::trim)
            .any(|candidate| candidate == "*" || candidate.eq_ignore_ascii_case(tool_name))
    })
}

/// 递归按对象键排序，确保相同 JSON 输入生成相同扩展指纹。
fn canonicalize_json(input: &Value) -> Value {
    match input {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(object) => {
            let mut sorted = object.iter().collect::<Vec<_>>();
            sorted.sort_by(|left, right| left.0.cmp(right.0));
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonicalize_json(value)))
                    .collect(),
            )
        }
        value => value.clone(),
    }
}

/// 计算完整扩展输入的稳定 SHA-256 指纹。
#[allow(clippy::too_many_arguments)]
fn extension_fingerprint(
    project_root: &Path,
    data_root: &Path,
    mcp_document: &McpDocument,
    plugins: &PluginRuntimeSnapshot,
    skills: &keencode_skills::SkillCatalog,
    agents: &AgentCatalog,
    hooks: &[HookSpec],
) -> Result<String, String> {
    let mut digest = Sha256::new();
    hash_path(&mut digest, project_root);
    hash_value(&mut digest, &mcp_document.root)?;
    for plugin in &plugins.plugins {
        digest.update(plugin.id.to_string().as_bytes());
        hash_path(&mut digest, &plugin.root);
        for file in plugin
            .commands
            .iter()
            .chain(plugin.skills.iter())
            .chain(plugin.agents.iter())
        {
            hash_path(&mut digest, &file.path);
            hash_file(&mut digest, &file.path)?;
        }
        if let Some(value) = &plugin.hooks {
            hash_value(&mut digest, value)?;
        }
        for (name, value) in &plugin.mcp_servers {
            digest.update(name.as_bytes());
            hash_value(&mut digest, value)?;
        }
        for server in &plugin.lsp_servers {
            digest.update(server.name.as_bytes());
            digest.update(server.command.as_bytes());
            hash_path(&mut digest, &server.current_dir);
            for argument in &server.args {
                digest.update(argument.as_bytes());
                digest.update([0]);
            }
            for (name, value) in &server.environment {
                digest.update(name.as_bytes());
                digest.update([0]);
                digest.update(value.as_bytes());
            }
            for (extension, language_id) in &server.extension_to_language {
                digest.update(extension.as_bytes());
                digest.update([0]);
                digest.update(language_id.as_bytes());
            }
            if let Some(options) = &server.initialization_options {
                hash_value(&mut digest, options)?;
            }
            digest.update(server.disabled.to_string().as_bytes());
            digest.update(server.max_restarts.to_le_bytes());
            digest.update(server.startup_timeout_ms.to_le_bytes());
        }
    }
    for entry in skills.entries() {
        digest.update(entry.name.as_bytes());
        digest.update([0]);
        digest.update(entry.description.as_bytes());
        digest.update([entry.source.priority()]);
    }
    hash_skill_tree(&mut digest, &data_root.join("skills"))?;
    hash_skill_tree(&mut digest, &project_root.join(".agents").join("skills"))?;
    for entry in agents.entries() {
        hash_agent_entry(&mut digest, entry);
    }
    for hook in hooks {
        digest.update(format!("{hook:?}").as_bytes());
    }
    Ok(hex_digest(&digest.finalize()))
}

/// 将一个已归约 Agent 条目的全部运行时语义写入扩展指纹。
fn hash_agent_entry(digest: &mut Sha256, entry: &AgentCatalogEntry) {
    hash_fingerprint_text(digest, &entry.name);
    hash_fingerprint_text(digest, entry.source.as_str());
    match &entry.path {
        Some(path) => {
            digest.update([1]);
            hash_path(digest, path);
        }
        None => digest.update([0]),
    }
    hash_optional_fingerprint_text(digest, entry.document.name.as_deref());
    hash_fingerprint_text(digest, &entry.document.description);
    hash_optional_fingerprint_text(digest, entry.document.model.as_deref());
    match &entry.document.tools {
        AgentTools::Inherit => digest.update([0]),
        AgentTools::None => digest.update([1]),
        AgentTools::List(tools) => {
            digest.update([2]);
            hash_fingerprint_text_list(digest, tools);
        }
    }
    hash_fingerprint_text_list(digest, &entry.document.disallowed_tools);
    match entry.document.max_turns {
        Some(max_turns) => {
            digest.update([1]);
            digest.update(max_turns.to_le_bytes());
        }
        None => digest.update([0]),
    }
    hash_fingerprint_text_list(digest, &entry.document.allowed_write_dirs);
    hash_fingerprint_text(digest, &entry.document.system_prompt);
}

/// 以长度前缀写入文本，避免相邻字段因拼接产生指纹碰撞。
fn hash_fingerprint_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value.as_bytes());
}

/// 将可选文本的存在标记与内容一并写入指纹。
fn hash_optional_fingerprint_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_fingerprint_text(digest, value);
        }
        None => digest.update([0]),
    }
}

/// 将有序文本列表的长度和每个条目写入指纹。
fn hash_fingerprint_text_list(digest: &mut Sha256, values: &[String]) {
    digest.update((values.len() as u64).to_le_bytes());
    for value in values {
        hash_fingerprint_text(digest, value);
    }
}

/// 递归哈希受 Skill 加载器约束的 SKILL.md 文件。
fn hash_skill_tree(digest: &mut Sha256, root: &Path) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("无法读取 Skill 根目录 {}：{error}", root.display())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("Skill 根路径必须是普通目录：{}", root.display()));
    }
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|error| format!("无法读取 Skill 目录 {}：{error}", directory.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法读取 Skill 目录项：{error}"))?;
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            visited += 1;
            if visited > 16 * 1024 {
                return Err("Skill 指纹扫描超过目录项上限".to_owned());
            }
            let file_type = entry
                .file_type()
                .map_err(|error| format!("无法读取 Skill 目录项类型：{error}"))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "Skill 目录不允许符号链接：{}",
                    entry.path().display()
                ));
            }
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file()
                && entry.path().file_name().and_then(|name| name.to_str()) == Some("SKILL.md")
            {
                hash_path(digest, &entry.path());
                hash_file(digest, &entry.path())?;
            }
        }
    }
    Ok(())
}

/// 将一个路径以跨平台稳定文本写入指纹。
fn hash_path(digest: &mut Sha256, path: &Path) {
    digest.update(path.to_string_lossy().replace('\\', "/").as_bytes());
    digest.update([0]);
}

/// 有界读取扩展文本文件并写入指纹。
fn hash_file(digest: &mut Sha256, path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取扩展文件 {}：{error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("扩展文件必须是普通文件：{}", path.display()));
    }
    if metadata.len() > MAX_EXTENSION_FILE_BYTES {
        return Err(format!("扩展文件超过 {MAX_EXTENSION_FILE_BYTES} 字节"));
    }
    let file = crate::storage::open_readonly_regular_file(path)
        .map_err(|error| format!("无法打开扩展文件 {}：{error}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("无法读取扩展文件 {} 的句柄元数据：{error}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() != metadata.len() {
        return Err(format!("扩展文件在打开期间发生变化：{}", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_EXTENSION_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取扩展文件 {}：{error}", path.display()))?;
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_len > MAX_EXTENSION_FILE_BYTES || actual_len != opened_metadata.len() {
        return Err(format!(
            "扩展文件在读取期间发生变化或超过大小上限：{}",
            path.display()
        ));
    }
    let final_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法复核扩展文件 {}：{error}", path.display()))?;
    if final_metadata.file_type().is_symlink()
        || !final_metadata.is_file()
        || final_metadata.len() != metadata.len()
    {
        return Err(format!("扩展文件在读取期间发生变化：{}", path.display()));
    }
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
    Ok(())
}

/// 将 JSON 以对象键稳定排序后写入指纹。
fn hash_value(digest: &mut Sha256, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(&canonicalize_json(value))
        .map_err(|error| format!("无法编码扩展 JSON 指纹：{error}"))?;
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
    Ok(())
}

/// 将字节摘要编码为小写十六进制。
fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 将外部命令错误压缩为单行有界文本。
fn bounded_error_text(value: &str) -> String {
    let value = value.replace(['\r', '\n'], " ");
    let mut end = value.len().min(1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// 规范并校验项目根，拒绝文件和不可访问路径。
fn canonical_project_root(project_root: &Path) -> Result<PathBuf, String> {
    let metadata = fs::metadata(project_root)
        .map_err(|error| format!("无法访问项目目录 {}：{error}", project_root.display()))?;
    if !metadata.is_dir() {
        return Err(format!("项目根不是目录：{}", project_root.display()));
    }
    fs::canonicalize(project_root)
        .map_err(|error| format!("无法规范化项目目录 {}：{error}", project_root.display()))
}

#[cfg(test)]
mod tests {
    /// 配置停用、移除或取消 OAuth 时只撤销旧 OAuth 绑定，不影响保留的 Server。
    #[test]
    fn removed_oauth_servers_excludes_retained_and_non_oauth_servers() {
        let previous =
            ["kept", "removed", "oauth-removed", "plain"].map(|name| RuntimeMcpServerSnapshot {
                name: name.to_owned(),
                transport: keencode_acp::McpTransportKind::StreamableHttp,
                connection_status: keencode_acp::McpConnectionStatus::Disconnected,
                tools_count: 0,
                oauth_status: if name == "plain" {
                    keencode_acp::McpOAuthStatus::NotRequired
                } else {
                    keencode_acp::McpOAuthStatus::Idle
                },
                error: None,
            });
        let current = ["kept", "oauth-removed"].map(|name| RuntimeMcpServer {
            id: name.to_owned(),
            config: keencode_mcp::McpServerConfig::StreamableHttp(
                keencode_mcp::StreamableHttpConfig::new("https://mcp.example.test/api"),
            ),
            oauth: (name == "kept").then(|| crate::mcp_oauth::McpOAuthSettings {
                client_id: "desktop".to_owned(),
                resource: "https://mcp.example.test/api".to_owned(),
                scopes: Vec::new(),
            }),
        });
        assert_eq!(
            removed_oauth_servers(&previous, &current),
            ["removed", "oauth-removed"]
        );
        assert_eq!(
            removed_oauth_servers(&previous, &[]),
            ["kept", "removed", "oauth-removed"]
        );
        assert!(removed_oauth_servers(&[], &current).is_empty());
    }

    use super::super::agent_catalog::{AgentDefinitionSource, ParsedAgentDocument};
    use super::*;
    use keencode_agent::{
        AgentId, AgentTool, SessionId, ToolCallId, ToolConcurrency, ToolContext, ToolFuture,
        ToolOutput, TurnCancellation, TurnId,
    };
    use keencode_model::ToolDefinition;
    use keencode_tools::{ExecuteExtraTool, ToolSearchTool};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 有资源入口时工具发现失败仍保持 Connected，无入口或 Server 不可用时保持 Failed。
    #[test]
    fn mcp_runtime_connection_state_maps_degraded_discovery() {
        let (status, error) = mcp_runtime_connection_state(3, None, Some("tools/list 失败"));
        assert_eq!(status, keencode_acp::McpConnectionStatus::Connected);
        assert_eq!(error.as_deref(), Some("tools/list 失败"));

        let (status, error) = mcp_runtime_connection_state(0, None, Some("tools/list 失败"));
        assert_eq!(status, keencode_acp::McpConnectionStatus::Failed);
        assert_eq!(error.as_deref(), Some("tools/list 失败"));

        let (status, error) =
            mcp_runtime_connection_state(3, Some("Server 不可用"), Some("tools/list 失败"));
        assert_eq!(status, keencode_acp::McpConnectionStatus::Failed);
        assert_eq!(error.as_deref(), Some("Server 不可用"));

        for tool_count in [0, 3] {
            let (status, error) = mcp_runtime_connection_state(tool_count, None, None);
            assert_eq!(status, keencode_acp::McpConnectionStatus::Connected);
            assert_eq!(error, None);
        }
    }

    /// 可观察执行次数、用于验证 MCP 工具撤销边界的测试工具。
    struct RecordedMcpTool {
        /// 工具对外暴露的冻结定义。
        definition: ToolDefinition,
        /// 工具执行时返回的项目标签。
        project: String,
        /// 实际进入工具执行体的次数。
        executions: Arc<AtomicUsize>,
    }

    impl RecordedMcpTool {
        /// 创建一个要求非空 value 字符串参数的只读 MCP 测试工具。
        fn new(name: &str, project: &str) -> (Arc<Self>, Arc<AtomicUsize>) {
            let executions = Arc::new(AtomicUsize::new(0));
            (
                Arc::new(Self {
                    definition: ToolDefinition::new(
                        name,
                        format!("{project} MCP 测试工具"),
                        json!({
                            "type": "object",
                            "properties": {
                                "value": { "type": "string", "minLength": 1 }
                            },
                            "required": ["value"],
                            "additionalProperties": false
                        }),
                    ),
                    project: project.to_owned(),
                    executions: Arc::clone(&executions),
                }),
                executions,
            )
        }
    }

    impl AgentTool for RecordedMcpTool {
        /// 返回测试工具的冻结名称、说明和输入 Schema。
        fn definition(&self) -> ToolDefinition {
            self.definition.clone()
        }

        /// 按冻结 Schema 校验输入并声明只读影响。
        fn effect(&self, input: &Value) -> Result<ToolEffect, keencode_agent::ToolError> {
            self.definition
                .validate_input(input)
                .map_err(|_| keencode_agent::ToolError::permanent("invalid", "测试输入无效"))?;
            Ok(ToolEffect::ReadOnly)
        }

        /// 只读测试工具允许并行执行。
        fn concurrency(&self) -> ToolConcurrency {
            ToolConcurrency::ParallelReadOnly
        }

        /// 记录执行并回传项目标签与输入值。
        fn execute(&self, _context: ToolContext, input: Value) -> ToolFuture<'_> {
            Box::pin(async move {
                self.executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text(format!(
                    "{}:{}",
                    self.project,
                    input["value"].as_str().unwrap_or_default()
                )))
            })
        }
    }

    /// 一个项目当前发布候选及其 MCP 搜索、执行入口的测试夹具。
    struct ProjectMcpFixture {
        /// 候选绑定的规范项目根。
        project_root: PathBuf,
        /// 候选共享的延迟 MCP 目录。
        catalog: Arc<DeferredToolCatalog>,
        /// 允许测试显式触发该项目 MCP 撤销的贡献器。
        contributor: Arc<NativeExtensionContributor>,
        /// 绑定候选目录的搜索入口。
        search: ToolSearchTool,
        /// 绑定候选目录的延迟执行入口。
        execute: ExecuteExtraTool,
        /// 候选的 Runtime 代次。
        candidate: RuntimeExtensionCandidate,
        /// 记录实际工具执行次数。
        executions: Arc<AtomicUsize>,
    }

    impl ProjectMcpFixture {
        /// 构造包含单个 MCP 工具和独立 Runtime 候选代次的项目夹具。
        fn new(base: &Path, project: &str, tool_name: &str, candidate_generation: u64) -> Self {
            let project_root = base.join(project);
            let data_root = base.join(format!("{project}-data"));
            fs::create_dir_all(&project_root).expect("创建 MCP 测试项目目录");
            fs::create_dir_all(&data_root).expect("创建 MCP 测试数据目录");
            let project_root = fs::canonicalize(project_root).expect("规范 MCP 测试项目目录");
            let data_root = fs::canonicalize(data_root).expect("规范 MCP 测试数据目录");
            let plugins = PluginRuntimeSnapshot::default();
            let skills = Arc::new(
                keencode_skills::discover_skills(&runtime_skill_config_from_snapshot(
                    data_root,
                    project_root.clone(),
                    plugins,
                ))
                .expect("建立空 MCP 测试 Skill 目录"),
            );
            let catalog = Arc::new(DeferredToolCatalog::new());
            let (tool, executions) = RecordedMcpTool::new(tool_name, project);
            catalog
                .replace_all(vec![tool])
                .expect("冻结 MCP 测试工具目录");
            let contributor = Arc::new(NativeExtensionContributor {
                project_root: project_root.clone(),
                skills,
                deferred_tools: Some(Arc::clone(&catalog)),
                mcp_servers: Vec::new(),
                hooks: Vec::new(),
                agents: AgentCatalog::default(),
                commands: Arc::new(crate::plugins::PluginCommandCatalog::default()),
                lsp_runtime: None,
                diagnostics: Vec::new(),
            });
            let candidate = RuntimeExtensionCandidate::new(
                candidate_generation,
                Arc::clone(&contributor) as Arc<dyn RuntimeExtensionContributor>,
            )
            .expect("测试候选代次应有效");
            Self {
                project_root,
                search: ToolSearchTool::new(Arc::clone(&catalog)),
                execute: ExecuteExtraTool::new(Arc::clone(&catalog)),
                catalog,
                contributor,
                candidate,
                executions,
            }
        }
    }

    /// 创建不依赖真实文件和凭据的延迟工具上下文。
    fn mcp_tool_context() -> ToolContext {
        ToolContext {
            session_id: SessionId::new("session-project-mcp").expect("测试 Session 标识有效"),
            turn_id: TurnId::new("turn-project-mcp").expect("测试 Turn 标识有效"),
            source_agent_id: AgentId::new("agent-project-mcp").expect("测试 Agent 标识有效"),
            tool_call_id: ToolCallId::new("call-project-mcp").expect("测试 ToolCall 标识有效"),
            cancellation: TurnCancellation::new(),
        }
    }

    /// 从 ToolSearch 的文本结果取出冻结目录代次与工具定义摘要。
    async fn search_snapshot(search: &ToolSearchTool, query: &str) -> Value {
        let output = search
            .execute(mcp_tool_context(), json!({ "query": query }))
            .await
            .expect("MCP 搜索不应失败");
        match &output.content[0] {
            keencode_model::ToolResultContent::Text { text } => {
                serde_json::from_str(text).expect("MCP 搜索结果应为 JSON")
            }
            _ => panic!("MCP 搜索结果应为文本"),
        }
    }

    /// 执行一个延迟工具并提取文本结果，便于区分项目候选实际执行的实现。
    async fn execute_text(
        execute: &ExecuteExtraTool,
        input: Value,
    ) -> Result<String, keencode_agent::ToolError> {
        let output = execute.execute(mcp_tool_context(), input).await?;
        match output.content.into_iter().next() {
            Some(keencode_model::ToolResultContent::Text { text }) => Ok(text),
            _ => panic!("MCP 执行结果应为文本"),
        }
    }

    /// 构造延迟执行入口需要的搜索代次、工具名称和参数。
    fn execute_input(snapshot: &Value, tool_name: &str, value: &str) -> Value {
        json!({
            "catalog_generation": snapshot["catalog_generation"],
            "tool_name": tool_name,
            "params": { "value": value }
        })
    }

    /// 返回单个 Agent 条目的独立摘要，便于验证各字段都会使候选失效。
    fn agent_entry_digest(entry: &AgentCatalogEntry) -> String {
        let mut digest = Sha256::new();
        hash_agent_entry(&mut digest, entry);
        hex_digest(&digest.finalize())
    }

    /// Agent 工具、限制、来源或路径变化均必须生成不同扩展指纹。
    #[test]
    fn agent_fingerprint_covers_all_runtime_template_fields() {
        let baseline = AgentCatalogEntry {
            name: "reviewer".to_owned(),
            source: AgentDefinitionSource::Project,
            path: Some(PathBuf::from("/project/.agents/agents/reviewer.md")),
            document: ParsedAgentDocument {
                name: Some("reviewer".to_owned()),
                description: "检查改动".to_owned(),
                model: Some("provider::model".to_owned()),
                tools: AgentTools::List(vec!["Read".to_owned()]),
                disallowed_tools: vec!["Write".to_owned()],
                max_turns: Some(8),
                allowed_write_dirs: vec!["reports".to_owned()],
                system_prompt: "只报告问题".to_owned(),
            },
        };
        let expected = agent_entry_digest(&baseline);

        let mut changed = baseline.clone();
        changed.document.tools = AgentTools::List(vec!["Read".to_owned(), "Grep".to_owned()]);
        assert_ne!(agent_entry_digest(&changed), expected);

        let mut changed = baseline.clone();
        changed.document.disallowed_tools.push("Edit".to_owned());
        assert_ne!(agent_entry_digest(&changed), expected);

        let mut changed = baseline.clone();
        changed.document.max_turns = Some(9);
        assert_ne!(agent_entry_digest(&changed), expected);

        let mut changed = baseline.clone();
        changed
            .document
            .allowed_write_dirs
            .push("artifacts".to_owned());
        assert_ne!(agent_entry_digest(&changed), expected);

        let mut changed = baseline.clone();
        changed.source = AgentDefinitionSource::Global;
        assert_ne!(agent_entry_digest(&changed), expected);

        let mut changed = baseline;
        changed.path = Some(PathBuf::from("/data/agents/reviewer.md"));
        assert_ne!(agent_entry_digest(&changed), expected);
    }

    /// 两个市场中的同名插件 Hook 必须保留各自独立的运行时名称。
    #[test]
    fn hook_namespace_includes_marketplace() {
        let plugin = |marketplace: &str| crate::plugins::RuntimePlugin {
            id: PluginId {
                plugin: "demo".to_owned(),
                marketplace: Some(marketplace.to_owned()),
            },
            root: PathBuf::from("/plugins/demo"),
            commands: Vec::new(),
            skills: Vec::new(),
            agents: Vec::new(),
            hooks: Some(json!({
                "Stop": {"type": "context", "context": "done"}
            })),
            unsupported_hooks: Vec::new(),
            mcp_servers: BTreeMap::new(),
            lsp_servers: Vec::new(),
        };
        let hooks = parse_plugin_hooks(&PluginRuntimeSnapshot {
            plugins: vec![plugin("alpha"), plugin("beta")],
        })
        .expect("同名插件 Hook 应完成命名空间归约");

        assert_eq!(
            hooks.iter().map(HookSpec::name).collect::<Vec<_>>(),
            ["plugin:alpha:demo:stop:0:0", "plugin:beta:demo:stop:0:0"]
        );
    }

    /// 返回执行后在当前目录创建标记文件的跨平台 Hook 命令。
    #[cfg(windows)]
    fn marker_command() -> String {
        "echo executed>executed.txt".to_owned()
    }

    /// 返回执行后在当前目录创建标记文件的跨平台 Hook 命令。
    #[cfg(not(windows))]
    fn marker_command() -> String {
        "printf executed > executed.txt".to_owned()
    }

    /// 返回持续运行到测试超时的跨平台 Hook 命令。
    #[cfg(windows)]
    fn long_running_command() -> String {
        "powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 10\""
            .to_owned()
    }

    /// 返回持续运行到测试超时的跨平台 Hook 命令。
    #[cfg(not(windows))]
    fn long_running_command() -> String {
        "sleep 10".to_owned()
    }

    /// 返回持续写标准输出直到命中容量限制的跨平台 Hook 命令。
    #[cfg(windows)]
    fn oversized_output_command() -> String {
        "for /L %i in (1,1,1000) do @echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_owned()
    }

    /// 返回持续写标准输出直到命中容量限制的跨平台 Hook 命令。
    #[cfg(not(windows))]
    fn oversized_output_command() -> String {
        "while :; do printf xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done".to_owned()
    }

    /// 构造会产生一个可观察文件副作用的命令 Hook。
    fn marker_hook(directory: &Path) -> CommandHookSpec {
        CommandHookSpec {
            name: "test:command".to_owned(),
            phase: HookPhase::Stop,
            matcher: None,
            command: marker_command(),
            current_dir: directory.to_owned(),
        }
    }

    /// Hook matcher 只接受星号或竖线分隔的精确名称。
    #[test]
    fn matcher_is_exact_and_case_insensitive() {
        assert!(matches_tool(&None, "Read"));
        assert!(matches_tool(&Some("*".to_owned()), "Edit"));
        assert!(matches_tool(&Some("Read | Grep".to_owned()), "grep"));
        assert!(!matches_tool(&Some("Read | Grep".to_owned()), "Write"));
    }

    /// 冻结贡献器必须把唯一 Agent Schema 无损投影为 Runtime 模板。
    #[test]
    fn contributor_resolves_complete_agent_template_contract() {
        let directory = tempfile::tempdir().expect("创建扩展测试目录");
        let data_root = directory.path().join("data");
        let project_root = directory.path().join("project");
        fs::create_dir_all(project_root.join(".agents/agents")).expect("创建项目 Agent 目录");
        fs::create_dir_all(&data_root).expect("创建数据目录");
        fs::write(
            project_root.join(".agents/agents/reviewer.md"),
            "---\n\
             name: reviewer\n\
             description: Review changes\n\
             model: \"provider-a::model-a\"\n\
             tools: [\"Read\", \"Grep\", \"Bash\"]\n\
             disallowedTools: [\"Bash\"]\n\
             maxTurns: 4\n\
             allowedWriteDirs: [\"reports/generated\"]\n\
             ---\n\
             Review the actual changes.",
        )
        .expect("写入 Agent 定义");
        let project_root = fs::canonicalize(project_root).expect("规范项目目录");
        let data_root = fs::canonicalize(data_root).expect("规范数据目录");
        let plugins = PluginRuntimeSnapshot::default();
        let skills = Arc::new(
            keencode_skills::discover_skills(&runtime_skill_config_from_snapshot(
                data_root.clone(),
                project_root.clone(),
                plugins.clone(),
            ))
            .expect("建立空 Skill 目录"),
        );
        let agents = build_agent_catalog(&data_root, &project_root, &plugins, &BTreeMap::new())
            .expect("建立 Agent 目录");
        let contributor = NativeExtensionContributor {
            project_root,
            skills,
            deferred_tools: None,
            mcp_servers: Vec::new(),
            hooks: Vec::new(),
            agents,
            commands: Arc::new(crate::plugins::PluginCommandCatalog::default()),
            lsp_runtime: None,
            diagnostics: Vec::new(),
        };
        let context = RuntimeAgentTemplateContext {
            session_id: "session-a".to_owned(),
            parent_agent_id: "root".to_owned(),
            root_turn_id: "turn-a".to_owned(),
        };

        let template = contributor
            .resolve_agent("REVIEWER", &context)
            .expect("模板解析不应失败")
            .expect("应找到项目 Agent");

        assert_eq!(template.name, "reviewer");
        assert_eq!(template.system_prompt, "Review the actual changes.");
        assert_eq!(template.model.as_deref(), Some("provider-a::model-a"));
        assert_eq!(
            template.tool_names,
            Some(vec![
                "Read".to_owned(),
                "Grep".to_owned(),
                "Bash".to_owned()
            ])
        );
        assert_eq!(template.disallowed_tool_names, vec!["Bash"]);
        assert_eq!(template.max_turns, Some(4));
        assert_eq!(
            template.allowed_write_dirs,
            vec![PathBuf::from("reports/generated")]
        );
        assert!(
            contributor
                .resolve_agent("missing", &context)
                .expect("未知模板查找不应失败")
                .is_none()
        );
    }

    /// Plan 只读守卫必须在进程启动前拒绝命令 Hook。
    #[tokio::test]
    async fn plan_guard_rejects_command_hook_before_execution() {
        let directory = tempfile::tempdir().expect("创建 Hook 测试目录");
        let spec = marker_hook(directory.path());
        let error = run_command_hook(&spec, PlanGuard::read_only(), &json!({}))
            .await
            .expect_err("Plan 必须拒绝命令 Hook");
        assert_eq!(error.code, "hook_plan_denied");
        assert!(!directory.path().join("executed.txt").exists());
    }

    /// 普通执行模式直接运行命令 Hook，不存在权限或二次审批分支。
    #[tokio::test]
    async fn inactive_plan_guard_executes_command_hook_directly() {
        let directory = tempfile::tempdir().expect("创建 Hook 测试目录");

        run_command_hook(
            &marker_hook(directory.path()),
            PlanGuard::inactive(),
            &json!({}),
        )
        .await
        .expect("普通模式应执行 Hook");

        assert_eq!(
            fs::read_to_string(directory.path().join("executed.txt"))
                .expect("Hook 应写出标记")
                .trim(),
            "executed"
        );
    }

    /// Hook 命令超时必须沿既有稳定错误码返回。
    #[tokio::test]
    async fn hook_command_timeout_is_bounded_and_stable() {
        let directory = tempfile::tempdir().expect("创建 Hook 测试目录");
        let mut spec = marker_hook(directory.path());
        spec.command = long_running_command();

        let error =
            execute_hook_command_with_limits(&spec, &json!({}), Duration::from_millis(100), 1024)
                .await
                .expect_err("Hook 超时必须失败");

        assert_eq!(error.code, "hook_command_timeout");
    }

    /// Hook 任一输出流超限必须终止进程树并沿既有稳定错误码返回。
    #[tokio::test]
    async fn hook_command_output_limit_is_bounded_and_stable() {
        let directory = tempfile::tempdir().expect("创建 Hook 测试目录");
        let mut spec = marker_hook(directory.path());
        spec.command = oversized_output_command();

        let error = execute_hook_command_with_limits(&spec, &json!({}), Duration::from_secs(5), 64)
            .await
            .expect_err("Hook 输出超限必须失败");

        assert_eq!(error.code, "hook_output_too_large");
    }

    /// 取 Windows 系统目录中的绝对 cmd.exe 路径，避免测试依赖 PATH 搜索结果。
    #[cfg(windows)]
    fn windows_cmd_path() -> PathBuf {
        let system_root = env::var_os("SystemRoot").expect("Windows 测试必须设置 SystemRoot");
        let path = PathBuf::from(system_root).join("System32").join("cmd.exe");
        assert!(path.is_file(), "系统 cmd.exe 不存在：{}", path.display());
        path
    }

    /// 构造使用真实命令 Hook 执行入口的 Windows 回归规格。
    #[cfg(windows)]
    fn windows_command_hook(directory: &Path, command: String) -> CommandHookSpec {
        CommandHookSpec {
            name: "test:windows-command".to_owned(),
            phase: HookPhase::Stop,
            matcher: None,
            command,
            current_dir: directory.to_owned(),
        }
    }

    /// Windows 命令参数必须保留双引号包围的绝对 cmd.exe 路径，并正确执行 /D /C。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_command_hook_quoted_absolute_cmd_path() {
        let directory = tempfile::tempdir().expect("创建 Windows Hook 测试目录");
        let command = format!("\"{}\" /D /C echo KC_QUOTED", windows_cmd_path().display());
        let output = execute_hook_command_with_limits(
            &windows_command_hook(directory.path(), command),
            &json!({}),
            Duration::from_secs(3),
            1024,
        )
        .await
        .expect("带引号的绝对 cmd.exe 路径应可执行");

        assert_eq!(output.trim(), "KC_QUOTED");
    }

    /// canonicalize 返回的 Windows 扩展路径也必须作为 Hook 的真实工作目录传给 cmd。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_command_hook_canonicalized_cwd_reads_relative_file() {
        let directory = tempfile::tempdir().expect("创建 Windows Hook 测试目录");
        fs::write(
            directory.path().join("relative-marker.txt"),
            "KC_CANONICAL_CWD",
        )
        .expect("写入相对路径标记文件");
        let canonical_directory = fs::canonicalize(directory.path()).expect("规范化 Hook 目录");
        let output = execute_hook_command_with_limits(
            &windows_command_hook(&canonical_directory, "type relative-marker.txt".to_owned()),
            &json!({}),
            Duration::from_secs(3),
            1024,
        )
        .await
        .expect("canonicalize 后的工作目录应可读取相对文件");

        assert_eq!(output.trim(), "KC_CANONICAL_CWD");
    }

    /// 引号内的空格和 & 必须只表示文件名，不得触发额外命令或写入副作用文件。
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_command_hook_quoted_ampersand_filename_has_no_side_effect() {
        let directory = tempfile::tempdir().expect("创建 Windows Hook 测试目录");
        fs::write(
            directory.path().join("marker & literal.txt"),
            "KC_LITERAL_FILE",
        )
        .expect("写入包含空格和 & 的标记文件");
        let side_effect = directory.path().join("unexpected-side-effect.txt");
        fs::write(
            directory.path().join("literal.txt.cmd"),
            "@echo KC_SIDE_EFFECT>unexpected-side-effect.txt\r\n",
        )
        .expect("写入用于检测额外命令的诱饵脚本");

        let output = execute_hook_command_with_limits(
            &windows_command_hook(
                directory.path(),
                r#"type "marker & literal.txt""#.to_owned(),
            ),
            &json!({}),
            Duration::from_secs(3),
            1024,
        )
        .await
        .expect("引号内的 & 应只作为文件名字符");

        assert_eq!(output.trim(), "KC_LITERAL_FILE");
        assert!(!side_effect.exists(), "文件名中的 & 不得执行诱饵脚本");
    }

    /// 项目 A 的 MCP 撤销必须隔离于 B/C，并允许 A 发布新候选恢复新工具。
    #[tokio::test]
    async fn project_mcp_revoke_is_isolated_across_a_b_c_candidates() {
        let directory = tempfile::tempdir().expect("创建项目 MCP 撤销测试目录");
        let a = ProjectMcpFixture::new(directory.path(), "project-a", "mcp__a__echo", 101);
        let b = ProjectMcpFixture::new(directory.path(), "project-b", "mcp__b__echo", 202);
        let c = ProjectMcpFixture::new(directory.path(), "project-c", "mcp__c__echo", 303);
        assert_ne!(a.project_root, b.project_root);
        assert_ne!(b.project_root, c.project_root);
        assert_eq!(a.candidate.generation(), 101);
        assert_eq!(b.candidate.generation(), 202);
        assert_eq!(c.candidate.generation(), 303);

        let a_before = search_snapshot(&a.search, "mcp").await;
        let b_before = search_snapshot(&b.search, "mcp").await;
        let c_before = search_snapshot(&c.search, "mcp").await;
        assert_eq!(a_before["tools"][0]["name"], "mcp__a__echo");
        assert_eq!(b_before["tools"][0]["name"], "mcp__b__echo");
        assert_eq!(c_before["tools"][0]["name"], "mcp__c__echo");
        let a_old_input = execute_input(&a_before, "mcp__a__echo", "before-revoke");
        let b_old_input = execute_input(&b_before, "mcp__b__echo", "before-revoke");
        let c_old_input = execute_input(&c_before, "mcp__c__echo", "before-revoke");
        assert_eq!(
            execute_text(&a.execute, a_old_input.clone())
                .await
                .expect("撤销前 A 应可执行"),
            "project-a:before-revoke"
        );
        assert_eq!(
            execute_text(&b.execute, b_old_input.clone())
                .await
                .expect("撤销前 B 应可执行"),
            "project-b:before-revoke"
        );
        assert_eq!(
            execute_text(&c.execute, c_old_input.clone())
                .await
                .expect("撤销前 C 应可执行"),
            "project-c:before-revoke"
        );

        a.contributor
            .revoke_mcp_tools()
            .expect("项目 A MCP 撤销不应失败");
        assert!(a.catalog.is_empty());
        let a_after_revoke = search_snapshot(&a.search, "mcp").await;
        assert!(
            a_after_revoke["tools"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );
        assert_ne!(
            a_after_revoke["catalog_generation"],
            a_before["catalog_generation"]
        );
        let a_error = execute_text(&a.execute, a_old_input.clone())
            .await
            .expect_err("撤销后 A 的旧 Execute 必须失效");
        assert_eq!(a_error.code, "deferred_tool_not_found");

        let b_after_revoke = search_snapshot(&b.search, "mcp").await;
        let c_after_revoke = search_snapshot(&c.search, "mcp").await;
        assert_eq!(b_after_revoke["tools"][0]["name"], "mcp__b__echo");
        assert_eq!(c_after_revoke["tools"][0]["name"], "mcp__c__echo");
        assert_eq!(
            execute_text(&b.execute, b_old_input.clone())
                .await
                .expect("撤销 A 不得影响 B"),
            "project-b:before-revoke"
        );
        assert_eq!(
            execute_text(&c.execute, c_old_input.clone())
                .await
                .expect("撤销 A 不得影响 C"),
            "project-c:before-revoke"
        );

        a.contributor
            .revoke_mcp_tools()
            .expect("重复撤销项目 A 不应失败");
        c.contributor
            .revoke_mcp_tools()
            .expect("项目 C MCP 撤销不应失败");
        c.contributor
            .revoke_mcp_tools()
            .expect("重复撤销项目 C 不应失败");
        assert!(a.catalog.is_empty());
        assert!(c.catalog.is_empty());
        assert_eq!(a.candidate.generation(), 101);
        assert_eq!(b.candidate.generation(), 202);
        assert_eq!(c.candidate.generation(), 303);
        let c_error = execute_text(&c.execute, c_old_input)
            .await
            .expect_err("撤销后 C 的旧 Execute 必须失效");
        assert_eq!(c_error.code, "deferred_tool_not_found");
        assert_eq!(
            execute_text(
                &b.execute,
                json!({
                    "catalog_generation": b_before["catalog_generation"],
                    "tool_name": "mcp__b__echo",
                    "params": { "value": "after-repeated-revoke" }
                })
            )
            .await
            .expect("重复撤销 A/C 不得影响 B"),
            "project-b:after-repeated-revoke"
        );

        let a_republished =
            ProjectMcpFixture::new(directory.path(), "project-a", "mcp__a__echo", 404);
        assert_eq!(a_republished.candidate.generation(), 404);
        let a_new = search_snapshot(&a_republished.search, "mcp").await;
        assert_eq!(a_new["tools"][0]["name"], "mcp__a__echo");
        assert_eq!(
            execute_text(
                &a_republished.execute,
                execute_input(&a_new, "mcp__a__echo", "new-candidate")
            )
            .await
            .expect("A 新候选中的新 MCP 工具应可执行"),
            "project-a:new-candidate"
        );
        let old_a_error = execute_text(
            &a.execute,
            json!({
                "catalog_generation": a_before["catalog_generation"],
                "tool_name": "mcp__a__echo",
                "params": { "value": "old-candidate" }
            }),
        )
        .await
        .expect_err("A 旧候选 Execute 不得复活");
        assert_eq!(old_a_error.code, "deferred_tool_not_found");
        assert_eq!(a.executions.load(Ordering::SeqCst), 1);
        assert_eq!(b.executions.load(Ordering::SeqCst), 3);
        assert_eq!(c.executions.load(Ordering::SeqCst), 2);
        assert_eq!(a_republished.executions.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
#[path = "runtime_contributor/agent_tools_tests.rs"]
mod agent_tools_tests;
