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

/// Claude marketplace 插件来源的完整本地表示；额外字段不能在 `PluginSource` 归一化时丢失。
#[derive(Clone, Debug, PartialEq, Eq)]
enum MarketplacePluginSourceSpec {
    /// 市场仓库内的相对插件目录。
    Relative { path: String },
    /// Git 仓库来源及可选子目录、稀疏路径和固定版本。
    Git {
        url: String,
        path: Option<String>,
        reference: Option<String>,
        sha: Option<String>,
        sparse_paths: Vec<String>,
    },
    /// npm 包来源。
    Npm {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
    /// Claude schema 允许声明 pip；当前官方加载器也会拒绝实际安装，因此保留字段后给出明确错误。
    Pip {
        package: String,
        version: Option<String>,
        registry: Option<String>,
    },
    /// 带请求头的 HTTP 归档来源；主要用于兼容私有 URL 扩展。
    HttpArchive {
        url: String,
        headers: BTreeMap<String, String>,
    },
}

/// 读取 Claude marketplace 原始 JSON 中某个插件的 source，保留 `path`、`sparsePaths`、headers 等未知字段。
fn load_raw_marketplace_plugin_source(
    marketplace_manifest: &Path,
    plugin_name: &str,
) -> Result<Option<Value>, String> {
    let bytes =
        fs::read(marketplace_manifest).map_err(|error| format!("无法读取市场清单：{error}"))?;
    if bytes.len() as u64 > MAX_EXTENSION_FILE_BYTES {
        return Err(format!("市场清单超过 {MAX_EXTENSION_FILE_BYTES} 字节"));
    }
    let document = serde_json::from_slice::<Value>(&bytes)
        .map_err(|error| format!("市场清单 JSON 格式无效：{error}"))?;
    let Some(plugins) = document.get("plugins").and_then(Value::as_array) else {
        return Ok(None);
    };
    Ok(plugins
        .iter()
        .find(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == plugin_name)
        })
        .and_then(|entry| entry.get("source").cloned()))
}

/// 将原始 marketplace source 解析为不会丢失关键固定字段的来源计划。
fn parse_marketplace_plugin_source(value: Value) -> Result<MarketplacePluginSourceSpec, String> {
    let Value::String(path) = value else {
        let object = value
            .as_object()
            .ok_or_else(|| "插件 source 必须是字符串或对象".to_owned())?;
        let source = object
            .get("source")
            .and_then(Value::as_str)
            .ok_or_else(|| "插件 source 缺少 source 字段".to_owned())?;
        let optional_text = |key: &str| -> Result<Option<String>, String> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
                Some(_) => Err(format!("插件 source.{key} 必须是非空字符串")),
            }
        };
        let sparse_paths = match object.get("sparsePaths") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(paths)) => paths
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|path| !path.trim().is_empty())
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| "插件 source.sparsePaths 必须是非空字符串数组".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err("插件 source.sparsePaths 必须是字符串数组".to_owned()),
        };
        let path = optional_text("path")?;
        return match source {
            "github" => {
                let repo = object
                    .get("repo")
                    .and_then(Value::as_str)
                    .filter(|repo| !repo.trim().is_empty())
                    .ok_or_else(|| "github 插件 source 缺少 repo".to_owned())?;
                let url = format!("https://github.com/{repo}.git");
                Ok(MarketplacePluginSourceSpec::Git {
                    url,
                    path,
                    reference: optional_text("ref")?,
                    sha: optional_text("sha")?,
                    sparse_paths,
                })
            }
            "url" | "git" | "git-subdir" => {
                let url = object
                    .get("url")
                    .and_then(Value::as_str)
                    .filter(|url| !url.trim().is_empty())
                    .ok_or_else(|| format!("{source} 插件 source 缺少 url"))?;
                let headers = parse_http_headers(object.get("headers"))?;
                if source == "url" && !headers.is_empty() && path.is_none() {
                    return Ok(MarketplacePluginSourceSpec::HttpArchive {
                        url: url.to_owned(),
                        headers,
                    });
                }
                Ok(MarketplacePluginSourceSpec::Git {
                    url: url.to_owned(),
                    path: if source == "git-subdir" {
                        Some(path.ok_or_else(|| "git-subdir 插件 source 缺少 path".to_owned())?)
                    } else {
                        path
                    },
                    reference: optional_text("ref")?,
                    sha: optional_text("sha")?,
                    sparse_paths,
                })
            }
            "npm" => Ok(MarketplacePluginSourceSpec::Npm {
                package: object
                    .get("package")
                    .and_then(Value::as_str)
                    .filter(|package| !package.trim().is_empty())
                    .ok_or_else(|| "npm 插件 source 缺少 package".to_owned())?
                    .to_owned(),
                version: optional_text("version")?,
                registry: optional_text("registry")?
                    .map(|registry| validate_http_source_url(&registry, "npm registry URL"))
                    .transpose()?,
            }),
            "pip" => Ok(MarketplacePluginSourceSpec::Pip {
                package: object
                    .get("package")
                    .and_then(Value::as_str)
                    .filter(|package| !package.trim().is_empty())
                    .ok_or_else(|| "pip 插件 source 缺少 package".to_owned())?
                    .to_owned(),
                version: optional_text("version")?,
                registry: optional_text("registry")?
                    .map(|registry| validate_http_source_url(&registry, "pip registry URL"))
                    .transpose()?,
            }),
            other => Err(format!("不支持的插件 source：{other}")),
        };
    };
    Ok(MarketplacePluginSourceSpec::Relative { path })
}

/// 解析 URL source 的 HTTP headers，并拒绝换行等会污染请求的值。
fn parse_http_headers(value: Option<&Value>) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "URL source headers 必须是对象".to_owned())?;
    let mut headers = BTreeMap::new();
    for (name, value) in object {
        let value = value
            .as_str()
            .ok_or_else(|| format!("URL source header {name} 必须是字符串"))?;
        if name.trim().is_empty()
            || name.bytes().any(|byte| byte.is_ascii_control())
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(format!("URL source header {name} 包含非法控制字符"));
        }
        headers.insert(name.clone(), interpolate_source_header(value)?);
    }
    Ok(headers)
}

/// 使用当前环境变量插值 `${NAME}`，不把环境变量值写入错误文本。
fn interpolate_source_header(value: &str) -> Result<String, String> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| "URL source header 变量缺少闭合 }".to_owned())?;
        let name = &after[..end];
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("URL source header 变量名无效".to_owned());
        }
        let value =
            env::var(name).map_err(|_| format!("URL source header 缺少环境变量：{name}"))?;
        output.push_str(&value);
        rest = &after[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

/// 安装来源的实际物化目录；本地来源直接复用，远程来源使用系统工具取得后校验。
fn materialize_claude_source(
    source: &str,
    workspace: &Path,
) -> Result<(PathBuf, Option<String>), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("插件来源不能为空".to_owned());
    }
    let expanded = expand_tilde(source)?;
    if expanded.exists() {
        let canonical = fs::canonicalize(&expanded)
            .map_err(|error| format!("无法访问插件来源 {}：{error}", expanded.display()))?;
        let root = if canonical.is_file() {
            canonical
                .parent()
                .ok_or_else(|| "插件清单缺少父目录".to_owned())?
                .to_path_buf()
        } else {
            canonical
        };
        return Ok((root, None));
    }
    let parsed = if source.starts_with("http://") || source.starts_with("https://") {
        PluginSource::Url {
            url: source.to_owned(),
            reference: None,
            sha: None,
        }
    } else if let Some(package) = source.strip_prefix("npm:") {
        PluginSource::Npm {
            package: package.to_owned(),
            version: None,
            registry: None,
        }
    } else if let Some(url) = source.strip_prefix("git:") {
        PluginSource::GitSubdir {
            url: url.to_owned(),
            path: ".".to_owned(),
            reference: None,
            sha: None,
        }
    } else if let Some(package) = source.strip_prefix("pip:") {
        PluginSource::Pip {
            package: package.to_owned(),
            version: None,
            registry: None,
        }
    } else {
        serde_json::from_value::<PluginSource>(Value::String(source.to_owned()))
            .map_err(|error| error.to_string())?
    };
    materialize_claude_plugin_source(&parsed, workspace)
}

/// 按插件清单中的完整来源执行物化；保留 `ref`/`sha`，避免先转成字符串时丢失固定版本。
fn materialize_claude_plugin_source(
    parsed: &PluginSource,
    workspace: &Path,
) -> Result<(PathBuf, Option<String>), String> {
    if let PluginSource::Relative { path } = parsed {
        let expanded = expand_tilde(path)?;
        if expanded.exists() {
            let canonical = fs::canonicalize(&expanded)
                .map_err(|error| format!("无法访问插件来源 {}：{error}", expanded.display()))?;
            let root = if canonical.is_file() {
                canonical
                    .parent()
                    .ok_or_else(|| "插件清单缺少父目录".to_owned())?
                    .to_path_buf()
            } else {
                canonical
            };
            return Ok((root, None));
        }
        return Err(format!("插件相对路径不存在：{path}"));
    }
    let plan = parsed
        .fetch_plan(workspace)
        .map_err(|error| error.to_string())?;
    let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
    match plan {
        crate::claude_plugins::SourceFetchPlan::Directory { path } => Ok((path, None)),
        crate::claude_plugins::SourceFetchPlan::File { path } => Ok((
            path.parent()
                .ok_or_else(|| "插件文件缺少父目录".to_owned())?
                .to_path_buf(),
            None,
        )),
        crate::claude_plugins::SourceFetchPlan::Git {
            url,
            reference,
            sha,
            subdir,
        } => {
            let sparse_paths = subdir
                .as_ref()
                .map(|path| vec![path.to_string_lossy().into_owned()])
                .unwrap_or_default();
            clone_git_source(
                &url,
                reference.as_deref(),
                sha.as_deref(),
                !sparse_paths.is_empty(),
                &target,
                "Git 插件来源",
            )?;
            if let Some(sha) = sha {
                checkout_git_sha(&target, &sha, "Git 插件来源")?;
            }
            apply_git_sparse_paths(&target, &sparse_paths, "Git 插件来源")?;
            let root = resolve_git_plugin_root(&target, subdir.as_deref(), "Git 插件来源")?;
            Ok((root, None))
        }
        crate::claude_plugins::SourceFetchPlan::Npm {
            package_spec,
            registry,
        } => {
            let archive_dir = target.join("npm");
            fs::create_dir_all(&archive_dir)
                .map_err(|error| format!("创建 npm 目录失败：{error}"))?;
            let mut pack = process::Command::new("npm");
            pack.current_dir(&archive_dir)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(&package_spec);
            if let Some(registry) = registry {
                pack.arg("--registry").arg(registry);
            }
            run_external(&mut pack, "npm 插件来源")?;
            let archive = fs::read_dir(&archive_dir)
                .map_err(|error| format!("读取 npm 归档失败：{error}"))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 插件来源",
            )?;
            let package_root = target.join("package");
            Ok((package_root, None))
        }
        crate::claude_plugins::SourceFetchPlan::Pip { package_spec, .. } => Err(format!(
            "pip 插件来源已解析为安全计划，但 Claude Code 当前加载器不支持 Python 包插件：{package_spec}"
        )),
        crate::claude_plugins::SourceFetchPlan::Http { url } => {
            let bytes = http_get_with_headers(
                &url,
                &BTreeMap::new(),
                "插件 URL",
                MAX_PLUGIN_HTTP_ARCHIVE_BYTES,
            )?;
            let archive = target.join("plugin.archive");
            fs::write(&archive, &bytes).map_err(|error| format!("保存插件归档失败：{error}"))?;
            extract_archive(&target, &archive, &url, "插件 URL")?;
            let root = find_plugin_root(&target)?;
            Ok((root, None))
        }
    }
}

/// 物化 marketplace 条目原始 source，保留 `path`/`sparsePaths`/URL headers。
fn materialize_marketplace_plugin_source(
    spec: MarketplacePluginSourceSpec,
    marketplace_root: &Path,
    workspace: &Path,
) -> Result<PathBuf, String> {
    match spec {
        MarketplacePluginSourceSpec::Relative { path } => {
            resolve_marketplace_relative_path(marketplace_root, &path)
        }
        MarketplacePluginSourceSpec::Git {
            url,
            path,
            reference,
            sha,
            mut sparse_paths,
        } => {
            let subdir = path
                .as_deref()
                .map(|path| validate_source_relative_path(path, "Git 插件 path"))
                .transpose()?;
            if let Some(path) = &subdir {
                let value = path.to_string_lossy().into_owned();
                if !sparse_paths.iter().any(|item| item == &value) {
                    sparse_paths.insert(0, value);
                }
            }
            for path in &sparse_paths {
                validate_source_relative_path(path, "Git 插件 sparsePaths")?;
            }
            let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
            clone_git_source(
                &url,
                reference.as_deref(),
                sha.as_deref(),
                !sparse_paths.is_empty(),
                &target,
                "Git 插件来源",
            )?;
            if let Some(sha) = sha.as_deref() {
                checkout_git_sha(&target, sha, "Git 插件来源")?;
            }
            apply_git_sparse_paths(&target, &sparse_paths, "Git 插件来源")?;
            let root = resolve_git_plugin_root(&target, subdir.as_deref(), "Git 插件来源")?;
            Ok(root)
        }
        MarketplacePluginSourceSpec::Npm {
            package,
            version,
            registry,
        } => {
            let package_spec = match version {
                Some(version) => format!("{package}@{version}"),
                None => package,
            };
            let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
            let archive_dir = target.join("npm");
            fs::create_dir_all(&archive_dir)
                .map_err(|error| format!("创建 npm 目录失败：{error}"))?;
            let mut pack = process::Command::new("npm");
            pack.current_dir(&archive_dir)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(&package_spec);
            if let Some(registry) = registry {
                pack.arg("--registry").arg(registry);
            }
            run_external(&mut pack, "npm 插件来源")?;
            let archive = fs::read_dir(&archive_dir)
                .map_err(|error| format!("读取 npm 归档失败：{error}"))?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 插件来源",
            )?;
            Ok(target.join("package"))
        }
        MarketplacePluginSourceSpec::Pip {
            package,
            version,
            registry,
        } => Err(format!(
            "pip 插件来源已解析（包 {package}，版本 {}，registry {}），但 Claude Code 当前加载器不支持 Python 包插件",
            version.as_deref().unwrap_or("latest"),
            if registry.is_some() {
                "已配置"
            } else {
                "默认"
            }
        )),
        MarketplacePluginSourceSpec::HttpArchive { url, headers } => {
            let target = create_unique_temp_dir(workspace, "fetch", "创建插件取得目录失败")?;
            let bytes =
                http_get_with_headers(&url, &headers, "插件 URL", MAX_PLUGIN_HTTP_ARCHIVE_BYTES)?;
            let archive = target.join("plugin.archive");
            fs::write(&archive, &bytes).map_err(|error| format!("保存插件归档失败：{error}"))?;
            extract_archive(&target, &archive, &url, "插件 URL")?;
            find_plugin_root(&target)
        }
    }
}

