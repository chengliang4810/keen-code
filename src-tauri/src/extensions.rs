use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tar::Archive;
use tauri::{AppHandle, Manager, State};
use zip::ZipArchive;

use crate::agent_runtime::RuntimeExtensionDiagnostic;
use crate::path_utils::{path_text_to_frontend, path_to_frontend};
use crate::plugin_secrets::SystemSecretStore;
use crate::plugins::{
    InstalledPlugin, MaterializedPlugin, PluginId, PluginManager, PluginRuntimeSnapshot,
    PluginSource, ResolvedUserConfig, UserConfigUpdate, extract_components, load_plugin_manifest,
    marketplace_name_key, resolve_internal_file_symlink,
};

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
/// 串行化扩展配置读写。
#[derive(Debug, Default)]
pub struct ExtensionsState {
    /// 防止多个 Tauri 命令并发覆盖同一个扩展配置文件。
    io_lock: Mutex<()>,
    /// 系统密钥库适配器；公开状态永远不保存插件敏感配置值。
    plugin_secrets: Mutex<SystemSecretStore>,
    /// 为完整扩展候选分配且永不复用的进程内代次。
    next_runtime_generation: AtomicU64,
    /// 按规范项目根隔离的候选构建单飞锁与已发布指纹。
    runtime_projects: Mutex<
        BTreeMap<
            PathBuf,
            std::sync::Arc<tokio::sync::Mutex<runtime_contributor::ProjectRuntimeCache>>,
        >,
    >,
}

impl ExtensionsState {
    /// 获取扩展配置读写锁。
    fn lock_io(&self) -> Result<MutexGuard<'_, ()>, String> {
        self.io_lock
            .lock()
            .map_err(|_| "扩展配置读写锁已损坏".to_owned())
    }

    /// 分配下一个非零扩展候选代次；耗尽后拒绝继续发布。
    fn reserve_runtime_generation(&self) -> Result<u64, String> {
        self.next_runtime_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map(|previous| previous + 1)
            .map_err(|_| "扩展运行时代次已经耗尽".to_owned())
    }

    /// 返回一个项目独占的异步构建锁，相同项目只允许一批 MCP 初始化。
    fn project_runtime_lock(
        &self,
        project_root: &Path,
    ) -> Result<std::sync::Arc<tokio::sync::Mutex<runtime_contributor::ProjectRuntimeCache>>, String>
    {
        let mut projects = self
            .runtime_projects
            .lock()
            .map_err(|_| "扩展项目缓存锁已损坏".to_owned())?;
        Ok(projects
            .entry(project_root.to_path_buf())
            .or_insert_with(|| {
                std::sync::Arc::new(tokio::sync::Mutex::new(
                    runtime_contributor::ProjectRuntimeCache::default(),
                ))
            })
            .clone())
    }

    /// 返回已经由 Session 注册过扩展候选的全部规范项目根。
    fn runtime_project_roots(&self) -> Result<Vec<PathBuf>, String> {
        self.runtime_projects
            .lock()
            .map(|projects| projects.keys().cloned().collect())
            .map_err(|_| "扩展项目缓存锁已损坏".to_owned())
    }
}

/// 并行重建所有已知项目的完整扩展候选；重建前先撤销旧 MCP 工具。
async fn refresh_known_runtime_projects(
    app: &AppHandle,
    runtime: &std::sync::Arc<crate::agent_runtime::AgentRuntime>,
) -> Result<(), String> {
    runtime
        .revoke_mcp_extension_tools()
        .map_err(|error| format!("撤销旧 MCP 运行时工具失败：{error}"))?;
    let roots = app
        .try_state::<ExtensionsState>()
        .ok_or_else(|| "扩展状态尚未初始化".to_owned())?
        .runtime_project_roots()?;
    if roots.is_empty() {
        return Ok(());
    }

    let mut tasks = tokio::task::JoinSet::new();
    for project_root in roots {
        let app = app.clone();
        let runtime = std::sync::Arc::clone(runtime);
        tasks.spawn(async move {
            let result = ensure_runtime_extension_candidate(&app, &project_root, &runtime, true)
                .await
                .map(|_| ());
            (project_root, result)
        });
    }

    let mut failures = Vec::new();
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((_, Ok(()))) => {}
            Ok((project_root, Err(error))) => {
                failures.push(format!("{}: {error}", project_root.display()));
            }
            Err(error) => failures.push(format!("扩展刷新任务异常退出：{error}")),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        failures.sort();
        Err(format!(
            "扩展配置已保存，但部分已知项目的新候选构建失败：{}",
            failures.join("; ")
        ))
    }
}

/// 返回 KeenCode 插件状态服务；插件缓存与配置均位于应用数据目录。
fn plugin_manager(app: &AppHandle) -> Result<PluginManager, String> {
    let root = crate::storage::root_dir(app)
        .map_err(|error| format!("无法确定 KeenCode 插件数据目录：{error}"))?;
    Ok(PluginManager::new(root))
}

/// 读取当前启用插件的 Provider 中立运行时快照。
pub(crate) fn plugin_runtime_snapshot(
    app: &AppHandle,
    project_dir: &Path,
) -> Result<PluginRuntimeSnapshot, String> {
    let manager = plugin_manager(app)?;
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let snapshot = if let Some(state) = app.try_state::<ExtensionsState>() {
        let secrets = state
            .plugin_secrets
            .lock()
            .map_err(|_| "KeenCode 插件敏感配置锁已损坏".to_owned())?;
        manager
            .runtime_snapshot(project_dir, &environment, &*secrets)
            .map_err(|error| error.to_string())?
    } else {
        let secrets = SystemSecretStore;
        manager
            .runtime_snapshot(project_dir, &environment, &secrets)
            .map_err(|error| error.to_string())?
    };
    Ok(snapshot)
}

/// 一个已经完成来源合并、可直接交给 KeenCode MCP 客户端的服务配置。
#[derive(Clone, Debug)]
pub(crate) struct RuntimeMcpServer {
    /// MCP Server 在当前运行时中的稳定唯一标识。
    pub(crate) id: String,
    /// Provider 中立的 MCP 传输配置；敏感值仅保留在当前进程内。
    pub(crate) config: keencode_mcp::McpServerConfig,
    /// 可选的非秘密 OAuth 绑定；令牌仅由应用级 Registry 提供，不进入配置指纹。
    pub(crate) oauth: Option<crate::mcp_oauth::McpOAuthSettings>,
}

/// 按已经核验的项目根解析当前启用的 MCP Server，供显式 OAuth 控制操作绑定来源。
/// 只读取当前配置和插件快照，不连接 MCP，也不使用当前聚焦 Session 推断项目。
pub(crate) fn runtime_mcp_server_for_project(
    app: &AppHandle,
    project_root: &Path,
    server_name: &str,
) -> Result<RuntimeMcpServer, String> {
    let state = app
        .try_state::<ExtensionsState>()
        .ok_or_else(|| "扩展状态尚未初始化".to_owned())?;
    let _guard = state.lock_io()?;
    let document =
        load_mcp_document(&mcp_user_config_path(app)?)?.unwrap_or_else(empty_mcp_document);
    let plugins = plugin_runtime_snapshot(app, project_root)?;
    let (servers, _) = runtime_mcp_servers_from_sources(&document, plugins, project_root)?;
    servers
        .into_iter()
        .find(|server| server.id == server_name)
        .ok_or_else(|| "当前项目中不存在该启用的 MCP Server".to_owned())
}

/// 从已经校验的用户文档和启用插件快照构造稳定排序的 MCP Server 列表。
fn runtime_mcp_servers_from_sources(
    document: &McpDocument,
    snapshot: PluginRuntimeSnapshot,
    project_root: &Path,
) -> Result<(Vec<RuntimeMcpServer>, Vec<RuntimeExtensionDiagnostic>), String> {
    let mut runtime_servers = BTreeMap::new();
    let mut diagnostics = Vec::new();
    for (id, value) in mcp_server_map(document)? {
        if !mcp_config_enabled(value) {
            continue;
        }
        let server = runtime_mcp_server_from_value(id, value, project_root)?;
        runtime_servers.insert(id.clone(), server);
    }

    for plugin in snapshot.plugins {
        let plugin_namespace = plugin
            .id
            .runtime_namespace()
            .map_err(|error| error.to_string())?;
        for (name, value) in plugin.mcp_servers {
            if !mcp_config_enabled(&value) {
                continue;
            }
            let id = format!("{plugin_namespace}:{name}");
            match runtime_mcp_server_from_value(&id, &value, &plugin.root) {
                Ok(server) => {
                    runtime_servers.insert(id, server);
                }
                Err(error) => diagnostics.push(RuntimeExtensionDiagnostic {
                    source: "mcp".to_owned(),
                    server: id,
                    code: "mcp_config_invalid".to_owned(),
                    message: error,
                    tool: None,
                }),
            }
        }
    }

    Ok((runtime_servers.into_values().collect(), diagnostics))
}

