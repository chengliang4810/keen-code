use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tar::Archive;
use tauri::{AppHandle, Manager, State};
use zip::ZipArchive;

use crate::claude_plugins::{
    ClaudePluginManager, InstalledPlugin, MaterializedPlugin, PluginId, PluginRuntimeSnapshot,
    PluginSource, ResolvedUserConfig, UserConfigUpdate, extract_components, load_plugin_manifest,
    marketplace_name_key, materialize_synthetic_marketplace_plugin, resolve_internal_file_symlink,
    synthetic_marketplace_plugin_manifest, synthetic_marketplace_plugin_manifest_for_root,
};
use crate::path_utils::{path_text_to_frontend, path_to_frontend};
use crate::plugin_secrets::SystemSecretStore;

use peri_middlewares::mcp::{ConfigSource, McpConfigFile, McpServerConfig};

/// 单个扩展清单或配置文件允许读取的最大字节数。
const MAX_EXTENSION_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// 远程 marketplace 清单允许读取的最大字节数。
const MAX_MARKETPLACE_MANIFEST_BYTES: usize = 8 * 1024 * 1024;
/// 远程插件 HTTP 归档允许读取的最大字节数。
const MAX_PLUGIN_HTTP_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
/// 单个插件归档允许包含的最大条目数；限制目录元数据和重复条目造成的资源消耗。
const MAX_PLUGIN_ARCHIVE_ENTRIES: usize = 4096;
/// 单个插件归档允许解出的最大普通文件字节数；压缩包下载大小之外再限制解压膨胀。
const MAX_PLUGIN_ARCHIVE_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
/// KeenCode 本地插件市场唯一允许的根目录清单文件名。
/// Claude Code 用户级已知市场登记文件；用于发现已经由 Claude Code 下载到本机的市场。
const CLAUDE_KNOWN_MARKETPLACES: &str = ".claude/plugins/known_marketplaces.json";
/// 新用户默认使用的 Claude Code 官方插件市场仓库。
///
/// 该仓库根目录包含标准 `.claude-plugin/marketplace.json`，其相对插件来源
/// 需要保留完整仓库目录，因此首次访问市场时会浅克隆到 KeenCode 缓存。
const DEFAULT_CLAUDE_MARKETPLACE_SOURCE: &str = "github:anthropics/claude-plugins-official";
const DEFAULT_CLAUDE_MARKETPLACE_REPOSITORY: &str =
    "https://github.com/anthropics/claude-plugins-official.git";
const DEFAULT_CLAUDE_MARKETPLACE_NAME: &str = "claude-plugins-official";
/// 远程插件来源的最长请求时间，避免网络不可达时让安装任务无限等待。
const PLUGIN_REMOTE_TIMEOUT: Duration = Duration::from_secs(60);
/// Git/npm/tar 等外部工具的最长运行时间，避免认证提示或网络重试永久阻塞。
const PLUGIN_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// 轮询外部进程退出状态的初始间隔；短命令可更快被检测到。
const PLUGIN_COMMAND_POLL_INTERVAL_INITIAL: Duration = Duration::from_millis(10);
/// 轮询外部进程退出状态的最大间隔；避免长时间运行的命令持续紧密轮询。
const PLUGIN_COMMAND_POLL_INTERVAL_MAX: Duration = Duration::from_millis(200);
/// 外部工具错误输出的最大保留字节数。
const MAX_EXTERNAL_ERROR_BYTES: usize = 8 * 1024;
/// 默认官方市场取得失败后的自动重试间隔；显式 Refresh 可立即重试。
const MARKETPLACE_RETRY_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
enum MarketplaceBootstrapStatus {
    #[default]
    Idle,
    Fetching,
    Ready,
    Failed {
        error: String,
        retry_at: Instant,
    },
}

#[derive(Debug, Default)]
struct MarketplaceBootstrapState {
    status: MarketplaceBootstrapStatus,
    /// 每次新任务或用户取消都递增；旧 worker 不能覆盖新状态或重新登记市场。
    generation: u64,
}

#[derive(Clone, Debug, Default)]
struct MarketplaceBootstrapView {
    loading: bool,
    error: Option<String>,
}

impl MarketplaceBootstrapState {
    /// 尝试启动一次默认市场取得；Fetching 去重，失败状态按退避时间限制自动重试。
    fn should_start(&self, force: bool, now: Instant) -> bool {
        match &self.status {
            MarketplaceBootstrapStatus::Fetching => false,
            MarketplaceBootstrapStatus::Ready if !force => false,
            MarketplaceBootstrapStatus::Failed { retry_at, .. } if !force && now < *retry_at => {
                false
            }
            MarketplaceBootstrapStatus::Idle
            | MarketplaceBootstrapStatus::Ready
            | MarketplaceBootstrapStatus::Failed { .. } => true,
        }
    }

    fn begin(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.status = MarketplaceBootstrapStatus::Fetching;
        self.generation
    }

    fn succeed(&mut self) {
        self.status = MarketplaceBootstrapStatus::Ready;
    }

    fn fail(&mut self, error: String, now: Instant) {
        self.status = MarketplaceBootstrapStatus::Failed {
            error,
            retry_at: now + MARKETPLACE_RETRY_BACKOFF,
        };
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation == generation && matches!(self.status, MarketplaceBootstrapStatus::Fetching)
    }

    /// 用户移除默认市场后取消尚未完成的 worker；旧 worker 只能清理自身临时目录。
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.status = MarketplaceBootstrapStatus::Idle;
    }

    fn view(&self) -> MarketplaceBootstrapView {
        match &self.status {
            MarketplaceBootstrapStatus::Fetching => MarketplaceBootstrapView {
                loading: true,
                error: None,
            },
            MarketplaceBootstrapStatus::Failed { error, .. } => MarketplaceBootstrapView {
                loading: false,
                error: Some(error.clone()),
            },
            MarketplaceBootstrapStatus::Idle | MarketplaceBootstrapStatus::Ready => {
                MarketplaceBootstrapView::default()
            }
        }
    }
}

/// 串行化扩展配置读写。
#[derive(Debug, Default)]
pub struct ExtensionsState {
    /// 防止多个 Tauri 命令并发覆盖同一个扩展配置文件。
    io_lock: Mutex<()>,
    /// 系统密钥库适配器；公开状态永远不保存插件敏感配置值。
    claude_secrets: Mutex<SystemSecretStore>,
    /// 默认 Claude 官方市场的后台取得状态，避免多个界面请求重复克隆。
    marketplace_bootstrap: Mutex<MarketplaceBootstrapState>,
}

impl ExtensionsState {
    /// 获取扩展配置读写锁。
    fn lock_io(&self) -> Result<MutexGuard<'_, ()>, String> {
        self.io_lock
            .lock()
            .map_err(|_| "扩展配置读写锁已损坏".to_owned())
    }
}

/// 返回 Claude Code 插件状态服务；插件缓存与配置均位于应用数据目录。
fn claude_plugin_manager(app: &AppHandle) -> Result<ClaudePluginManager, String> {
    let root = crate::storage::root_dir(app)
        .map_err(|error| format!("无法确定 Claude 插件数据目录：{error}"))?;
    Ok(ClaudePluginManager::new(root))
}

/// 读取当前启用插件的运行时快照，并生成交给 peri 会话装配的 Hooks。
pub(crate) fn claude_runtime_snapshot(
    app: &AppHandle,
    project_dir: &Path,
) -> Result<PluginRuntimeSnapshot, String> {
    let manager = claude_plugin_manager(app)?;
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let snapshot = if let Some(state) = app.try_state::<ExtensionsState>() {
        let secrets = state
            .claude_secrets
            .lock()
            .map_err(|_| "Claude 插件敏感配置锁已损坏".to_owned())?;
        manager
            .runtime_snapshot(project_dir, &environment, &*secrets)
            .map_err(|error| error.to_string())?
    } else {
        let secrets = SystemSecretStore::default();
        manager
            .runtime_snapshot(project_dir, &environment, &secrets)
            .map_err(|error| error.to_string())?
    };
    Ok(attach_claude_hooks(snapshot))
}

/// 把转换后的 Hook 记录放回快照，确保有无 Tauri managed state 都走同一路径。
fn attach_claude_hooks(mut snapshot: PluginRuntimeSnapshot) -> PluginRuntimeSnapshot {
    snapshot.plugin_hooks = collect_claude_hooks(&snapshot);
    snapshot
}

/// 将 Claude 插件快照中的 Hooks 转换为 peri 的生命周期注册记录。
fn collect_claude_hooks(
    snapshot: &PluginRuntimeSnapshot,
) -> Vec<peri_middlewares::hooks::RegisteredHook> {
    use peri_middlewares::hooks::{HookEvent, HookType, RegisteredHook};

    let mut registrations = Vec::new();
    for plugin in &snapshot.plugins {
        let Some(Value::Object(events)) = plugin.hooks.as_ref() else {
            continue;
        };
        for (event_name, groups) in events {
            let Some(event) = HookEvent::parse(event_name) else {
                tracing::warn!(plugin = %plugin.id, event = %event_name, "忽略未实现的 Hook 事件");
                continue;
            };
            let groups = groups
                .get("hooks")
                .cloned()
                .unwrap_or_else(|| groups.clone());
            let groups = match groups {
                Value::Array(groups) => groups,
                value => vec![value],
            };
            for group in groups {
                let (matcher, hooks) = if let Value::Object(mut object) = group {
                    let matcher = object
                        .remove("matcher")
                        .and_then(|value| value.as_str().map(ToOwned::to_owned));
                    let hooks = object
                        .remove("hooks")
                        .unwrap_or_else(|| Value::Object(object));
                    (matcher, hooks)
                } else {
                    (None, group)
                };
                let hooks = match hooks {
                    Value::Array(hooks) => hooks,
                    value => vec![value],
                };
                for hook in hooks {
                    let hook = match hook {
                        Value::String(command) => {
                            serde_json::json!({"type": "command", "command": command})
                        }
                        value => value,
                    };
                    let hook_type = match serde_json::from_value::<HookType>(hook) {
                        Ok(hook_type) => hook_type,
                        Err(error) => {
                            tracing::warn!(
                                plugin = %plugin.id,
                                event = ?event,
                                error = %error,
                                "忽略格式无效的 Claude Hook"
                            );
                            continue;
                        }
                    };
                    registrations.push(RegisteredHook {
                        hook: hook_type,
                        event: event.clone(),
                        matcher: matcher.clone(),
                        plugin_name: plugin.id.plugin.clone(),
                        plugin_id: plugin.id.to_string(),
                        plugin_root: plugin.root.clone(),
                        plugin_data_dir: plugin.root.join("data"),
                        plugin_options: Default::default(),
                    });
                }
            }
        }
    }
    registrations
}

/// 将用户 MCP 与 Claude 插件 MCP 合并写入 Peri 使用的运行时文件。
fn refresh_mcp_runtime_config(app: &AppHandle) -> Result<PathBuf, String> {
    let user_path = mcp_user_config_path(app)?;
    // 运行时文件只保存用户显式维护的 MCP 配置。插件配置（尤其是由
    // userConfig 插值出的敏感值）由 `runtime_mcp_config` 在进程内构造，
    // 再直接交给 Peri 的内存初始化入口，绝不经过此文件。
    let document = load_mcp_document(&user_path)?.unwrap_or_else(empty_mcp_document);
    let runtime_path = crate::storage::root_dir(app)
        .map_err(|error| format!("无法确定 MCP 运行时目录：{error}"))?
        .join("mcp-runtime.json");
    save_mcp_document(&runtime_path, &document)?;
    Ok(runtime_path)
}

/// 构造当前项目的完整 MCP 运行时配置。
///
/// 用户 MCP 配置来自唯一持久化文件；启用插件的 MCP 则从当前插件快照取得。
/// 快照中的 userConfig 敏感值只存在于返回的进程内结构，调用方应直接把它
/// 传给 Peri，不得序列化或写回 `mcp-runtime.json`。
pub(crate) fn runtime_mcp_config(
    app: &AppHandle,
    project_dir: &Path,
) -> Result<McpConfigFile, String> {
    let user_path = mcp_user_config_path(app)?;
    let mut document = load_mcp_document(&user_path)?.unwrap_or_else(empty_mcp_document);
    let snapshot = claude_runtime_snapshot(app, project_dir)?;
    let servers = mcp_server_map_mut(&mut document)?;
    let mut plugin_servers = BTreeSet::new();
    for plugin in snapshot.plugins {
        for (name, config) in plugin.mcp_servers {
            // Claude Code 的插件 MCP 使用 `plugin:<pluginName>:<server>`；
            // marketplace 仅参与插件 ID 去重，不进入 MCP 运行时名称。
            let runtime_name = format!("plugin:{}:{}", plugin.id.plugin, name);
            plugin_servers.insert(runtime_name.clone());
            servers.insert(runtime_name, config);
        }
    }
    mcp_config_from_document(&document, &user_path, &plugin_servers)
}

/// 把已经通过当前 MCP 结构校验的 JSON 文档转为 Peri 的运行时类型。
///
/// `McpServerConfig.source` 是运行时旁路元数据，serde 不会把它写入任何
/// JSON；这里仅为连接池状态展示设置来源，不复制或持久化敏感值。
fn mcp_config_from_document(
    document: &McpDocument,
    user_path: &Path,
    plugin_servers: &BTreeSet<String>,
) -> Result<McpConfigFile, String> {
    let mut config = McpConfigFile::default();
    for (name, value) in mcp_server_map(document)? {
        let mut server =
            serde_json::from_value::<McpServerConfig>(value.clone()).map_err(|error| {
                format!("MCP Server {name} 配置无法转换为 Peri 运行时结构：{error}")
            })?;
        server.source = Some(if plugin_servers.contains(name) {
            ConfigSource::Plugin
        } else {
            ConfigSource::Project(user_path.to_path_buf())
        });
        config.mcp_servers.insert(name.clone(), server);
    }
    Ok(config)
}

/// 最佳努力生成当前 MCP 快照。用户 MCP 配置损坏时先备份并重置；快照无法
/// 生成时写入空配置，阻止旧快照继续生效。
pub(crate) fn prepare_mcp_runtime_config(app: &AppHandle) -> Result<PathBuf, String> {
    let runtime_path = crate::storage::root_dir(app)
        .map_err(|error| format!("无法确定 MCP 运行时目录：{error}"))?
        .join("mcp-runtime.json");
    let user_path = mcp_user_config_path(app)?;
    if let Err(error) = load_mcp_document(&user_path) {
        match backup_invalid_mcp_config(&user_path) {
            Ok(backup_path) => {
                tracing::warn!(
                    %error,
                    backup = %backup_path.display(),
                    "MCP 用户配置无效，已备份并重置为默认配置"
                );
                if let Err(write_error) = save_mcp_document(&user_path, &empty_mcp_document()) {
                    tracing::warn!(%write_error, "MCP 用户配置重置失败，仅在本次运行使用默认配置");
                }
            }
            Err(backup_error) => tracing::warn!(
                %error,
                %backup_error,
                "MCP 用户配置无效且无法备份，不覆盖原文件；本次运行使用默认配置"
            ),
        }
    }
    if let Err(error) = refresh_mcp_runtime_config(app) {
        tracing::warn!(%error, "MCP 配置快照生成失败，按空配置继续");
        if let Err(write_error) = save_mcp_document(&runtime_path, &empty_mcp_document()) {
            let fallback_path = unavailable_mcp_runtime_path(&runtime_path);
            tracing::warn!(
                %write_error,
                path = %runtime_path.display(),
                fallback = %fallback_path.display(),
                "无法覆盖失效的 MCP 运行时快照，改用不存在的隔离路径"
            );
            return Ok(fallback_path);
        }
    }
    Ok(runtime_path)
}