/// 校验 Git 克隆后的插件子目录仍是克隆根内的真实目录。
///
/// 先检查未跟随符号链接的选中项，再规范化路径并验证目录边界，避免把
/// 仓库中的链接当作插件根目录加载到克隆根外。
fn resolve_git_plugin_root(
    target: &Path,
    subdir: Option<&Path>,
    label: &str,
) -> Result<PathBuf, String> {
    let canonical_target =
        fs::canonicalize(target).map_err(|error| format!("无法访问{label}取得目录：{error}"))?;
    let candidate = subdir
        .map(|path| target.join(path))
        .unwrap_or_else(|| target.to_path_buf());
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|error| format!("{label}缺少目录 {}：{error}", candidate.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{label}目录不能是符号链接：{}",
            candidate.display()
        ));
    }
    let canonical_candidate = fs::canonicalize(&candidate)
        .map_err(|error| format!("无法访问{label}目录 {}：{error}", candidate.display()))?;
    if !canonical_candidate.starts_with(&canonical_target) || !canonical_candidate.is_dir() {
        return Err(format!(
            "{label}目录必须位于取得目录内：{}",
            candidate.display()
        ));
    }
    Ok(canonical_candidate)
}

/// 物化一个 Claude marketplace 条目，并在官方市场允许省略 plugin.json 时生成
/// 受控的合成清单。该函数只复制/解包来源，不执行插件自身的安装脚本。
fn materialize_marketplace_plugin_entry(
    entry: &crate::claude_plugins::MarketplacePlugin,
    marketplace_manifest: &Path,
    marketplace_root: &Path,
    marketplace_plugin_root: Option<&Path>,
    downloads: &Path,
) -> Result<PathBuf, String> {
    let materialized_root = match entry.source.clone() {
        PluginSource::Relative { path } => {
            // `./` 是 Claude marketplace 表示“市场根目录即插件根目录”的合法来源。
            let relative = validate_source_relative_path(&path, "插件相对路径")?;
            let base = marketplace_plugin_root
                .as_deref()
                .map(|root| {
                    canonical_child_without_symlinks(marketplace_root, root, "市场 pluginRoot")
                })
                .transpose()?;
            canonical_child_without_symlinks(
                base.as_deref().unwrap_or(marketplace_root),
                &relative,
                "市场插件路径",
            )?
        }
        other => {
            let raw_source = load_raw_marketplace_plugin_source(marketplace_manifest, &entry.name)?;
            if let Some(raw_source) = raw_source {
                let spec = parse_marketplace_plugin_source(raw_source)?;
                materialize_marketplace_plugin_source(spec, marketplace_root, downloads)?
            } else {
                materialize_claude_plugin_source(&other, downloads)?.0
            }
        }
    };
    validate_directory_tree_without_symlinks(&materialized_root, "市场插件")?;
    if materialized_root
        .join(crate::claude_plugins::CLAUDE_PLUGIN_MANIFEST)
        .is_file()
    {
        return Ok(materialized_root);
    }

    // 官方市场的部分条目只声明 lspServers/skills 等组件；只在 KeenCode
    // 自有下载缓存中生成清单，绝不改写用户添加的市场源目录。
    let synthetic_workspace =
        create_unique_temp_dir(downloads, "synthetic", "创建插件合成目录失败")?;
    let destination = synthetic_workspace.join("plugin");
    materialize_synthetic_marketplace_plugin(&materialized_root, &destination, entry)
        .map_err(|error| error.to_string())?;
    validate_directory_tree_without_symlinks(&destination, "市场合成插件")?;
    Ok(destination)
}

/// 解析 marketplace 条目的完整依赖闭包，返回依赖在前的物化安装计划。
///
/// 先取得并校验闭包中每个插件的 `.claude-plugin/plugin.json`，再调用共享依赖
/// 拓扑解析器检查缺失/循环；调用方只有在本函数成功后才可写入插件状态。
fn resolve_marketplace_plugin_install_plan(
    requested: &PluginId,
    market: &MarketplaceRecord,
    marketplace: &crate::claude_plugins::MarketplaceManifest,
    downloads: &Path,
) -> Result<Vec<MaterializedPlugin>, String> {
    let marketplace_name = marketplace.name.clone();
    if let Some(namespace) = requested.marketplace.as_deref() {
        if !namespace.eq_ignore_ascii_case(&marketplace_name) {
            return Err(format!(
                "请求插件市场 {namespace} 与当前市场 {marketplace_name} 不一致"
            ));
        }
    }
    let entries = marketplace
        .plugins
        .iter()
        .try_fold(BTreeMap::new(), |mut entries, entry| {
            let key = marketplace_name_key(&entry.name);
            if entries.insert(key, entry).is_some() {
                return Err(format!("市场插件名称重复（忽略大小写）：{}", entry.name));
            }
            Ok(entries)
        })?;
    let requested_entry = entries
        .get(&marketplace_name_key(&requested.plugin))
        .ok_or_else(|| format!("市场 {marketplace_name} 中不存在插件 {}", requested.plugin))?;
    let requested = PluginId {
        plugin: requested_entry.name.clone(),
        marketplace: Some(marketplace_name.clone()),
    };
    let marketplace_root =
        fs::canonicalize(&market.path).map_err(|error| format!("无法访问市场根目录：{error}"))?;
    let marketplace_plugin_root = marketplace
        .metadata
        .get("pluginRoot")
        .and_then(Value::as_str)
        .map(|value| validate_source_relative_path(value, "市场 metadata.pluginRoot"))
        .transpose()?;

    let mut manifests = BTreeMap::new();
    let mut materialized = BTreeMap::new();
    let mut pending = vec![requested.clone()];
    let mut queued = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if !queued.insert(id.clone()) {
            continue;
        }
        let Some(entry) = entries.get(&marketplace_name_key(&id.plugin)) else {
            // 由 dependency_closure 统一生成明确的缺失依赖错误。
            continue;
        };
        let source_root = materialize_marketplace_plugin_entry(
            entry,
            Path::new(&market.manifest_path),
            &marketplace_root,
            marketplace_plugin_root.as_deref(),
            downloads,
        )?;
        let manifest = load_plugin_manifest(&source_root).map_err(|error| error.to_string())?;
        if !manifest.name.eq_ignore_ascii_case(&id.plugin) {
            return Err(format!(
                "市场插件 {} 与 plugin.json name {} 不一致",
                id, manifest.name
            ));
        }
        let mut dependencies = entry.dependencies.clone();
        dependencies.extend(manifest.dependencies.clone());
        for dependency in dependencies.keys() {
            let dependency = PluginId::parse(dependency).map_err(|error| error.to_string())?;
            match dependency.marketplace.as_deref() {
                None => {
                    let plugin = entries
                        .get(&marketplace_name_key(&dependency.plugin))
                        .map(|entry| entry.name.clone())
                        .unwrap_or_else(|| dependency.plugin.clone());
                    pending.push(PluginId {
                        plugin,
                        marketplace: Some(marketplace_name.clone()),
                    });
                }
                Some(namespace) if namespace.eq_ignore_ascii_case(&marketplace_name) => {
                    let plugin = entries
                        .get(&marketplace_name_key(&dependency.plugin))
                        .map(|entry| entry.name.clone())
                        .unwrap_or_else(|| dependency.plugin.clone());
                    pending.push(PluginId {
                        plugin,
                        marketplace: Some(marketplace_name.clone()),
                    });
                }
                Some(_) => {
                    // 不尝试取得其他市场；共享拓扑解析器会返回跨市场错误。
                }
            }
        }
        manifests.insert(id.plugin.clone(), manifest);
        materialized.insert(id, source_root);
    }

    let order = crate::claude_plugins::dependency_closure(&requested, marketplace, &manifests)
        .map_err(|error| error.to_string())?;
    order
        .into_iter()
        .map(|id| {
            let source_root = materialized
                .remove(&id)
                .ok_or_else(|| format!("没有已物化的插件来源，无法安装依赖：{id}"))?;
            Ok(MaterializedPlugin { id, source_root })
        })
        .collect()
}

/// 解析并校验 marketplace 根目录下的相对来源路径。
fn resolve_marketplace_relative_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let relative = validate_source_relative_path(raw, "插件相对路径")?;
    let candidate = canonical_child_without_symlinks(root, &relative, "市场插件路径")?;
    if !candidate.is_dir() {
        return Err("市场插件路径必须位于市场根目录内".to_owned());
    }
    Ok(candidate)
}

/// 仅允许跨平台安全的相对路径；保留 Claude 常见的 `./` 前缀。
fn validate_source_relative_path(raw: &str, label: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{label}不能为空"));
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err(format!("{label}必须是安全相对路径：{raw}"));
    }
    Ok(path.to_path_buf())
}

/// 通过 reqwest 下载带 headers 的 HTTP 内容；错误文本不会回显 header 值。
fn http_get_with_headers(
    url: &str,
    headers: &BTreeMap<String, String>,
    label: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(PLUGIN_REMOTE_TIMEOUT)
        .timeout(PLUGIN_REMOTE_TIMEOUT)
        .build()
        .map_err(|error| format!("构建{label}客户端失败：{error}"))?;
    let mut request = client.get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .map_err(|error| format!("下载{label}失败：{error}"))?
        .error_for_status()
        .map_err(|error| format!("下载{label}返回错误：{error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(format!("{label}响应超过 {max_bytes} 字节"));
    }
    let mut bytes = Vec::new();
    response
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("读取{label}响应失败：{error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("{label}响应超过 {max_bytes} 字节"));
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
}

/// 按来源扩展名选择归档格式；未知 URL 延续原有 tar.gz 约定。
fn archive_format(source: &str) -> ArchiveFormat {
    let source = source.split(['?', '#']).next().unwrap_or(source);
    let source = source.to_ascii_lowercase();
    if source.ends_with(".zip") {
        ArchiveFormat::Zip
    } else if source.ends_with(".tar") {
        ArchiveFormat::Tar
    } else {
        ArchiveFormat::TarGz
    }
}

/// 归档内路径必须是非空的普通相对路径；任何 `..`、绝对路径或 NUL 都拒绝。
fn validate_archive_entry_path(path: &Path, label: &str) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() || path.to_string_lossy().contains('\0') {
        return Err(format!("{label}路径为空或包含 NUL"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(value) => normalized.push(value),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir
            | std::path::Component::Prefix(_)
            | std::path::Component::RootDir => {
                return Err(format!("{label}路径越界：{}", path.display()));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(format!("{label}路径为空"));
    }
    Ok(normalized)
}

/// 归档解包只能写入自己创建的普通目录，且不能以路径组件符号链接穿透边界。
fn ensure_archive_directory(root: &Path, directory: &Path, label: &str) -> Result<(), String> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| format!("{label}目录越出解包根目录"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(format!("{label}目录路径无效：{}", directory.display()));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("{label}目录不能是符号链接：{}", current.display()));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("{label}目录不是普通目录：{}", current.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| {
                    format!("创建{label}目录失败 {}：{error}", current.display())
                })?;
            }
            Err(error) => {
                return Err(format!(
                    "读取{label}目录失败 {}：{error}",
                    current.display()
                ));
            }
        }
    }
    let canonical =
        fs::canonicalize(directory).map_err(|error| format!("无法规范化{label}目录：{error}"))?;
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化解包根目录：{error}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!("{label}目录越出解包根目录"));
    }
    Ok(())
}

/// 校验归档文件本身，避免通过链接读取受控临时目录外的文件。
fn canonical_archive_file(target: &Path, archive: &Path, label: &str) -> Result<PathBuf, String> {
    let target_metadata = fs::symlink_metadata(target)
        .map_err(|error| format!("读取{label}解包根目录失败：{error}"))?;
    if target_metadata.file_type().is_symlink() || !target_metadata.is_dir() {
        return Err(format!("{label}解包根目录必须是普通目录"));
    }
    let canonical_target = fs::canonicalize(target)
        .map_err(|error| format!("无法规范化{label}解包根目录：{error}"))?;
    let metadata =
        fs::symlink_metadata(archive).map_err(|error| format!("读取{label}归档失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label}归档必须是普通文件"));
    }
    let canonical_archive =
        fs::canonicalize(archive).map_err(|error| format!("无法规范化{label}归档：{error}"))?;
    if !canonical_archive.starts_with(&canonical_target) {
        return Err(format!("{label}归档必须位于解包根目录内"));
    }
    Ok(canonical_archive)
}

/// 为归档条目建立一个安全的、不可覆盖既有文件的目标路径。
fn archive_file_destination(
    target: &Path,
    relative: &Path,
    archive: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let destination = target.join(relative);
    let parent = destination
        .parent()
        .ok_or_else(|| format!("{label}条目缺少父目录"))?;
    ensure_archive_directory(target, parent, label)?;
    let canonical_target =
        fs::canonicalize(target).map_err(|error| format!("无法规范化解包根目录：{error}"))?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|error| format!("无法规范化{label}父目录：{error}"))?;
    if !canonical_parent.starts_with(&canonical_target) {
        return Err(format!("{label}条目越出解包根目录"));
    }
    if destination == archive {
        return Err(format!("{label}条目不能覆盖归档文件"));
    }
    match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "{label}条目不能是符号链接：{}",
            destination.display()
        )),
        Ok(_) => Err(format!(
            "{label}条目重复或覆盖既有文件：{}",
            destination.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(destination),
        Err(error) => Err(format!("读取{label}条目失败：{error}")),
    }
}

/// 限制 tar 解码器读取的总流量，连同 PAX/GNU 元数据也不能无限膨胀。
struct LimitedArchiveReader<R> {
    inner: R,
    remaining: u64,
}

impl<R> LimitedArchiveReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<R: Read> Read for LimitedArchiveReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tar 解码流超过安全限制",
            ));
        }
        let length = buffer.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buffer[..length])?;
        self.remaining = self.remaining.saturating_sub(read as u64);
        Ok(read)
    }
}