/// 把一个已归一化 JSON Server 映射成 Provider 中立的 stdio 或 HTTP 配置。
fn runtime_mcp_server_from_value(
    id: &str,
    value: &Value,
    stdio_current_dir: &Path,
) -> Result<RuntimeMcpServer, String> {
    validate_mcp_server_config(id, value)?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("MCP Server {id} 配置必须是对象"))?;
    let config = if let Some(command) = object.get("command").and_then(Value::as_str) {
        let mut config = keencode_mcp::StdioServerConfig::new(command);
        config.args = mcp_string_array(object, "args", id)?;
        config.current_dir = Some(stdio_current_dir.to_path_buf());
        config.environment = mcp_string_map(object, "env", id)?;
        config.inherit_environment = true;
        keencode_mcp::McpServerConfig::Stdio(config)
    } else {
        let endpoint = object
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("MCP Server {id} 缺少 HTTP url"))?;
        let mut config = keencode_mcp::StreamableHttpConfig::new(endpoint);
        config.headers = mcp_string_map(object, "headers", id)?;
        config.terminate_session_on_close = true;
        keencode_mcp::McpServerConfig::StreamableHttp(config)
    };
    Ok(RuntimeMcpServer {
        id: id.to_owned(),
        config,
        oauth: mcp_oauth_settings(object, id)?,
    })
}

/// 读取已经校验过的 MCP 字符串数组字段；缺失字段返回空数组。
fn mcp_string_array(
    object: &Map<String, Value>,
    field: &str,
    server_id: &str,
) -> Result<Vec<String>, String> {
    object
        .get(field)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| format!("MCP Server {server_id} 的 {field} 只能包含字符串"))
                })
                .collect()
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

/// 读取已经校验过的 MCP 字符串映射字段；缺失字段返回空映射。
fn mcp_string_map(
    object: &Map<String, Value>,
    field: &str,
    server_id: &str,
) -> Result<BTreeMap<String, String>, String> {
    object
        .get(field)
        .and_then(Value::as_object)
        .map(|entries| {
            entries
                .iter()
                .map(|(name, value)| {
                    value
                        .as_str()
                        .map(|value| (name.clone(), value.to_owned()))
                        .ok_or_else(|| {
                            format!("MCP Server {server_id} 的 {field}.{name} 必须是字符串")
                        })
                })
                .collect()
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

/// 将设置界面传入的 `plugin` 或 `plugin@marketplace` 解析成唯一已安装 ID。
fn resolve_installed_plugin_id(manager: &PluginManager, raw: &str) -> Result<PluginId, String> {
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

/// 把 KeenCode 插件运行时快照转换为当前界面使用的组件统计。
fn plugin_provides(plugin: &crate::plugins::RuntimePlugin) -> PluginProvidesDto {
    PluginProvidesDto {
        commands: plugin.commands.len(),
        skills: plugin.skills.len(),
        agents: plugin.agents.len(),
        hooks: usize::from(plugin.hooks.is_some()),
        mcp: plugin.mcp_servers.len(),
        lsp: plugin.lsp_servers.len(),
    }
}

/// 将查询传入的项目路径解析为已登记项目根；空值只表示全局视图。
fn resolve_extension_project_root(
    app: &AppHandle,
    project_path: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    resolve_extension_project_root_with(project_path, |path| {
        crate::workspace::registered_project_root(app, path)
    })
}

/// 统一处理查询项目路径，并让非空路径经过调用方提供的授权解析器。
fn resolve_extension_project_root_with<F>(
    project_path: Option<&str>,
    resolve: F,
) -> Result<Option<PathBuf>, String>
where
    F: FnOnce(&str) -> Result<PathBuf, String>,
{
    let Some(project_path) = project_path.map(str::trim).filter(|path| !path.is_empty()) else {
        return Ok(None);
    };
    resolve(project_path).map(Some)
}

/// 为无项目上下文的全局扩展视图提供不会读取进程工作目录的占位根。
fn extension_global_view_root(data_root: &Path) -> PathBuf {
    data_root.join(".keencode-global-view")
}

// 市场来源取得与归档处理体量较大，保持为独立职责模块。
#[path = "extensions/agent_catalog.rs"]
mod agent_catalog;
use agent_catalog::{AgentTools, build_agent_catalog, parse_agent_document, validate_agent_name};

#[path = "extensions/runtime_contributor.rs"]
mod runtime_contributor;
pub(crate) use runtime_contributor::{ensure_runtime_extension_candidate, mcp_oauth_status};

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
    /// 子智能体来源，当前为 builtin、global、project 或 plugin。
    pub source: String,
    /// 项目定义文件路径；内置子智能体没有外部文件。
    pub path: Option<String>,
    /// 子智能体的模型覆盖（`"{provider_id}::{model}"`）；None 表示跟随会话 Provider。
    pub model: Option<String>,
}

/// 子智能体列举结果。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsListResult {
    /// 当前项目完成内置、全局、项目和插件优先级归约后的全部子智能体。
    pub agents: Vec<AgentDto>,
}

/// 创建子智能体时可选择的工具目录。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCatalog {
    /// 全局 Agent 模板支持选择的固定工具名；条件工具可能在当前 Session 不可用。
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
    /// MCP Server 的配置来源；插件来源只能在插件清单中修改。
    pub source: String,
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
    /// 插件包含的 KeenCode Commands 数量。
    #[serde(default, skip_serializing_if = "is_zero")]
    pub commands: usize,
    /// 插件包含的 Skill 数量。
    pub skills: usize,
    /// 插件包含的 KeenCode Agents 数量。
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
    /// 插件 hooks.json 中声明但 KeenCode Runtime 不识别的事件名。
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

/// KeenCode 插件 userConfig 的可视化字段，不返回敏感字段实际值。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUserConfigFieldDto {
    /// 配置字段名。
    pub name: String,
    /// KeenCode 声明的字段类型。
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
    /// 默认 KeenCode 官方市场是否仍在后台取得。
    pub loading: bool,
    /// 默认市场取得失败时的可展示错误；失败状态带退避，不会每次请求重复克隆。
    pub error: Option<String>,
}

/// KeenCode 持久化的本地市场记录。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MarketplaceRecord {
    /// 市场清单中的稳定名称。
    name: String,
    /// 本地市场根目录的规范化绝对路径。
    path: String,
    /// 实际使用的 marketplace.json 规范化绝对路径；支持 Git source.path 指定的嵌套清单。
    manifest_path: String,
}

/// 本地插件市场状态的固定 schema 名称。
const MARKETPLACE_STORE_SCHEMA: &str = "keencode/marketplace-store";
/// 本地插件市场状态的唯一格式版本。
const MARKETPLACE_STORE_VERSION: u32 = 1;

/// KeenCode 持久化的市场列表。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MarketplaceStore {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// 用户显式添加的本地市场来源。
    sources: Vec<MarketplaceRecord>,
}

impl Default for MarketplaceStore {
    /// 创建当前格式的空插件市场状态。
    fn default() -> Self {
        Self {
            schema: MARKETPLACE_STORE_SCHEMA.to_owned(),
            version: MARKETPLACE_STORE_VERSION,
            sources: Vec::new(),
        }
    }
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
    /// 是否来自 KeenCode 插件；插件配置可能包含已插值的 userConfig 敏感值。
    plugin_source: bool,
}