/// 空快照也无法落盘时返回本进程唯一的不存在路径，避免再次读取旧快照。
fn unavailable_mcp_runtime_path(runtime_path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for suffix in 0_u64.. {
        let candidate = runtime_path.with_file_name(format!(
            ".mcp-runtime-unavailable-{}-{nonce}-{suffix}.json",
            process::id()
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("无界后缀必须能找到不存在的 MCP 隔离路径")
}

/// 为损坏的 MCP 用户配置创建带日期且不覆盖既有文件的备份。
fn backup_invalid_mcp_config(path: &Path) -> Result<PathBuf, String> {
    crate::storage::backup_private_file(path)
        .map_err(|error| format!("备份 MCP 配置失败 {}：{error:#}", path.display()))
}

/// 将设置界面传入的 `plugin` 或 `plugin@marketplace` 解析成唯一已安装 ID。
fn resolve_installed_claude_id(
    manager: &ClaudePluginManager,
    raw: &str,
) -> Result<PluginId, String> {
    let requested = PluginId::parse(raw).map_err(|error| error.to_string())?;
    let state = manager.load_state().map_err(|error| error.to_string())?;
    let matches = state
        .plugins
        .into_iter()
        .filter(|item| {
            item.id.plugin.eq_ignore_ascii_case(&requested.plugin)
                && requested.marketplace.as_deref().is_none_or(|marketplace| {
                    item.id
                        .marketplace
                        .as_deref()
                        .is_some_and(|installed| installed.eq_ignore_ascii_case(marketplace))
                })
        })
        .map(|item| item.id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(format!("找不到已安装插件 {raw}")),
        _ => Err(format!("多个市场包含插件 {raw}，请使用 plugin@marketplace")),
    }
}

/// 把 Claude 插件运行时快照转换为旧 UI 仍可消费的组件统计。
fn claude_plugin_provides(plugin: &crate::claude_plugins::RuntimePlugin) -> PluginProvidesDto {
    PluginProvidesDto {
        commands: plugin.commands.len(),
        skills: plugin.skills.len(),
        agents: plugin.agents.len(),
        hooks: usize::from(plugin.hooks.is_some()),
        mcp: plugin.mcp_servers.len(),
        lsp: plugin.lsp_servers.len(),
    }
}

// 市场来源取得与归档处理体量较大，保持为独立职责模块。
#[path = "extensions/marketplace_source.rs"]
mod marketplace_source;
use marketplace_source::*;

/// 前端展示的 Skill 记录。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDto {
    /// Skill 在斜杠命令中的稳定名称。
    pub name: String,
    /// Skill YAML 前置元数据中的用途说明。
    pub description: String,
    /// Skill 来源，当前为 user、project 或 plugin。
    pub source: String,
    /// Skill 主文件的绝对路径。
    pub path: String,
    /// 是否允许用户通过斜杠命令直接调用。
    pub user_invocable: bool,
}

/// Skills 列举结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillsListResult {
    /// 合并用户级和项目级目录后的 Skills。
    pub skills: Vec<SkillDto>,
}

/// 前端展示的子智能体定义。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDto {
    /// 子智能体的稳定标识。
    pub name: String,
    /// 主智能体用于判断委托时机的说明。
    pub description: String,
    /// 子智能体来源，当前为 global 或 builtin。
    pub source: String,
    /// 项目定义文件路径；内置子智能体没有外部文件。
    pub path: Option<String>,
    /// 全局子智能体的模型覆盖（`"{provider_id}::{model}"`）；None 表示跟随会话 provider。
    pub model: Option<String>,
}

/// 子智能体列举结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsListResult {
    /// KeenCode 全局与运行时内置的全部子智能体。
    pub agents: Vec<AgentDto>,
}

/// 创建子智能体时可选择的工具目录。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCatalog {
    /// 可分配给子智能体的工具名；不含 Agent（子智能体禁用，防递归）。
    pub tools: Vec<String>,
}

/// 单个子智能体定义的只读详情（frontmatter 字段 + 系统提示）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDetail {
    /// 子智能体稳定标识。
    pub name: String,
    /// 主智能体用于判断委托时机的说明。
    pub description: String,
    /// 子智能体来源，当前为 global、builtin 或 plugin。
    pub source: String,
    /// 定义文件路径；内置子智能体没有外部文件。
    pub path: Option<String>,
    /// 模型覆盖（`"{provider_id}::{model}"`）；None 表示跟随会话 provider。
    pub model: Option<String>,
    /// 允许使用的工具；None 表示继承主智能体全部工具。
    pub tools: Option<Vec<String>>,
    /// 显式排除的工具。
    pub disallowed_tools: Vec<String>,
    /// 停止前的最大轮数；None 表示使用运行时默认。
    pub max_turns: Option<u32>,
    /// SandboxWrite 白名单相对目录（内置只读子智能体的方案沙箱）。
    pub allowed_write_dirs: Vec<String>,
    /// Markdown 正文中的系统提示。
    pub system_prompt: String,
}

/// 前端展示的 MCP Server 记录。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDto {
    /// MCP Server 在配置映射中的名称。
    pub name: String,
    /// MCP 传输类型，当前为 stdio 或 http。
    pub transport: String,
    /// 用户配置 MCP 的 stdio 命令及参数或远端 URL；插件来源的目标不返回，避免暴露
    /// userConfig 敏感值。
    pub target: Option<String>,
    /// MCP 唯一配置中的启用状态。
    pub enabled: bool,
}

/// MCP Server 列举结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InspectMcpResult {
    /// KeenCode 唯一 MCP 配置中的 Server。
    pub servers: Vec<McpDto>,
}

/// 插件提供的 Skill 数量。
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginProvidesDto {
    /// 插件包含的 Claude Commands 数量。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub commands: usize,
    /// 插件包含的 Skill 数量。
    pub skills: usize,
    /// 插件包含的 Claude Agents 数量。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub agents: usize,
    /// 插件是否声明 Hooks。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hooks: usize,
    /// 插件声明的 MCP Server 数量。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub mcp: usize,
    /// 插件声明的 LSP Server 数量。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub lsp: usize,
}

/// serde 条件序列化辅助函数，保持仅 Skills 插件响应的紧凑性。
fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// 前端展示的本地插件记录。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDto {
    /// 插件稳定名称。
    pub name: String,
    /// 插件清单中的版本。
    pub version: Option<String>,
    /// 插件来源市场名称。
    pub marketplace: Option<String>,
    /// 插件根目录的绝对路径。
    pub path: String,
    /// KeenCode 插件开关状态。
    pub enabled: bool,
    /// 从插件目录实时统计的组件信息。
    pub provides: PluginProvidesDto,
    /// 插件 hooks.json 中声明了但 peri 无法识别的事件名（拼写错误或未实现事件）；运行时会静默跳过这些事件。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_hooks: Vec<String>,
}

/// 已安装插件列举结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginsListResult {
    /// KeenCode 管理的本地插件。
    pub plugins: Vec<PluginDto>,
}

/// 插件详情结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetailsResult {
    /// 插件稳定名称。
    pub name: String,
    /// 不包含密钥或环境变量的本地详情文本。
    pub details: String,
}

/// Claude 插件 userConfig 的可视化字段，不返回敏感字段实际值。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUserConfigFieldDto {
    /// 配置字段名。
    pub name: String,
    /// Claude 声明的字段类型。
    pub value_type: String,
    /// 设置页面标题。
    pub title: Option<String>,
    /// 字段说明。
    pub description: Option<String>,
    /// 是否必填。
    pub required: bool,
    /// 是否敏感；敏感字段值不通过 IPC 返回。
    pub sensitive: bool,
    /// 是否多选数组。
    pub multiple: bool,
    /// `select` 字段允许的候选值。
    pub enum_values: Vec<Value>,
    /// 数字最小值或文本/路径最短长度。
    pub min: Option<f64>,
    /// 数字最大值或文本/路径最大长度。
    pub max: Option<f64>,
    /// 默认值（敏感默认值也不返回）。
    pub default: Option<Value>,
    /// 当前公开值；敏感字段始终为 null。
    pub value: Option<Value>,
}

/// 一个插件的 userConfig 展示结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUserConfigResult {
    /// 插件完整 ID。
    pub plugin: String,
    /// 配置字段定义与当前值。
    pub fields: Vec<PluginUserConfigFieldDto>,
}

/// MCP Doctor 的单项检查。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDoctorCheck {
    /// 检查项目名称。
    pub label: String,
    /// 检查是否通过。
    pub passed: bool,
    /// 检查结果说明。
    pub detail: String,
}

/// MCP Doctor 的配置源状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDoctorSource {
    /// MCP 配置文件绝对路径。
    pub path: String,
    /// configured 或 missing。
    pub status: String,
    /// 当前配置中的 Server 数量。
    pub server_count: usize,
}

/// MCP Doctor 的单个 Server 结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDoctorServer {
    /// MCP Server 名称。
    pub name: String,
    /// 所有结构和本机命令检查是否通过。
    pub healthy: bool,
    /// MCP 传输类型。
    pub transport: String,
    /// 用户配置 MCP 的 stdio 命令及参数或远端 URL；插件来源的目标不返回，避免暴露
    /// userConfig 敏感值。
    pub target: Option<String>,
    /// 不包含环境变量值的检查列表。
    pub checks: Vec<McpDoctorCheck>,
}

/// MCP Doctor 汇总。
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDoctorSummary {
    /// 结构与本机命令检查通过的 Server 数量。
    pub healthy: usize,
    /// 至少一项检查失败的 Server 数量。
    pub unhealthy: usize,
    /// 参与检查的 Server 总数。
    pub total: usize,
}

/// MCP Doctor 返回给前端的完整报告。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDoctorReport {
    /// 所有已列举 Server 是否健康。
    pub ok: bool,
    /// MCP Server 检查明细。
    pub servers: Vec<McpDoctorServer>,
    /// KeenCode 唯一 MCP 配置源状态。
    pub sources: Vec<McpDoctorSource>,
    /// Doctor 汇总计数。
    pub summary: McpDoctorSummary,
    /// 空列表或指定名称不存在时的说明。
    pub raw_text: Option<String>,
}

/// 本地插件市场来源。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceSourceDto {
    /// 市场清单中的稳定名称。
    pub name: String,
    /// 本地市场根目录。
    pub path: String,
}

/// 本地市场中可安装的插件。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailablePluginDto {
    /// 插件稳定名称。
    pub name: String,
    /// 插件所在的市场名称。
    pub marketplace: String,
    /// 市场清单或插件清单中的说明。
    pub description: Option<String>,
    /// 插件清单中的版本。
    pub version: Option<String>,
    /// 插件包含的 Skill 数量。
    pub skill_count: usize,
    /// 插件包含的 LSP Server 数量。
    pub lsp_count: usize,
}

/// 插件市场可安装列表。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketplaceAvailableResult {
    /// 所有本地市场中尚未安装的插件。
    pub plugins: Vec<AvailablePluginDto>,
    /// 默认 Claude 官方市场是否仍在后台取得。
    pub loading: bool,
    /// 默认市场取得失败时的可展示错误；失败状态带退避，不会每次请求重复克隆。
    pub error: Option<String>,
}

/// KeenCode 持久化的本地市场记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct MarketplaceRecord {
    /// 市场清单中的稳定名称。
    name: String,
    /// 本地市场根目录的规范化绝对路径。
    path: String,
    /// 实际使用的 marketplace.json 规范化绝对路径；支持 Git source.path 指定的嵌套清单。
    manifest_path: String,
}

/// KeenCode 持久化的市场列表。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct MarketplaceStore {
    /// 用户显式添加的本地市场来源。
    sources: Vec<MarketplaceRecord>,
}

/// Claude Code `known_marketplaces.json` 的最小可用记录。
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeKnownMarketplaceRecord {
    /// Claude Code 已物化的市场目录；目录不存在时跳过该记录，避免启动时隐式联网。
    install_location: Option<String>,
}

/// 从磁盘读取的 MCP 配置文档。
#[derive(Clone, Debug)]
struct McpDocument {
    /// 已通过当前唯一结构校验的完整 JSON 根节点。
    root: Value,
}

/// 合并配置源后的 MCP Server。
#[derive(Clone, Debug)]
struct ResolvedMcpServer {
    /// Server 的原始 JSON 配置。
    config: Value,
    /// 是否来自 Claude 插件；插件配置可能包含已插值的 userConfig 敏感值。
    plugin_source: bool,
}

/// 设置一个 MCP Server 的唯一启用状态。
#[tauri::command]
pub fn extensions_set_mcp(
    name: String,
    enabled: bool,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_extension_name(&name, "MCP Server")?;
    persist_mcp_enabled(&app, &[(&name, enabled)])
}

/// 批量启用前端当前列出的 MCP Server。
#[tauri::command]
pub fn extensions_enable_all_mcp(
    names: Vec<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let names = normalized_extension_names(names, "MCP Server")?;
    let updates = names
        .iter()
        .map(|name| (name.as_str(), true))
        .collect::<Vec<_>>();
    persist_mcp_enabled(&app, &updates)
}

/// 列出 KeenCode 用户级与项目级 Skills。
#[tauri::command]
pub fn skills_list(
    project_path: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<SkillsListResult, String> {
    let _guard = state.lock_io()?;
    let project = project_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|path| crate::workspace::registered_project_root(&app, path))
        .transpose()?;
    let mut skills = BTreeMap::new();
    let project_context = project
        .clone()
        .unwrap_or(std::env::current_dir().map_err(|error| format!("无法确定当前目录：{error}"))?);
    let runtime_roots = runtime_skill_roots(&app, &project_context)?;
    let mut scan_roots = runtime_roots
        .iter()
        .filter(|root| root.source == peri_middlewares::skills::SkillSource::User)
        .cloned()
        .collect::<Vec<_>>();
    if let Some(project) = &project {
        scan_roots.push(peri_middlewares::skills::SkillRoot {
            path: project.join(".agents/skills"),
            source: peri_middlewares::skills::SkillSource::Project,
            plugin_name: None,
        });
    }
    scan_roots.extend(
        runtime_roots
            .iter()
            .filter(|root| root.source == peri_middlewares::skills::SkillSource::Plugin)
            .cloned(),
    );
    for skill in peri_middlewares::skills::scan_skill_roots(&scan_roots) {
        let source = match skill.source {
            peri_middlewares::skills::SkillSource::User => "user",
            peri_middlewares::skills::SkillSource::Project => "project",
            peri_middlewares::skills::SkillSource::Plugin => "plugin",
            peri_middlewares::skills::SkillSource::Builtin => continue,
        };
        skills
            .entry(skill.name.to_ascii_lowercase())
            .or_insert(SkillDto {
                name: skill.name,
                description: skill.description,
                source: source.to_owned(),
                path: path_to_frontend(&skill.path),
                user_invocable: true,
            });
    }
    let snapshot = claude_runtime_snapshot(&app, &project_context)?;
    for plugin in snapshot.plugins {
        let plugin_namespace = format!("plugin:{}", plugin.id.plugin);
        for file in plugin.commands {
            let namespace = plugin_command_namespace(&plugin_namespace, &file.relative_path);
            let description = std::fs::read_to_string(&file.path)
                .ok()
                .and_then(|content| peri_middlewares::parse_agent_file(&content))
                .map(|definition| definition.frontmatter.description)
                .unwrap_or_default();
            skills.entry(namespace.clone()).or_insert(SkillDto {
                name: namespace,
                description,
                source: "plugin".to_owned(),
                path: path_to_frontend(&file.path),
                user_invocable: true,
            });
        }
    }
    Ok(SkillsListResult {
        skills: skills.into_values().collect(),
    })
}