/// 按限额读取 ZIP 归档；所有文件类型、路径和父目录均在写入前校验。
fn extract_zip_archive(
    target: &Path,
    archive: &Path,
    label: &str,
    max_entries: usize,
    max_bytes: u64,
) -> Result<(), String> {
    let archive = canonical_archive_file(target, archive, label)?;
    let file = File::open(&archive).map_err(|error| format!("打开{label}失败：{error}"))?;
    let mut zip = ZipArchive::new(file).map_err(|error| format!("读取{label}失败：{error}"))?;
    let mut seen = BTreeSet::new();
    let mut unpacked_bytes = 0_u64;
    for index in 0..zip.len() {
        let entry_number = index + 1;
        if entry_number > max_entries {
            return Err(format!("{label}条目数超过 {max_entries}"));
        }
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("读取{label}条目失败：{error}"))?;
        if entry.is_symlink() {
            return Err(format!("{label}不允许包含符号链接：{}", entry.name()));
        }
        let is_directory = entry.is_dir();
        let unix_type = entry.unix_mode().map(|mode| mode & 0o170000);
        let type_is_valid = match unix_type {
            None | Some(0) => true,
            Some(0o040000) => is_directory,
            Some(0o100000) => !is_directory,
            Some(_) => false,
        };
        if !type_is_valid || (!is_directory && !entry.is_file()) {
            return Err(format!("{label}包含不支持的特殊文件：{}", entry.name()));
        }
        let relative =
            validate_archive_entry_path(Path::new(entry.name()), &format!("{label}条目"))?;
        if !seen.insert(relative.clone()) {
            return Err(format!("{label}包含重复条目：{}", relative.display()));
        }
        if is_directory {
            if entry.size() != 0 {
                return Err(format!(
                    "{label}目录条目包含文件数据：{}",
                    relative.display()
                ));
            }
            ensure_archive_directory(target, &target.join(&relative), label)?;
            continue;
        }
        let size = entry.size();
        let next_total = unpacked_bytes
            .checked_add(size)
            .ok_or_else(|| format!("{label}解包字节数溢出"))?;
        if next_total > max_bytes {
            return Err(format!("{label}解包后超过 {max_bytes} 字节"));
        }
        let destination = archive_file_destination(target, &relative, &archive, label)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| format!("创建{label}条目失败 {}：{error}", destination.display()))?;
        let mut limited = entry.by_ref().take(size.saturating_add(1));
        let copied = io::copy(&mut limited, &mut output)
            .map_err(|error| format!("写入{label}条目失败 {}：{error}", destination.display()))?;
        output
            .flush()
            .map_err(|error| format!("刷新{label}条目失败 {}：{error}", destination.display()))?;
        if copied != size {
            return Err(format!(
                "{label}条目大小与声明不一致：{}",
                relative.display()
            ));
        }
        unpacked_bytes = next_total;
    }
    Ok(())
}

/// 按限额读取 tar 或 tar.gz 归档；拒绝链接、设备、FIFO 及其他特殊条目。
fn extract_tar_reader<R: Read>(
    target: &Path,
    reader: R,
    archive: &Path,
    label: &str,
    max_entries: usize,
    max_bytes: u64,
) -> Result<(), String> {
    let stream_limit = max_bytes
        .saturating_add((max_entries as u64).saturating_mul(1024))
        .saturating_add(1024);
    let mut tar = Archive::new(LimitedArchiveReader::new(reader, stream_limit));
    let mut entries = tar
        .entries()
        .map_err(|error| format!("读取{label}条目失败：{error}"))?;
    let mut seen = BTreeSet::new();
    let mut unpacked_bytes = 0_u64;
    let mut entry_count = 0_usize;
    for entry_result in &mut entries {
        let mut entry = entry_result.map_err(|error| format!("读取{label}条目失败：{error}"))?;
        entry_count = entry_count.saturating_add(1);
        if entry_count > max_entries {
            return Err(format!("{label}条目数超过 {max_entries}"));
        }
        let raw_path = entry
            .path()
            .map_err(|error| format!("读取{label}条目路径失败：{error}"))?
            .into_owned();
        let relative = validate_archive_entry_path(&raw_path, &format!("{label}条目"))?;
        if !seen.insert(relative.clone()) {
            return Err(format!("{label}包含重复条目：{}", relative.display()));
        }
        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(format!("{label}不允许包含链接：{}", relative.display()));
        }
        if entry_type.is_dir() {
            if entry
                .header()
                .size()
                .map_err(|error| format!("读取{label}目录大小失败：{error}"))?
                != 0
            {
                return Err(format!(
                    "{label}目录条目包含文件数据：{}",
                    relative.display()
                ));
            }
            ensure_archive_directory(target, &target.join(&relative), label)?;
            continue;
        }
        if !entry_type.is_file() && !entry_type.is_contiguous() {
            return Err(format!(
                "{label}包含不支持的特殊文件：{}",
                relative.display()
            ));
        }
        let size = entry
            .header()
            .size()
            .map_err(|error| format!("读取{label}条目大小失败：{error}"))?;
        let next_total = unpacked_bytes
            .checked_add(size)
            .ok_or_else(|| format!("{label}解包字节数溢出"))?;
        if next_total > max_bytes {
            return Err(format!("{label}解包后超过 {max_bytes} 字节"));
        }
        let destination = archive_file_destination(target, &relative, archive, label)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|error| format!("创建{label}条目失败 {}：{error}", destination.display()))?;
        let mut limited = entry.by_ref().take(size.saturating_add(1));
        let copied = io::copy(&mut limited, &mut output)
            .map_err(|error| format!("写入{label}条目失败 {}：{error}", destination.display()))?;
        output
            .flush()
            .map_err(|error| format!("刷新{label}条目失败 {}：{error}", destination.display()))?;
        if copied != size {
            return Err(format!(
                "{label}条目大小与声明不一致：{}",
                relative.display()
            ));
        }
        unpacked_bytes = next_total;
    }
    Ok(())
}

/// 解包 ZIP、tar 或 tar.gz；不调用外部解包命令，生产路径统一使用上述限额。
fn extract_archive(target: &Path, archive: &Path, source: &str, label: &str) -> Result<(), String> {
    match archive_format(source) {
        ArchiveFormat::Zip => extract_zip_archive(
            target,
            archive,
            label,
            MAX_PLUGIN_ARCHIVE_ENTRIES,
            MAX_PLUGIN_ARCHIVE_UNPACKED_BYTES,
        ),
        ArchiveFormat::Tar => {
            let archive_path = canonical_archive_file(target, archive, label)?;
            let file =
                File::open(&archive_path).map_err(|error| format!("打开{label}失败：{error}"))?;
            extract_tar_reader(
                target,
                file,
                &archive_path,
                label,
                MAX_PLUGIN_ARCHIVE_ENTRIES,
                MAX_PLUGIN_ARCHIVE_UNPACKED_BYTES,
            )
        }
        ArchiveFormat::TarGz => {
            let archive_path = canonical_archive_file(target, archive, label)?;
            let file =
                File::open(&archive_path).map_err(|error| format!("打开{label}失败：{error}"))?;
            extract_tar_reader(
                target,
                GzDecoder::new(file),
                &archive_path,
                label,
                MAX_PLUGIN_ARCHIVE_ENTRIES,
                MAX_PLUGIN_ARCHIVE_UNPACKED_BYTES,
            )
        }
    }
}

/// 克隆一个 Git 来源；当同时提供 `ref` 与 `sha` 时，按 Claude Code 规则以 `sha` 为准。
fn clone_git_source(
    url: &str,
    reference: Option<&str>,
    sha: Option<&str>,
    sparse: bool,
    target: &Path,
    label: &str,
) -> Result<(), String> {
    let mut command = process::Command::new("git");
    command.arg("clone").arg("--depth").arg("1");
    if sparse {
        command.arg("--filter=blob:none").arg("--sparse");
    }
    if sha.is_none()
        && let Some(reference) = reference
    {
        command.arg("--branch").arg(reference);
    }
    command.arg(url).arg(target);
    run_external(&mut command, label)
}

/// 对 Git 克隆启用有限目录检出，避免 monorepo 下载无关文件。
fn apply_git_sparse_paths(target: &Path, paths: &[String], label: &str) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let mut command = process::Command::new("git");
    command
        .current_dir(target)
        .arg("sparse-checkout")
        .arg("set")
        .arg("--no-cone");
    for path in paths {
        // 非 cone 模式下未锚定的隐藏目录路径可能被 Git 当作模糊模式，
        // 甚至漏掉 `.claude-plugin/marketplace.json`；所有已校验相对路径
        // 都转换成仓库根锚定模式。
        command.arg(sparse_checkout_pattern(path));
    }
    run_external(&mut command, label)
}

/// 将已校验的仓库相对路径转换成非 cone sparse-checkout 根锚定模式。
fn sparse_checkout_pattern(path: &str) -> String {
    let normalized = path
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<_>>()
        .join("/");
    format!("/{normalized}")
}

/// 执行外部取得工具并限制输出，错误中不回显潜在密钥参数。
///
/// `Command::output` 会一直等待子进程结束；Git 在无法访问远端或等待
/// 凭据时可能永不返回。这里统一关闭 stdin、禁用 Git 终端提示并轮询
/// 子进程，在固定时限后杀掉它，让 Tauri 命令能够确定性地结束。
fn run_external(command: &mut process::Command, label: &str) -> Result<(), String> {
    run_external_with_timeout(command, label, PLUGIN_COMMAND_TIMEOUT)
}

/// 可注入时限的外部命令执行实现；生产调用使用统一的插件命令时限，
/// 测试可用更短时限验证超时路径而不等待两分钟。
fn run_external_with_timeout(
    command: &mut process::Command,
    label: &str,
    timeout: Duration,
) -> Result<(), String> {
    let executable = Path::new(command.get_program())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if executable == "git" || executable == "git.exe" {
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("GCM_INTERACTIVE", "Never");
    }
    if executable == "npm" || executable == "npm.cmd" {
        command.env("NPM_CONFIG_YES", "true");
        command.env("NPM_CONFIG_IGNORE_SCRIPTS", "true");
    }
    let mut child = command
        .stdin(Stdio::null())
        // 标准输出只包含进度信息，不参与错误判断；直接丢弃可避免
        // Git/npm 大量输出填满管道后反向阻塞子进程。
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{label}执行失败：{error}"))?;
    let stderr_reader = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                match pipe.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let remaining = MAX_EXTERNAL_ERROR_BYTES.saturating_sub(output.len());
                        if remaining > 0 {
                            output.extend_from_slice(&buffer[..read.min(remaining)]);
                        }
                    }
                }
            }
            output
        })
    });
    let deadline = Instant::now() + timeout;
    let mut poll_interval = PLUGIN_COMMAND_POLL_INTERVAL_INITIAL;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                // 不等待超时进程的 stderr 读取线程；丢弃 JoinHandle 可避免
                // 其子进程仍持有管道时再次阻塞当前 Tauri 命令。
                drop(stderr_reader);
                return Err(format!(
                    "{label}执行超时（{:.1} 秒）",
                    timeout.as_secs_f64()
                ));
            }
            Ok(None) => {
                std::thread::sleep(poll_interval);
                poll_interval = (poll_interval * 2).min(PLUGIN_COMMAND_POLL_INTERVAL_MAX);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(stderr_reader);
                return Err(format!("{label}等待执行结果失败：{error}"));
            }
        }
    };

    let stderr = stderr_reader
        .map(|reader| reader.join().unwrap_or_default())
        .unwrap_or_default();
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        return Err(format!(
            "{label}返回失败状态：{}",
            detail.trim().chars().take(512).collect::<String>()
        ));
    }
    Ok(())
}

/// 在浅克隆后取得并检出 marketplace/plugin 指定的固定提交。
fn checkout_git_sha(root: &Path, sha: &str, label: &str) -> Result<(), String> {
    let mut fetch = process::Command::new("git");
    fetch
        .current_dir(root)
        .args(["fetch", "--depth", "1", "origin", sha]);
    run_external(&mut fetch, label)?;
    let mut checkout = process::Command::new("git");
    checkout
        .current_dir(root)
        .args(["checkout", "--detach", sha]);
    run_external(&mut checkout, label)
}

/// 在已有根目录下读取一个不经过任何符号链接的子路径，并确认仍位于根目录内。
fn canonical_child_without_symlinks(
    root: &Path,
    relative: &Path,
    label: &str,
) -> Result<PathBuf, String> {
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("读取{label}根目录失败：{error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(format!("{label}根目录必须是普通目录：{}", root.display()));
    }
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化{label}根目录：{error}"))?;
    let mut current = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Ok(canonical_root);
    }
    for (index, component) in components.iter().enumerate() {
        match component {
            std::path::Component::CurDir => continue,
            std::path::Component::Normal(name) => current.push(name),
            std::path::Component::ParentDir
            | std::path::Component::Prefix(_)
            | std::path::Component::RootDir => {
                return Err(format!("{label}路径越界：{}", relative.display()));
            }
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("读取{label}路径失败 {}：{error}", current.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "{label}路径不能包含符号链接：{}",
                current.display()
            ));
        }
        if index + 1 < components.len() && !metadata.is_dir() {
            return Err(format!("{label}路径父项不是目录：{}", current.display()));
        }
    }
    let canonical =
        fs::canonicalize(&current).map_err(|error| format!("无法规范化{label}路径：{error}"))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(format!("{label}路径越出根目录：{}", current.display()));
    }
    Ok(canonical)
}

/// 递归确认插件目录只包含普通目录、文件或指向根内文件的链接。
fn validate_directory_tree_without_symlinks(root: &Path, label: &str) -> Result<(), String> {
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化{label}根目录：{error}"))?;
    validate_directory_tree(&canonical_root, &canonical_root, label)
}

fn validate_directory_tree(root: &Path, current: &Path, label: &str) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(current).map_err(|error| format!("读取{label}根目录失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{label}根目录必须是普通目录：{}",
            current.display()
        ));
    }
    for entry in fs::read_dir(current).map_err(|error| format!("遍历{label}失败：{error}"))? {
        let entry = entry.map_err(|error| format!("读取{label}条目失败：{error}"))?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| format!("读取{label}条目失败：{error}"))?;
        if metadata.file_type().is_symlink() {
            resolve_internal_file_symlink(root, &path).map_err(|error| error.to_string())?;
        }
        if metadata.is_dir() {
            validate_directory_tree(root, &path, label)?;
        } else if !metadata.is_file() && !metadata.file_type().is_symlink() {
            return Err(format!("{label}不能包含特殊文件：{}", path.display()));
        }
    }
    Ok(())
}