/// 设置一个 MCP Server 的唯一启用状态。
#[tauri::command]
pub async fn extensions_set_mcp(
    name: String,
    enabled: bool,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_extension_name(&name, "MCP Server")?;
    persist_mcp_enabled(&app, &[(&name, enabled)], runtime.inner())?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 批量启用前端当前列出的 MCP Server。
#[tauri::command]
pub async fn extensions_enable_all_mcp(
    names: Vec<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let names = normalized_extension_names(names, "MCP Server")?;
    let updates = names
        .iter()
        .map(|name| (name.as_str(), true))
        .collect::<Vec<_>>();
    persist_mcp_enabled(&app, &updates, runtime.inner())?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 列出 KeenCode 用户级与项目级 Skills。
#[tauri::command]
pub fn skills_list(
    project_path: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<SkillsListResult, String> {
    let _guard = state.lock_io()?;
    let data_root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定 KeenCode Skill 数据目录：{error}"))?;
    let project = resolve_extension_project_root(&app, project_path.as_deref())?;
    let project_context = project
        .clone()
        .unwrap_or_else(|| extension_global_view_root(&data_root));
    let snapshot = project
        .as_deref()
        .map(|project_root| plugin_runtime_snapshot(&app, project_root))
        .transpose()?
        .unwrap_or_default();
    let config = runtime_skill_config_from_snapshot(
        data_root.clone(),
        project_context.clone(),
        snapshot.clone(),
    );
    let catalog = keencode_skills::discover_skills(&config)
        .map_err(|error| format!("无法建立 Skill 目录：{error}"))?;
    let mut paths = BTreeMap::new();
    for skill in scan_skill_directory(&data_root.join("skills")) {
        paths
            .entry((
                keencode_skills::SkillSource::Data,
                skill.name.to_ascii_lowercase(),
            ))
            .or_insert(skill.path);
    }
    if project.is_some() {
        for skill in scan_skill_directory(&project_context.join(".agents").join("skills")) {
            paths
                .entry((
                    keencode_skills::SkillSource::Project,
                    skill.name.to_ascii_lowercase(),
                ))
                .or_insert(skill.path);
        }
    }
    let mut plugin_skill_paths = Vec::new();
    for plugin in &snapshot.plugins {
        for file in &plugin.skills {
            if file.path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
                continue;
            }
            let Ok((name, _)) = parse_skill_file(&file.path) else {
                continue;
            };
            plugin_skill_paths.push((
                file.path
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase(),
                name,
                file.path.clone(),
            ));
        }
    }
    plugin_skill_paths
        .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.2.cmp(&right.2)));
    for (_, name, path) in plugin_skill_paths {
        paths
            .entry((
                keencode_skills::SkillSource::Plugin,
                name.to_ascii_lowercase(),
            ))
            .or_insert(path);
    }
    let mut skills = BTreeMap::new();
    for skill in catalog.entries() {
        let source = match skill.source {
            keencode_skills::SkillSource::Data => "user",
            keencode_skills::SkillSource::Project => "project",
            keencode_skills::SkillSource::Plugin => "plugin",
        };
        let path = paths
            .get(&(skill.source, skill.name.to_ascii_lowercase()))
            .ok_or_else(|| format!("Skill {} 的目录路径缺失", skill.name))?;
        skills.insert(
            skill.name.to_ascii_lowercase(),
            SkillDto {
                name: skill.name.clone(),
                description: skill.description.clone(),
                source: source.to_owned(),
                path: path_to_frontend(path),
                user_invocable: true,
            },
        );
    }
    for plugin in snapshot.plugins {
        let plugin_namespace = plugin
            .id
            .runtime_namespace()
            .map_err(|error| error.to_string())?;
        for file in plugin.commands {
            let namespace = plugin_command_namespace(&plugin_namespace, &file.relative_path);
            let description = crate::plugins::plugin_command_description(&plugin.root, &file.path)
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

/// 列出当前项目可用的内置、全局、项目与插件子智能体。
#[tauri::command]
pub fn agents_list(
    project_path: Option<String>,
    app: AppHandle,
) -> Result<AgentsListResult, String> {
    let data_root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?;
    let project_root = resolve_extension_project_root(&app, project_path.as_deref())?;
    let project_context = project_root
        .clone()
        .unwrap_or_else(|| extension_global_view_root(&data_root));
    let snapshot = project_root
        .as_deref()
        .map(|project_root| plugin_runtime_snapshot(&app, project_root))
        .transpose()?
        .unwrap_or_default();
    let model_overrides = read_agent_model_overrides(&app)?;
    let catalog = build_agent_catalog(&data_root, &project_context, &snapshot, &model_overrides)?;
    let agents = catalog
        .entries()
        .map(|entry| AgentDto {
            name: entry.name.clone(),
            description: entry.document.description.clone(),
            source: entry.source.as_str().to_owned(),
            path: entry.path.as_ref().map(|path| path_to_frontend(path)),
            model: entry
                .document
                .model
                .as_deref()
                .and_then(normalize_model_reference_for_ui),
        })
        .collect::<Vec<_>>();
    Ok(AgentsListResult { agents })
}

/// 返回创建全局 Agent 模板时可选择的固定工具支持目录。
///
/// 该命令没有 Session、项目或扩展候选上下文，因此 Web、插件、LSP 等条件工具
/// 仍会出现在支持目录中；模板实际被显式应用时，父 Agent 快照缺少所选工具会在
/// 启动前以 `agent_template_invalid` 拒绝，不会静默降级。MCP 工具名是动态发现的，
/// 不写入这个固定目录。子 Agent 后台非交互运行，无法直接使用宿主问答流程；进度
/// 由 Agent 生命周期事件上报而非 TodoWrite，因此根 Agent 专用的五项工具均排除。
#[tauri::command]
pub fn agents_tool_catalog() -> Result<AgentToolCatalog, String> {
    // 先使用完整固定工具名称集合，再复用 Runtime 的根专用工具过滤规则，避免
    // 子 Agent 工具边界在模板目录和运行时快照之间出现第二套排除逻辑。
    let mut tools = [
        "Bash",
        "PowerShell",
        "Git",
        "Write",
        "Edit",
        "Read",
        "Glob",
        "Grep",
        "TaskOutput",
        "TaskStop",
        "TodoWrite",
        "Goal",
        "Plan",
        "AskUser",
        "WebFetch",
        "WebSearch",
        "Skill",
        "PluginCommand",
        "ToolSearch",
        "ExecuteExtraTool",
        "LSP",
        "spawn_agent",
        "send_message",
        "followup_task",
        "interrupt_agent",
        "retry_agent",
        "list_agents",
        "wait_agent",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    keencode_tools::retain_child_agent_tool_snapshot(&mut tools);
    Ok(AgentToolCatalog { tools })
}

/// 读取单个子智能体定义详情；查找优先级与 `agents_list` 一致：
/// 查找顺序与运行时目录完全相同，并返回当前生效定义。
#[tauri::command]
pub fn agent_detail(
    name: String,
    project_path: Option<String>,
    app: AppHandle,
) -> Result<AgentDetail, String> {
    let name = name.trim();
    let data_root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?;
    let project_root = resolve_extension_project_root(&app, project_path.as_deref())?;
    let project_context = project_root
        .clone()
        .unwrap_or_else(|| extension_global_view_root(&data_root));
    let snapshot = project_root
        .as_deref()
        .map(|project_root| plugin_runtime_snapshot(&app, project_root))
        .transpose()?
        .unwrap_or_default();
    let overrides = read_agent_model_overrides(&app)?;
    let catalog = build_agent_catalog(&data_root, &project_context, &snapshot, &overrides)?;
    let entry = catalog
        .get(name)
        .ok_or_else(|| format!("找不到子智能体 {name}"))?;
    let tools = match &entry.document.tools {
        AgentTools::Inherit => None,
        AgentTools::None => Some(Vec::new()),
        AgentTools::List(list) => Some(list.clone()),
    };
    Ok(AgentDetail {
        name: entry.name.clone(),
        description: entry.document.description.clone(),
        source: entry.source.as_str().to_owned(),
        path: entry.path.as_ref().map(|path| path_to_frontend(path)),
        // 设置页只展示合法的 `provider_id::model` 引用；非法值不进入模型选项。
        model: entry
            .document
            .model
            .as_deref()
            .and_then(normalize_model_reference_for_ui),
        tools,
        disallowed_tools: entry.document.disallowed_tools.clone(),
        max_turns: entry.document.max_turns,
        allowed_write_dirs: entry.document.allowed_write_dirs.clone(),
        system_prompt: entry.document.system_prompt.clone(),
    })
}

/// 在 KeenCode 全局目录创建一个当前运行时可直接加载的子智能体定义。
#[tauri::command]
#[allow(clippy::too_many_arguments)] // Tauri IPC 当前字段需保持平铺并与前端命令契约一致。
pub async fn agent_create(
    name: String,
    description: String,
    prompt: String,
    tools: Option<Vec<String>>,
    max_turns: Option<u32>,
    model: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_agent_name(&name)?;
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
    if path.exists() || name.eq_ignore_ascii_case("plan") {
        return Err(format!("子智能体 {name} 已存在"));
    }

    let name_yaml =
        serde_json::to_string(&name).map_err(|error| format!("无法序列化子智能体名称：{error}"))?;
    let description_yaml = serde_json::to_string(description)
        .map_err(|error| format!("无法序列化子智能体说明：{error}"))?;
    // None 表示继承主智能体全部工具；显式列表（包括空列表）会冻结独立可用工具快照。
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
    parse_agent_document(&content).map_err(|error| format!("生成的子智能体定义无效：{error}"))?;
    atomic_write_private(&path, content.as_bytes())?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 删除 KeenCode 全局目录中的一个子智能体定义。
#[tauri::command]
pub async fn agent_remove(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_agent_name(&name)?;
    let path = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
        .join("agents")
        .join(format!("{name}.md"));
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("找不到全局子智能体 {name}：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("子智能体定义必须是普通文件：{}", path.display()));
    }
    fs::remove_file(&path).map_err(|error| format!("无法删除子智能体 {name}：{error}"))?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 更新子智能体的模型覆盖字段。
///
/// 全局定义（`~/.keencode/agents/{name}.md` 存在）：只修改 frontmatter 的
/// `model:` 键，系统提示、工具等其余内容原样保留。内置定义：写入
/// `agent-model-overrides.json` 覆盖表，KeenCode 在装配内置定义时套用。
/// `model` 编码为 `"{provider_id}::{model}"`；None 表示清除覆盖，恢复为
/// 跟随会话 provider。
#[tauri::command]
pub async fn agent_update(
    name: String,
    model: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_agent_name(&name)?;
    let model = model
        .as_deref()
        .map(str::trim)
        .map(normalize_model_reference)
        .transpose()?;
    let path = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定全局子智能体目录：{error}"))?
        .join("agents")
        .join(format!("{name}.md"));
    let update_result = match fs::symlink_metadata(&path) {
        // symlink_metadata 对符号链接返回 link 类型：is_file 为 false，落入下方分支。
        Ok(metadata) if metadata.file_type().is_file() => {
            let content = fs::read_to_string(&path)
                .map_err(|error| format!("无法读取子智能体定义：{error}"))?;
            let updated = set_frontmatter_model(&content, model.as_deref())?;
            parse_agent_document(&updated)
                .map_err(|error| format!("更新后的子智能体定义无效：{error}"))?;
            atomic_write_private(&path, updated.as_bytes())
        }
        Ok(_) => Err(format!("子智能体定义必须是普通文件：{}", path.display())),
        Err(_) => {
            if name.eq_ignore_ascii_case("plan") {
                write_agent_model_override(&app, &name, model.as_deref())
            } else {
                Err(format!("找不到全局子智能体 {name}"))
            }
        }
    };
    update_result?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
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
/// 构建当前项目 Agent catalog 时读取该覆盖表并套用。
fn agent_model_overrides_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|directory| directory.join("agent-model-overrides.json"))
        .map_err(|error| format!("无法确定模型覆盖表路径：{error}"))
}

/// 当前内置子智能体模型覆盖表的固定 schema 名称。
const AGENT_MODEL_OVERRIDES_SCHEMA: &str = "keencode/agent-model-overrides";
/// 当前内置子智能体模型覆盖表的唯一格式版本。
const AGENT_MODEL_OVERRIDES_VERSION: u32 = 1;

/// 内置子智能体模型覆盖表的严格持久化外壳。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AgentModelOverridesFile {
    /// 固定 schema 名称。
    schema: String,
    /// 固定格式版本。
    version: u32,
    /// 当前完整的子智能体模型覆盖映射。
    #[serde(deserialize_with = "deserialize_unique_string_map")]
    overrides: BTreeMap<String, String>,
}

impl AgentModelOverridesFile {
    /// 为当前模型覆盖映射构造严格持久化文件。
    fn from_overrides(overrides: &BTreeMap<String, String>) -> Result<Self, String> {
        validate_agent_model_overrides(overrides)?;
        Ok(Self {
            schema: AGENT_MODEL_OVERRIDES_SCHEMA.to_owned(),
            version: AGENT_MODEL_OVERRIDES_VERSION,
            overrides: overrides.clone(),
        })
    }

    /// 校验文件身份和所有条目，并返回当前模型覆盖映射。
    fn into_overrides(self) -> Result<BTreeMap<String, String>, String> {
        if self.schema != AGENT_MODEL_OVERRIDES_SCHEMA
            || self.version != AGENT_MODEL_OVERRIDES_VERSION
        {
            return Err("模型覆盖表 schema 或版本不受支持".to_owned());
        }
        validate_agent_model_overrides(&self.overrides)?;
        Ok(self.overrides)
    }
}

/// 反序列化字符串映射并拒绝重复键，避免后出现的 JSON 键静默覆盖先出现的条目。
fn deserialize_unique_string_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// 只接受字符串键和值，并在解析过程中检查重复键。
    struct UniqueStringMapVisitor;

    impl<'de> serde::de::Visitor<'de> for UniqueStringMapVisitor {
        type Value = BTreeMap<String, String>;

        /// 返回该字段需要的 JSON 类型说明。
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("不含重复键的字符串 JSON 对象")
        }

        /// 读取字符串映射，并在同一对象中发现重复键时失败。
        fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
        where
            M: serde::de::MapAccess<'de>,
        {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, String>()? {
                if values.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "模型覆盖表包含重复的子智能体键：{key}"
                    )));
                }
            }
            Ok(values)
        }
    }

    deserializer.deserialize_map(UniqueStringMapVisitor)
}