/// 列出 KeenCode 全局定义与 peri 内置的子智能体。
#[tauri::command]
pub fn agents_list(app: AppHandle) -> Result<AgentsListResult, String> {
    let agents_dir = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
        .join("agents");
    let plugin_agents = claude_runtime_snapshot(
        &app,
        &std::env::current_dir().map_err(|error| format!("无法确定当前目录：{error}"))?,
    )?
    .plugins
    .into_iter()
    .flat_map(|plugin| {
        plugin.agents.into_iter().filter_map(move |file| {
            let stem = file.path.file_stem()?.to_str()?.to_owned();
            Some((
                format!("{}:{}", plugin.id.to_string().replace('@', ":"), stem),
                file.path,
            ))
        })
    })
    .collect::<BTreeMap<_, _>>();
    let project_dir =
        std::env::current_dir().map_err(|error| format!("无法确定当前目录：{error}"))?;
    // 项目级 + KeenCode 全局目录 + peri 内置子智能体。
    let scanned = peri_middlewares::subagent::scan_agents_with_extra_dirs(
        &project_dir.to_string_lossy(),
        std::slice::from_ref(&agents_dir),
    );
    let model_overrides = read_agent_model_overrides(&app)?;
    let mut agents = scanned
        .into_iter()
        .map(|(agent_id, _, description)| {
            let path = agents_dir.join(format!("{agent_id}.md"));
            let global_defined = path.is_file();
            // 全局定义回读 frontmatter；内置定义回读覆盖表（项目定义无 UI
            // 模型入口，保持 None）。运行时同源：frontmatter 或覆盖表生效。
            let model = if global_defined {
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|content| {
                        peri_middlewares::claude_agent_parser::parse_agent_file(&content)
                    })
                    .and_then(|agent| agent.frontmatter.model)
                    .and_then(|model| normalize_model_reference_for_ui(&model))
            } else if peri_middlewares::subagent::get_built_in_agent(&agent_id).is_some() {
                model_overrides
                    .get(&agent_id)
                    .and_then(|model| normalize_model_reference_for_ui(model))
            } else {
                None
            };
            AgentDto {
                name: agent_id,
                description,
                source: if global_defined { "global" } else { "builtin" }.to_owned(),
                path: global_defined.then(|| path_to_frontend(&path)),
                model,
            }
        })
        .collect::<Vec<_>>();
    // 插件声明的 Agent 使用 `plugin:<pluginName>:<agent>` 命名空间补充展示。
    for (agent_id, plugin_path) in plugin_agents {
        let description = std::fs::read_to_string(&plugin_path)
            .ok()
            .and_then(|content| peri_middlewares::parse_agent_file(&content))
            .map(|definition| definition.frontmatter.description)
            .unwrap_or_default();
        agents.push(AgentDto {
            name: agent_id,
            description,
            source: "plugin".to_owned(),
            path: Some(path_to_frontend(&plugin_path)),
            model: None,
        });
    }
    agents.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(AgentsListResult { agents })
}

/// 返回创建子智能体时可选择的工具目录。
///
/// 子智能体后台非交互运行，无法直接使用宿主问答流程；进度由 agent 生命周期
/// 事件上报而非 TodoWrite，因此这两类工具均不提供。
#[tauri::command]
pub fn agents_tool_catalog() -> Result<AgentToolCatalog, String> {
    use peri_middlewares::tool_search::core_tools::{
        TOOL_BASH, TOOL_EDIT, TOOL_FOLDER_OPS, TOOL_GLOB, TOOL_GREP, TOOL_READ, TOOL_WRITE,
    };
    // Agent 工具会被 peri 子智能体过滤器无条件排除（防递归），不列入候选。
    Ok(AgentToolCatalog {
        tools: vec![
            TOOL_BASH,
            TOOL_WRITE,
            TOOL_EDIT,
            TOOL_READ,
            TOOL_GLOB,
            TOOL_GREP,
            TOOL_FOLDER_OPS,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    })
}

/// 读取单个子智能体定义详情；查找优先级与 `agents_list` 一致：
/// 插件命名空间 → KeenCode 全局目录 → peri 内置。
#[tauri::command]
pub fn agent_detail(name: String, app: AppHandle) -> Result<AgentDetail, String> {
    use peri_middlewares::claude_agent_parser::ToolsValue;

    let name = name.trim();
    let current_dir =
        std::env::current_dir().map_err(|error| format!("无法确定当前目录：{error}"))?;
    let plugin_agents = claude_runtime_snapshot(&app, &current_dir)?
        .plugins
        .into_iter()
        .flat_map(|plugin| {
            plugin.agents.into_iter().filter_map(move |file| {
                let stem = file.path.file_stem()?.to_str()?.to_owned();
                Some((
                    format!("{}:{}", plugin.id.to_string().replace('@', ":"), stem),
                    file.path,
                ))
            })
        })
        .collect::<BTreeMap<_, _>>();
    let (source, path, content) = if let Some(path) = plugin_agents.get(name) {
        (
            "plugin",
            Some(path_to_frontend(path)),
            std::fs::read_to_string(path)
                .map_err(|error| format!("无法读取插件子智能体 {name}：{error}"))?,
        )
    } else if validate_agent_id(name).is_ok() {
        let global_path = crate::storage::root_dir(&app)
            .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
            .join("agents")
            .join(format!("{name}.md"));
        if global_path.is_file() {
            (
                "global",
                Some(path_to_frontend(&global_path)),
                std::fs::read_to_string(&global_path)
                    .map_err(|error| format!("无法读取全局子智能体 {name}：{error}"))?,
            )
        } else if let Some(built_in) = peri_middlewares::subagent::get_built_in_agent(name) {
            ("builtin", None, built_in.content.to_owned())
        } else {
            return Err(format!("找不到子智能体 {name}"));
        }
    } else {
        return Err(format!("找不到子智能体 {name}"));
    };
    let agent = peri_middlewares::claude_agent_parser::parse_agent_file(&content)
        .ok_or_else(|| format!("子智能体 {name} 定义解析失败"))?;
    // 内置定义的生效模型 = 覆盖表优先（与运行时套用逻辑同源）。
    let mut agent = agent;
    if source == "builtin"
        && let Some(model) = read_agent_model_overrides(&app)?.get(name)
    {
        agent.frontmatter.model = Some(model.clone());
    }
    let tools = match &agent.frontmatter.tools {
        ToolsValue::Empty => None,
        ToolsValue::NoTools => Some(Vec::new()),
        ToolsValue::List(list) => Some(list.clone()),
    };
    let disallowed_tools = match &agent.frontmatter.disallowed_tools {
        ToolsValue::List(list) => list.clone(),
        _ => Vec::new(),
    };
    Ok(AgentDetail {
        name: name.to_owned(),
        description: agent.frontmatter.description,
        source: source.to_owned(),
        path,
        // 设置页只展示合法的 `provider_id::model` 引用；非法值不进入模型选项。
        model: agent
            .frontmatter
            .model
            .as_deref()
            .and_then(normalize_model_reference_for_ui),
        tools,
        disallowed_tools,
        max_turns: agent.frontmatter.max_turns,
        allowed_write_dirs: agent.frontmatter.allowed_write_dirs,
        system_prompt: agent.system_prompt,
    })
}

/// 校验 KeenCode 全局子智能体名称的唯一当前格式。
fn validate_agent_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Agent name '{id}' 只允许小写 ASCII 字母、数字和非首尾连字符"
        ))
    }
}

/// 在 KeenCode 全局目录创建一个符合 peri 当前结构的子智能体定义。
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri IPC 当前字段需保持平铺并与前端命令契约一致。
pub fn agent_create(
    name: String,
    description: String,
    prompt: String,
    tools: Option<Vec<String>>,
    max_turns: Option<u32>,
    model: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = name.trim();
    validate_agent_id(name)?;
    let description = description.trim();
    if description.is_empty() {
        return Err("子智能体说明不能为空".to_owned());
    }
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("子智能体系统提示不能为空".to_owned());
    }
    if matches!(max_turns, Some(0)) {
        return Err("最大轮数必须大于 0".to_owned());
    }
    let tools = match tools {
        Some(tools) => {
            let tools = tools
                .into_iter()
                .map(|tool| tool.trim().to_owned())
                .filter(|tool| !tool.is_empty())
                .collect::<Vec<_>>();
            if tools.iter().collect::<BTreeSet<_>>().len() != tools.len() {
                return Err("工具列表不能包含重复项".to_owned());
            }
            Some(tools)
        }
        None => None,
    };
    // None 表示跟随会话 Provider：省略 frontmatter 的 model 字段。
    let model = model
        .as_deref()
        .map(str::trim)
        .map(normalize_model_reference)
        .transpose()?;

    let agents_dir = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
        .join("agents");
    if let Ok(metadata) = fs::symlink_metadata(&agents_dir)
        && (metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err(format!(
            "子智能体路径必须是普通目录：{}",
            agents_dir.display()
        ));
    }
    let path = agents_dir.join(format!("{name}.md"));
    if path.exists() || peri_middlewares::subagent::get_built_in_agent(name).is_some() {
        return Err(format!("子智能体 {name} 已存在"));
    }

    let name_yaml =
        serde_json::to_string(name).map_err(|error| format!("无法序列化子智能体名称：{error}"))?;
    let description_yaml = serde_json::to_string(description)
        .map_err(|error| format!("无法序列化子智能体说明：{error}"))?;
    // None 表示继承主智能体全部工具：省略 frontmatter 的 tools 字段（Inherit），
    // 显式列表才写入 `tools: [...]`（List）；两者由 peri 解析为不同权限语义。
    let tools_line = tools
        .as_ref()
        .map(|tools| {
            serde_json::to_string(tools)
                .map(|yaml| format!("tools: {yaml}\n"))
                .map_err(|error| format!("无法序列化工具列表：{error}"))
        })
        .transpose()?
        .unwrap_or_default();
    let max_turns_yaml = max_turns
        .map(|value| format!("maxTurns: {value}\n"))
        .unwrap_or_default();
    let model_yaml = model
        .map(|value| {
            serde_json::to_string(&value)
                .map(|yaml| format!("model: {yaml}\n"))
                .map_err(|error| format!("无法序列化模型覆盖：{error}"))
        })
        .transpose()?
        .unwrap_or_default();
    let content = format!(
        "---\nname: {name_yaml}\ndescription: {description_yaml}\n{model_yaml}{tools_line}{max_turns_yaml}---\n\n{prompt}\n"
    );
    peri_middlewares::parse_agent_file(&content)
        .ok_or_else(|| "生成的子智能体定义无效".to_owned())?;
    atomic_write_private(&path, content.as_bytes())
}

/// 删除 KeenCode 全局目录中的一个子智能体定义。
#[tauri::command]
pub fn agent_remove(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = name.trim();
    validate_agent_id(name)?;
    let path = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
        .join("agents")
        .join(format!("{name}.md"));
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("找不到全局子智能体 {name}：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("子智能体定义必须是普通文件：{}", path.display()));
    }
    fs::remove_file(&path).map_err(|error| format!("无法删除子智能体 {name}：{error}"))
}

/// 更新子智能体的模型覆盖字段。
///
/// 全局定义（`~/.keencode/agents/{name}.md` 存在）：只修改 frontmatter 的
/// `model:` 键，系统提示、工具等其余内容原样保留。内置定义：写入
/// `agent-model-overrides.json` 覆盖表，peri 在加载内置定义时套用。
/// `model` 编码为 `"{provider_id}::{model}"`；None 表示清除覆盖，恢复为
/// 跟随会话 provider。
#[tauri::command]
pub fn agent_update(
    name: String,
    model: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = name.trim();
    validate_agent_id(name)?;
    let model = model
        .as_deref()
        .map(str::trim)
        .map(normalize_model_reference)
        .transpose()?;
    let path = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
        .join("agents")
        .join(format!("{name}.md"));
    match fs::symlink_metadata(&path) {
        // symlink_metadata 对符号链接返回 link 类型：is_file 为 false，落入下方分支。
        Ok(metadata) if metadata.file_type().is_file() => {
            let content = fs::read_to_string(&path)
                .map_err(|error| format!("无法读取子智能体定义：{error}"))?;
            let updated = set_frontmatter_model(&content, model.as_deref())?;
            // 运行时按 claude_agent_parser 宽松解析（Claude Code 兼容字段共存），
            // 写入前整体校验防止生成不可解析的文件。
            if peri_middlewares::claude_agent_parser::parse_agent_file(&updated).is_none() {
                return Err("更新后的子智能体定义无效".to_owned());
            }
            atomic_write_private(&path, updated.as_bytes())
        }
        Ok(_) => Err(format!("子智能体定义必须是普通文件：{}", path.display())),
        Err(_) => {
            if peri_middlewares::subagent::get_built_in_agent(name).is_some() {
                write_agent_model_override(&app, name, model.as_deref())
            } else {
                Err(format!("找不到全局子智能体 {name}"))
            }
        }
    }
}

/// 规范化子智能体模型覆盖引用：只允许 `providerId::modelId`。
///
/// 空值由命令层解释为跟随当前会话，这个函数只处理非空覆盖值。两段会
/// Some 值去除边界空白并拒绝控制字符；None 由调用方解释为跟随当前会话。
fn normalize_model_reference(value: &str) -> Result<String, String> {
    let value = value.trim();
    let Some((raw_provider, raw_model)) = value.split_once("::") else {
        return Err(format!(
            "模型覆盖格式无效（应为 providerId::modelId）：{value}"
        ));
    };
    if raw_provider.chars().any(char::is_control) || raw_model.chars().any(char::is_control) {
        return Err(format!(
            "模型覆盖格式无效（应为 providerId::modelId）：{value}"
        ));
    }
    let provider = raw_provider.trim();
    let model = raw_model.trim();
    if provider.is_empty() || model.is_empty() || model.contains("::") {
        return Err(format!(
            "模型覆盖格式无效（应为 providerId::modelId）：{value}"
        ));
    }
    Ok(format!("{provider}::{model}"))
}

/// 设置页只展示合法的 `provider_id::model` 引用。
fn normalize_model_reference_for_ui(value: &str) -> Option<String> {
    normalize_model_reference(value).ok()
}

/// 内置子智能体模型覆盖表路径（`~/.keencode/agent-model-overrides.json`）。
/// peri 侧经 `PERI_AGENT_MODEL_OVERRIDES` 环境变量在加载内置定义时套用。
fn agent_model_overrides_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|directory| directory.join("agent-model-overrides.json"))
        .map_err(|error| format!("无法确定模型覆盖表路径：{error}"))
}

/// 读取覆盖表：文件不存在视为空表；存在但损坏时报错，不静默重置用户数据。
fn read_agent_model_overrides(
    app: &AppHandle,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let path = agent_model_overrides_path(app)?;
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Default::default());
        }
        Err(error) => return Err(format!("无法读取模型覆盖表：{error}")),
    };
    let overrides =
        match serde_json::from_str::<std::collections::BTreeMap<String, String>>(&content) {
            Ok(overrides) => overrides,
            Err(error) => {
                tracing::warn!(%error, "模型覆盖表损坏，本次按空配置继续");
                return Ok(Default::default());
            }
        };
    for (agent_id, model) in &overrides {
        if let Err(error) = validate_agent_id(agent_id) {
            tracing::warn!(%error, %agent_id, "忽略无效的子智能体模型覆盖");
            continue;
        }
        if let Err(error) = normalize_model_reference(model) {
            tracing::warn!(%error, %agent_id, "忽略无效的子智能体模型引用");
        }
    }
    Ok(overrides
        .into_iter()
        .filter(|(agent_id, model)| {
            validate_agent_id(agent_id).is_ok() && normalize_model_reference(model).is_ok()
        })
        .collect())
}

/// 写入内置子智能体的模型覆盖；None 表示移除覆盖、恢复定义默认值。
fn write_agent_model_override(
    app: &AppHandle,
    name: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let mut overrides = read_agent_model_overrides(app)?;
    match model {
        Some(value) => {
            overrides.insert(name.to_owned(), value.to_owned());
        }
        None => {
            overrides.remove(name);
        }
    }
    let path = agent_model_overrides_path(app)?;
    let content = serde_json::to_string_pretty(&overrides)
        .map_err(|error| format!("无法序列化模型覆盖表：{error}"))?;
    atomic_write_private(&path, content.as_bytes())
}