/// 在远程归档的有限深度内定位唯一 `.claude-plugin/plugin.json` 根目录。
fn find_plugin_root(root: &Path) -> Result<PathBuf, String> {
    let canonical_root =
        fs::canonicalize(root).map_err(|error| format!("无法规范化插件归档根目录：{error}"))?;
    let root_metadata =
        fs::symlink_metadata(root).map_err(|error| format!("读取插件归档根目录失败：{error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("插件归档根目录必须是普通目录".to_owned());
    }
    let has_manifest = |candidate: &Path| -> Result<bool, String> {
        let relative = candidate
            .strip_prefix(root)
            .map_err(|_| "插件归档候选根目录越出解包根目录".to_owned())?;
        let canonical_candidate =
            canonical_child_without_symlinks(root, relative, "插件归档候选根目录")?;
        let manifest_path = canonical_candidate.join(".claude-plugin/plugin.json");
        let manifest_metadata = match fs::symlink_metadata(&manifest_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("读取插件清单失败：{error}")),
        };
        if manifest_metadata.file_type().is_symlink() {
            return Err(format!(
                "插件清单不能是符号链接：{}",
                manifest_path.display()
            ));
        }
        if !manifest_metadata.is_file() {
            return Ok(false);
        }
        let manifest = canonical_child_without_symlinks(
            &canonical_candidate,
            Path::new(".claude-plugin/plugin.json"),
            "插件清单",
        )?;
        let metadata = fs::symlink_metadata(&manifest)
            .map_err(|error| format!("读取插件清单失败：{error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Ok(false);
        }
        Ok(manifest.starts_with(&canonical_root))
    };

    if has_manifest(root)? {
        return Ok(canonical_root);
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| format!("遍历插件归档失败：{error}"))?
    {
        let entry = entry.map_err(|error| format!("读取插件归档条目失败：{error}"))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("读取插件归档条目失败：{error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("插件归档条目不能是符号链接：{}", path.display()));
        }
        if !metadata.is_dir() {
            continue;
        }
        if has_manifest(&path)? {
            matches.push(canonical_child_without_symlinks(
                root,
                path.strip_prefix(root)
                    .map_err(|_| "插件归档候选根目录越界".to_owned())?,
                "插件归档候选根目录",
            )?);
        }
    }
    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => Err("插件归档中缺少 .claude-plugin/plugin.json".to_owned()),
        _ => Err("插件归档包含多个插件根目录，无法安全选择".to_owned()),
    }
}

/// 生成仅用于临时目录的进程内唯一后缀。
fn unique_suffix() -> String {
    format!(
        "{}-{}",
        process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )
}

/// 在指定父目录下独占创建临时工作区；名称冲突时重试，而不是复用已有目录。
///
/// `unique_suffix` 只是降低冲突概率，不能承担并发安全职责。真正的所有权
/// 边界由 `create_dir` 提供：只有成功创建目录的调用方才拥有该工作区。
fn create_unique_temp_dir(parent: &Path, prefix: &str, label: &str) -> Result<PathBuf, String> {
    const MAX_ATTEMPTS: usize = 16;

    fs::create_dir_all(parent).map_err(|error| format!("{label}：{error}"))?;
    for attempt in 0..MAX_ATTEMPTS {
        let candidate = parent.join(format!("{prefix}-{}-{attempt}", unique_suffix()));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{label}：{error}")),
        }
    }
    Err(format!(
        "{label}：临时目录名称冲突，重试 {MAX_ATTEMPTS} 次后失败"
    ))
}

/// 临时市场取得目录的失败清理守卫；成功登记后显式释放，避免留下半成品。
struct TemporaryMarketplaceDirectory {
    path: Option<PathBuf>,
}

impl TemporaryMarketplaceDirectory {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn keep(&mut self) {
        self.path = None;
    }
}

impl Drop for TemporaryMarketplaceDirectory {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if let Err(error) = fs::remove_dir_all(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %path.display(), %error, "清理插件市场临时目录失败");
        }
    }
}

/// 清理一次插件安装/更新计划在 downloads 下创建的独占临时工作区。
///
/// 每个操作拥有独立的 plan-* 目录，因此并发安装/更新不会误删其他操作刚创建
/// 的 fetch-/synthetic- 来源，也不会触碰用户市场目录。
struct TemporaryPluginDownloads {
    path: PathBuf,
}

impl TemporaryPluginDownloads {
    fn new(downloads_root: &Path) -> Result<Self, String> {
        let path = create_unique_temp_dir(downloads_root, "plan", "创建插件临时工作区失败")?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryPluginDownloads {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "清理插件下载临时工作区失败"
            );
        }
    }
}

/// 已解析的 Claude marketplace 及其取得目录所有权。
///
/// 本地 file/directory 来源没有清理令牌；HTTP/Git/npm 来源的目录只有在
/// 调用方完成登记后才应调用 `keep`，否则离开作用域时自动删除。
struct MaterializedMarketplace {
    root: PathBuf,
    manifest_path: PathBuf,
    catalog: crate::claude_plugins::MarketplaceManifest,
    cleanup: Option<TemporaryMarketplaceDirectory>,
}

/// Claude marketplace 来源的完整本地表示；兼容 settings 中的 path/sparsePaths/headers。
#[derive(Clone, Debug, PartialEq, Eq)]
enum MarketplaceSourceSpec {
    /// 直接下载 marketplace.json 文件。
    Url {
        url: String,
        headers: BTreeMap<String, String>,
    },
    /// Git 仓库及可选仓库内目录。
    Git {
        url: String,
        reference: Option<String>,
        path: Option<String>,
        sparse_paths: Vec<String>,
    },
    /// npm 市场包。
    Npm {
        package: String,
        version: Option<String>,
    },
    /// 本地 marketplace.json 文件。
    File { path: String },
    /// 本地市场目录。
    Directory { path: String },
}

/// 将扩展命令传入的来源解析为支持目录 path、稀疏路径和 URL headers 的结构。
fn parse_marketplace_source_spec(source: &str) -> Result<Option<MarketplaceSourceSpec>, String> {
    let source = source.trim();
    if expand_tilde(source)?.exists() {
        return Ok(None);
    }
    if source.starts_with('{') {
        let value = serde_json::from_str::<Value>(source)
            .map_err(|error| format!("市场来源 JSON 格式无效：{error}"))?;
        return parse_marketplace_source_value(value).map(Some);
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(Some(MarketplaceSourceSpec::Url {
            url: source.to_owned(),
            headers: BTreeMap::new(),
        }));
    }
    if let Some(repo) = source.strip_prefix("github:") {
        let (repo, reference) = split_github_ref(repo);
        return Ok(Some(MarketplaceSourceSpec::Git {
            url: format!("https://github.com/{repo}.git"),
            reference,
            path: None,
            sparse_paths: Vec::new(),
        }));
    }
    if let Some(value) = source.strip_prefix("git:") {
        let (url, reference) = split_git_ref(value);
        return Ok(Some(MarketplaceSourceSpec::Git {
            url,
            reference,
            path: None,
            sparse_paths: Vec::new(),
        }));
    }
    if let Some(package) = source.strip_prefix("npm:") {
        return Ok(Some(MarketplaceSourceSpec::Npm {
            package: package.to_owned(),
            version: None,
        }));
    }
    // Claude Code 接受 owner/repo 形式作为 GitHub 市场简写。
    if source.matches('/').count() == 1
        && !source.starts_with("./")
        && !source.starts_with("../")
        && !source.starts_with('/')
        && !source.starts_with('~')
    {
        let (repo, reference) = split_github_ref(source);
        if repo.split('/').count() == 2 && !repo.contains(char::is_whitespace) {
            return Ok(Some(MarketplaceSourceSpec::Git {
                url: format!("https://github.com/{repo}.git"),
                reference,
                path: None,
                sparse_paths: Vec::new(),
            }));
        }
    }
    Ok(None)
}

/// 解析 JSON 对象形式的 marketplace source。
fn parse_marketplace_source_value(value: Value) -> Result<MarketplaceSourceSpec, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "市场来源必须是 JSON 对象".to_owned())?;
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| "市场来源缺少 source 字段".to_owned())?;
    let required_text = |key: &str| -> Result<String, String> {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("市场来源缺少 {key}"))
    };
    let optional_text = |key: &str| -> Result<Option<String>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
            Some(_) => Err(format!("市场来源 {key} 必须是非空字符串")),
        }
    };
    let sparse_paths = match object.get("sparsePaths") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(paths)) => paths
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|path| !path.trim().is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| "市场来源 sparsePaths 必须是非空字符串数组".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err("市场来源 sparsePaths 必须是字符串数组".to_owned()),
    };
    match source {
        "github" => {
            let repo = required_text("repo")?;
            let (repo, shorthand_ref) = split_github_ref(&repo);
            let reference = optional_text("ref")?.or(shorthand_ref);
            Ok(MarketplaceSourceSpec::Git {
                url: format!("https://github.com/{repo}.git"),
                reference,
                path: optional_text("path")?,
                sparse_paths,
            })
        }
        "git" => {
            let value = required_text("url")?;
            let (url, shorthand_ref) = split_git_ref(&value);
            let reference = optional_text("ref")?.or(shorthand_ref);
            Ok(MarketplaceSourceSpec::Git {
                url,
                reference,
                path: optional_text("path")?,
                sparse_paths,
            })
        }
        "url" => Ok(MarketplaceSourceSpec::Url {
            url: validate_http_source_url(&required_text("url")?, "市场 URL")?,
            headers: parse_http_headers(object.get("headers"))?,
        }),
        "npm" => Ok(MarketplaceSourceSpec::Npm {
            package: required_text("package")?,
            version: optional_text("version")?,
        }),
        "file" => Ok(MarketplaceSourceSpec::File {
            path: required_text("path")?,
        }),
        "directory" => Ok(MarketplaceSourceSpec::Directory {
            path: required_text("path")?,
        }),
        other => Err(format!("不支持的市场 source：{other}")),
    }
}

/// URL 型 marketplace 来源只允许 HTTP(S)，避免把未知协议交给网络层。
fn validate_http_source_url(url: &str, label: &str) -> Result<String, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(format!("{label} 只允许 http 或 https"));
    }
    Ok(url.to_owned())
}

/// 从 GitHub 简写中拆出可选 `@ref`。
fn split_github_ref(value: &str) -> (String, Option<String>) {
    match value.rsplit_once('@') {
        Some((repo, reference)) if !repo.is_empty() && !reference.is_empty() => {
            (repo.to_owned(), Some(reference.to_owned()))
        }
        _ => (value.to_owned(), None),
    }
}

/// 从 Git URL 中拆出可选 `#ref`。
fn split_git_ref(value: &str) -> (String, Option<String>) {
    match value.rsplit_once('#') {
        Some((url, reference)) if !url.is_empty() && !reference.is_empty() => {
            (url.to_owned(), Some(reference.to_owned()))
        }
        _ => (value.to_owned(), None),
    }
}

/// 按来源 spec 物化市场清单。
fn materialize_claude_marketplace_spec(
    spec: MarketplaceSourceSpec,
    workspace: &Path,
) -> Result<MaterializedMarketplace, String> {
    match spec {
        MarketplaceSourceSpec::Url { url, headers } => {
            let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
            let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
            let manifest_dir = target.join(".claude-plugin");
            fs::create_dir_all(&manifest_dir)
                .map_err(|error| format!("创建市场临时目录失败：{error}"))?;
            let bytes =
                http_get_with_headers(&url, &headers, "市场清单", MAX_MARKETPLACE_MANIFEST_BYTES)?;
            let manifest_path = manifest_dir.join("marketplace.json");
            fs::write(&manifest_path, &bytes)
                .map_err(|error| format!("保存市场清单失败：{error}"))?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            Ok(MaterializedMarketplace {
                root: target,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            })
        }
        MarketplaceSourceSpec::Git {
            url,
            reference,
            path,
            mut sparse_paths,
        } => {
            // 未配置 sparsePaths 时必须保留 marketplace.json 引用的相对插件目录；
            // 使用完整浅克隆比先只检出清单、再猜测插件路径更可靠。
            let use_sparse_checkout = !sparse_paths.is_empty();
            let manifest_relative = path.as_deref().unwrap_or(".claude-plugin/marketplace.json");
            let manifest_relative = validate_source_relative_path(manifest_relative, "市场 path")?;
            if !manifest_relative
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            {
                return Err("市场 path 必须指向 JSON 清单".to_owned());
            }
            // 非 cone sparse-checkout 不能可靠地仅凭父目录名保留隐藏目录中的清单；
            // 直接加入 marketplace.json 文件路径，避免默认官方市场被检出为空。
            let manifest_path = manifest_relative.to_string_lossy().into_owned();
            if !sparse_paths.iter().any(|item| item == &manifest_path) {
                sparse_paths.insert(0, manifest_path);
            }
            for path in &sparse_paths {
                validate_source_relative_path(path, "市场 sparsePaths")?;
            }
            let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
            let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
            clone_git_source(
                &url,
                reference.as_deref(),
                None,
                use_sparse_checkout,
                &target,
                "Git 市场来源",
            )?;
            if use_sparse_checkout {
                apply_git_sparse_paths(&target, &sparse_paths, "Git 市场来源")?;
            }
            let manifest_path = target.join(&manifest_relative);
            let bytes = fs::read(&manifest_path).map_err(|error| {
                format!("无法读取 Git 市场清单 {}：{error}", manifest_path.display())
            })?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            let market_root = manifest_path
                .parent()
                .and_then(|parent| {
                    (parent.file_name().and_then(|name| name.to_str()) == Some(".claude-plugin"))
                        .then(|| parent.parent().unwrap_or(parent))
                })
                .unwrap_or_else(|| manifest_path.parent().unwrap_or(&target))
                .to_path_buf();
            Ok(MaterializedMarketplace {
                root: market_root,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            })
        }
        MarketplaceSourceSpec::Npm { package, version } => {
            let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
            let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
            let package_spec = version
                .map(|version| format!("{package}@{version}"))
                .unwrap_or(package);
            let mut pack = process::Command::new("npm");
            pack.current_dir(&target)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(package_spec);
            run_external(&mut pack, "npm 市场来源")?;
            let archive = fs::read_dir(&target)
                .map_err(|error| error.to_string())?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成市场归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 市场来源",
            )?;
            let package_root = target.join("package");
            let (manifest_path, root) = locate_claude_marketplace(&package_root)?;
            let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
                .map_err(|error| error.to_string())?;
            Ok(MaterializedMarketplace {
                root,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            })
        }
        MarketplaceSourceSpec::File { path } => {
            let path = expand_tilde(&path)?;
            let bytes = fs::read(&path).map_err(|error| format!("读取市场清单失败：{error}"))?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            let canonical =
                fs::canonicalize(&path).map_err(|error| format!("无法访问市场清单：{error}"))?;
            let root = if canonical
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(".claude-plugin")
            {
                canonical
                    .parent()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
                    .ok_or_else(|| "市场清单缺少市场根目录".to_owned())?
            } else {
                canonical
                    .parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| "市场清单缺少父目录".to_owned())?
            };
            Ok(MaterializedMarketplace {
                root,
                manifest_path: canonical,
                catalog: manifest,
                cleanup: None,
            })
        }
        MarketplaceSourceSpec::Directory { path } => {
            let path = expand_tilde(&path)?;
            let (manifest_path, root) = locate_claude_marketplace(&path)?;
            let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
                .map_err(|error| error.to_string())?;
            Ok(MaterializedMarketplace {
                root,
                manifest_path,
                catalog: manifest,
                cleanup: None,
            })
        }
    }
}