/// 校验模型覆盖表中的每个键和值，拒绝任何不能由当前运行时解释的条目。
fn validate_agent_model_overrides(overrides: &BTreeMap<String, String>) -> Result<(), String> {
    for (agent_id, model) in overrides {
        let normalized_agent_id = validate_agent_name(agent_id)
            .map_err(|error| format!("模型覆盖表中的子智能体键无效：{error}"))?;
        if normalized_agent_id != *agent_id {
            return Err(format!("模型覆盖表中的子智能体键不是规范格式：{agent_id}"));
        }
        let normalized_model = normalize_model_reference(model)
            .map_err(|error| format!("子智能体 {agent_id} 的模型覆盖无效：{error}"))?;
        if normalized_model != *model {
            return Err(format!(
                "子智能体 {agent_id} 的模型覆盖不是规范格式：{model}"
            ));
        }
    }
    Ok(())
}

/// 严格解析当前模型覆盖表，不填充缺失字段、不忽略未知字段、不改写原文。
fn parse_agent_model_overrides(content: &str) -> Result<BTreeMap<String, String>, String> {
    let file: AgentModelOverridesFile = serde_json::from_str(content)
        .map_err(|error| format!("模型覆盖表 JSON 格式无效：{error}"))?;
    file.into_overrides()
}

/// 从指定路径严格读取模型覆盖表；只有目标文件不存在时才返回空映射。
fn read_agent_model_overrides_from_path(path: &Path) -> Result<BTreeMap<String, String>, String> {
    if !current_regular_file_exists(path, "模型覆盖表")? {
        return Ok(BTreeMap::new());
    }
    let content = read_text_limited(path)?;
    parse_agent_model_overrides(&content)
}

/// 读取覆盖表：文件不存在视为空表；存在但损坏、过期或含非法条目时报错。
fn read_agent_model_overrides(
    app: &AppHandle,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let path = agent_model_overrides_path(app)?;
    read_agent_model_overrides_from_path(&path)
}

/// 写入内置子智能体的模型覆盖；None 表示移除覆盖、恢复定义默认值。
fn write_agent_model_override(
    app: &AppHandle,
    name: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let path = agent_model_overrides_path(app)?;
    write_agent_model_override_at_path(&path, name, model)
}