/// 在 YAML frontmatter 中插入、替换或删除顶层 `model:` 键，其余行原样保留。
///
/// `model` 为 None 时删除现有键（回退跟随会话 provider）；值按 JSON 字符串
/// 序列化写入（与 `agent_create` 的 YAML 生成方式一致）。
fn set_frontmatter_model(content: &str, model: Option<&str>) -> Result<String, String> {
    if !content.starts_with("---") {
        return Err("子智能体文件缺少 YAML frontmatter".to_owned());
    }
    let lines = content.split_inclusive('\n').collect::<Vec<_>>();
    let close_index = lines[1..]
        .iter()
        .position(|line| line.trim() == "---")
        .map(|index| index + 1)
        .ok_or_else(|| "子智能体文件缺少闭合的 '---' 分隔符".to_owned())?;
    let existing = lines[1..close_index].iter().position(|line| {
        let line = line.trim_end_matches('\n');
        line.starts_with("model:") || line.starts_with("model :")
    });
    let mut kept = lines[..close_index]
        .iter()
        .map(|line| (*line).to_string())
        .collect::<Vec<_>>();
    let body = lines[close_index..].concat();
    match (model, existing) {
        (Some(value), Some(index)) => {
            let value_yaml = serde_json::to_string(value)
                .map_err(|error| format!("无法序列化模型覆盖：{error}"))?;
            kept[index + 1] = format!("model: {value_yaml}\n");
        }
        (Some(value), None) => {
            let value_yaml = serde_json::to_string(value)
                .map_err(|error| format!("无法序列化模型覆盖：{error}"))?;
            kept.insert(close_index, format!("model: {value_yaml}\n"));
        }
        (None, Some(index)) => {
            kept.remove(index + 1);
        }
        (None, None) => {}
    }
    Ok(kept.concat() + body.as_str())
}