/// 在本地或远程来源取得 Claude marketplace.json，并返回其根目录与清单。
fn materialize_claude_marketplace(
    source: &str,
    workspace: &Path,
) -> Result<MaterializedMarketplace, String> {
    let source = source.trim();
    if let Some(spec) = parse_marketplace_source_spec(source)? {
        return materialize_claude_marketplace_spec(spec, workspace);
    }
    let expanded = expand_tilde(source)?;
    if expanded.exists() {
        let (manifest_path, root) = locate_claude_marketplace(&expanded)?;
        let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
            .map_err(|error| error.to_string())?;
        return Ok(MaterializedMarketplace {
            root,
            manifest_path,
            catalog: manifest,
            cleanup: None,
        });
    }
    let target = create_unique_temp_dir(workspace, "market", "创建市场临时目录失败")?;
    let cleanup = TemporaryMarketplaceDirectory::new(target.clone());
    let parsed = if source.starts_with("http://") || source.starts_with("https://") {
        crate::claude_plugins::MarketplaceSource::Url {
            url: source.to_owned(),
            headers: BTreeMap::new(),
        }
    } else if let Some(repo) = source.strip_prefix("github:") {
        crate::claude_plugins::MarketplaceSource::Github {
            repo: repo.to_owned(),
            reference: None,
            path: None,
            sparse_paths: Vec::new(),
        }
    } else if let Some(url) = source.strip_prefix("git:") {
        crate::claude_plugins::MarketplaceSource::Git {
            url: url.to_owned(),
            reference: None,
            path: None,
            sparse_paths: Vec::new(),
        }
    } else if let Some(package) = source.strip_prefix("npm:") {
        crate::claude_plugins::MarketplaceSource::Npm {
            package: package.to_owned(),
            version: None,
            registry: None,
        }
    } else {
        serde_json::from_value::<crate::claude_plugins::MarketplaceSource>(Value::String(
            source.to_owned(),
        ))
        .map_err(|error| error.to_string())?
    };
    let plan = parsed
        .fetch_plan(&EmptyMarketplaceSettings)
        .map_err(|error| error.to_string())?;
    match plan {
        crate::claude_plugins::SourceFetchPlan::Http { url } => {
            let bytes = http_get_with_headers(
                &url,
                &BTreeMap::new(),
                "市场清单",
                MAX_MARKETPLACE_MANIFEST_BYTES,
            )?;
            let manifest_dir = target.join(".claude-plugin");
            fs::create_dir_all(&manifest_dir)
                .map_err(|error| format!("创建市场清单目录失败：{error}"))?;
            let manifest_path = manifest_dir.join("marketplace.json");
            fs::write(&manifest_path, &bytes)
                .map_err(|error| format!("保存市场清单失败：{error}"))?;
            let bytes = fs::read(&manifest_path).map_err(|error| error.to_string())?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            return Ok(MaterializedMarketplace {
                root: target,
                manifest_path,
                catalog: manifest,
                cleanup: Some(cleanup),
            });
        }
        crate::claude_plugins::SourceFetchPlan::Git { url, reference, .. } => {
            let mut command = process::Command::new("git");
            command.arg("clone").arg("--depth").arg("1");
            if let Some(reference) = reference {
                command.arg("--branch").arg(reference);
            }
            command.arg(url).arg(&target);
            run_external(&mut command, "Git 市场来源")?;
        }
        crate::claude_plugins::SourceFetchPlan::Npm {
            package_spec,
            registry,
        } => {
            let mut pack = process::Command::new("npm");
            pack.current_dir(&target)
                .arg("pack")
                .arg("--ignore-scripts")
                .arg(package_spec);
            if let Some(registry) = registry {
                pack.arg("--registry").arg(registry);
            }
            run_external(&mut pack, "npm 市场来源")?;
            let archive = fs::read_dir(&target)
                .map_err(|error| error.to_string())?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .find(|path| path.extension().and_then(|value| value.to_str()) == Some("tgz"))
                .ok_or_else(|| "npm pack 未生成市场归档".to_owned())?;
            extract_archive(
                &target,
                &archive,
                archive.to_string_lossy().as_ref(),
                "npm 市场来源",
            )?;
        }
        crate::claude_plugins::SourceFetchPlan::Pip { package_spec, .. } => {
            return Err(format!(
                "pip 不是 Claude marketplace 的市场来源：{package_spec}"
            ));
        }
        crate::claude_plugins::SourceFetchPlan::Directory { path } => {
            let manifest = crate::claude_plugins::load_marketplace_manifest(&path)
                .map_err(|error| error.to_string())?;
            return Ok(MaterializedMarketplace {
                root: path.clone(),
                manifest_path: path.join(crate::claude_plugins::CLAUDE_MARKETPLACE_MANIFEST),
                catalog: manifest,
                cleanup: None,
            });
        }
        crate::claude_plugins::SourceFetchPlan::File { path } => {
            let bytes = fs::read(&path).map_err(|error| error.to_string())?;
            let manifest = crate::claude_plugins::parse_marketplace_manifest(&bytes)
                .map_err(|error| error.to_string())?;
            return Ok(MaterializedMarketplace {
                root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
                manifest_path: path,
                catalog: manifest,
                cleanup: None,
            });
        }
    }
    let (manifest_path, root) = locate_claude_marketplace(&target)?;
    let manifest = crate::claude_plugins::load_marketplace_manifest(&root)
        .map_err(|error| error.to_string())?;
    Ok(MaterializedMarketplace {
        root,
        manifest_path,
        catalog: manifest,
        cleanup: Some(cleanup),
    })
}

/// Claude marketplace settings 来源暂由调用方显式管理，避免读取未知配置键。
struct EmptyMarketplaceSettings;

impl crate::claude_plugins::MarketplaceSettings for EmptyMarketplaceSettings {
    /// 当前扩展命令不自动解析 settings 引用。
    fn marketplace_source(&self, _key: &str) -> Option<crate::claude_plugins::MarketplaceSource> {
        None
    }
}

/// 定位 `.claude-plugin/marketplace.json` 所在的市场根目录。
fn locate_claude_marketplace(input: &Path) -> Result<(PathBuf, PathBuf), String> {
    let input_metadata =
        fs::symlink_metadata(input).map_err(|error| format!("无法读取市场来源：{error}"))?;
    if input_metadata.file_type().is_symlink() {
        return Err(format!("市场来源不能是符号链接：{}", input.display()));
    }
    let input = if input_metadata.is_file() {
        input.to_path_buf()
    } else if input_metadata.is_dir() {
        input.join(crate::claude_plugins::CLAUDE_MARKETPLACE_MANIFEST)
    } else {
        return Err(format!("市场来源不是目录或清单文件：{}", input.display()));
    };
    if input.file_name().and_then(|name| name.to_str()) != Some("marketplace.json") {
        return Err(format!(
            "市场清单必须位于 {}",
            crate::claude_plugins::CLAUDE_MARKETPLACE_MANIFEST
        ));
    }
    let root = input
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "市场清单缺少市场根目录".to_owned())?
        .to_path_buf();
    let relative = input
        .strip_prefix(&root)
        .map_err(|_| "市场清单不在市场根目录内".to_owned())?;
    let canonical = canonical_child_without_symlinks(&root, relative, "市场清单")?;
    let canonical_root =
        fs::canonicalize(&root).map_err(|error| format!("无法规范化市场根目录：{error}"))?;
    let metadata =
        fs::symlink_metadata(&canonical).map_err(|error| format!("无法读取市场清单：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("市场清单必须是普通文件".to_owned());
    }
    if !canonical.starts_with(&canonical_root)
        || canonical.file_name().and_then(|name| name.to_str()) != Some("marketplace.json")
    {
        return Err("市场清单必须位于市场根目录内".to_owned());
    }
    Ok((canonical, canonical_root))
}

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
mod tests {
    use super::*;

    /// 有 managed state 的正常生产路径也必须把插件 Hooks 放入会话快照。
    #[test]
    fn attaches_plugin_hooks_to_runtime_snapshot() {
        let snapshot = PluginRuntimeSnapshot {
            plugins: vec![crate::claude_plugins::RuntimePlugin {
                id: PluginId {
                    plugin: "demo".to_owned(),
                    marketplace: Some("local".to_owned()),
                },
                root: PathBuf::from("/plugins/demo"),
                commands: Vec::new(),
                skills: Vec::new(),
                agents: Vec::new(),
                hooks: Some(serde_json::json!({
                    "PreToolUse": [{
                        "matcher": "Bash",
                        "hooks": [{"type": "command", "command": "echo checked"}]
                    }]
                })),
                unsupported_hooks: Vec::new(),
                mcp_servers: BTreeMap::new(),
                lsp_servers: Vec::new(),
            }],
            plugin_hooks: Vec::new(),
        };

        let snapshot = attach_claude_hooks(snapshot);

        assert_eq!(snapshot.plugin_hooks.len(), 1);
        assert_eq!(snapshot.plugin_hooks[0].plugin_id, "demo@local");
        assert_eq!(snapshot.plugin_hooks[0].matcher.as_deref(), Some("Bash"));
    }

    /// Claude 命令命名空间必须保留嵌套目录，但不能把 `commands` 根目录当作名称。
    #[test]
    fn plugin_command_namespace_uses_command_relative_path() {
        assert_eq!(
            plugin_command_namespace("plugin:demo", Path::new("commands/foo.md")),
            "plugin:demo:foo"
        );
        assert_eq!(
            plugin_command_namespace("plugin:demo", Path::new("commands/admin/check.md")),
            "plugin:demo:admin:check"
        );
    }

    /// 自定义嵌套 marketplace.json 必须按记录保存的 manifestPath 重载，而不是回退到根目录默认路径。
    #[test]
    fn loads_nested_claude_marketplace_manifest_from_record() {
        let root = test_directory("nested-claude-marketplace");
        let manifest_path = root.join("catalog/.claude-plugin/marketplace.json");
        fs::create_dir_all(manifest_path.parent().expect("清单应有父目录"))
            .expect("应创建嵌套清单目录");
        fs::write(
            &manifest_path,
            br#"{"name":"nested","plugins":[{"name":"demo","source":"./plugin"}]}"#,
        )
        .expect("应写入嵌套 marketplace.json");
        let record = MarketplaceRecord {
            name: "nested".to_owned(),
            path: root.join("catalog").display().to_string(),
            manifest_path: manifest_path.display().to_string(),
        };
        let manifest = load_claude_marketplace_manifest_from_record(&record)
            .expect("应按 manifestPath 读取嵌套清单");
        assert_eq!(manifest.name, "nested");
        fs::remove_dir_all(root).expect("应清理嵌套市场测试目录");
    }