/// 在指定路径严格更新一个内置子智能体的模型覆盖，并以当前 Schema 原子保存。
fn write_agent_model_override_at_path(
    path: &Path,
    name: &str,
    model: Option<&str>,
) -> Result<(), String> {
    let mut overrides = read_agent_model_overrides_from_path(path)?;
    let name = validate_agent_name(name)?;
    let model = model.map(normalize_model_reference).transpose()?;
    match model {
        Some(value) => {
            overrides.insert(name.clone(), value);
        }
        None => {
            overrides.remove(&name);
        }
    }
    let file = AgentModelOverridesFile::from_overrides(&overrides)?;
    let mut content = serde_json::to_vec_pretty(&file)
        .map_err(|error| format!("无法序列化模型覆盖表：{error}"))?;
    content.push(b'\n');
    atomic_write_private(path, &content)
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
    project_path: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<InspectMcpResult, String> {
    let _guard = state.lock_io()?;
    let project_root = resolve_extension_project_root(&app, project_path.as_deref())?;
    let (resolved, _) = load_effective_mcp(&app, runtime.inner(), project_root.as_deref())?;
    let servers = resolved
        .into_iter()
        .map(|(name, server)| mcp_dto(name, server))
        .collect();
    Ok(InspectMcpResult { servers })
}

/// 列出 KeenCode 管理的本地插件。
#[tauri::command]
pub fn plugins_list(
    project_path: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<PluginsListResult, String> {
    let _guard = state.lock_io()?;
    let manager = plugin_manager(&app)?;
    let installed = manager.load_state().map_err(|error| error.to_string())?;
    let project_root = resolve_extension_project_root(&app, project_path.as_deref())?;
    let snapshot = project_root
        .as_deref()
        .map(|project_root| plugin_runtime_snapshot(&app, project_root))
        .transpose()?
        .unwrap_or_default();
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
            .map(|plugin| plugin_provides(plugin))
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
pub async fn plugin_enable(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    set_plugin_enabled(&app, &state, runtime.inner(), &name, true).await
}

/// 禁用一个 KeenCode 管理的本地插件。
#[tauri::command]
pub async fn plugin_disable(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    set_plugin_enabled(&app, &state, runtime.inner(), &name, false).await
}

/// 修改 KeenCode 插件启用状态并立即刷新 Skills、Agents、Hooks 与 MCP 投影。
async fn set_plugin_enabled(
    app: &AppHandle,
    state: &State<'_, ExtensionsState>,
    runtime: &std::sync::Arc<crate::agent_runtime::AgentRuntime>,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    {
        let _guard = state.lock_io()?;
        let manager = plugin_manager(app)?;
        let id = resolve_installed_plugin_id(&manager, name)?;
        manager
            .set_enabled(&id, enabled)
            .map_err(|error| error.to_string())?;
    }
    refresh_known_runtime_projects(app, runtime).await
}

/// 从 KeenCode 本地插件清单中卸载一个插件，不删除用户的来源目录。
#[tauri::command]
pub async fn plugin_uninstall(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    {
        let _guard = state.lock_io()?;
        let manager = plugin_manager(&app)?;
        let id = resolve_installed_plugin_id(&manager, &name)?;
        let mut secrets = state
            .plugin_secrets
            .lock()
            .map_err(|_| "KeenCode 插件敏感配置锁已损坏".to_owned())?;
        manager
            .uninstall(&id, &mut *secrets)
            .map_err(|error| error.to_string())?;
    }
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 返回一个本地插件的安全详情。
#[tauri::command]
pub fn plugin_details(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<PluginDetailsResult, String> {
    let _guard = state.lock_io()?;
    let manager = plugin_manager(&app)?;
    let id = resolve_installed_plugin_id(&manager, &name)?;
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

/// 返回 KeenCode 插件 userConfig 定义与非敏感当前值。
#[tauri::command]
pub fn plugin_user_config_get(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<PluginUserConfigResult, String> {
    let _guard = state.lock_io()?;
    let manager = plugin_manager(&app)?;
    let id = resolve_installed_plugin_id(&manager, &name)?;
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

/// 校验并保存 KeenCode 插件 userConfig，保存后立即热刷新运行时。
#[tauri::command]
pub async fn plugin_user_config_set(
    name: String,
    values: BTreeMap<String, Value>,
    replace: Option<bool>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<PluginUserConfigResult, String> {
    let id = {
        let _guard = state.lock_io()?;
        let manager = plugin_manager(&app)?;
        let id = resolve_installed_plugin_id(&manager, &name)?;
        let mut secrets = state
            .plugin_secrets
            .lock()
            .map_err(|_| "KeenCode 插件敏感配置锁已损坏".to_owned())?;
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
        id
    };
    refresh_known_runtime_projects(&app, runtime.inner()).await?;
    plugin_user_config_get(id.to_string(), app, state)
}

/// 从本地目录或已添加的本地市场安装一个插件引用。
#[tauri::command]
pub async fn plugin_install(
    source: String,
    app: AppHandle,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    tracing::info!(target: "ipc.plugin_install", "插件安装命令进入");
    let blocking_app = app.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || plugin_install_blocking(source, blocking_app))
            .await
            .map_err(|error| format!("插件安装线程异常：{error}"))?;
    if let Err(error) = result {
        tracing::error!(target: "ipc.plugin_install", %error, "插件安装失败");
        return Err(error);
    }
    refresh_known_runtime_projects(&app, runtime.inner()).await?;
    tracing::info!(target: "ipc.plugin_install", "插件安装命令完成");
    Ok(())
}

/// 在 Tauri blocking 线程中执行插件安装；远程取得不会阻塞窗口线程。
fn plugin_install_blocking(source: String, app: AppHandle) -> Result<(), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("插件来源不能为空".to_owned());
    }
    let root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定插件缓存目录：{error}"))?
        .join("plugins");
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
        let manifest_bytes = read_bytes_limited(
            Path::new(&market.manifest_path),
            MAX_MARKETPLACE_MANIFEST_BYTES as u64,
            "市场清单",
        )?;
        let manifest = crate::plugins::parse_marketplace_manifest(&manifest_bytes)
            .map_err(|error| error.to_string())?;
        resolve_marketplace_plugin_install_plan(&requested, &market, &manifest, &downloads)?
    } else {
        let (materialized_root, _) = materialize_keencode_source(source, &downloads)?;
        let manifest =
            load_plugin_manifest(&materialized_root).map_err(|error| error.to_string())?;
        if !manifest.dependencies.is_empty() {
            return Err(
                "本地插件声明了依赖，但没有对应的 KeenCode marketplace 清单可解析".to_owned(),
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
    let _guard = state.lock_io()?;
    let manager = plugin_manager(&app)?;
    let mut secrets = state
        .plugin_secrets
        .lock()
        .map_err(|_| "KeenCode 插件敏感配置锁已损坏".to_owned())?;
    manager
        .install_from_directories(materials, UserConfigUpdate::default(), &mut *secrets)
        .map_err(|error| error.to_string())?;
    // 后续项目候选重建会再次读取 plugin_secrets，本函数只负责提交持久状态。
    drop(secrets);
    drop(_guard);
    drop(download_cleanup);
    Ok(())
}

/// 重新解析一个或全部已安装插件及其依赖，并按拓扑顺序原子更新。
#[tauri::command]
pub async fn plugin_update(
    name: Option<String>,
    app: AppHandle,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let blocking_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || plugin_update_blocking(name, blocking_app))
        .await
        .map_err(|error| format!("插件更新线程异常：{error}"))??;
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 在 Tauri blocking 线程中取得远程来源并提交插件更新。
fn plugin_update_blocking(name: Option<String>, app: AppHandle) -> Result<(), String> {
    let selected = {
        let state = app.state::<ExtensionsState>();
        let _guard = state.lock_io()?;
        let manager = plugin_manager(&app)?;
        let installed = manager.load_state().map_err(|error| error.to_string())?;
        let target = name
            .as_deref()
            .map(|value| resolve_installed_plugin_id(&manager, value))
            .transpose()?;
        let selected = installed
            .plugins
            .into_iter()
            .filter(|record| target.as_ref().is_none_or(|id| id == &record.id))
            .collect::<Vec<_>>();
        if target.is_some() && selected.is_empty() {
            return Err("找不到要更新的 KeenCode 插件".to_owned());
        }
        selected
    };

    // 所有远程取得、Git/npm/HTTP 和依赖清单解析都在锁外执行。临时下载目录
    // 由 guard 持有到状态提交完成，失败或成功都不会残留本次取得目录。
    let root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定插件缓存目录：{error}"))?
        .join("plugins");
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
                    "本地插件 {} 声明了依赖，但没有对应的 KeenCode marketplace 清单可解析",
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
        let manifest_bytes = read_bytes_limited(
            Path::new(&market.manifest_path),
            MAX_MARKETPLACE_MANIFEST_BYTES as u64,
            "市场清单",
        )?;
        let manifest = crate::plugins::parse_marketplace_manifest(&manifest_bytes)
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
    let manager = plugin_manager(&app)?;
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
        let current_manifest = read_bytes_limited(
            Path::new(&current.manifest_path),
            MAX_MARKETPLACE_MANIFEST_BYTES as u64,
            &format!("插件更新期间市场 {} 清单", current.name),
        )?;
        if current_manifest.as_slice() != manifest_bytes.as_slice() {
            return Err(format!(
                "插件更新期间市场 {} 清单已改变，已放弃提交",
                current.name
            ));
        }
    }
    if !materials.is_empty() {
        let mut secrets = state
            .plugin_secrets
            .lock()
            .map_err(|_| "KeenCode 插件敏感配置锁已损坏".to_owned())?;
        manager
            .install_from_directories(materials, UserConfigUpdate::default(), &mut *secrets)
            .map_err(|error| error.to_string())?;
        drop(secrets);
    }
    drop(_guard);
    drop(download_cleanup);
    Ok(())
}

/// 确认插件更新期间持久状态未被其他操作改写，避免提交过期取得结果。
fn ensure_plugin_update_snapshot_current(
    expected: &[InstalledPlugin],
    current: &crate::plugins::PluginState,
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

/// 比较会影响插件更新提交安全性的完整安装态快照。
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
pub async fn mcp_add(
    name: String,
    command: String,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
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
    let mut document = load_mcp_document_fail_closed(&app, &path, runtime.inner())?
        .unwrap_or_else(empty_mcp_document);
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
    runtime
        .revoke_mcp_extension_tools()
        .map_err(|error| format!("MCP 配置已保存，但撤销旧运行时工具失败：{error}"))?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
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
pub async fn mcp_import(
    config: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let imported =
        parse_mcp_import_text(&config).map_err(|error| format!("MCP 导入配置无效：{error}"))?;
    let imported_servers = mcp_server_map(&imported)?.clone();
    if imported_servers.is_empty() {
        return Err("MCP 导入配置至少需要包含一个 Server".to_owned());
    }

    let path = mcp_user_config_path(&app)?;
    let existing = load_mcp_document_fail_closed(&app, &path, runtime.inner())?
        .unwrap_or_else(empty_mcp_document);
    let merged = merge_mcp_documents(existing, imported)?;
    save_mcp_document(&path, &merged)?;
    runtime
        .revoke_mcp_extension_tools()
        .map_err(|error| format!("MCP 配置已保存，但撤销旧运行时工具失败：{error}"))?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 从 KeenCode 唯一 MCP 配置删除一个 Server。
#[tauri::command]
pub async fn mcp_remove(
    name: String,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let name = validate_extension_name(&name, "MCP Server")?;
    let path = mcp_user_config_path(&app)?;
    let Some(mut document) = load_mcp_document_fail_closed(&app, &path, runtime.inner())? else {
        return Err(format!("找不到 MCP Server {name}"));
    };
    if mcp_server_map_mut(&mut document)?.remove(&name).is_none() {
        return Err(format!("找不到 MCP Server {name}"));
    }
    save_mcp_document(&path, &document)?;
    runtime
        .revoke_mcp_extension_tools()
        .map_err(|error| format!("MCP 配置已保存，但撤销旧运行时工具失败：{error}"))?;
    drop(_guard);
    refresh_known_runtime_projects(&app, runtime.inner()).await
}

/// 对 MCP 配置结构和本机 stdio 命令可用性执行无副作用检查。
#[tauri::command]
pub fn mcp_doctor(
    focus: Option<String>,
    project_path: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
    runtime: State<'_, std::sync::Arc<crate::agent_runtime::AgentRuntime>>,
) -> Result<McpDoctorReport, String> {
    let _guard = state.lock_io()?;
    let focus = focus
        .as_deref()
        .map(|value| validate_extension_name(value, "MCP Server"))
        .transpose()?;
    let project_root = resolve_extension_project_root(&app, project_path.as_deref())?;
    let (resolved, sources) = load_effective_mcp(&app, runtime.inner(), project_root.as_deref())?;
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
    let data_root = crate::storage::root_dir(&app)
        .map_err(|error| format!("无法确定插件市场数据目录：{error}"))?;
    let marketplace_store = load_marketplace_store(&app)?;
    if marketplace_store.sources.is_empty() {
        return Ok(MarketplaceAvailableResult {
            plugins: Vec::new(),
            loading: false,
            error: None,
        });
    }
    let manager = plugin_manager(&app)?;
    let plugin_store = manager.load_state().map_err(|error| error.to_string())?;
    let installed = plugin_store
        .plugins
        .iter()
        .map(|plugin| plugin.id.to_string().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut plugins = Vec::new();
    for source in marketplace_store.sources {
        let (root, _) = canonical_marketplace_record_paths(&source)?;
        let catalog = load_marketplace_manifest_from_record(&source)?;
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
                            // 市场预览只统计组件，不执行插件；使用应用数据根，不能把
                            // 进程 current_dir 当作插件 project_dir。
                            let Ok(snapshot) = extract_components(
                                id.clone(),
                                &path,
                                &manifest,
                                &data_root,
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
                        Err(_) => continue,
                    }
                }
                _ => (plugin.description.clone(), plugin.version.clone(), 0, 0),
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
    Ok(MarketplaceAvailableResult {
        plugins,
        loading: false,
        error: None,
    })
}

/// 添加一个包含 `.keencode-plugin/marketplace.json` 的本地目录或清单文件。
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
        .join("plugins/marketplaces");
    let MaterializedMarketplace {
        root,
        manifest_path,
        catalog,
        mut cleanup,
    } = materialize_marketplace(source, &workspace)?;
    crate::plugins::validate_marketplace_name(&catalog.name).map_err(|error| error.to_string())?;
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
    save_marketplace_store(&app, &store)
}

/// 重新校验一个或全部用户显式登记的市场清单。
#[tauri::command]
pub fn marketplace_update(
    name: Option<String>,
    app: AppHandle,
    state: State<'_, ExtensionsState>,
) -> Result<(), String> {
    let _guard = state.lock_io()?;
    let target = name
        .as_deref()
        .map(|value| validate_extension_name(value, "市场"))
        .transpose()?;
    let store = load_marketplace_store(&app)?;
    let mut updated = 0usize;
    for source in &store.sources {
        if target
            .as_deref()
            .is_some_and(|name| !source.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        let _ = load_marketplace_manifest_from_record(source)?;
        updated += 1;
    }
    if let Some(target) = target.as_deref()
        && updated == 0
    {
        return Err(format!("找不到本地市场 {target}"));
    }
    Ok(())
}

/// 返回用户手工维护的 MCP 配置；插件 MCP 不直接写入此文件。
fn mcp_user_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|directory| directory.join("mcp.json"))
        .map_err(|error| format!("无法确定 KeenCode MCP 配置目录：{error}"))
}

/// 从启用插件快照提取不递归的精确 Skill 根，避免加载未声明的相邻目录。
fn runtime_skill_config_from_snapshot(
    data_root: PathBuf,
    project_root: PathBuf,
    snapshot: PluginRuntimeSnapshot,
) -> keencode_skills::SkillDiscoveryConfig {
    use keencode_skills::{SkillRoot, SkillSource};

    let mut additional_roots = Vec::new();
    let mut seen = BTreeSet::new();
    for plugin in snapshot.plugins {
        for file in plugin.skills {
            if file.path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
                continue;
            }
            let Some(root) = file.path.parent().map(Path::to_path_buf) else {
                continue;
            };
            if seen.insert(root.clone()) {
                additional_roots.push(SkillRoot {
                    path: root,
                    source: SkillSource::Plugin,
                    recursive: false,
                });
            }
        }
    }
    additional_roots.sort_by(|left, right| left.path.cmp(&right.path));
    keencode_skills::SkillDiscoveryConfig::new(data_root, project_root)
        .with_additional_roots(additional_roots)
}

/// 将插件命令的插件根相对路径转换为 KeenCode 的稳定命名空间。
///
/// 默认 `commands/foo.md` 映射为 `plugin:market:demo:foo`，嵌套
/// `commands/admin/check.md` 映射为 `plugin:market:demo:admin:check`；文件名去掉
/// `.md`，而不是把 `commands` 目录本身暴露到命令名中。
fn plugin_command_namespace(plugin_namespace: &str, relative_path: &Path) -> String {
    crate::plugins::plugin_command_namespace(plugin_namespace, relative_path)
}

/// 返回 KeenCode 本地市场清单路径。
fn marketplace_store_path(app: &AppHandle) -> Result<PathBuf, String> {
    crate::storage::root_dir(app)
        .map(|dir| dir.join("marketplaces.json"))
        .map_err(|error| format!("无法确定应用配置目录：{error}"))
}

/// 读取 KeenCode 本地市场清单。
fn load_marketplace_store(app: &AppHandle) -> Result<MarketplaceStore, String> {
    let path = marketplace_store_path(app)?;
    load_marketplace_store_from_path(&path)
}

/// 从明确路径严格读取插件市场状态；只有文件不存在时才返回当前空状态。
fn load_marketplace_store_from_path(path: &Path) -> Result<MarketplaceStore, String> {
    let store = read_json_or_default(path, "插件市场清单")?;
    validate_marketplace_store(&store)?;
    Ok(store)
}

/// 按市场记录保存的实际清单路径读取 KeenCode marketplace.json。
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

/// 从已校验的市场记录读取并解析当前唯一 marketplace 清单。
fn load_marketplace_manifest_from_record(
    source: &MarketplaceRecord,
) -> Result<crate::plugins::MarketplaceManifest, String> {
    let (_, manifest) = canonical_marketplace_record_paths(source)?;
    let bytes = read_bytes_limited(&manifest, MAX_MARKETPLACE_MANIFEST_BYTES as u64, "市场清单")?;
    crate::plugins::parse_marketplace_manifest(&bytes).map_err(|error| error.to_string())
}

/// 原子保存 KeenCode 本地市场清单。
fn save_marketplace_store(app: &AppHandle, store: &MarketplaceStore) -> Result<(), String> {
    save_marketplace_store_to_path(&marketplace_store_path(app)?, store)
}

/// 在明确路径原子保存当前插件市场状态。
fn save_marketplace_store_to_path(path: &Path, store: &MarketplaceStore) -> Result<(), String> {
    validate_marketplace_store(store)?;
    write_json_private(path, store, "插件市场清单")
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
    let bytes = read_bytes_limited(path, MAX_EXTENSION_FILE_BYTES, "文件")?;
    String::from_utf8(bytes)
        .map_err(|error| format!("文件不是 UTF-8 文本 {}：{error}", path.display()))
}

/// 按打开句柄有界读取普通文件，并复核读取期间路径未变成其他文件或符号链接。
fn read_bytes_limited(path: &Path, max_bytes: u64, label: &str) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取 {label} {}：{error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label}不是普通文件：{}", path.display()));
    }
    if metadata.len() > max_bytes {
        return Err(format!("{label}超过 {max_bytes} 字节：{}", path.display()));
    }
    let file = crate::storage::open_readonly_regular_file(path)
        .map_err(|error| format!("无法打开 {label} {}：{error}", path.display()))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("无法读取已打开{label}元数据 {}：{error}", path.display()))?;
    if !opened_metadata.is_file() || opened_metadata.len() != metadata.len() {
        return Err(format!("{label}在打开期间发生变化：{}", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取 {label} {}：{error}", path.display()))?;
    let actual_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if actual_len > max_bytes || actual_len != opened_metadata.len() {
        return Err(format!(
            "{label}在读取期间发生变化或超过 {max_bytes} 字节：{}",
            path.display()
        ));
    }
    let final_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法复核 {label} {}：{error}", path.display()))?;
    if final_metadata.file_type().is_symlink()
        || !final_metadata.is_file()
        || final_metadata.len() != metadata.len()
    {
        return Err(format!("{label}在读取期间发生变化：{}", path.display()));
    }
    Ok(bytes)
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
    if store.schema != MARKETPLACE_STORE_SCHEMA || store.version != MARKETPLACE_STORE_VERSION {
        return Err("插件市场状态 schema 或版本不受支持".to_owned());
    }
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
    /// 已通过边界检查的 SKILL.md 绝对路径。
    path: PathBuf,
    /// 与 `keencode-skills` 一致的根内稳定路径排序键。
    stable_path: String,
}

/// 递归扫描当前 Skill 根目录；不安全或无效候选按运行时发现规则跳过。
fn scan_skill_directory(dir: &Path) -> Vec<ScannedSkill> {
    let root_metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Vec::new();
    }
    let Ok(canonical_root) = fs::canonicalize(dir) else {
        return Vec::new();
    };
    let limits = keencode_skills::SkillLimits::default();
    let mut skills = Vec::new();
    let mut pending = vec![(canonical_root.clone(), PathBuf::new(), 0usize)];
    let mut entries_seen = 0usize;
    let mut manifests_seen = 0usize;
    'scan: while let Some((directory, relative_directory, depth)) = pending.pop() {
        let Ok(directory_entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut entries = directory_entries.filter_map(Result::ok).collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        entries_seen = entries_seen.saturating_add(entries.len());
        if entries_seen > limits.max_entries {
            break;
        }
        for entry in entries {
            let entry_path = entry.path();
            let relative = relative_directory.join(entry.file_name());
            let Ok(entry_type) = entry.file_type() else {
                continue;
            };
            if entry_type.is_symlink() {
                continue;
            }
            if entry_type.is_dir() {
                if depth >= limits.max_depth {
                    continue;
                }
                let Ok(canonical_directory) = fs::canonicalize(&entry_path) else {
                    continue;
                };
                if !canonical_directory.starts_with(&canonical_root) {
                    continue;
                }
                pending.push((canonical_directory, relative, depth + 1));
                continue;
            }
            if entry.file_name() != "SKILL.md" {
                continue;
            }
            if !entry_type.is_file() {
                continue;
            }
            manifests_seen = manifests_seen.saturating_add(1);
            if manifests_seen > limits.max_manifests {
                break 'scan;
            }
            let Ok(canonical_manifest) = fs::canonicalize(&entry_path) else {
                continue;
            };
            if !canonical_manifest.starts_with(&canonical_root) {
                continue;
            }
            let Ok((name, _)) = parse_skill_file(&canonical_manifest) else {
                continue;
            };
            skills.push(ScannedSkill {
                name,
                path: canonical_manifest,
                stable_path: relative
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase(),
            });
        }
    }
    skills.sort_by(|left, right| {
        left.stable_path
            .cmp(&right.stable_path)
            .then_with(|| left.path.cmp(&right.path))
    });
    skills
}

/// 读取并解析一个 SKILL.md 的 name 与 description。
fn parse_skill_file(path: &Path) -> Result<(String, String), String> {
    let limits = keencode_skills::SkillLimits::default();
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取 Skill 文件 {}：{error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("Skill 主文件必须是普通文件：{}", path.display()));
    }
    if metadata.len() > limits.max_skill_bytes {
        return Err(format!(
            "Skill 文件超过 {} 字节：{}",
            limits.max_skill_bytes,
            path.display()
        ));
    }
    let content = read_text_limited(path)?;
    let document = keencode_skills::parse_skill_document(&content, &limits)
        .map_err(|error| format!("Skill 无效 {}：{error}", path.display()))?;
    Ok((document.name, document.description))
}

/// 读取 KeenCode 唯一 MCP 配置。
fn load_effective_mcp(
    app: &AppHandle,
    runtime: &std::sync::Arc<crate::agent_runtime::AgentRuntime>,
    project_root: Option<&Path>,
) -> Result<(BTreeMap<String, ResolvedMcpServer>, Vec<McpDoctorSource>), String> {
    let path = mcp_user_config_path(app)?;
    let document = load_mcp_document_fail_closed(app, &path, runtime)?;
    let persisted = document.is_some();
    let document = document.unwrap_or_else(empty_mcp_document);
    let mut resolved = BTreeMap::new();
    for (name, config) in mcp_server_map(&document)? {
        validate_mcp_server_config(name, config)?;
        resolved.insert(
            name.clone(),
            ResolvedMcpServer {
                config: config.clone(),
                plugin_source: false,
            },
        );
    }
    if let Some(project_root) = project_root {
        let snapshot = plugin_runtime_snapshot(app, project_root)?;
        for plugin in snapshot.plugins {
            let plugin_namespace = plugin
                .id
                .runtime_namespace()
                .map_err(|error| error.to_string())?;
            for (name, config) in plugin.mcp_servers {
                let name = format!("{plugin_namespace}:{name}");
                validate_mcp_server_config(&name, &config)?;
                resolved.insert(
                    name,
                    ResolvedMcpServer {
                        config,
                        plugin_source: true,
                    },
                );
            }
        }
    }
    let source = if resolved.is_empty() && !persisted {
        McpDoctorSource {
            path: path_to_frontend(&path),
            status: "missing".to_owned(),
            server_count: 0,
        }
    } else {
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
/// `type` 是部分厂商配置中的传输提示，不属于 KeenCode 持久化 Schema：
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

/// 运行期发现用户 MCP 文件损坏时，同步撤销共享运行时旧 MCP 工具并返回原错误。
fn load_mcp_document_fail_closed(
    _app: &AppHandle,
    path: &Path,
    runtime: &std::sync::Arc<crate::agent_runtime::AgentRuntime>,
) -> Result<Option<McpDocument>, String> {
    match load_mcp_document(path) {
        Ok(document) => Ok(document),
        Err(error) => {
            // 配置读写锁由调用方持有；撤销只操作进程内延迟目录，不等待异步
            // 候选构建，因此返回错误前即可让当前 Turn 的旧 MCP 解析失效。
            runtime
                .revoke_mcp_extension_tools()
                .map_err(|revoke_error| {
                    format!("{error}；同时无法撤销旧 MCP 运行时工具：{revoke_error}")
                })?;
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
    /// 当前唯一 MCP Server Schema 允许的字段。
    const ALLOWED_FIELDS: &[&str] = &[
        "command", "args", "env", "url", "headers", "disabled", "oauth",
    ];
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
            if object.contains_key("oauth") {
                return Err(format!("stdio MCP Server {name} 不能声明 OAuth"));
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
            if mcp_oauth_settings(object, name)?.is_some()
                && object
                    .get("headers")
                    .and_then(Value::as_object)
                    .is_some_and(|headers| {
                        headers
                            .keys()
                            .any(|key| key.eq_ignore_ascii_case("authorization"))
                    })
            {
                return Err(format!(
                    "HTTP MCP Server {name} 的 OAuth 与 Authorization 请求头互斥"
                ));
            }
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

/// 解析显式预注册公共客户端配置；只接受当前字段，不存储或传递任何 OAuth 令牌。
fn mcp_oauth_settings(
    object: &Map<String, Value>,
    server_name: &str,
) -> Result<Option<crate::mcp_oauth::McpOAuthSettings>, String> {
    let Some(value) = object.get("oauth") else {
        return Ok(None);
    };
    let settings: crate::mcp_oauth::McpOAuthSettings = serde_json::from_value(value.clone())
        .map_err(|_| format!("MCP Server {server_name} 的 OAuth 配置不符合当前结构"))?;
    settings
        .validate()
        .map_err(|_| format!("MCP Server {server_name} 的 OAuth 配置无效"))?;
    Ok(Some(settings))
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

/// 将 MCP 启用状态写回 KeenCode 唯一配置文件。
fn persist_mcp_enabled(
    app: &AppHandle,
    updates: &[(&str, bool)],
    runtime: &std::sync::Arc<crate::agent_runtime::AgentRuntime>,
) -> Result<(), String> {
    let path = mcp_user_config_path(app)?;
    let Some(mut document) = load_mcp_document_fail_closed(app, &path, runtime)? else {
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
        runtime
            .revoke_mcp_extension_tools()
            .map_err(|error| format!("MCP 配置已保存，但撤销旧运行时工具失败：{error}"))?;
    }
    Ok(())
}

/// 在一个 MCP 文档中写入 KeenCode 唯一 Schema 的 disabled 字段。
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
        source: if server.plugin_source {
            "plugin".to_owned()
        } else {
            "user".to_owned()
        },
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
mod agent_model_overrides_tests {
    use super::*;

    /// 当前覆盖表使用固定外壳，并能还原规范的模型映射。
    #[test]
    fn current_schema_round_trips() {
        let overrides = BTreeMap::from([("plan".to_owned(), "openai::gpt-5".to_owned())]);
        let file = AgentModelOverridesFile::from_overrides(&overrides).expect("当前映射应有效");
        let content = serde_json::to_string(&file).expect("当前覆盖表应可序列化");
        let object: Value = serde_json::from_str(&content).expect("序列化结果应是 JSON 对象");
        assert_eq!(object["schema"], AGENT_MODEL_OVERRIDES_SCHEMA);
        assert_eq!(object["version"], AGENT_MODEL_OVERRIDES_VERSION);
        assert_eq!(object["overrides"]["plan"], "openai::gpt-5");
        assert_eq!(parse_agent_model_overrides(&content).unwrap(), overrides);
    }

    /// 当前解析必须拒绝未知字段、缺失外壳、旧版本和损坏 JSON。
    #[test]
    fn schema_rejects_unknown_missing_legacy_and_corrupt_documents() {
        let invalid_documents = [
            r#"{"schema":"keencode/agent-model-overrides","version":1,"overrides":{},"extra":true}"#,
            r#"{"schema":"keencode/agent-model-overrides","version":1}"#,
            r#"{"plan":"openai::gpt-5"}"#,
            r#"{"schema":"keencode/agent-model-overrides","version":0,"overrides":{}}"#,
            "{ invalid json",
        ];
        for content in invalid_documents {
            assert!(
                parse_agent_model_overrides(content).is_err(),
                "文档必须被拒绝：{content}"
            );
        }
    }

    /// 当前解析必须拒绝非法条目和重复键，不能静默过滤或选择最后一个值。
    #[test]
    fn schema_rejects_invalid_entries_and_duplicate_keys() {
        let invalid_documents = [
            r#"{"schema":"keencode/agent-model-overrides","version":1,"overrides":{"bad name":"openai::gpt-5"}}"#,
            r#"{"schema":"keencode/agent-model-overrides","version":1,"overrides":{"plan":"gpt-5"}}"#,
            r#"{"schema":"keencode/agent-model-overrides","version":1,"overrides":{" plan":"openai::gpt-5"}}"#,
            r#"{"schema":"keencode/agent-model-overrides","version":1,"overrides":{"plan":" openai::gpt-5"}}"#,
            r#"{"schema":"keencode/agent-model-overrides","version":1,"overrides":{"plan":"openai::gpt-5","plan":"openai::gpt-4"}}"#,
        ];
        for content in invalid_documents {
            assert!(
                parse_agent_model_overrides(content).is_err(),
                "条目必须被拒绝：{content}"
            );
        }
    }

    /// 只有目标文件不存在时才返回空映射，空文件和损坏文件都必须失败。
    #[test]
    fn missing_path_is_empty_but_present_invalid_path_is_error() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let missing = directory.path().join("missing.json");
        assert!(
            read_agent_model_overrides_from_path(&missing)
                .expect("缺失文件应返回空映射")
                .is_empty()
        );

        let present = directory.path().join("present.json");
        fs::write(&present, b"{}").expect("写入空 JSON");
        assert!(read_agent_model_overrides_from_path(&present).is_err());
    }

    /// 非普通文件与超限文件必须在解析前失败，并保持原目标不变。
    #[test]
    fn non_file_and_oversized_paths_are_rejected_without_replacement() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let non_file = directory.path().join("directory.json");
        fs::create_dir(&non_file).expect("创建目录目标");
        assert!(read_agent_model_overrides_from_path(&non_file).is_err());
        assert!(non_file.is_dir());

        let oversized = directory.path().join("oversized.json");
        let original = vec![b'x'; MAX_EXTENSION_FILE_BYTES as usize + 1];
        fs::write(&oversized, &original).expect("写入超限覆盖表");
        assert!(read_agent_model_overrides_from_path(&oversized).is_err());
        assert_eq!(fs::read(&oversized).expect("读取原超限文件"), original);
    }

    /// 严格读取失败时不能用空映射覆盖原始文件字节。
    #[test]
    fn failed_update_preserves_original_bytes() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("agent-model-overrides.json");
        let original = br#"{"schema":"keencode/agent-model-overrides","version":1,"overrides":{"plan":"openai::gpt-5"},"extra":true}"#;
        fs::write(&path, original).expect("写入非法覆盖表");

        assert!(write_agent_model_override_at_path(&path, "plan", Some("openai::gpt-4")).is_err());
        assert_eq!(fs::read(&path).expect("读取原始覆盖表"), original);
    }

    /// 成功更新会写入当前外壳，并仍然支持清除已有覆盖。
    #[test]
    fn update_writes_current_schema_and_removes_override() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("agent-model-overrides.json");

        write_agent_model_override_at_path(&path, "plan", Some("openai::gpt-5"))
            .expect("首次写入应成功");
        let saved = fs::read_to_string(&path).expect("读取保存结果");
        assert!(saved.ends_with('\n'));
        assert!(
            parse_agent_model_overrides(&saved)
                .expect("保存结果应符合当前 Schema")
                .contains_key("plan")
        );

        write_agent_model_override_at_path(&path, "plan", None).expect("清除覆盖应成功");
        assert!(
            parse_agent_model_overrides(&fs::read_to_string(&path).expect("读取清除结果"))
                .expect("清除结果应符合当前 Schema")
                .is_empty()
        );
    }
}

#[cfg(test)]
// 大型回归测试独立存放，避免生产模块重新膨胀。
#[path = "extensions/tests.rs"]
mod tests;