/// 列出 KeenCode 唯一 MCP 配置中的 Server。
#[tauri::command]
pub fn inspect_mcp(
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<InspectMcpResult, String> {
    let _guard = state.lock_io()?;
    let (resolved, _) = load_effective_mcp(&app)?;
    let servers = resolved
        .into_iter()
        .map(|(name, server)| mcp_dto(name, server))
        .collect();
    Ok(InspectMcpResult { servers })
}

/// 列出 KeenCode 管理的本地插件。
#[tauri::command]
pub fn plugins_list(
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<PluginsListResult, String> {
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(&app)?;
    let installed = manager.load_state().map_err(|error| error.to_string())?;
    let project_dir =
        std::env::current_dir().map_err(|error| format!("无法确定当前目录：{error}"))?;
    let snapshot = claude_runtime_snapshot(&app, &project_dir)?;
    let by_id = snapshot
        .plugins
        .iter()
        .map(|plugin| (plugin.id.clone(), plugin))
        .collect::<BTreeMap<_, _>>();
    let mut plugins = Vec::new();
    for record in installed.plugins {
        let manifest =
            load_plugin_manifest(&record.install_path).map_err(|error| error.to_string())?;
        let provides = by_id
            .get(&record.id)
            .map(|plugin| claude_plugin_provides(plugin))
            .unwrap_or_else(|| PluginProvidesDto {
                commands: manifest.commands.paths.len(),
                skills: manifest.skills.paths.len(),
                agents: manifest.agents.paths.len(),
                hooks: usize::from(manifest.hooks.is_some()),
                mcp: manifest.mcp_servers.inline.len() + manifest.mcp_servers.files.len(),
                lsp: manifest.lsp_servers.len(),
            });
        let unsupported_hooks = by_id
            .get(&record.id)
            .map(|plugin| plugin.unsupported_hooks.clone())
            .unwrap_or_default();
        plugins.push(PluginDto {
            name: record.id.to_string(),
            version: manifest.version,
            marketplace: record.id.marketplace,
            path: path_to_frontend(&record.install_path),
            enabled: record.enabled,
            provides,
            unsupported_hooks,
        });
    }
    plugins.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(PluginsListResult { plugins })
}

/// 启用一个 KeenCode 管理的本地插件。
#[tauri::command]
pub fn plugin_enable(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::peri_runtime::PeriRuntime>>,
) -> Result<(), String> {
    set_claude_plugin_enabled(&app, &state, &runtime, &name, true)
}

/// 禁用一个 KeenCode 管理的本地插件。
#[tauri::command]
pub fn plugin_disable(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::peri_runtime::PeriRuntime>>,
) -> Result<(), String> {
    set_claude_plugin_enabled(&app, &state, &runtime, &name, false)
}

/// 修改 Claude 插件启用状态并立即刷新 Skills、Agents、Hooks 与 MCP 投影。
fn set_claude_plugin_enabled(
    app: &AppHandle,
    state: &State<'_, ExtensionsState>,
    runtime: &State<'_, std::sync::Arc<crate::peri_runtime::PeriRuntime>>,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(app)?;
    let id = resolve_installed_claude_id(&manager, name)?;
    manager
        .set_enabled(&id, enabled)
        .map_err(|error| error.to_string())?;
    runtime
        .reload_plugins(app)
        .map_err(|error| format!("Claude 插件状态变更后热加载失败：{error}"))
}

/// 从 KeenCode 本地插件清单中卸载一个插件，不删除用户的来源目录。
#[tauri::command]
pub fn plugin_uninstall(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::peri_runtime::PeriRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(&app)?;
    let id = resolve_installed_claude_id(&manager, &name)?;
    let mut secrets = state
        .claude_secrets
        .lock()
        .map_err(|_| "Claude 插件敏感配置锁已损坏".to_owned())?;
    manager
        .uninstall(&id, &mut *secrets)
        .map_err(|error| error.to_string())?;
    // reload_plugins 会重新读取敏感配置；不能在热加载期间持有同一把锁。
    drop(secrets);
    drop(_guard);
    runtime
        .reload_plugins(&app)
        .map_err(|error| format!("插件卸载后热加载失败：{error}"))?;
    Ok(())
}

/// 返回一个本地插件的安全详情。
#[tauri::command]
pub fn plugin_details(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<PluginDetailsResult, String> {
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(&app)?;
    let id = resolve_installed_claude_id(&manager, &name)?;
    let record = manager
        .load_state()
        .map_err(|error| error.to_string())?
        .plugins
        .into_iter()
        .find(|plugin| plugin.id == id)
        .ok_or_else(|| format!("找不到已安装插件 {name}"))?;
    let metadata = load_plugin_manifest(&record.install_path).map_err(|error| error.to_string())?;
    let mut details = vec![format!("名称：{}", id)];
    if let Some(version) = metadata.version.as_deref() {
        details.push(format!("版本：{version}"));
    }
    if let Some(description) = metadata.description.as_deref() {
        details.push(format!("说明：{description}"));
    }
    details.push(format!("目录：{}", path_to_frontend(&record.install_path)));
    if let Some(marketplace) = id.marketplace.as_deref() {
        details.push(format!("市场：{marketplace}"));
    }
    details.push(format!(
        "组件：{} Commands、{} Skills、{} Agents、{} MCP、{} LSP、{} Hooks",
        metadata.commands.paths.len(),
        metadata.skills.paths.len(),
        metadata.agents.paths.len(),
        metadata.mcp_servers.inline.len() + metadata.mcp_servers.files.len(),
        metadata.lsp_servers.len(),
        usize::from(metadata.hooks.is_some())
    ));
    Ok(PluginDetailsResult {
        name: id.to_string(),
        details: details.join("\n"),
    })
}

/// 返回 Claude 插件 userConfig 定义与非敏感当前值。
#[tauri::command]
pub fn plugin_user_config_get(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<PluginUserConfigResult, String> {
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(&app)?;
    let id = resolve_installed_claude_id(&manager, &name)?;
    let installed = manager
        .load_state()
        .map_err(|error| error.to_string())?
        .plugins
        .into_iter()
        .find(|plugin| plugin.id == id)
        .ok_or_else(|| format!("找不到已安装插件 {id}"))?;
    let manifest =
        load_plugin_manifest(&installed.install_path).map_err(|error| error.to_string())?;
    let fields = manifest
        .user_config
        .into_iter()
        .map(|(name, definition)| PluginUserConfigFieldDto {
            name: name.clone(),
            value_type: serde_json::to_value(&definition.value_type)
                .ok()
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| "string".to_owned()),
            title: definition.title,
            description: definition.description,
            required: definition.required,
            sensitive: definition.sensitive,
            multiple: definition.multiple,
            enum_values: definition.enum_values,
            min: definition.min,
            max: definition.max,
            default: (!definition.sensitive)
                .then_some(definition.default)
                .flatten(),
            value: (!definition.sensitive)
                .then(|| installed.public_user_config.get(&name).cloned())
                .flatten(),
        })
        .collect();
    Ok(PluginUserConfigResult {
        plugin: id.to_string(),
        fields,
    })
}

/// 校验并保存 Claude 插件 userConfig，保存后立即热刷新运行时。
#[tauri::command]
pub fn plugin_user_config_set(
    name: String,
    values: BTreeMap<String, Value>,
    replace: Option<bool>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::peri_runtime::PeriRuntime>>,
) -> Result<PluginUserConfigResult, String> {
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(&app)?;
    let id = resolve_installed_claude_id(&manager, &name)?;
    let mut secrets = state
        .claude_secrets
        .lock()
        .map_err(|_| "Claude 插件敏感配置锁已损坏".to_owned())?;
    manager
        .update_user_config(
            &id,
            UserConfigUpdate {
                values,
                replace: replace.unwrap_or(false),
            },
            &mut *secrets,
        )
        .map_err(|error| error.to_string())?;
    // 热刷新路径会再次锁定 claude_secrets，必须先释放本次写入锁。
    drop(secrets);
    drop(_guard);
    runtime
        .reload_plugins(&app)
        .map_err(|error| format!("插件配置保存后热刷新失败：{error}"))?;
    plugin_user_config_get(id.to_string(), app, state)
}

/// 从本地目录或已添加的本地市场安装一个插件引用。
#[tauri::command]
pub async fn plugin_install(source: String, app: AppHandle) -> Result<(), String> {
    let runtime = app
        .state::<std::sync::Arc<crate::peri_runtime::PeriRuntime>>()
        .inner()
        .clone();
    runtime.log("info", "ipc.plugin_install", "命令进入");
    let result = tauri::async_runtime::spawn_blocking(move || plugin_install_blocking(source, app))
        .await
        .map_err(|error| format!("插件安装线程异常：{error}"))?;
    match &result {
        Ok(()) => runtime.log("info", "ipc.plugin_install", "命令完成"),
        Err(error) => runtime.log("error", "ipc.plugin_install", error),
    }
    result
}

/// 在 Tauri blocking 线程中执行插件安装；远程取得不会阻塞窗口线程。
fn plugin_install_blocking(source: String, app: AppHandle) -> Result<(), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("插件来源不能为空".to_owned());
    }
    let root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定插件缓存目录：{error}"))?
        .join("claude-plugins");
    let downloads_root = root.join("downloads");
    let download_cleanup = TemporaryPluginDownloads::new(&downloads_root)?;
    let downloads = download_cleanup.path().to_path_buf();
    let materials = if let Ok(requested) = PluginId::parse(source)
        && requested.marketplace.is_some()
        && !Path::new(source).exists()
    {
        let markets = load_marketplace_store(&app)?;
        let market = markets
            .sources
            .into_iter()
            .find(|market| {
                requested
                    .marketplace
                    .as_deref()
                    .is_some_and(|name| market.name.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| {
                format!(
                    "找不到插件市场 {}",
                    requested.marketplace.as_deref().unwrap_or_default()
                )
            })?;
        let manifest = crate::claude_plugins::parse_marketplace_manifest(
            &fs::read(&market.manifest_path)
                .map_err(|error| format!("无法读取市场清单：{error}"))?,
        )
        .map_err(|error| error.to_string())?;
        resolve_marketplace_plugin_install_plan(&requested, &market, &manifest, &downloads)?
    } else {
        let (materialized_root, _) = materialize_claude_source(source, &downloads)?;
        let manifest =
            load_plugin_manifest(&materialized_root).map_err(|error| error.to_string())?;
        if !manifest.dependencies.is_empty() {
            return Err(
                "本地插件声明了依赖，但没有对应的 Claude marketplace 清单可解析".to_owned(),
            );
        }
        let id = PluginId {
            plugin: manifest.name.clone(),
            marketplace: Some("local".to_owned()),
        };
        vec![MaterializedPlugin {
            id,
            source_root: materialized_root,
        }]
    };
    // 来源物化（尤其 Git/npm）可能耗时数分钟，不持有配置锁；否则此期间
    // 任何插件列表或设置命令都会在窗口线程上等待同一把锁。
    let state = app.state::<ExtensionsState>();
    let runtime = app.state::<std::sync::Arc<crate::peri_runtime::PeriRuntime>>();
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(&app)?;
    let mut secrets = state
        .claude_secrets
        .lock()
        .map_err(|_| "Claude 插件敏感配置锁已损坏".to_owned())?;
    manager
        .install_from_directories(materials, UserConfigUpdate::default(), &mut *secrets)
        .map_err(|error| error.to_string())?;
    // reload_plugins -> runtime_skill_roots -> claude_runtime_snapshot
    // 会再次读取 claude_secrets；必须先释放本次安装持有的锁，否则安装
    // 成功后会在热加载阶段自锁，表现为点击安装永久卡住。
    drop(secrets);
    drop(_guard);
    drop(download_cleanup);
    runtime
        .reload_plugins(&app)
        .map_err(|error| format!("插件安装后热加载失败：{error}"))?;
    Ok(())
}

/// 重新解析一个或全部已安装插件及其依赖，并按拓扑顺序原子更新。
#[tauri::command]
pub async fn plugin_update(name: Option<String>, app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || plugin_update_blocking(name, app))
        .await
        .map_err(|error| format!("插件更新线程异常：{error}"))?
}

/// 在 Tauri blocking 线程中取得远程来源并提交插件更新。
fn plugin_update_blocking(name: Option<String>, app: AppHandle) -> Result<(), String> {
    let selected = {
        let state = app.state::<ExtensionsState>();
        let _guard = state.lock_io()?;
        let manager = claude_plugin_manager(&app)?;
        let installed = manager.load_state().map_err(|error| error.to_string())?;
        let target = name
            .as_deref()
            .map(|value| resolve_installed_claude_id(&manager, value))
            .transpose()?;
        let selected = installed
            .plugins
            .into_iter()
            .filter(|record| target.as_ref().is_none_or(|id| id == &record.id))
            .collect::<Vec<_>>();
        if target.is_some() && selected.is_empty() {
            return Err("找不到要更新的 Claude 插件".to_owned());
        }
        selected
    };

    // 所有远程取得、Git/npm/HTTP 和依赖清单解析都在锁外执行。临时下载目录
    // 由 guard 持有到状态提交完成，失败或成功都不会残留本次 fetch/synthetic。
    let root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定插件缓存目录：{error}"))?
        .join("claude-plugins");
    let downloads_root = root.join("downloads");
    let download_cleanup = TemporaryPluginDownloads::new(&downloads_root)?;
    let downloads = download_cleanup.path().to_path_buf();
    let markets = load_marketplace_store(&app)?;
    let mut market_snapshots = BTreeMap::<String, (MarketplaceRecord, Vec<u8>)>::new();
    let mut plan = BTreeMap::<PluginId, MaterializedPlugin>::new();
    let mut plan_order = Vec::new();
    for record in &selected {
        let Some(marketplace) = record.id.marketplace.as_deref() else {
            return Err(format!("插件记录 {} 缺少市场命名空间", record.id));
        };
        if marketplace.eq_ignore_ascii_case("local") {
            let manifest =
                load_plugin_manifest(&record.install_path).map_err(|error| error.to_string())?;
            if !manifest.name.eq_ignore_ascii_case(&record.id.plugin) {
                return Err(format!("插件记录 {} 与 plugin.json 名称不一致", record.id));
            }
            if !manifest.dependencies.is_empty() {
                return Err(format!(
                    "本地插件 {} 声明了依赖，但没有对应的 Claude marketplace 清单可解析",
                    record.id
                ));
            }
            let materialized = MaterializedPlugin {
                id: record.id.clone(),
                source_root: record.install_path.clone(),
            };
            let id = materialized.id.clone();
            if plan.insert(id.clone(), materialized).is_none() {
                plan_order.push(id);
            }
            continue;
        }
        let market = markets
            .sources
            .iter()
            .find(|market| market.name.eq_ignore_ascii_case(marketplace))
            .ok_or_else(|| format!("找不到插件市场 {marketplace}"))?;
        let manifest_bytes = fs::read(&market.manifest_path)
            .map_err(|error| format!("无法读取市场清单：{error}"))?;
        let manifest = crate::claude_plugins::parse_marketplace_manifest(&manifest_bytes)
            .map_err(|error| error.to_string())?;
        market_snapshots
            .entry(marketplace_name_key(&market.name))
            .or_insert_with(|| (market.clone(), manifest_bytes));
        for materialized in
            resolve_marketplace_plugin_install_plan(&record.id, market, &manifest, &downloads)?
        {
            let id = materialized.id.clone();
            if plan.insert(id.clone(), materialized).is_none() {
                plan_order.push(id);
            }
        }
    }

    let materials = plan_order
        .into_iter()
        .filter_map(|id| plan.remove(&id))
        .collect::<Vec<_>>();
    let state = app.state::<ExtensionsState>();
    let _guard = state.lock_io()?;
    let manager = claude_plugin_manager(&app)?;
    let latest = manager.load_state().map_err(|error| error.to_string())?;
    ensure_plugin_update_snapshot_current(&selected, &latest)?;
    let latest_markets = load_marketplace_store(&app)?;
    for (key, (expected, manifest_bytes)) in &market_snapshots {
        let current = latest_markets
            .sources
            .iter()
            .find(|market| marketplace_name_key(&market.name) == *key)
            .ok_or_else(|| format!("插件更新期间市场 {} 已被移除，已放弃提交", expected.name))?;
        if current.name != expected.name
            || current.path != expected.path
            || current.manifest_path != expected.manifest_path
        {
            return Err(format!(
                "插件更新期间市场 {} 记录已改变，已放弃提交",
                expected.name
            ));
        }
        let current_manifest = fs::read(&current.manifest_path)
            .map_err(|error| format!("插件更新期间无法读取市场 {}：{error}", current.name))?;
        if current_manifest.as_slice() != manifest_bytes.as_slice() {
            return Err(format!(
                "插件更新期间市场 {} 清单已改变，已放弃提交",
                current.name
            ));
        }
    }
    if !materials.is_empty() {
        let mut secrets = state
            .claude_secrets
            .lock()
            .map_err(|_| "Claude 插件敏感配置锁已损坏".to_owned())?;
        manager
            .install_from_directories(materials, UserConfigUpdate::default(), &mut *secrets)
            .map_err(|error| error.to_string())?;
        drop(secrets);
    }
    drop(_guard);
    drop(download_cleanup);
    let runtime = app.state::<std::sync::Arc<crate::peri_runtime::PeriRuntime>>();
    runtime
        .reload_plugins(&app)
        .map_err(|error| format!("插件更新后热加载失败：{error}"))?;
    Ok(())
}

fn ensure_plugin_update_snapshot_current(
    expected: &[InstalledPlugin],
    current: &crate::claude_plugins::PluginState,
) -> Result<(), String> {
    for expected_plugin in expected {
        let Some(current_plugin) = current
            .plugins
            .iter()
            .find(|plugin| plugin.id == expected_plugin.id)
        else {
            return Err(format!(
                "插件更新期间 {} 已被卸载，已放弃提交",
                expected_plugin.id
            ));
        };
        if !same_installed_plugin_snapshot(expected_plugin, current_plugin) {
            return Err(format!(
                "插件更新期间 {} 状态已改变，已放弃提交",
                expected_plugin.id
            ));
        }
    }
    Ok(())
}

fn same_installed_plugin_snapshot(left: &InstalledPlugin, right: &InstalledPlugin) -> bool {
    left.id == right.id
        && left.version == right.version
        && left.install_path == right.install_path
        && left.enabled == right.enabled
        && left.public_user_config == right.public_user_config
        && left.sensitive_user_config_keys == right.sensitive_user_config_keys
        && left.secret_generation == right.secret_generation
}

/// 向 KeenCode 唯一 MCP 配置添加一个 stdio Server。
#[tauri::command]
pub fn mcp_add(
    name: String,
    command: String,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_extension_name(&name, "MCP Server")?;
    let command = command.trim();
    if command.is_empty() {
        return Err("MCP Server 命令不能为空".to_owned());
    }
    if command.contains('\r') || command.contains('\n') {
        return Err("MCP Server 命令不能包含换行符".to_owned());
    }
    let path = mcp_user_config_path(&app)?;
    let mut document =
        load_mcp_document_fail_closed(&app, &path)?.unwrap_or_else(empty_mcp_document);
    if mcp_server_map(&document)?.contains_key(&name) {
        return Err(format!("MCP Server {name} 已存在于 {}", path.display()));
    }
    let mut config = Map::new();
    config.insert("command".to_owned(), Value::String(command.to_owned()));
    config.insert(
        "args".to_owned(),
        Value::Array(
            args.unwrap_or_default()
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    config.insert(
        "env".to_owned(),
        serde_json::to_value(env.unwrap_or_default())
            .map_err(|error| format!("无法序列化 MCP 环境变量：{error}"))?,
    );
    mcp_server_map_mut(&mut document)?.insert(name, Value::Object(config));
    save_mcp_document(&path, &document)?;
    publish_mcp_runtime_config(&app)?;
    Ok(())
}

/// 导入厂商提供的 MCP JSON 配置。
///
/// 接受两种等价的当前格式：
/// - `{"mcpServers":{"name":{...}}}`
/// - `{"name":{...}}`
///
/// 导入会先在内存中完成完整解析、校验和冲突检查，再一次性写入用户配置；
/// 任意一个 Server 冲突都会使整个导入失败，不会留下部分结果。
#[tauri::command]
pub fn mcp_import(
    config: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let imported =
        parse_mcp_import_text(&config).map_err(|error| format!("MCP 导入配置无效：{error}"))?;
    let imported_servers = mcp_server_map(&imported)?.clone();
    if imported_servers.is_empty() {
        return Err("MCP 导入配置至少需要包含一个 Server".to_owned());
    }

    let path = mcp_user_config_path(&app)?;
    let existing = load_mcp_document_fail_closed(&app, &path)?.unwrap_or_else(empty_mcp_document);
    let merged = merge_mcp_documents(existing, imported)?;
    save_mcp_document(&path, &merged)?;
    publish_mcp_runtime_config(&app)?;
    Ok(())
}

/// 从 KeenCode 唯一 MCP 配置删除一个 Server。
#[tauri::command]
pub fn mcp_remove(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_extension_name(&name, "MCP Server")?;
    let path = mcp_user_config_path(&app)?;
    let Some(mut document) = load_mcp_document_fail_closed(&app, &path)? else {
        return Err(format!("找不到 MCP Server {name}"));
    };
    if mcp_server_map_mut(&mut document)?.remove(&name).is_none() {
        return Err(format!("找不到 MCP Server {name}"));
    }
    save_mcp_document(&path, &document)?;
    publish_mcp_runtime_config(&app)?;
    Ok(())
}

/// 对 MCP 配置结构和本机 stdio 命令可用性执行无副作用检查。
#[tauri::command]
pub fn mcp_doctor(
    focus: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<McpDoctorReport, String> {
    let _guard = state.lock_io()?;
    let focus = focus
        .as_deref()
        .map(|value| validate_extension_name(value, "MCP Server"))
        .transpose()?;
    let (resolved, sources) = load_effective_mcp(&app)?;
    let mut servers = Vec::new();
    for (name, server) in resolved {
        if focus
            .as_deref()
            .is_some_and(|focus| !name.eq_ignore_ascii_case(focus))
        {
            continue;
        }
        servers.push(doctor_server(name, server));
    }
    let summary = McpDoctorSummary {
        healthy: servers.iter().filter(|server| server.healthy).count(),
        unhealthy: servers.iter().filter(|server| !server.healthy).count(),
        total: servers.len(),
    };
    if let Some(focus) = focus.as_deref()
        && servers.is_empty()
    {
        return Err(format!("未找到 MCP Server {focus}"));
    }
    let raw_text = if servers.is_empty() {
        Some("未配置 MCP Server".to_owned())
    } else {
        None
    };
    Ok(McpDoctorReport {
        ok: summary.unhealthy == 0,
        servers,
        sources,
        summary,
        raw_text,
    })
}

/// 列出用户显式添加的本地插件市场。
#[tauri::command]
pub fn marketplace_list(
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<Vec<MarketplaceSourceDto>, String> {
    let _guard = state.lock_io()?;
    let store = load_marketplace_store(&app)?;
    let sources = store
        .sources
        .into_iter()
        .map(|source| MarketplaceSourceDto {
            name: source.name,
            path: path_text_to_frontend(&source.path),
        })
        .collect();
    Ok(sources)
}

/// 列出本地市场中能够在本机解析且尚未安装的插件。
#[tauri::command]
pub fn marketplace_available(
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<MarketplaceAvailableResult, String> {
    let _guard = state.lock_io()?;
    let marketplace_store = load_marketplace_store(&app)?;
    let store_path = marketplace_store_path(&app)?;
    let store_exists = current_regular_file_exists(&store_path, "插件市场清单")?;
    let has_default = marketplace_store.sources.iter().any(|source| {
        source
            .name
            .eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME)
    });
    let default_needs_fetch = marketplace_store.sources.iter().any(|source| {
        source
            .name
            .eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME)
            && !marketplace_record_is_materialized(source)
    });
    let mut bootstrap_view = if (!store_exists && !has_default) || default_needs_fetch {
        // 已登记但目录/清单失效时必须绕过 Ready 状态重新取得；空的
        // marketplaces.json 仍表示用户显式删除市场，不能因此强制恢复默认市场。
        Some(start_default_marketplace_fetch(
            &app,
            &state,
            default_needs_fetch,
        )?)
    } else {
        None
    };
    if marketplace_store.sources.is_empty() {
        let bootstrap = match bootstrap_view.take() {
            Some(view) => view,
            None => marketplace_bootstrap_view(&state)?,
        };
        return Ok(MarketplaceAvailableResult {
            plugins: Vec::new(),
            loading: bootstrap.loading,
            error: bootstrap.error,
        });
    }
    let manager = claude_plugin_manager(&app)?;
    let plugin_store = manager.load_state().map_err(|error| error.to_string())?;
    let installed = plugin_store
        .plugins
        .iter()
        .map(|plugin| plugin.id.to_string().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut plugins = Vec::new();
    for source in marketplace_store.sources {
        // 默认记录损坏时由后台 worker 重取；其间保留其他市场可用，避免旧路径
        // 的读取错误遮蔽整个市场列表。
        if source
            .name
            .eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME)
            && !marketplace_record_is_materialized(&source)
        {
            continue;
        }
        let (root, _) = canonical_marketplace_record_paths(&source)?;
        let catalog = load_claude_marketplace_manifest_from_record(&source)?;
        for plugin in catalog.plugins {
            let id = PluginId {
                plugin: plugin.name.clone(),
                marketplace: Some(catalog.name.clone()),
            };
            if installed.contains(&id.to_string().to_ascii_lowercase()) {
                continue;
            }
            let (description, version, skill_count, lsp_count) = match &plugin.source {
                PluginSource::Relative { path } => {
                    let path = match resolve_marketplace_relative_path(&root, path) {
                        Ok(path) => path,
                        Err(error) => {
                            tracing::warn!(
                                marketplace = %catalog.name,
                                plugin = %plugin.name,
                                error = %error,
                                "跳过越界或不安全的 marketplace 插件目录"
                            );
                            continue;
                        }
                    };
                    if let Err(error) = validate_directory_tree_without_symlinks(&path, "市场插件")
                    {
                        tracing::warn!(
                            marketplace = %catalog.name,
                            plugin = %plugin.name,
                            error = %error,
                            "跳过包含符号链接或特殊文件的 marketplace 插件"
                        );
                        continue;
                    }
                    match load_plugin_manifest(&path) {
                        Ok(manifest) => {
                            let Ok(snapshot) = extract_components(
                                id.clone(),
                                &path,
                                &manifest,
                                Path::new("."),
                                &BTreeMap::new(),
                                &ResolvedUserConfig::default(),
                            ) else {
                                continue;
                            };
                            (
                                manifest.description,
                                manifest.version,
                                snapshot.skills.len(),
                                snapshot.lsp_servers.len(),
                            )
                        }
                        Err(_)
                            if !path
                                .join(crate::claude_plugins::CLAUDE_PLUGIN_MANIFEST)
                                .exists() =>
                        {
                            // Peri 3.6.5 的官方市场允许仅在条目上声明 lspServers；
                            // 此处只验证并展示，安装时才在 KeenCode 缓存副本生成清单。
                            match synthetic_marketplace_plugin_manifest_for_root(&plugin, &path) {
                                Ok(Some(manifest)) => (
                                    manifest.description,
                                    manifest.version,
                                    manifest.skills.paths.len(),
                                    manifest.lsp_servers.len(),
                                ),
                                Ok(None) => continue,
                                Err(error) => {
                                    tracing::warn!(
                                        marketplace = %catalog.name,
                                        plugin = %plugin.name,
                                        error = %error,
                                        "验证 marketplace 合成插件清单失败"
                                    );
                                    continue;
                                }
                            }
                        }
                        Err(_) => continue,
                    }
                }
                _ => match synthetic_marketplace_plugin_manifest(&plugin) {
                    Ok(Some(manifest)) => (
                        plugin.description.clone(),
                        plugin.version.clone(),
                        manifest.skills.paths.len(),
                        manifest.lsp_servers.len(),
                    ),
                    Ok(None) => (plugin.description.clone(), plugin.version.clone(), 0, 0),
                    Err(error) => {
                        tracing::warn!(
                            marketplace = %catalog.name,
                            plugin = %plugin.name,
                            error = %error,
                            "验证远程 marketplace 合成插件清单失败"
                        );
                        (plugin.description.clone(), plugin.version.clone(), 0, 0)
                    }
                },
            };
            plugins.push(AvailablePluginDto {
                name: plugin.name,
                marketplace: source.name.clone(),
                description,
                version,
                skill_count,
                lsp_count,
            });
        }
    }
    plugins.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.marketplace.cmp(&right.marketplace))
    });
    let bootstrap = match bootstrap_view.take() {
        Some(view) => view,
        None => marketplace_bootstrap_view(&state)?,
    };
    Ok(MarketplaceAvailableResult {
        plugins,
        loading: bootstrap.loading,
        error: bootstrap.error,
    })
}

/// 添加一个包含 `.claude-plugin/marketplace.json` 的本地目录或清单文件。
#[tauri::command]
pub async fn marketplace_add(source: String, app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || marketplace_add_blocking(source, app))
        .await
        .map_err(|error| format!("市场添加线程异常：{error}"))?
}

/// 在 blocking 线程中取得并登记市场；网络/Git/npm 取得不持有扩展配置锁。
fn marketplace_add_blocking(source: String, app: AppHandle) -> Result<(), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("市场来源不能为空".to_owned());
    }
    let workspace = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定市场缓存目录：{error}"))?
        .join("claude-plugins/marketplaces");
    let MaterializedMarketplace {
        root,
        manifest_path,
        catalog,
        mut cleanup,
    } = materialize_claude_marketplace(source, &workspace)?;
    crate::claude_plugins::validate_marketplace_name_source(&catalog.name, source)
        .map_err(|error| error.to_string())?;
    let state = app.state::<ExtensionsState>();
    let _guard = state.lock_io()?;
    let mut store = load_marketplace_store(&app)?;
    if store.sources.iter().any(|existing| {
        existing.name.eq_ignore_ascii_case(&catalog.name) || Path::new(&existing.path) == root
    }) {
        return Err(format!("本地市场 {} 已添加", catalog.name));
    }
    store.sources.push(MarketplaceRecord {
        name: catalog.name.clone(),
        path: root.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
    });
    store
        .sources
        .sort_by(|left, right| left.name.cmp(&right.name));
    save_marketplace_store(&app, &store)?;
    if let Some(cleanup) = cleanup.as_mut() {
        cleanup.keep();
    }
    Ok(())
}