    /// 首次启动应发现 Claude Code 已下载的官方市场，而不是返回空市场列表。
    #[test]
    fn discovers_claude_known_marketplaces_from_install_location() {
        let root = test_directory("known-marketplaces");
        let marketplace_root = root.join("marketplaces/official");
        let manifest_path = marketplace_root.join(".claude-plugin/marketplace.json");
        fs::create_dir_all(manifest_path.parent().expect("清单应有父目录"))
            .expect("应创建 Claude 市场目录");
        fs::write(
            &manifest_path,
            br#"{"name":"claude-plugins-official","plugins":[]}"#,
        )
        .expect("应写入 Claude 市场清单");
        let known_path = root.join("known_marketplaces.json");
        fs::write(
            &known_path,
            serde_json::to_vec(&serde_json::json!({
                "claude-plugins-official": {
                    "source": {"source": "github", "repo": "anthropics/claude-plugins-official"},
                    "installLocation": marketplace_root,
                    "lastUpdated": "2026-08-03T00:00:00Z"
                }
            }))
            .expect("应序列化 Claude 已知市场"),
        )
        .expect("应写入 Claude 已知市场登记");

        let discovered = discover_claude_known_marketplaces_from_path(&known_path);

        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "claude-plugins-official");
        assert_eq!(
            discovered[0].manifest_path,
            manifest_path.display().to_string()
        );
        fs::remove_dir_all(root).expect("应清理 Claude 已知市场测试目录");
    }

    /// 新用户默认来源必须指向 Anthropic 管理的 Claude Code 官方插件仓库。
    #[test]
    fn default_claude_marketplace_source_points_to_official_repository() {
        assert_eq!(
            DEFAULT_CLAUDE_MARKETPLACE_SOURCE,
            "github:anthropics/claude-plugins-official"
        );
        assert_eq!(DEFAULT_CLAUDE_MARKETPLACE_NAME, "claude-plugins-official");
        assert!(
            crate::claude_plugins::validate_marketplace_name_source(
                DEFAULT_CLAUDE_MARKETPLACE_NAME,
                DEFAULT_CLAUDE_MARKETPLACE_SOURCE,
            )
            .is_ok()
        );
    }

    /// 默认市场后台取得必须去重，并在失败后按退避时间允许下一次自动重试。
    #[test]
    fn marketplace_bootstrap_deduplicates_and_backs_off_failures() {
        let now = Instant::now();
        let mut state = MarketplaceBootstrapState::default();
        assert!(state.should_start(false, now));
        let generation = state.begin();
        assert!(state.is_current(generation));
        assert!(!state.should_start(false, now));
        state.fail("network unavailable".to_owned(), now);
        assert!(!state.should_start(false, now + Duration::from_secs(1)));
        assert!(state.should_start(false, now + MARKETPLACE_RETRY_BACKOFF));
        assert!(state.should_start(true, now + Duration::from_secs(1)));

        state.succeed();
        assert!(!state.should_start(false, now));
        assert!(state.should_start(true, now));

        let generation = state.begin();
        state.invalidate();
        assert!(!state.is_current(generation));
    }

    /// 顶部“刷新目录”必须能在默认源尚未登记时绕过失败退避；普通自定义源刷新不能抢回默认源。
    #[test]
    fn explicit_catalog_refresh_can_restore_the_missing_default_marketplace() {
        assert!(should_refresh_default_marketplace(None, false, true));
        assert!(!should_refresh_default_marketplace(None, false, false));
        assert!(should_refresh_default_marketplace(None, true, false));
        assert!(should_refresh_default_marketplace(
            Some("CLAUDE-PLUGINS-OFFICIAL"),
            false,
            false,
        ));
        assert!(!should_refresh_default_marketplace(
            Some("custom-market"),
            false,
            false,
        ));
    }

    /// 默认官方市场只有在清单含插件时才算已取得，避免合法但空的缓存永久阻止重试。
    #[test]
    fn empty_default_marketplace_manifest_is_not_materialized() {
        let root = test_directory("empty-default-marketplace");
        let manifest_path = root.join(".claude-plugin/marketplace.json");
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
        fs::write(
            &manifest_path,
            br#"{"name":"claude-plugins-official","plugins":[]}"#,
        )
        .unwrap();
        let record = MarketplaceRecord {
            name: DEFAULT_CLAUDE_MARKETPLACE_NAME.to_owned(),
            path: root.display().to_string(),
            manifest_path: manifest_path.display().to_string(),
        };
        assert!(!marketplace_record_is_materialized(&record));

        fs::write(
            &manifest_path,
            br#"{"name":"claude-plugins-official","plugins":[{"name":"demo","source":"./demo"}]}"#,
        )
        .unwrap();
        assert!(marketplace_record_is_materialized(&record));
        fs::remove_dir_all(root).unwrap();
    }

    /// 官方市场的隐藏清单必须转换成仓库根锚定 sparse pattern，避免被 Git 模糊匹配漏掉。
    #[test]
    fn sparse_checkout_patterns_anchor_marketplace_manifest() {
        assert_eq!(
            sparse_checkout_pattern(".claude-plugin/marketplace.json"),
            "/.claude-plugin/marketplace.json"
        );
        assert_eq!(sparse_checkout_pattern("./plugins"), "/plugins");
    }

    /// 自定义 Git 市场未声明 sparsePaths 时必须保留清单引用的相对插件目录。
    #[test]
    fn git_marketplace_without_sparse_paths_checks_out_relative_plugins() {
        let directory = tempfile::tempdir().expect("创建 Git 市场测试目录");
        let repository = directory.path().join("repository");
        let plugin = repository.join("plugins/demo/.claude-plugin");
        fs::create_dir_all(&plugin).expect("创建测试插件目录");
        fs::create_dir_all(repository.join(".claude-plugin")).expect("创建测试市场目录");
        fs::write(
            repository.join(".claude-plugin/marketplace.json"),
            br#"{"name":"custom","plugins":[{"name":"demo","source":"./plugins/demo"}]}"#,
        )
        .expect("写入测试市场清单");
        fs::write(plugin.join("plugin.json"), br#"{"name":"demo"}"#).expect("写入测试插件清单");

        let mut init = process::Command::new("git");
        init.current_dir(&repository).args(["init", "--quiet"]);
        run_external(&mut init, "初始化 Git 市场测试仓库").expect("初始化测试仓库");
        let mut add = process::Command::new("git");
        add.current_dir(&repository).args(["add", "."]);
        run_external(&mut add, "暂存 Git 市场测试仓库").expect("暂存测试仓库");
        let mut commit = process::Command::new("git");
        commit.current_dir(&repository).args([
            "-c",
            "user.name=KeenCode Test",
            "-c",
            "user.email=keencode-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ]);
        run_external(&mut commit, "提交 Git 市场测试仓库").expect("提交测试仓库");

        let workspace = directory.path().join("workspace");
        let materialized = materialize_claude_marketplace_spec(
            MarketplaceSourceSpec::Git {
                url: repository.display().to_string(),
                reference: None,
                path: None,
                sparse_paths: Vec::new(),
            },
            &workspace,
        )
        .expect("取得自定义 Git 市场");
        assert!(
            materialized
                .root
                .join("plugins/demo/.claude-plugin/plugin.json")
                .is_file(),
            "未配置 sparsePaths 时应检出相对插件目录"
        );
        assert_eq!(materialized.catalog.name, "custom");
    }

    /// Git 插件子目录不能是指向克隆根外的符号链接；两条 Git 物化路径都必须拒绝。
    #[cfg(unix)]
    #[test]
    fn rejects_git_plugin_subdir_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("创建 Git 插件测试目录");
        let repository = directory.path().join("repository");
        let outside = directory.path().join("outside");
        fs::create_dir_all(&outside).expect("创建克隆根外目录");
        fs::create_dir_all(repository.join("plugins")).expect("创建 Git 插件目录");
        symlink(&outside, repository.join("plugins/demo")).expect("创建 Git 插件符号链接");

        let mut init = process::Command::new("git");
        init.current_dir(&repository).args(["init", "--quiet"]);
        run_external(&mut init, "初始化 Git 插件测试仓库").expect("初始化测试仓库");
        let mut add = process::Command::new("git");
        add.current_dir(&repository).args(["add", "."]);
        run_external(&mut add, "暂存 Git 插件测试仓库").expect("暂存测试仓库");
        let mut commit = process::Command::new("git");
        commit.current_dir(&repository).args([
            "-c",
            "user.name=KeenCode Test",
            "-c",
            "user.email=keencode-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ]);
        run_external(&mut commit, "提交 Git 插件测试仓库").expect("提交测试仓库");

        let git_url = repository.display().to_string();
        let marketplace_error = materialize_marketplace_plugin_source(
            MarketplacePluginSourceSpec::Git {
                url: git_url.clone(),
                path: Some("plugins/demo".to_owned()),
                reference: None,
                sha: None,
                sparse_paths: Vec::new(),
            },
            directory.path(),
            &directory.path().join("marketplace-workspace"),
        )
        .expect_err("marketplace Git 插件不能跟随越界符号链接");
        assert!(marketplace_error.contains("符号链接"));

        let claude_error = materialize_claude_plugin_source(
            &PluginSource::GitSubdir {
                url: git_url,
                path: "plugins/demo".to_owned(),
                reference: None,
                sha: None,
            },
            &directory.path().join("claude-workspace"),
        )
        .expect_err("Claude Git 插件不能跟随越界符号链接");
        assert!(claude_error.contains("符号链接"));
    }

    fn run_http_fixture(response: &'static str, max_bytes: usize) -> Result<Vec<u8>, String> {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("绑定 HTTP 测试端口");
        let address = listener.local_addr().expect("读取 HTTP 测试端口");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("接受 HTTP 测试请求");
            stream
                .write_all(response.as_bytes())
                .expect("写入 HTTP 测试响应");
        });
        let result = http_get_with_headers(
            &format!("http://{address}/fixture"),
            &BTreeMap::new(),
            "测试下载",
            max_bytes,
        );
        server.join().expect("HTTP 测试服务线程不应 panic");
        result
    }

    /// HTTP Content-Length 超过限制时必须在读取响应体前失败。
    #[test]
    fn http_download_rejects_content_length_over_limit() {
        let error = run_http_fixture(
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde",
            4,
        )
        .expect_err("Content-Length 超限必须失败");
        assert!(error.contains("超过 4 字节"));
    }

    /// Chunked HTTP 响应即使没有 Content-Length，也不能绕过下载大小限制。
    #[test]
    fn http_download_rejects_chunked_body_over_limit() {
        let error = run_http_fixture(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
            4,
        )
        .expect_err("chunked 响应超限必须失败");
        assert!(error.contains("超过 4 字节"));
    }

    /// 市场取得失败时临时目录应自动清理，成功登记则由调用方保留目录。
    #[test]
    fn temporary_marketplace_directory_cleans_only_on_failure() {
        let failed = test_directory("market-cleanup-failed");
        {
            let _cleanup = TemporaryMarketplaceDirectory::new(failed.clone());
        }
        assert!(!failed.exists());

        let kept = test_directory("market-cleanup-kept");
        {
            let mut cleanup = TemporaryMarketplaceDirectory::new(kept.clone());
            cleanup.keep();
        }
        assert!(kept.is_dir());
        fs::remove_dir_all(kept).expect("应清理成功取得目录");
    }

    /// 并发插件操作各自只清理独占 plan 目录，不误删其他操作或用户市场目录。
    #[test]
    fn plugin_download_cleanup_is_operation_owned() {
        let root = test_directory("plugin-download-cleanup");
        let downloads = root.join("downloads");
        fs::create_dir_all(downloads.join("fetch-existing")).expect("应创建既有下载目录");
        let market = root.join("user-market");
        fs::create_dir_all(&market).expect("应创建用户市场目录");
        let first = TemporaryPluginDownloads::new(&downloads).expect("应创建第一个独占工作区");
        let second = TemporaryPluginDownloads::new(&downloads).expect("应创建第二个独占工作区");
        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();
        fs::create_dir_all(first_path.join("fetch-owned")).expect("应创建第一个 fetch 目录");
        fs::create_dir_all(second_path.join("synthetic-owned"))
            .expect("应创建第二个 synthetic 目录");
        drop(first);
        assert!(!first_path.exists());
        assert!(second_path.exists());
        drop(second);
        assert!(!second_path.exists());
        assert!(downloads.join("fetch-existing").exists());
        assert!(market.exists());
        fs::remove_dir_all(root).expect("应清理插件下载守卫测试目录");
    }

    /// 更新提交前必须拒绝已卸载或已被其他操作改变的安装记录。
    #[test]
    fn update_snapshot_rejects_removed_or_changed_plugin() {
        let expected = InstalledPlugin {
            id: PluginId::parse("demo@official").expect("应解析测试插件 ID"),
            version: "1.0.0".to_owned(),
            install_path: PathBuf::from("/tmp/keencode-test-cache/demo"),
            enabled: true,
            public_user_config: BTreeMap::new(),
            sensitive_user_config_keys: BTreeSet::new(),
            secret_generation: 0,
        };
        assert!(
            ensure_plugin_update_snapshot_current(
                std::slice::from_ref(&expected),
                &crate::claude_plugins::PluginState::default(),
            )
            .expect_err("已卸载插件必须拒绝提交")
            .contains("已被卸载")
        );

        let mut changed = expected.clone();
        changed.enabled = false;
        let current = crate::claude_plugins::PluginState {
            plugins: vec![changed],
        };
        assert!(
            ensure_plugin_update_snapshot_current(std::slice::from_ref(&expected), &current)
                .expect_err("已改变插件必须拒绝提交")
                .contains("状态已改变")
        );

        let mut changed_generation = expected.clone();
        changed_generation.secret_generation = 1;
        let current = crate::claude_plugins::PluginState {
            plugins: vec![changed_generation],
        };
        assert!(
            ensure_plugin_update_snapshot_current(std::slice::from_ref(&expected), &current)
                .expect_err("敏感配置代际已改变时必须拒绝过期更新提交")
                .contains("状态已改变")
        );
    }

    fn write_zip_archive(path: &Path, entries: &[(&str, &[u8])]) {
        use std::io::Cursor;
        use zip::write::{SimpleFileOptions, ZipWriter};

        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for (name, bytes) in entries {
            writer
                .start_file(*name, options)
                .expect("应写入 ZIP 测试条目");
            writer.write_all(bytes).expect("应写入 ZIP 测试内容");
        }
        let bytes = writer.finish().expect("应完成 ZIP 测试归档").into_inner();
        fs::write(path, bytes).expect("应写入 ZIP 测试归档文件");
    }

    fn write_tar_archive(path: &Path, entries: &[(&str, &[u8])]) {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_entry_type(tar::EntryType::Regular);
            if name.contains("..") {
                // `Builder::append_data` 主动拒绝危险路径；这里直接构造一个
                // 已校验和的恶意测试头，验证读取侧不会静默修剪 `..`。
                header.set_path("safe.txt").expect("应设置 TAR 测试路径");
                {
                    let raw = header.as_mut_bytes();
                    raw[..100].fill(0);
                    raw[..name.len()].copy_from_slice(name.as_bytes());
                }
                header.set_cksum();
                builder
                    .append(&header, *bytes)
                    .expect("应写入 TAR 测试条目");
            } else {
                header.set_cksum();
                builder
                    .append_data(&mut header, *name, *bytes)
                    .expect("应写入 TAR 测试条目");
            }
        }
        fs::write(path, builder.into_inner().expect("应完成 TAR 测试归档"))
            .expect("应写入 TAR 测试归档文件");
    }

    #[test]
    fn zip_archive_rejects_path_escape_and_limits_entries_and_bytes() {
        let root = test_directory("safe-zip-archive");
        let archive = root.join("archive.zip");
        write_zip_archive(&archive, &[("../escaped.txt", b"escape")]);
        let error = extract_zip_archive(&root, &archive, "ZIP 测试", 8, 1024)
            .expect_err("ZIP 路径越界必须失败");
        assert!(error.contains("路径越界"));
        assert!(!root.parent().unwrap().join("escaped.txt").exists());

        write_zip_archive(&archive, &[("one.txt", b"one"), ("two.txt", b"two")]);
        assert!(
            extract_zip_archive(&root, &archive, "ZIP 测试", 1, 1024)
                .expect_err("ZIP 条目数超限必须失败")
                .contains("条目数超过")
        );
        assert!(
            extract_zip_archive(&root, &archive, "ZIP 测试", 8, 2)
                .expect_err("ZIP 解包字节数超限必须失败")
                .contains("解包后超过")
        );
        fs::remove_dir_all(root).expect("应清理 ZIP 安全测试目录");
    }

    #[cfg(unix)]
    #[test]
    fn zip_archive_rejects_symlink_entries() {
        use std::io::Cursor;
        use zip::write::{SimpleFileOptions, ZipWriter};

        let root = test_directory("safe-zip-symlink");
        let archive = root.join("archive.zip");
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        writer
            .add_symlink("linked", "../../outside", options)
            .expect("应写入 ZIP 符号链接条目");
        fs::write(
            &archive,
            writer
                .finish()
                .expect("应完成 ZIP 符号链接归档")
                .into_inner(),
        )
        .expect("应写入 ZIP 符号链接归档文件");

        let error = extract_zip_archive(&root, &archive, "ZIP 测试", 8, 1024)
            .expect_err("ZIP 符号链接必须失败");
        assert!(error.contains("符号链接"));
        fs::remove_dir_all(root).expect("应清理 ZIP 符号链接测试目录");
    }

    #[test]
    fn tar_archive_rejects_path_escape_and_limits_bytes() {
        let root = test_directory("safe-tar-archive");
        let archive = root.join("archive.tar");
        write_tar_archive(&archive, &[("../escaped.txt", b"escape")]);
        let error = extract_tar_reader(
            &root,
            File::open(&archive).expect("应打开 TAR 测试归档"),
            &archive,
            "TAR 测试",
            8,
            1024,
        )
        .expect_err("TAR 路径越界必须失败");
        assert!(error.contains("路径越界"));
        assert!(!root.parent().unwrap().join("escaped.txt").exists());

        write_tar_archive(&archive, &[("one.txt", b"one")]);
        assert!(
            extract_tar_reader(
                &root,
                File::open(&archive).expect("应打开 TAR 测试归档"),
                &archive,
                "TAR 测试",
                8,
                2,
            )
            .expect_err("TAR 解包字节数超限必须失败")
            .contains("解包后超过")
        );
        fs::remove_dir_all(root).expect("应清理 TAR 安全测试目录");
    }

    #[cfg(unix)]
    #[test]
    fn tar_archive_rejects_symlink_entries() {
        let root = test_directory("safe-tar-symlink");
        let archive = root.join("archive.tar");
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder
            .append_link(&mut header, "linked", "../../outside")
            .expect("应写入 TAR 符号链接条目");
        fs::write(
            &archive,
            builder.into_inner().expect("应完成 TAR 符号链接归档"),
        )
        .expect("应写入 TAR 符号链接归档文件");

        let error = extract_tar_reader(
            &root,
            File::open(&archive).expect("应打开 TAR 测试归档"),
            &archive,
            "TAR 测试",
            8,
            1024,
        )
        .expect_err("TAR 符号链接必须失败");
        assert!(error.contains("链接"));
        fs::remove_dir_all(root).expect("应清理 TAR 符号链接测试目录");
    }

    #[cfg(unix)]
    #[test]
    fn plugin_root_and_marketplace_preview_reject_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = test_directory("plugin-preview-symlink");
        let outside = root.join("outside");
        fs::create_dir_all(outside.join(".claude-plugin")).expect("应创建外部插件目录");
        fs::write(
            outside.join(".claude-plugin/plugin.json"),
            br#"{"name":"escaped"}"#,
        )
        .expect("应写入外部插件清单");
        symlink(&outside, root.join("linked")).expect("应创建插件根符号链接");
        assert!(find_plugin_root(&root).is_err());

        let market = root.join("market");
        fs::create_dir_all(market.join("plugin/.claude-plugin")).expect("应创建市场目录");
        symlink(&outside, market.join("linked")).expect("应创建市场插件符号链接");
        assert!(resolve_marketplace_relative_path(&market, "linked").is_err());
        symlink(
            outside.join(".claude-plugin/plugin.json"),
            market.join("plugin/.claude-plugin/plugin.json"),
        )
        .expect("应创建市场清单符号链接");
        let plugin = resolve_marketplace_relative_path(&market, "plugin")
            .expect("市场插件根目录本身应在市场根内");
        assert!(validate_directory_tree_without_symlinks(&plugin, "市场插件").is_err());
        fs::remove_dir_all(root).expect("应清理插件预览符号链接测试目录");
    }

    /// 顶层没有清单时仍应继续查找唯一的一级插件目录。
    #[test]
    fn find_plugin_root_continues_after_manifest_missing_at_archive_root() {
        let root = test_directory("plugin-root-nested-manifest");
        let plugin = root.join("package");
        fs::create_dir_all(plugin.join(".claude-plugin")).expect("应创建嵌套插件清单目录");
        fs::write(
            plugin.join(".claude-plugin/plugin.json"),
            br#"{"name":"nested"}"#,
        )
        .expect("应写入嵌套插件清单");

        assert_eq!(
            find_plugin_root(&root).expect("应找到嵌套插件根目录"),
            fs::canonicalize(plugin).expect("插件目录应可规范化")
        );
        fs::remove_dir_all(root).expect("应清理嵌套插件根目录测试目录");
    }

    /// tar.gz 应复用同一套路径与解包大小限制，而不是走外部命令。
    #[test]
    fn tar_gz_archive_extracts_with_archive_safety_checks() {
        use flate2::{Compression, write::GzEncoder};

        let root = test_directory("safe-tar-gz-archive");
        let archive = root.join("archive.tgz");
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, "nested/file.txt", &b"hello"[..])
                .expect("应写入 TAR.GZ 测试条目");
            builder.finish().expect("应完成 TAR.GZ 测试归档");
        }
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&tar_bytes)
            .expect("应压缩 TAR.GZ 测试归档");
        fs::write(&archive, encoder.finish().expect("应完成 TAR.GZ 测试归档"))
            .expect("应写入 TAR.GZ 测试归档文件");

        extract_archive(&root, &archive, "archive.tgz", "TAR.GZ 测试")
            .expect("TAR.GZ 应能安全解包");
        assert_eq!(
            fs::read(root.join("nested/file.txt")).expect("应读取 TAR.GZ 文件"),
            b"hello"
        );
        fs::remove_dir_all(root).expect("应清理 TAR.GZ 安全测试目录");
    }

    /// 创建并清理一个当前测试专用的临时目录。
    fn test_directory(label: &str) -> PathBuf {
        let path = env::temp_dir().join(format!("keencode-extensions-{label}-{}", process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("应创建测试目录");
        fs::canonicalize(path).expect("测试目录应返回规范绝对路径")
    }

    /// 当前原子写必须直接覆盖已有目标，不能先删除旧文件制造缺失窗口。
    #[test]
    fn atomic_private_write_replaces_existing_target() {
        let root = test_directory("atomic-replace");
        let path = root.join("state.json");
        fs::write(&path, b"old").expect("应写入旧文件");

        atomic_write_private(&path, b"new").expect("应原子覆盖已有文件");

        assert_eq!(fs::read(&path).expect("应读取新文件"), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path)
                .expect("应读取文件元数据")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        fs::remove_dir_all(root).expect("应清理原子覆盖测试目录");
    }

    /// 原子替换失败后必须删除同目录临时文件并保留原目标。
    #[test]
    fn atomic_private_write_cleans_temporary_file_after_failure() {
        let root = test_directory("atomic-cleanup");
        let path = root.join("occupied");
        fs::create_dir(&path).expect("应创建不可由文件覆盖的目标目录");

        assert!(atomic_write_private(&path, b"new").is_err());

        assert!(path.is_dir());
        let temporary_files = fs::read_dir(&root)
            .expect("应读取测试目录")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temporary_files, 0);
        fs::remove_dir_all(root).expect("应清理失败回收测试目录");
    }

    /// 外部来源工具超时后必须主动结束，而不是让插件安装永久等待。
    #[cfg(unix)]
    #[test]
    fn external_command_timeout_terminates_child() {
        let mut command = process::Command::new("sleep");
        command.arg("2");
        let started = Instant::now();
        let error =
            run_external_with_timeout(&mut command, "测试外部命令", Duration::from_millis(100))
                .expect_err("超时命令必须返回错误");

        assert!(error.contains("执行超时"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// 验证 Skill 前置元数据支持普通标量和折叠多行说明。
    #[test]
    fn parses_skill_frontmatter_scalars_and_folded_description() {
        let fields = parse_yaml_frontmatter(
            "---\nname: demo\ndescription: >-\n  第一行\n  第二行\n---\n# Demo\n",
        )
        .expect("应解析 Skill 前置元数据");
        assert_eq!(fields.get("name").map(String::as_str), Some("demo"));
        assert_eq!(
            fields.get("description").map(String::as_str),
            Some("第一行 第二行")
        );
    }

    /// 验证 Skill 前置元数据拒绝缺失闭合分隔符的内容。
    #[test]
    fn rejects_unclosed_skill_frontmatter() {
        let error =
            parse_yaml_frontmatter("---\nname: demo\n").expect_err("未闭合前置元数据必须失败");
        assert!(error.contains("未闭合"));
    }

    /// Skill 扫描不得通过符号链接目录项或主文件读取当前根目录外的内容。
    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_skill_entries_and_manifests() {
        use std::os::unix::fs::symlink;

        let root = test_directory("skill-symlink-boundary");
        let skills = root.join("skills");
        let outside = root.join("outside");
        fs::create_dir_all(&skills).expect("应创建 Skill 根目录");
        fs::create_dir_all(&outside).expect("应创建外部 Skill 目录");
        let outside_manifest = outside.join("SKILL.md");
        fs::write(
            &outside_manifest,
            "---\nname: escaped\ndescription: 越界 Skill\n---\n",
        )
        .expect("应写入外部 Skill");

        symlink(&outside, skills.join("linked")).expect("应创建目录符号链接");
        assert!(scan_skill_directory(&skills).is_err());
        fs::remove_file(skills.join("linked")).expect("应删除目录符号链接");

        let local = skills.join("local");
        fs::create_dir_all(&local).expect("应创建本地 Skill 目录");
        symlink(&outside_manifest, local.join("SKILL.md")).expect("应创建文件符号链接");
        assert!(scan_skill_directory(&skills).is_err());

        fs::remove_dir_all(root).expect("应清理 Skill 符号链接测试目录");
    }

    /// Skill 根目录本身不得是指向其他位置的符号链接。
    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_skill_root() {
        use std::os::unix::fs::symlink;

        let root = test_directory("skill-root-symlink");
        let real = root.join("real");
        fs::create_dir_all(real.join("demo")).expect("应创建真实 Skill 目录");
        fs::write(
            real.join("demo/SKILL.md"),
            "---\nname: demo\ndescription: 测试 Skill\n---\n",
        )
        .expect("应写入真实 Skill");
        let linked = root.join("linked");
        symlink(&real, &linked).expect("应创建 Skill 根目录符号链接");

        assert!(scan_skill_directory(&linked).is_err());
        fs::remove_dir_all(root).expect("应清理 Skill 根目录符号链接测试目录");
    }

    /// 市场插件 DTO 必须把 LSP 数量暴露给安装卡片与重启确认。
    #[test]
    fn available_plugin_dto_serializes_lsp_count() {
        let dto = AvailablePluginDto {
            name: "jdtls-lsp".to_owned(),
            marketplace: "claude-plugins-official".to_owned(),
            description: Some("Java language server".to_owned()),
            version: Some("1.0.0".to_owned()),
            skill_count: 0,
            lsp_count: 1,
        };

        assert_eq!(
            serde_json::to_value(dto).expect("应序列化市场插件 DTO"),
            serde_json::json!({
                "name": "jdtls-lsp",
                "marketplace": "claude-plugins-official",
                "description": "Java language server",
                "version": "1.0.0",
                "skillCount": 0,
                "lspCount": 1
            })
        );
    }

    /// Skill DTO 必须暴露显式来源和必填主文件路径。
    #[test]
    fn skill_dto_serializes_only_current_fields() {
        let dto = SkillDto {
            name: "demo".to_owned(),
            description: "Demo Skill".to_owned(),
            source: "plugin".to_owned(),
            path: "/tmp/demo/SKILL.md".to_owned(),
            user_invocable: true,
        };

        assert_eq!(
            serde_json::to_value(dto).expect("应序列化当前 Skill DTO"),
            serde_json::json!({
                "name": "demo",
                "description": "Demo Skill",
                "source": "plugin",
                "path": "/tmp/demo/SKILL.md",
                "userInvocable": true
            })
        );
    }

    /// 验证 MCP 只读取 peri 当前定义的 disabled 字段。
    #[test]
    fn mcp_enabled_state_uses_disabled_field() {
        let config = serde_json::json!({"disabled": true});
        assert!(!mcp_config_enabled(&config));
        assert!(mcp_config_enabled(&serde_json::json!({})));
    }

    /// 验证 MCP 列表始终反映外部配置，而不是 KeenCode 本地启用偏好。
    #[test]
    fn mcp_dto_uses_runtime_config_enabled_state() {
        let dto = mcp_dto(
            "demo".to_owned(),
            ResolvedMcpServer {
                config: serde_json::json!({
                    "command": "demo-server",
                    "disabled": true
                }),
                plugin_source: false,
            },
        );
        assert!(!dto.enabled);
        assert_eq!(
            serde_json::to_value(&dto).expect("应序列化当前 MCP DTO"),
            serde_json::json!({
                "name": "demo",
                "transport": "stdio",
                "target": "demo-server",
                "enabled": false
            })
        );
    }

    /// 验证 HTTP MCP 会按当前唯一传输类型输出。
    #[test]
    fn detects_http_mcp_transport_from_url() {
        let config = serde_json::json!({"url": "https://example.com/mcp"});
        assert_eq!(mcp_transport(&config), "http");
        let dto = mcp_dto(
            "http-demo".to_owned(),
            ResolvedMcpServer {
                config,
                plugin_source: false,
            },
        );
        assert_eq!(dto.transport, "http");
    }

    /// 插件 MCP 的已插值命令、参数和 URL 不能通过 inspect/doctor DTO 返回前端。
    #[test]
    fn plugin_mcp_dtos_hide_interpolated_sensitive_values() {
        let secret = "plugin-secret-value";
        let config = serde_json::json!({
            "command": format!("mcp-{secret}"),
            "args": ["--token", secret]
        });
        let server = ResolvedMcpServer {
            config,
            plugin_source: true,
        };

        let dto = mcp_dto("plugin:demo:server".to_owned(), server.clone());
        let dto_json = serde_json::to_string(&dto).expect("应序列化插件 MCP DTO");
        assert!(dto.target.is_none());
        assert!(!dto_json.contains(secret));

        let doctor = doctor_server("plugin:demo:server".to_owned(), server);
        let doctor_json = serde_json::to_string(&doctor).expect("应序列化 MCP Doctor DTO");
        assert!(doctor.target.is_none());
        assert!(!doctor_json.contains(secret));
    }

    /// 用户显式配置的 MCP 仍保留命令参数展示，避免无关功能退化。
    #[test]
    fn user_mcp_dto_keeps_target_arguments() {
        let dto = mcp_dto(
            "demo".to_owned(),
            ResolvedMcpServer {
                config: serde_json::json!({
                    "command": "demo-server",
                    "args": ["--mode", "safe mode"]
                }),
                plugin_source: false,
            },
        );
        assert_eq!(
            dto.target.as_deref(),
            Some("demo-server --mode \"safe mode\"")
        );
    }

    /// 验证 MCP 文件必须使用当前两种公开根结构之一。
    #[test]
    fn rejects_mcp_document_with_unknown_root_shape() {
        let root = test_directory("empty-mcp");
        let path = root.join("mcp.json");
        fs::write(&path, "{\"servers\":{}}\n").expect("应写入别名 MCP 测试配置");
        assert!(load_mcp_document(&path).is_err());

        fs::write(&path, "{\"demo\":{\"command\":\"demo\"}}\n").expect("应写入 flat MCP 测试配置");
        assert!(load_mcp_document(&path).is_err());

        fs::write(&path, "{\"mcpServers\":{},\"config\":{}}\n")
            .expect("应写入包含未知根字段的 MCP 测试配置");
        assert!(load_mcp_document(&path).is_err());
        fs::remove_dir_all(root).expect("应清理 MCP 测试目录");
    }

    /// 厂商常见的单层 Server 映射必须归一化为 canonical mcpServers 结构。
    #[test]
    fn accepts_vendor_root_server_map() {
        let document = parse_mcp_import_text(
            r#"{
              "gitee-ent": {
                "type": "stdio",
                "command": "npx",
                "args": ["-y", "@gitee/mcp-gitee-ent@latest"],
                "env": {
                  "GITEE_ENT_API_BASE": "https://api.gitee.com/enterprises",
                  "GITEE_ENT_MCP_ACCESS_TOKEN": "token"
                }
              }
            }"#,
        )
        .expect("厂商 MCP 配置应通过校验");
        assert_eq!(
            mcp_server_map(&document)
                .expect("应读取归一化的 MCP Server 映射")
                .keys()
                .collect::<Vec<_>>(),
            vec![&"gitee-ent".to_owned()]
        );
        assert_eq!(
            document
                .root
                .get("mcpServers")
                .and_then(Value::as_object)
                .map(|_| true),
            Some(true)
        );
        assert!(
            mcp_server_map(&document)
                .expect("应读取导入后的 MCP Server")
                .get("gitee-ent")
                .and_then(|config| config.get("type"))
                .is_none()
        );
    }

    /// 导入的 type 提示必须与实际传输字段一致，且归一化后不落盘。
    #[test]
    fn mcp_import_type_must_match_transport() {
        for text in [
            r#"{"demo":{"type":"stdio","url":"https://example.com"}}"#,
            r#"{"demo":{"type":"http","command":"demo"}}"#,
            r#"{"demo":{"type":"sse","url":"https://example.com"}}"#,
        ] {
            assert!(parse_mcp_import_text(text).is_err(), "{text}");
        }
        let document = parse_mcp_import_text(
            r#"{"mcpServers":{"demo":{"type":"http","url":"https://example.com"}}}"#,
        )
        .expect("type=http 与 url 应通过导入");
        assert!(
            mcp_server_map(&document).unwrap()["demo"]
                .get("type")
                .is_none()
        );
    }

    /// 导入必须在写入前完成全量冲突检查，冲突时不产生部分合并结果。
    #[test]
    fn merge_mcp_documents_rejects_any_conflict_atomically() {
        let existing =
            parse_mcp_document_text(r#"{"mcpServers":{"existing":{"command":"existing"}}}"#)
                .expect("现有 MCP 配置应通过校验");
        let imported = parse_mcp_document_text(
            r#"{"mcpServers":{"new":{"command":"new"},"existing":{"command":"replacement"}}}"#,
        )
        .expect("待导入 MCP 配置应通过校验");
        let error = merge_mcp_documents(existing.clone(), imported).expect_err("应拒绝冲突导入");
        assert!(error.contains("existing"));
        let existing_servers = mcp_server_map(&existing).expect("应读取现有映射");
        assert_eq!(existing_servers.len(), 1);
        assert!(existing_servers.contains_key("existing"));
        assert!(!existing_servers.contains_key("new"));
    }

    /// 验证 MCP Server 不接受未知字段、混合传输或缺失传输。
    #[test]
    fn rejects_non_current_mcp_server_shapes() {
        let root = test_directory("strict-mcp-server");
        let path = root.join("mcp.json");
        for document in [
            r#"{"mcpServers":{"demo":{"command":"demo","vendor":"old"}}}"#,
            r#"{"mcpServers":{"demo":{"command":"demo","url":"https://example.com"}}}"#,
            r#"{"mcpServers":{"demo":{"disabled":false}}}"#,
            r#"{"mcpServers":{"demo":{"command":"demo","disabled":false}}}"#,
            r#"{"mcpServers":{"demo":{"url":"ftp://example.com"}}}"#,
            r#"{"mcpServers":{"demo":{"url":"https://example.com","oauth":{"enabled":true}}}}"#,
        ] {
            fs::write(&path, document).expect("应写入无效 MCP 测试配置");
            assert!(load_mcp_document(&path).is_err(), "{document}");
        }
        fs::remove_dir_all(root).expect("应清理 MCP Server 结构测试目录");
    }

    /// 验证当前 stdio 与 HTTP MCP 结构能严格通过。
    #[test]
    fn accepts_current_mcp_server_shapes() {
        let root = test_directory("current-mcp-server");
        let path = root.join("mcp.json");
        fs::write(
            &path,
            r#"{
              "mcpServers": {
                "stdio": {"command":"npx","args":["-y","tool"],"env":{"TOKEN":"${TOKEN}"}},
                "http": {"url":"https://example.com/mcp","headers":{"Authorization":"Bearer ${TOKEN}"}}
              }
            }"#,
        )
        .expect("应写入当前 MCP 测试配置");
        let document = load_mcp_document(&path)
            .expect("当前 MCP 配置应通过")
            .expect("MCP 配置应存在");
        assert_eq!(
            mcp_server_map(&document).expect("应读取 MCP Server").len(),
            2
        );
        fs::remove_dir_all(root).expect("应清理当前 MCP 结构测试目录");
    }

    /// 验证 MCP 开关只写入 peri 当前定义的 disabled 字段。
    #[test]
    fn persists_mcp_enabled_and_disabled_consistently() {
        let mut document = empty_mcp_document();
        mcp_server_map_mut(&mut document)
            .expect("应返回可写 Server 映射")
            .insert(
                "demo".to_owned(),
                serde_json::json!({"command": "demo-server"}),
            );
        assert!(set_mcp_document_enabled(&mut document, "demo", false).expect("应设置 MCP 状态"));
        let config = &mcp_server_map(&document).expect("应返回 Server 映射")["demo"];
        assert_eq!(config.get("disabled").and_then(Value::as_bool), Some(true));

        assert!(set_mcp_document_enabled(&mut document, "demo", true).expect("应设置 MCP 状态"));
        let config = &mcp_server_map(&document).expect("应返回 Server 映射")["demo"];
        assert!(config.get("disabled").is_none());
    }

    /// 验证扩展名称会拒绝控制字符。
    #[test]
    fn rejects_control_characters_in_extension_names() {
        assert!(validate_extension_name("bad\nname", "插件").is_err());
        assert!(validate_extension_name(" demo ", "插件").is_err());
    }

    /// marketplace 插件 source 必须保留 ref、sha、path 与 sparsePaths，不能先转为字符串丢失固定版本。
    #[test]
    fn preserves_marketplace_plugin_source_pins_and_paths() {
        let spec = parse_marketplace_plugin_source(serde_json::json!({
            "source": "github",
            "repo": "acme/tools",
            "ref": "release",
            "sha": "0123456789012345678901234567890123456789",
            "path": "plugins/demo",
            "sparsePaths": ["plugins/demo", "shared/schema"]
        }))
        .expect("应解析完整插件 source");
        assert_eq!(
            spec,
            MarketplacePluginSourceSpec::Git {
                url: "https://github.com/acme/tools.git".to_owned(),
                path: Some("plugins/demo".to_owned()),
                reference: Some("release".to_owned()),
                sha: Some("0123456789012345678901234567890123456789".to_owned()),
                sparse_paths: vec!["plugins/demo".to_owned(), "shared/schema".to_owned()],
            }
        );
    }

    /// marketplace source 对象支持仓库 path、sparsePaths，并保留 URL headers。
    #[test]
    fn parses_marketplace_path_sparse_paths_and_url_headers() {
        let git = parse_marketplace_source_spec(
            r#"{"source":"github","repo":"acme/monorepo","ref":"main","path":"marketplace","sparsePaths":["marketplace","shared"]}"#,
        )
        .expect("应解析 marketplace JSON source")
        .expect("应返回 marketplace source");
        assert_eq!(
            git,
            MarketplaceSourceSpec::Git {
                url: "https://github.com/acme/monorepo.git".to_owned(),
                reference: Some("main".to_owned()),
                path: Some("marketplace".to_owned()),
                sparse_paths: vec!["marketplace".to_owned(), "shared".to_owned()],
            }
        );

        let url = parse_marketplace_source_spec(
            r#"{"source":"url","url":"https://example.test/marketplace.json","headers":{"Authorization":"Bearer token","X-Source":"test"}}"#,
        )
        .expect("应解析 URL marketplace source")
        .expect("应返回 URL source");
        assert_eq!(
            url,
            MarketplaceSourceSpec::Url {
                url: "https://example.test/marketplace.json".to_owned(),
                headers: BTreeMap::from([
                    ("Authorization".to_owned(), "Bearer token".to_owned()),
                    ("X-Source".to_owned(), "test".to_owned()),
                ]),
            }
        );
    }

    /// URL headers 和 Git sparse 路径必须拒绝控制字符、空路径及目录穿越。
    #[test]
    fn rejects_unsafe_marketplace_source_options() {
        assert!(
            parse_http_headers(Some(&serde_json::json!({
                "Authorization": "Bearer\nsecret"
            })))
            .is_err()
        );
        assert!(validate_source_relative_path("../outside", "市场 path").is_err());
        assert!(validate_source_relative_path("/absolute", "市场 path").is_err());
        assert!(
            parse_marketplace_source_spec(
                r#"{"source":"github","repo":"acme/tools","sparsePaths":"plugins"}"#,
            )
            .is_err()
        );
    }

    /// marketplace 允许用 `./` 声明市场根目录本身就是插件目录。
    #[test]
    fn resolves_marketplace_root_plugin_source() {
        let root = test_directory("marketplace-root-plugin");

        assert_eq!(
            resolve_marketplace_relative_path(&root, "./").expect("应解析市场根目录"),
            fs::canonicalize(&root).expect("应规范化市场根目录"),
        );

        fs::remove_dir_all(root).expect("应清理测试目录");
    }

    /// 无 model 键时插入新的模型覆盖行，正文与其他 frontmatter 原样保留。
    #[test]
    fn frontmatter_model_inserts_new_key() {
        let content = "---\nname: \"code-reviewer\"\ndescription: \"审查代码\"\n---\n\n正文";
        let updated =
            set_frontmatter_model(content, Some("openai::gpt-5")).expect("应插入 model 键");
        assert!(updated.contains("model: \"openai::gpt-5\"\n"));
        assert!(updated.ends_with("\n\n正文"));
        assert!(updated.starts_with("---\nname: \"code-reviewer\"\ndescription: \"审查代码\"\n"));
    }

    /// 已有 model 键时替换为新值，不产生重复行。
    #[test]
    fn frontmatter_model_replaces_existing_key() {
        let content =
            "---\nname: \"reviewer\"\ndescription: \"审查\"\nmodel: \"old::model\"\n---\n\n正文";
        let updated =
            set_frontmatter_model(content, Some("provider-a::model-a")).expect("应替换 model 键");
        assert_eq!(updated.matches("model:").count(), 1);
        assert!(updated.contains("model: \"provider-a::model-a\"\n"));
    }

    /// None 删除已有 model 键（回退跟随会话 provider）。
    #[test]
    fn frontmatter_model_removes_existing_key() {
        let content =
            "---\nname: \"reviewer\"\ndescription: \"审查\"\nmodel: \"old::model\"\n---\n\n正文";
        let updated = set_frontmatter_model(content, None).expect("应删除 model 键");
        assert!(!updated.contains("model:"));
        assert_eq!(
            updated,
            "---\nname: \"reviewer\"\ndescription: \"审查\"\n---\n\n正文"
        );
    }

    /// None 且原本无 model 键时内容保持不变。
    #[test]
    fn frontmatter_model_noop_when_absent() {
        let content = "---\nname: \"reviewer\"\ndescription: \"审查\"\n---\n\n正文";
        assert_eq!(
            set_frontmatter_model(content, None).expect("应保持内容不变"),
            content
        );
    }

    /// 只触碰顶层 model 键，缩进的嵌套键（如 MCP 配置）不受影响。
    #[test]
    fn frontmatter_model_ignores_indented_keys() {
        let content = "---\nname: \"reviewer\"\ndescription: \"审查\"\nmcp_servers:\n  - server: \"demo\"\n    model: \"nested\"\n---\n\n正文";
        let updated =
            set_frontmatter_model(content, Some("openai::gpt-5")).expect("应插入顶层 model 键");
        assert!(updated.contains("    model: \"nested\"\n"));
        assert!(updated.contains("model: \"openai::gpt-5\"\n"));
    }

    /// 缺少闭合分隔符的文件必须报错而不是静默截断。
    #[test]
    fn frontmatter_model_rejects_unclosed_frontmatter() {
        let content = "---\nname: \"reviewer\"\ndescription: \"审查\"\n";
        assert!(set_frontmatter_model(content, Some("openai::gpt-5")).is_err());
    }

    /// 设置页模型覆盖只接受规范的 Provider/模型限定引用。
    #[test]
    fn model_reference_accepts_only_provider_and_model() {
        assert_eq!(
            normalize_model_reference(" provider-a :: model-a ").expect("应规范化引用"),
            "provider-a::model-a"
        );
        for invalid in [
            "",
            "unqualified-model",
            "::model",
            "provider::",
            "provider::model::extra",
        ] {
            assert!(normalize_model_reference(invalid).is_err(), "{invalid:?}");
        }
        assert!(normalize_model_reference("provider\n::model").is_err());
    }

    /// 损坏的 MCP 用户配置备份必须带日期、避免冲突且不修改原文件。
    #[test]
    fn invalid_mcp_config_backup_is_dated_and_non_destructive() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let path = directory.path().join("mcp.json");
        fs::write(&path, "{broken").expect("写入损坏配置");

        let first = backup_invalid_mcp_config(&path).expect("创建首个备份");
        let second = backup_invalid_mcp_config(&path).expect("创建不冲突备份");

        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(".bak")
        );
        assert_eq!(fs::read_to_string(first).unwrap(), "{broken");
        assert_eq!(fs::read_to_string(second).unwrap(), "{broken");
        assert_eq!(fs::read_to_string(path).unwrap(), "{broken");
    }

    /// 空快照无法写入时必须切到不存在路径，不能再次读取旧运行时内容。
    #[test]
    fn unavailable_mcp_runtime_path_never_reuses_old_snapshot() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let runtime_path = directory.path().join("mcp-runtime.json");
        fs::write(&runtime_path, r#"{"mcpServers":{"old":{"command":"old"}}}"#)
            .expect("写入旧快照");

        let fallback = unavailable_mcp_runtime_path(&runtime_path);

        assert_ne!(fallback, runtime_path);
        assert!(!fallback.exists());
        assert!(fs::read_to_string(runtime_path).unwrap().contains("old"));
    }

    /// 插件敏感值只进入进程内 MCP 类型；写入运行时快照的文档只含用户配置。
    #[test]
    fn plugin_mcp_secret_stays_in_memory_and_out_of_runtime_document() {
        let directory = tempfile::tempdir().expect("创建临时目录");
        let runtime_path = directory.path().join("mcp-runtime.json");
        let secret = "plugin-secret-value";
        let user_document = empty_mcp_document();
        let mut runtime_document = user_document.clone();
        mcp_server_map_mut(&mut runtime_document)
            .expect("用户文档应包含 MCP 映射")
            .insert(
                "plugin:demo:secret".to_owned(),
                serde_json::json!({
                    "command": "demo-mcp",
                    "env": {"TOKEN": secret}
                }),
            );

        save_mcp_document(&runtime_path, &user_document).expect("用户 MCP 快照应可写入");
        let persisted = fs::read_to_string(&runtime_path).expect("读取运行时快照");
        assert!(!persisted.contains(secret));

        let plugin_servers = BTreeSet::from(["plugin:demo:secret".to_owned()]);
        let in_memory = mcp_config_from_document(&runtime_document, &runtime_path, &plugin_servers)
            .expect("插件配置应转换为 Peri 内存配置");
        assert_eq!(
            in_memory
                .mcp_servers
                .get("plugin:demo:secret")
                .and_then(|server| server.env.as_ref())
                .and_then(|env| env.get("TOKEN")),
            Some(&secret.to_owned())
        );
    }

    /// 损坏配置备份不得跟随符号链接读取或替换链接目标。
    #[cfg(unix)]
    #[test]
    fn invalid_mcp_backup_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("创建临时目录");
        let target = directory.path().join("outside.json");
        let path = directory.path().join("mcp.json");
        fs::write(&target, "{broken target").expect("写入链接目标");
        symlink(&target, &path).expect("创建 MCP 符号链接");

        assert!(backup_invalid_mcp_config(&path).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "{broken target");
    }
}