/// 删除一个本地插件市场记录，不删除市场目录。
#[tauri::command]
pub fn marketplace_remove(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let target = validate_extension_name(&name, "市场")?;
    let mut store = load_marketplace_store(&app)?;
    let index = store
        .sources
        .iter()
        .position(|source| source.name.eq_ignore_ascii_case(&target))
        .ok_or_else(|| format!("找不到本地市场 {target}"))?;
    store.sources.remove(index);
    save_marketplace_store(&app, &store)?;
    if target.eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME) {
        let mut bootstrap = state
            .marketplace_bootstrap
            .lock()
            .map_err(|_| "插件市场后台状态锁已损坏".to_owned())?;
        bootstrap.invalidate();
    }
    Ok(())
}

/// 重新校验一个或全部本地市场清单；显式刷新时可重新取得默认官方市场。
#[tauri::command]
pub fn marketplace_update(
    name: Option<String>,
    restore_default: bool,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let target = name
        .as_deref()
        .map(|value| validate_extension_name(value, "市场"))
        .transpose()?;
    let store = load_marketplace_store(&app)?;
    let explicit_default = target
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME));
    let has_default = store.sources.iter().any(|source| {
        source
            .name
            .eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME)
    });
    let refresh_default =
        should_refresh_default_marketplace(target.as_deref(), has_default, restore_default);
    if refresh_default {
        // 官方远程源在锁外后台刷新；本命令只触发任务，前端通过
        // marketplace_available 的 loading/error 状态观察结果。
        start_default_marketplace_fetch(&app, &state, true)?;
    }
    let mut updated = 0usize;
    for source in &store.sources {
        if target
            .as_deref()
            .is_some_and(|name| !source.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let _ = load_claude_marketplace_manifest_from_record(source)?;
        updated += 1;
    }
    if let Some(target) = target.as_deref()
        && updated == 0
        && !explicit_default
    {
        return Err(format!("找不到本地市场 {target}"));
    }
    Ok(())
}

fn should_refresh_default_marketplace(
    target: Option<&str>,
    has_default: bool,
    restore_default: bool,
) -> bool {
    restore_default
        || target.is_some_and(|value| value.eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME))
        || (target.is_none() && has_default)
}

/// 返回 KeenCode 唯一的 MCP 配置路径。
pub(crate) fn mcp_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    prepare_mcp_runtime_config(app)
}

/// 返回用户手工维护的 MCP 配置；插件 MCP 不直接写入此文件。
fn mcp_user_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|directory| directory.join("mcp.json"))
        .map_err(|error| format!("无法确定 KeenCode MCP 配置目录：{error}"))
}

/// 返回插件声明的 Agent 根目录与 KeenCode 全局子智能体目录，供 ACP 服务器
/// 装配 `plugin_agent_dirs`（主 Agent 目录渲染；同名去重时全局优先级最低，
/// 不会遮蔽项目或内置定义）。
pub(crate) fn runtime_plugin_agent_dirs(app: &AppHandle) -> Result<Vec<PathBuf>, String> {
    let project_dir =
        std::env::current_dir().map_err(|error| format!("无法确定当前目录：{error}"))?;
    let snapshot = claude_runtime_snapshot(app, &project_dir)?;
    let mut dirs = snapshot
        .plugins
        .iter()
        .flat_map(|plugin| plugin.agents.iter())
        .filter_map(|file| file.path.parent().map(Path::to_path_buf))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let global_agents = crate::storage::root_dir(app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
        .join("agents");
    if !dirs.contains(&global_agents) {
        dirs.push(global_agents);
    }
    Ok(dirs)
}

/// 返回 KeenCode 当前运行时能够加载的用户与插件 Skill 根目录。
pub(crate) fn runtime_skill_roots(
    app: &AppHandle,
    project_dir: &Path,
) -> Result<Vec<peri_middlewares::skills::SkillRoot>, String> {
    use peri_middlewares::skills::{SkillRoot, SkillSource};

    let mut roots = Vec::new();
    let user_root = crate::storage::root_dir(app)
        .map_err(|error| format!("无法确定 KeenCode Skill 数据目录：{error}"))?
        .join("skills");
    if !scan_skill_directory(&user_root)?.is_empty() {
        roots.push(SkillRoot {
            path: user_root,
            source: SkillSource::User,
            plugin_name: None,
        });
    }
    let snapshot = claude_runtime_snapshot(app, project_dir)?;
    for plugin in snapshot.plugins {
        // Each declared SKILL.md parent is itself a valid Peri root. Keeping
        // the leaf directory exact prevents a narrow `skills/foo` declaration
        // from loading undeclared sibling Skills under `skills/`.
        let mut seen = BTreeSet::new();
        for file in plugin.skills {
            if file.path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
                continue;
            }
            let Some(root) = file.path.parent() else {
                continue;
            };
            if seen.insert(root.to_path_buf()) {
                roots.push(SkillRoot {
                    path: root.to_path_buf(),
                    source: SkillSource::Plugin,
                    plugin_name: Some(format!("plugin:{}", plugin.id.plugin)),
                });
            }
        }
    }
    Ok(roots)
}

/// 将插件命令的插件根相对路径转换为 Claude 的稳定命名空间。
///
/// 默认 `commands/foo.md` 映射为 `plugin:demo:foo`，嵌套
/// `commands/admin/check.md` 映射为 `plugin:demo:admin:check`；文件名去掉
/// `.md`，而不是把 `commands` 目录本身暴露到命令名中。
fn plugin_command_namespace(plugin_namespace: &str, relative_path: &Path) -> String {
    let mut components = relative_path.components().collect::<Vec<_>>();
    if components
        .first()
        .is_some_and(|component| component.as_os_str() == "commands")
    {
        components.remove(0);
    }
    let Some(file) = components.pop() else {
        return plugin_namespace.to_owned();
    };
    let mut parts = vec![plugin_namespace.to_owned()];
    parts.extend(
        components
            .into_iter()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .filter(|part| !part.is_empty()),
    );
    let filename = file.as_os_str().to_string_lossy();
    let command = filename.strip_suffix(".md").unwrap_or(&filename);
    if !command.is_empty() {
        parts.push(command.to_owned());
    }
    parts.join(":")
}

/// 返回 KeenCode 本地市场清单路径。
fn marketplace_store_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|dir| dir.join("marketplaces.json"))
        .map_err(|error| format!("无法确定应用配置目录：{error}"))
}

/// 首次使用时取得 Claude Code 官方市场，并转换为 KeenCode 当前记录结构。
fn materialize_default_claude_marketplace(app: &AppHandle) -> Result<MarketplaceRecord, String> {
    let workspace = crate::storage::root_dir(app)
        .map_err(|error| format!("无法确定市场缓存目录：{error}"))?
        .join("claude-plugins/marketplaces");
    let source_preference = crate::app_settings::get(app)
        .map_err(|error| format!("无法读取 GitHub 访问源设置：{error}"))?
        .app_update_download_source;
    let github_url = url::Url::parse(DEFAULT_CLAUDE_MARKETPLACE_REPOSITORY)
        .map_err(|error| format!("Claude Code 官方市场地址无效：{error}"))?;
    let attempts = crate::app_updates::github_url_attempts(source_preference, &github_url)?;
    let mut failures = Vec::new();
    let materialized = attempts
        .into_iter()
        .find_map(|(source, url)| {
            match materialize_claude_marketplace_spec(
                MarketplaceSourceSpec::Git {
                    url: url.to_string(),
                    reference: None,
                    path: None,
                    sparse_paths: vec!["plugins".to_owned(), "external_plugins".to_owned()],
                },
                &workspace,
            ) {
                Ok(value) => Some(value),
                Err(error) => {
                    failures.push(format!("{source:?}: {error}"));
                    None
                }
            }
        })
        .ok_or_else(|| format!("Claude Code 官方市场取得失败：{}", failures.join("；")))?;
    let MaterializedMarketplace {
        root,
        manifest_path,
        catalog,
        mut cleanup,
    } = materialized;
    if catalog.plugins.is_empty() {
        return Err("Claude Code 官方市场清单不包含任何插件".to_owned());
    }
    let record = MarketplaceRecord {
        name: catalog.name,
        path: root.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
    };
    if let Err(error) = crate::claude_plugins::validate_marketplace_name_source(
        &record.name,
        DEFAULT_CLAUDE_MARKETPLACE_SOURCE,
    ) {
        discard_marketplace_record(&record);
        return Err(error.to_string());
    }
    if record.name != DEFAULT_CLAUDE_MARKETPLACE_NAME {
        discard_marketplace_record(&record);
        return Err(format!(
            "Claude Code 官方市场清单名称不符合当前默认配置：{}",
            record.name
        ));
    }
    if let Some(cleanup) = cleanup.as_mut() {
        cleanup.keep();
    }
    Ok(record)
}

/// 读取默认市场后台取得状态，供市场列表把 loading/error 投影给前端。
fn marketplace_bootstrap_view(state: &ExtensionsState) -> Result<MarketplaceBootstrapView, String> {
    state
        .marketplace_bootstrap
        .lock()
        .map_err(|_| "插件市场后台状态锁已损坏".to_owned())
        .map(|status| status.view())
}

/// 启动一次默认官方市场后台取得；调用方不持有 io_lock，多个请求只会触发一个 worker。
fn start_default_marketplace_fetch(
    app: &AppHandle,
    state: &ExtensionsState,
    force: bool,
) -> Result<MarketplaceBootstrapView, String> {
    let generation = {
        let mut bootstrap = state
            .marketplace_bootstrap
            .lock()
            .map_err(|_| "插件市场后台状态锁已损坏".to_owned())?;
        bootstrap
            .should_start(force, Instant::now())
            .then(|| bootstrap.begin())
    };
    if let Some(generation) = generation {
        let worker_app = app.clone();
        let _worker = tauri::async_runtime::spawn_blocking(move || {
            let result = materialize_default_claude_marketplace(&worker_app).and_then(|record| {
                persist_default_claude_marketplace(&worker_app, record, force, generation)
            });
            let state = worker_app.state::<ExtensionsState>();
            if let Ok(mut bootstrap) = state.marketplace_bootstrap.lock() {
                if bootstrap.is_current(generation) {
                    match result {
                        Ok(()) => bootstrap.succeed(),
                        Err(error) => bootstrap.fail(error, Instant::now()),
                    }
                }
            } else {
                tracing::error!("插件市场后台状态锁已损坏，无法发布取得结果");
            }
        });
        // 即使 worker 在本次命令返回前就完成，也要让首次响应保持 loading。
        // 调用方随后再次读取即可看到已登记的市场，避免把竞态下的空列表当成最终结果。
        return Ok(MarketplaceBootstrapView {
            loading: true,
            error: None,
        });
    }
    marketplace_bootstrap_view(state)
}

/// 后台取得成功后在 io_lock 内原子登记市场；显式移除或用户先添加其他来源时不抢回配置。
fn persist_default_claude_marketplace(
    app: &AppHandle,
    record: MarketplaceRecord,
    force: bool,
    generation: u64,
) -> Result<(), String> {
    let result = (|| -> Result<bool, String> {
        let state = app.state::<ExtensionsState>();
        let _guard = state.lock_io()?;
        {
            let bootstrap = state
                .marketplace_bootstrap
                .lock()
                .map_err(|_| "插件市场后台状态锁已损坏".to_owned())?;
            if !bootstrap.is_current(generation) {
                return Ok(false);
            }
        }
        let path = marketplace_store_path(app)?;
        let exists = current_regular_file_exists(&path, "插件市场清单")?;
        let mut store = load_marketplace_store(app)?;
        let previous = store
            .sources
            .iter()
            .find(|source| {
                source
                    .name
                    .eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME)
            })
            .cloned();
        if !force {
            match previous.as_ref() {
                // 用户显式移除或在首次取得期间添加其他来源后，不抢回配置。
                None if exists => return Ok(false),
                // 已有可用的 Claude Code 外部缓存时复用它，不重复替换目录。
                Some(previous) if marketplace_record_is_materialized(previous) => {
                    return Ok(false);
                }
                _ => {}
            }
        }
        store.sources.retain(|source| {
            !source
                .name
                .eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME)
        });
        store.sources.push(record.clone());
        store
            .sources
            .sort_by(|left, right| left.name.cmp(&right.name));
        save_marketplace_store(app, &store)?;
        Ok(true)
    })();
    match result {
        Ok(true) => Ok(()),
        Ok(false) => {
            discard_marketplace_record(&record);
            Ok(())
        }
        Err(error) => {
            // 包括路径读取、状态锁、校验和原子保存失败；任何未登记的新目录
            // 都必须清理，避免下次启动误把半成品当作市场。
            discard_marketplace_record(&record);
            Err(error)
        }
    }
}

/// 删除后台取得但尚未登记的市场目录；失败路径不得留下半成品供后续误读。
fn discard_marketplace_record(record: &MarketplaceRecord) {
    if let Err(error) = fs::remove_dir_all(&record.path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %record.path, %error, "清理插件市场临时目录失败");
    }
}

/// 读取 KeenCode 本地市场清单。
fn load_marketplace_store(app: &AppHandle) -> Result<MarketplaceStore, String> {
    let path = marketplace_store_path(app)?;
    let exists = current_regular_file_exists(&path, "插件市场清单")?;
    let mut store = match read_json_or_default(&path, "插件市场清单").and_then(|store| {
        validate_marketplace_store(&store)?;
        Ok(store)
    }) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "插件市场配置无效，本次按空配置继续");
            MarketplaceStore::default()
        }
    };
    // 首次使用时复用 Claude Code 已经下载好的市场，避免用户必须再次手工添加官方市场。
    // 仅在 KeenCode 自己的登记文件不存在时执行，用户显式移除市场后不会被下次启动重新加回。
    if !exists && store.sources.is_empty() {
        let discovered = discover_claude_known_marketplaces();
        if !discovered.is_empty() {
            store.sources = discovered;
            store
                .sources
                .sort_by(|left, right| left.name.cmp(&right.name));
        }
    }
    Ok(store)
}

/// 返回 Claude Code 已知市场登记文件路径；没有 HOME 时不尝试读取用户配置。
fn claude_known_marketplaces_path() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(CLAUDE_KNOWN_MARKETPLACES))
}

/// 将 Claude Code 已下载的本地市场转换为 KeenCode 当前市场记录。
fn discover_claude_known_marketplaces() -> Vec<MarketplaceRecord> {
    let Some(path) = claude_known_marketplaces_path() else {
        return Vec::new();
    };
    discover_claude_known_marketplaces_from_path(&path)
}

/// 从指定路径读取 Claude Code 已知市场；独立参数便于验证发现逻辑而不修改进程环境。
fn discover_claude_known_marketplaces_from_path(path: &Path) -> Vec<MarketplaceRecord> {
    let Ok(exists) = current_regular_file_exists(path, "Claude Code 已知插件市场") else {
        return Vec::new();
    };
    if !exists {
        return Vec::new();
    }
    let Ok(text) = read_text_limited(path) else {
        return Vec::new();
    };
    let Ok(entries) = serde_json::from_str::<BTreeMap<String, ClaudeKnownMarketplaceRecord>>(&text)
    else {
        return Vec::new();
    };
    let mut records = BTreeMap::<String, MarketplaceRecord>::new();
    for (_key, entry) in entries {
        let Some(install_location) = entry.install_location else {
            continue;
        };
        let Ok(install_location) = expand_tilde(&install_location) else {
            continue;
        };
        let Ok((manifest_path, root)) = locate_claude_marketplace(&install_location) else {
            continue;
        };
        let Ok(catalog) = crate::claude_plugins::load_marketplace_manifest(&root) else {
            continue;
        };
        let Ok(name) = validate_extension_name(&catalog.name, "市场") else {
            continue;
        };
        records.entry(name.clone()).or_insert(MarketplaceRecord {
            name,
            path: root.display().to_string(),
            manifest_path: manifest_path.display().to_string(),
        });
    }
    records.into_values().collect()
}

/// 按市场记录保存的实际清单路径读取 Claude marketplace.json。
fn canonical_marketplace_record_paths(
    source: &MarketplaceRecord,
) -> Result<(PathBuf, PathBuf), String> {
    let root = Path::new(&source.path);
    let manifest = Path::new(&source.manifest_path);
    if manifest.file_name().and_then(|value| value.to_str()) != Some("marketplace.json") {
        return Err(format!(
            "市场清单文件名必须是 marketplace.json：{}",
            source.manifest_path
        ));
    }
    let relative = manifest
        .strip_prefix(root)
        .map_err(|_| "市场清单路径必须位于市场根目录内".to_owned())?;
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法访问市场根目录：{error}"))?;
    let canonical_manifest = canonical_child_without_symlinks(root, relative, "市场清单")?;
    let metadata = fs::symlink_metadata(&canonical_manifest)
        .map_err(|error| format!("无法读取市场清单：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("市场清单必须是普通文件：{}", manifest.display()));
    }
    if !canonical_manifest.starts_with(&canonical_root) {
        return Err("市场清单路径越出市场根目录".to_owned());
    }
    Ok((canonical_root, canonical_manifest))
}

fn load_claude_marketplace_manifest_from_record(
    source: &MarketplaceRecord,
) -> Result<crate::claude_plugins::MarketplaceManifest, String> {
    let (_, manifest) = canonical_marketplace_record_paths(source)?;
    let metadata = fs::metadata(&manifest).map_err(|error| format!("无法读取市场清单：{error}"))?;
    if metadata.len() > MAX_EXTENSION_FILE_BYTES {
        return Err(format!("市场清单超过 {MAX_EXTENSION_FILE_BYTES} 字节"));
    }
    let mut bytes = Vec::new();
    File::open(&manifest)
        .map_err(|error| format!("无法打开市场清单：{error}"))?
        .take(MAX_EXTENSION_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取市场清单：{error}"))?;
    if bytes.len() as u64 > MAX_EXTENSION_FILE_BYTES {
        return Err(format!("市场清单超过 {MAX_EXTENSION_FILE_BYTES} 字节"));
    }
    crate::claude_plugins::parse_marketplace_manifest(&bytes).map_err(|error| error.to_string())
}

/// 判断默认市场记录是否仍指向可读取的当前清单。
///
/// 记录可能来自 Claude Code 的外部缓存，也可能来自 KeenCode 自己的下载目录；
/// 这里只做只读检查，绝不删除或改写现有目录。清单损坏同样视为需要重取。
fn marketplace_record_is_materialized(source: &MarketplaceRecord) -> bool {
    load_claude_marketplace_manifest_from_record(source).is_ok_and(|catalog| {
        !source
            .name
            .eq_ignore_ascii_case(DEFAULT_CLAUDE_MARKETPLACE_NAME)
            || !catalog.plugins.is_empty()
    })
}

/// 原子保存 KeenCode 本地市场清单。
fn save_marketplace_store(app: &AppHandle, store: &MarketplaceStore) -> Result<(), String> {
    validate_marketplace_store(store)?;
    write_json_private(&marketplace_store_path(app)?, store, "插件市场清单")
}

/// 读取当前 JSON 文件；文件不存在时返回首次启动值。
fn read_json_or_default<T>(path: &Path, label: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de> + Default,
{
    if !current_regular_file_exists(path, label)? {
        return Ok(T::default());
    }
    let text = read_text_limited(path)?;
    if text.trim().is_empty() {
        return Err(format!("{label}格式无效，文件不能为空：{}", path.display()));
    }
    serde_json::from_str(&text).map_err(|error| {
        format!(
            "{label}格式无效，文件保持未修改：{}：{error}",
            path.display()
        )
    })
}

/// 判断当前配置文件是否存在；存在时必须是普通文件且不能是符号链接。
fn current_regular_file_exists(path: &Path, label: &str) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("无法读取{label} {}：{error}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        return Err(format!("{label}不能是符号链接：{}", path.display()));
    }
    if !metadata.is_file() {
        return Err(format!("{label}不是普通文件：{}", path.display()));
    }
    Ok(true)
}

/// 读取受大小限制的 UTF-8 文本文件。
fn read_text_limited(path: &Path) -> Result<String, String> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    if metadata.len() > MAX_EXTENSION_FILE_BYTES {
        return Err(format!(
            "文件超过 {} MiB 限制：{}",
            MAX_EXTENSION_FILE_BYTES / 1024 / 1024,
            path.display()
        ));
    }
    fs::read_to_string(path).map_err(|error| format!("无法读取 {}：{error}", path.display()))
}

/// 将可序列化对象以仅当前用户可读写的权限原子写入 JSON 文件。
fn write_json_private<T>(path: &Path, value: &T, label: &str) -> Result<(), String>
where
    T: Serialize,
{
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|error| format!("无法序列化{label}：{error}"))?;
    bytes.push(b'\n');
    atomic_write_private(path, &bytes)
}

/// 使用同目录临时文件原子写入私有数据。
fn atomic_write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    crate::storage::atomic_write_private(path, bytes).map_err(|error| error.to_string())
}

/// 校验扩展名称，避免空名称、控制字符和异常大的状态键。
fn validate_extension_name(raw: &str, label: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 128 {
        return Err(format!("{label} 名称长度必须为 1 到 128 个字符"));
    }
    if name != raw {
        return Err(format!("{label} 名称不能包含首尾空白"));
    }
    if name.chars().any(char::is_control) {
        return Err(format!("{label} 名称不能包含控制字符"));
    }
    Ok(name.to_owned())
}

/// 校验持久化文本必须已按当前写入规则规范化。
fn validate_stored_text(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(format!("{label}不能为空或包含首尾空白、控制字符"));
    }
    Ok(())
}

/// 校验持久化路径必须是没有相对跳转的绝对路径。
fn validate_stored_path(value: &str, label: &str) -> Result<(), String> {
    validate_stored_text(value, label)?;
    let path = Path::new(value);
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(format!("{label}必须是规范化绝对路径：{value}"));
    }
    Ok(())
}

/// 校验本地市场清单完整符合当前唯一的持久化结构。
fn validate_marketplace_store(store: &MarketplaceStore) -> Result<(), String> {
    let mut names = BTreeSet::new();
    let mut roots = BTreeSet::new();
    let mut manifests = BTreeSet::new();
    for source in &store.sources {
        if validate_extension_name(&source.name, "市场")? != source.name {
            return Err(format!("市场名称不是规范格式：{}", source.name));
        }
        let folded_name = marketplace_name_key(&source.name);
        if !names.insert(folded_name) {
            return Err(format!("市场名称重复：{}", source.name));
        }
        validate_stored_path(&source.path, "市场根目录")?;
        if !roots.insert(source.path.as_str()) {
            return Err(format!("市场根目录重复：{}", source.path));
        }
        validate_stored_path(&source.manifest_path, "市场清单路径")?;
        let root = Path::new(&source.path);
        let manifest = Path::new(&source.manifest_path);
        if !manifest.starts_with(root)
            || manifest.file_name().and_then(|value| value.to_str()) != Some("marketplace.json")
        {
            return Err(format!(
                "市场清单路径必须位于市场根目录内且文件名为 marketplace.json：{}",
                source.manifest_path
            ));
        }
        if !manifests.insert(source.manifest_path.as_str()) {
            return Err(format!("市场清单路径重复：{}", source.manifest_path));
        }
    }
    Ok(())
}

/// 校验、去重并排序一组扩展名称。
fn normalized_extension_names(names: Vec<String>, label: &str) -> Result<BTreeSet<String>, String> {
    names
        .into_iter()
        .map(|name| validate_extension_name(&name, label))
        .collect()
}

/// 已通过目录边界校验的 Skill 文件元数据。
struct ScannedSkill {
    /// Skill 稳定名称。
    name: String,
}

/// 严格扫描当前 Skill 根目录，不跟随目录项或 `SKILL.md` 符号链接。
fn scan_skill_directory(dir: &Path) -> Result<Vec<ScannedSkill>, String> {
    let root_metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("无法读取 {}：{error}", dir.display())),
    };
    if root_metadata.file_type().is_symlink() {
        return Err(format!("Skill 根目录不能是符号链接：{}", dir.display()));
    }
    if !root_metadata.is_dir() {
        return Err(format!("Skill 根路径不是目录：{}", dir.display()));
    }
    let canonical_root = fs::canonicalize(dir)
        .map_err(|error| format!("无法规范化 Skill 根目录 {}：{error}", dir.display()))?;
    let entries =
        fs::read_dir(dir).map_err(|error| format!("无法读取 {}：{error}", dir.display()))?;
    let mut skills = Vec::new();
    let mut names = BTreeSet::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("无法读取 {} 中的目录项：{error}", dir.display()))?;
        let entry_type = entry
            .file_type()
            .map_err(|error| format!("无法读取目录项类型 {}：{error}", entry.path().display()))?;
        if entry_type.is_symlink() {
            return Err(format!(
                "Skill 根目录不能包含符号链接目录项：{}",
                entry.path().display()
            ));
        }
        if !entry_type.is_dir() {
            continue;
        }
        let skill_dir = fs::canonicalize(entry.path()).map_err(|error| {
            format!("无法规范化 Skill 目录 {}：{error}", entry.path().display())
        })?;
        if !skill_dir.starts_with(&canonical_root) {
            return Err(format!(
                "Skill 目录必须位于当前根目录内：{}",
                entry.path().display()
            ));
        }
        let manifest = entry.path().join("SKILL.md");
        let manifest_metadata = match fs::symlink_metadata(&manifest) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "无法读取 Skill 文件 {}：{error}",
                    manifest.display()
                ));
            }
        };
        if manifest_metadata.file_type().is_symlink() {
            return Err(format!("SKILL.md 不能是符号链接：{}", manifest.display()));
        }
        if !manifest_metadata.is_file() {
            return Err(format!("SKILL.md 不是普通文件：{}", manifest.display()));
        }
        let canonical_manifest = fs::canonicalize(&manifest)
            .map_err(|error| format!("无法规范化 Skill 文件 {}：{error}", manifest.display()))?;
        if !canonical_manifest.starts_with(&skill_dir)
            || !canonical_manifest.starts_with(&canonical_root)
        {
            return Err(format!(
                "SKILL.md 必须位于当前 Skill 目录内：{}",
                manifest.display()
            ));
        }
        let (name, _description) = parse_skill_file(&canonical_manifest)?;
        if !names.insert(name.to_ascii_lowercase()) {
            return Err(format!("Skill 根目录包含重复名称：{name}"));
        }
        skills.push(ScannedSkill { name });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(skills)
}

/// 读取并解析一个 SKILL.md 的 name 与 description。
fn parse_skill_file(path: &Path) -> Result<(String, String), String> {
    let content = read_text_limited(path)?;
    let fields = parse_yaml_frontmatter(&content)
        .map_err(|error| format!("Skill 无效 {}：{error}", path.display()))?;
    let name = fields
        .get("name")
        .map(String::as_str)
        .ok_or_else(|| format!("Skill 缺少 name：{}", path.display()))
        .and_then(|name| validate_extension_name(name, "Skill"))?;
    let description = fields
        .get("description")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Skill 缺少 description：{}", path.display()))?;
    Ok((name, description))
}

/// 解析 SKILL.md 顶部 YAML 前置元数据中的标量字段。
fn parse_yaml_frontmatter(content: &str) -> Result<BTreeMap<String, String>, String> {
    let content = content.trim_start_matches('\u{feff}');
    let mut lines = content.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Err("缺少 YAML 前置元数据".to_owned());
    }
    let mut frontmatter = Vec::new();
    let mut closed = false;
    for line in lines {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        frontmatter.push(line);
    }
    if !closed {
        return Err("YAML 前置元数据未闭合".to_owned());
    }
    let mut fields = BTreeMap::new();
    let mut index = 0usize;
    while index < frontmatter.len() {
        let line = frontmatter[index];
        index += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let Some((key, raw_value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let raw_value = raw_value.trim();
        if matches!(raw_value, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            let folded = raw_value.starts_with('>');
            let mut block = Vec::new();
            while index < frontmatter.len() {
                let candidate = frontmatter[index];
                if !candidate.trim().is_empty()
                    && !candidate.starts_with(' ')
                    && !candidate.starts_with('\t')
                {
                    break;
                }
                index += 1;
                block.push(candidate.trim().to_owned());
            }
            fields.insert(
                key.to_owned(),
                if folded {
                    block.join(" ").trim().to_owned()
                } else {
                    block.join("\n").trim().to_owned()
                },
            );
        } else {
            fields.insert(key.to_owned(), unquote_yaml_scalar(raw_value));
        }
    }
    Ok(fields)
}

/// 去除 YAML 单行标量的常见引号。
fn unquote_yaml_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && value.starts_with('"')
        && value.ends_with('"')
        && let Ok(decoded) = serde_json::from_str::<String>(value)
    {
        return decoded;
    }
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].replace("''", "'");
    }
    value.to_owned()
}

/// 读取 KeenCode 唯一 MCP 配置。
fn load_effective_mcp(
    app: &AppHandle,
) -> Result<(BTreeMap<String, ResolvedMcpServer>, Vec<McpDoctorSource>), String> {
    let path = publish_mcp_runtime_config(app)?;
    let project_dir =
        std::env::current_dir().map_err(|error| format!("无法确定当前目录：{error}"))?;
    let runtime_config = runtime_mcp_config(app, &project_dir)?;
    let persisted = load_mcp_document(&path)?.is_some();
    let mut resolved = BTreeMap::new();
    let source = if runtime_config.mcp_servers.is_empty() && !persisted {
        McpDoctorSource {
            path: path_to_frontend(&path),
            status: "missing".to_owned(),
            server_count: 0,
        }
    } else {
        for (name, config) in runtime_config.mcp_servers {
            let plugin_source = matches!(config.source.as_ref(), Some(ConfigSource::Plugin));
            let config = serde_json::to_value(config)
                .map_err(|error| format!("MCP Server {name} 运行时配置无法读取：{error}"))?;
            resolved.insert(
                name,
                ResolvedMcpServer {
                    config,
                    plugin_source,
                },
            );
        }
        let server_count = resolved.len();
        McpDoctorSource {
            path: path_to_frontend(&path),
            status: "configured".to_owned(),
            server_count,
        }
    };
    Ok((resolved, vec![source]))
}

/// 读取并严格校验 KeenCode 当前唯一 MCP JSON 结构。
fn load_mcp_document(path: &Path) -> Result<Option<McpDocument>, String> {
    if !current_regular_file_exists(path, "MCP 配置")? {
        return Ok(None);
    }
    let text = read_text_limited(path)?;
    parse_mcp_document_text(&text)
        .map(Some)
        .map_err(|error| format!("MCP 配置格式无效 {}：{error}", path.display()))
}

/// 解析并严格校验一段已经使用 canonical 根结构的 MCP JSON 文本。
///
/// 用户磁盘上的唯一持久化结构仍是 `{"mcpServers": {...}}`；厂商 flat
/// 结构只在显式导入边界通过 [`parse_mcp_import_text`] 接受。
fn parse_mcp_document_text(text: &str) -> Result<McpDocument, String> {
    let root: Value =
        serde_json::from_str(text).map_err(|error| format!("JSON 格式无效：{error}"))?;
    let document = McpDocument { root };
    validate_mcp_document(&document)?;
    Ok(document)
}

/// 解析厂商提供的 MCP JSON，并把其根结构、`type` 字段归一化到运行时结构。
///
/// `type` 是部分厂商配置中的传输提示，不属于 Peri 的持久化协议字段：
/// `stdio` 必须配合 `command`，`http` 必须配合 `url`，归一化后移除该字段。
fn parse_mcp_import_text(text: &str) -> Result<McpDocument, String> {
    if text.len() as u64 > MAX_EXTENSION_FILE_BYTES {
        return Err(format!("MCP 导入配置超过 {MAX_EXTENSION_FILE_BYTES} 字节"));
    }
    let root: Value =
        serde_json::from_str(text).map_err(|error| format!("JSON 格式无效：{error}"))?;
    let mut root = normalize_mcp_root(root)?;
    normalize_import_server_types(&mut root)?;
    let document = McpDocument { root };
    validate_mcp_document(&document)?;
    Ok(document)
}

/// 把两种公开 MCP 配置根结构归一化为 `{"mcpServers": {...}}`。
fn normalize_mcp_root(root: Value) -> Result<Value, String> {
    let Value::Object(mut object) = root else {
        return Err("MCP 配置顶层必须是对象".to_owned());
    };

    if let Some(servers) = object.remove("mcpServers") {
        if !object.is_empty() {
            return Err("MCP 配置顶层只能包含 mcpServers，或直接包含 Server 映射".to_owned());
        }
        let mut canonical = Map::new();
        canonical.insert("mcpServers".to_owned(), servers);
        return Ok(Value::Object(canonical));
    }

    let mut canonical = Map::new();
    canonical.insert("mcpServers".to_owned(), Value::Object(object));
    Ok(Value::Object(canonical))
}

/// 校验并移除导入格式中的可选传输 `type` 字段。
fn normalize_import_server_types(root: &mut Value) -> Result<(), String> {
    let servers = root
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "MCP 导入配置的 mcpServers 必须是对象".to_owned())?;
    for (name, config) in servers {
        let Some(object) = config.as_object_mut() else {
            continue;
        };
        let Some(kind) = object.remove("type") else {
            continue;
        };
        let kind = kind
            .as_str()
            .ok_or_else(|| format!("MCP Server {name} 的 type 必须是字符串"))?;
        match kind {
            "stdio" => {
                if !object.contains_key("command") || object.contains_key("url") {
                    return Err(format!(
                        "MCP Server {name} 的 type=stdio 必须只配合 command"
                    ));
                }
            }
            "http" => {
                if !object.contains_key("url") || object.contains_key("command") {
                    return Err(format!("MCP Server {name} 的 type=http 必须只配合 url"));
                }
            }
            _ => {
                return Err(format!("MCP Server {name} 的 type 只支持 stdio 或 http"));
            }
        }
    }
    Ok(())
}

/// 在内存中合并两份已经校验过的 MCP 文档。
///
/// 该函数不修改输入文档；冲突检查发生在任何写入前，保证导入不会部分成功。
fn merge_mcp_documents(
    mut existing: McpDocument,
    imported: McpDocument,
) -> Result<McpDocument, String> {
    let incoming = mcp_server_map(&imported)?.clone();
    {
        let current = mcp_server_map(&existing)?;
        if let Some(name) = incoming.keys().find(|name| current.contains_key(*name)) {
            return Err(format!("MCP Server {name} 已存在，导入未写入任何配置"));
        }
    }
    mcp_server_map_mut(&mut existing)?.extend(incoming);
    validate_mcp_document(&existing)?;
    Ok(existing)
}

/// 运行期发现用户 MCP 文件损坏时，先把共享运行时切到空快照，再返回原错误。
fn load_mcp_document_fail_closed(
    app: &AppHandle,
    path: &Path,
) -> Result<Option<McpDocument>, String> {
    match load_mcp_document(path) {
        Ok(document) => Ok(document),
        Err(error) => {
            if let Err(publish_error) = publish_mcp_runtime_config(app) {
                return Err(format!(
                    "{error}；同时无法发布空 MCP 运行时快照：{publish_error}"
                ));
            }
            Err(error)
        }
    }
}

/// 创建使用 canonical mcpServers 键的空 MCP 文档。
fn empty_mcp_document() -> McpDocument {
    let mut root = Map::new();
    root.insert("mcpServers".to_owned(), Value::Object(Map::new()));
    McpDocument {
        root: Value::Object(root),
    }
}

/// 返回 MCP 文档中的只读 Server 映射。
fn mcp_server_map(document: &McpDocument) -> Result<&Map<String, Value>, String> {
    document
        .root
        .as_object()
        .and_then(|root| root.get("mcpServers"))
        .and_then(Value::as_object)
        .ok_or_else(|| "MCP 配置缺少对象字段 mcpServers".to_owned())
}

/// 返回 MCP 文档中的可写 Server 映射。
fn mcp_server_map_mut(document: &mut McpDocument) -> Result<&mut Map<String, Value>, String> {
    let root = document
        .root
        .as_object_mut()
        .ok_or_else(|| "MCP 配置顶层必须是对象".to_owned())?;
    let value = root
        .get_mut("mcpServers")
        .ok_or_else(|| "MCP 配置缺少对象字段 mcpServers".to_owned())?;
    value
        .as_object_mut()
        .ok_or_else(|| "MCP 配置的 mcpServers 必须是对象".to_owned())
}

/// 校验 MCP 文档只包含当前根字段和可执行的 Server 结构。
fn validate_mcp_document(document: &McpDocument) -> Result<(), String> {
    let root = document
        .root
        .as_object()
        .ok_or_else(|| "MCP 配置顶层必须是对象".to_owned())?;
    if root.len() != 1 || !root.contains_key("mcpServers") {
        return Err("MCP 配置顶层只能包含 mcpServers".to_owned());
    }
    let servers = root
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or_else(|| "MCP 配置的 mcpServers 必须是对象".to_owned())?;
    for (name, config) in servers {
        if validate_extension_name(name, "MCP Server")? != *name {
            return Err(format!("MCP Server 名称不是规范格式：{name}"));
        }
        validate_mcp_server_config(name, config)?;
    }
    Ok(())
}

/// 校验单个 MCP Server 只使用 stdio 或 HTTP 的当前字段集。
fn validate_mcp_server_config(name: &str, config: &Value) -> Result<(), String> {
    const ALLOWED_FIELDS: &[&str] = &["command", "args", "env", "url", "headers", "disabled"];
    let object = config
        .as_object()
        .ok_or_else(|| format!("MCP Server {name} 配置必须是对象"))?;
    if let Some(field) = object
        .keys()
        .find(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
    {
        return Err(format!("MCP Server {name} 包含未知字段 {field}"));
    }
    if let Some(disabled) = object.get("disabled")
        && disabled.as_bool() != Some(true)
    {
        return Err(format!(
            "MCP Server {name} 的 disabled 只能为 true；启用时应省略该字段"
        ));
    }
    let command = optional_non_empty_string(object, "command", name)?;
    let url = optional_non_empty_string(object, "url", name)?;
    match (command, url) {
        (Some(_), None) => {
            if object.contains_key("headers") {
                return Err(format!("stdio MCP Server {name} 不能声明 headers"));
            }
            validate_optional_string_array(object, "args", name)?;
            validate_optional_string_map(object, "env", name)?;
        }
        (None, Some(url)) => {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(format!(
                    "HTTP MCP Server {name} 的 url 必须使用 http:// 或 https://"
                ));
            }
            if object.contains_key("args") || object.contains_key("env") {
                return Err(format!("HTTP MCP Server {name} 不能声明 args 或 env"));
            }
            validate_optional_string_map(object, "headers", name)?;
        }
        (Some(_), Some(_)) => {
            return Err(format!("MCP Server {name} 只能声明 command 或 url 之一"));
        }
        (None, None) => {
            return Err(format!("MCP Server {name} 必须声明 command 或 url"));
        }
    }
    Ok(())
}

/// 读取必须为规范非空文本的可选 MCP 字段。
fn optional_non_empty_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    server_name: &str,
) -> Result<Option<&'a str>, String> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .ok_or_else(|| format!("MCP Server {server_name} 的 {field} 必须是字符串"))?;
    if value.trim().is_empty() || value.trim() != value || value.contains('\0') {
        return Err(format!(
            "MCP Server {server_name} 的 {field} 不能为空、包含首尾空白或 NUL"
        ));
    }
    Ok(Some(value))
}

/// 校验可选 MCP 字符串数组字段。
fn validate_optional_string_array(
    object: &Map<String, Value>,
    field: &str,
    server_name: &str,
) -> Result<(), String> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    let items = value
        .as_array()
        .ok_or_else(|| format!("MCP Server {server_name} 的 {field} 必须是数组"))?;
    for item in items {
        let text = item
            .as_str()
            .ok_or_else(|| format!("MCP Server {server_name} 的 {field} 只能包含字符串"))?;
        if text.contains('\0') {
            return Err(format!(
                "MCP Server {server_name} 的 {field} 包含无效字符串"
            ));
        }
    }
    Ok(())
}

/// 校验可选 MCP 字符串映射字段。
fn validate_optional_string_map(
    object: &Map<String, Value>,
    field: &str,
    server_name: &str,
) -> Result<(), String> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    let entries = value
        .as_object()
        .ok_or_else(|| format!("MCP Server {server_name} 的 {field} 必须是对象"))?;
    for (key, value) in entries {
        if key.trim().is_empty() || key.trim() != key || key.chars().any(char::is_control) {
            return Err(format!("MCP Server {server_name} 的 {field} 包含无效键"));
        }
        let text = value
            .as_str()
            .ok_or_else(|| format!("MCP Server {server_name} 的 {field}.{key} 必须是字符串"))?;
        if text.contains('\0') {
            return Err(format!(
                "MCP Server {server_name} 的 {field}.{key} 不能包含 NUL"
            ));
        }
    }
    Ok(())
}

/// 原子保存 MCP JSON 文档。
fn save_mcp_document(path: &Path, document: &McpDocument) -> Result<(), String> {
    validate_mcp_document(document)?;
    write_json_private(path, &document.root, "MCP 配置")
}

/// 发布最新 MCP 快照并让下一轮从该路径重新计算配置指纹。
fn publish_mcp_runtime_config(app: &AppHandle) -> Result<PathBuf, String> {
    app.state::<std::sync::Arc<crate::peri_runtime::PeriRuntime>>()
        .reload_mcp_snapshot(app)
        .map_err(|error| format!("发布 MCP 运行时快照失败：{error:#}"))
}

/// 将 MCP 启用状态写回 KeenCode 唯一配置文件。
fn persist_mcp_enabled(app: &AppHandle, updates: &[(&str, bool)]) -> Result<(), String> {
    let path = mcp_user_config_path(app)?;
    let Some(mut document) = load_mcp_document_fail_closed(app, &path)? else {
        return Err(format!("MCP 配置不存在：{}", path.display()));
    };
    let mut changed = false;
    for (name, enabled) in updates {
        if !set_mcp_document_enabled(&mut document, name, *enabled)? {
            return Err(format!("找不到 MCP Server {name}"));
        }
        changed = true;
    }
    if changed {
        save_mcp_document(&path, &document)?;
        publish_mcp_runtime_config(app)?;
    }
    Ok(())
}

/// 在一个 MCP 文档中写入 peri 当前使用的 disabled 字段。
fn set_mcp_document_enabled(
    document: &mut McpDocument,
    name: &str,
    enabled: bool,
) -> Result<bool, String> {
    let Some(config) = mcp_server_map_mut(document)?.get_mut(name) else {
        return Ok(false);
    };
    let object = config
        .as_object_mut()
        .ok_or_else(|| format!("MCP Server {name} 配置必须是对象"))?;
    if enabled {
        object.remove("disabled");
    } else {
        object.insert("disabled".to_owned(), Value::Bool(true));
    }
    Ok(true)
}

/// 将合并后的 MCP Server 转为前端 DTO。
fn mcp_dto(name: String, server: ResolvedMcpServer) -> McpDto {
    let transport = mcp_transport(&server.config);
    let config_enabled = mcp_config_enabled(&server.config);
    McpDto {
        enabled: config_enabled,
        name,
        transport,
        target: mcp_target(&server.config, server.plugin_source),
    }
}

/// 返回 MCP Server 的标准化传输类型。
fn mcp_transport(config: &Value) -> String {
    if config.get("url").and_then(Value::as_str).is_some() {
        "http".to_owned()
    } else {
        "stdio".to_owned()
    }
}

/// 返回 MCP Server 的安全展示目标。
///
/// 插件 MCP 配置已经完成 userConfig 插值，无法再从原始字符串区分普通值和
/// 敏感值。对插件来源统一不返回 target，避免命令、参数或 URL 中的敏感值经
/// inspect_mcp 进入前端；用户显式配置的 MCP 继续保留现有展示行为。
fn mcp_target(config: &Value, plugin_source: bool) -> Option<String> {
    if plugin_source {
        return None;
    }
    if let Some(url) = config
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(url.to_owned());
    }
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let mut parts = vec![command.to_owned()];
    if let Some(args) = config.get("args").and_then(Value::as_array) {
        parts.extend(
            args.iter()
                .filter_map(Value::as_str)
                .map(quote_display_argument),
        );
    }
    Some(parts.join(" "))
}

/// 为包含空白的 MCP 参数添加仅用于展示的引号。
fn quote_display_argument(argument: &str) -> String {
    if argument.chars().any(char::is_whitespace) {
        format!("{argument:?}")
    } else {
        argument.to_owned()
    }
}

/// 返回 MCP 配置自身声明的启用状态。
fn mcp_config_enabled(config: &Value) -> bool {
    !config
        .get("disabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// 构建单个 MCP Doctor Server 结果。
fn doctor_server(name: String, server: ResolvedMcpServer) -> McpDoctorServer {
    let transport = mcp_transport(&server.config);
    let plugin_source = server.plugin_source;
    let mut checks = Vec::new();
    if transport == "stdio" {
        let command = server
            .config
            .get("command")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let available = command.is_some_and(executable_available);
        checks.push(McpDoctorCheck {
            label: "命令可用性".to_owned(),
            passed: available,
            detail: match command {
                Some(_) if available && plugin_source => {
                    "在本机路径中找到插件 MCP 可执行文件".to_owned()
                }
                Some(command) if available => format!("在本机路径中找到可执行文件：{command}"),
                Some(_) if plugin_source => "未在本机路径中找到插件 MCP 可执行文件".to_owned(),
                Some(command) => format!("未在本机路径中找到可执行文件：{command}"),
                None => "配置中缺少 command 字段".to_owned(),
            },
        });
    } else if transport == "http" {
        let url = server
            .config
            .get("url")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        checks.push(McpDoctorCheck {
            label: "HTTP 地址".to_owned(),
            passed: url.is_some(),
            detail: if plugin_source {
                if url.is_some() {
                    "插件 MCP HTTP 地址已配置".to_owned()
                } else {
                    "配置中缺少 url 字段".to_owned()
                }
            } else {
                url.unwrap_or("配置中缺少 url 字段").to_owned()
            },
        });
    }
    let healthy = checks.iter().all(|check| check.passed);
    McpDoctorServer {
        name,
        healthy,
        transport,
        target: mcp_target(&server.config, plugin_source),
        checks,
    }
}

/// 判断 stdio MCP 命令是否能够在本机文件系统或 PATH 中找到。
fn executable_available(command: &str) -> bool {
    let expanded = expand_tilde(command).unwrap_or_else(|_| PathBuf::from(command));
    if expanded.is_absolute() || command.contains('/') || command.contains('\\') {
        return expanded.is_file();
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    #[cfg(windows)]
    let extensions = env::var_os("PATHEXT")
        .map(|value| {
            value
                .to_string_lossy()
                .split(';')
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![".EXE".to_owned(), ".CMD".to_owned(), ".BAT".to_owned()]);
    for directory in env::split_paths(&path) {
        if directory.join(command).is_file() {
            return true;
        }
        #[cfg(windows)]
        for extension in &extensions {
            if directory.join(format!("{command}{extension}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// 展开以 ~ 或 ~/ 开头的用户路径。
fn expand_tilde(raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "无法展开 ~：HOME 未设置".to_owned());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "无法展开 ~：HOME 未设置".to_owned())?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(raw))
}

#[cfg(test)]
// 大型回归测试独立存放，避免生产模块重新膨胀。
#[path = "extensions/tests.rs"]
mod tests;
