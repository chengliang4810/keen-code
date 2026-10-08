//! Native Host 的项目扩展候选装配。
//!
//! 这里负责读取 Native 数据边界内的插件和 MCP 配置，并把一次构建所需的
//! Plugins、Skills、Hook、MCP 与 LSP 输入冻结后交给贡献器。构建完成前不会
//! 发布候选，失败时也不会覆盖 Runtime 中已经存在的代次。

use super::NativeHost;
use crate::agent_runtime::{AgentRuntime, RuntimeExtensionCandidate};
use crate::native_agents::discover_agents;
use crate::native_extension_contributor::{
    NativeExtensionContributor, NativeExtensionInputs, NativeMcpServerInput, parse_plugin_hooks,
    prepare_lsp_runtime, prepare_mcp_tools,
};
use crate::native_hooks::{HookInputFingerprint, input_fingerprint, runtime_hooks};
use crate::native_paths::NativePaths;
use crate::plugin_secrets::SystemSecretStore;
use crate::plugins::{
    PluginCommandCatalog, PluginCommandTool, PluginManager, PluginRuntimeSnapshot,
};
use keencode_acp::McpOAuthStatus;
use keencode_agent::{AgentTool, HookCircuitStore};
use keencode_mcp::{McpServerConfig, StdioServerConfig, StreamableHttpConfig};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_MCP_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const EXTENSION_INPUT_CHANGED_DURING_BUILD: &str = "扩展输入在候选构建期间发生变化，请重试";

#[derive(Clone, Debug, Eq, PartialEq)]
struct PluginInputFingerprint {
    /// 只保留插件状态和 manifest 输入的摘要，不保留原始配置内容。
    digest: String,
    /// 读取异常时禁止将候选作为稳定缓存复用。
    readable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExtensionInputFingerprint {
    /// Native Hook 配置、信任和脚本输入摘要。
    native_hooks: HookInputFingerprint,
    /// 插件 registry 与所有已安装插件 manifest 摘要。
    plugins: PluginInputFingerprint,
}

#[derive(Clone, Debug)]
struct PublishedExtensionInput {
    generation: u64,
    fingerprint: ExtensionInputFingerprint,
}

// RuntimeExtensionCandidate 保持不可变，因此把只用于缓存命中的扩展输入状态
// 放在同一进程的候选索引中，并以项目 canonical root 隔离不同工作区。
static PUBLISHED_EXTENSION_INPUTS: OnceLock<Mutex<BTreeMap<PathBuf, PublishedExtensionInput>>> =
    OnceLock::new();

impl NativeHost {
    /// 确保项目拥有一代已发布的扩展候选；重复调用只读取当前代次。
    pub(crate) async fn ensure_extension_candidate(
        &self,
        project_root: &Path,
    ) -> Result<u64, String> {
        let project_root = canonical_project_root(project_root)?;
        if let Some(generation) = self
            .inner
            .runtime
            .extension_generation(&project_root)
            .map_err(|error| format!("读取扩展候选代次失败：{error}"))?
            && !self
                .inner
                .runtime
                .extension_candidate_needs_refresh(&project_root)
                .map_err(|error| format!("读取扩展候选刷新状态失败：{error}"))?
        {
            if extension_candidate_matches(&self.inner.paths, &project_root, generation)? {
                return Ok(generation);
            }
            self.inner
                .runtime
                .invalidate_extension_candidate(&project_root)
                .map_err(|error| format!("标记扩展候选失效失败：{error}"))?;
        }

        let _build = self.inner.extension_build_lock.lock().await;
        if let Some(generation) = self
            .inner
            .runtime
            .extension_generation(&project_root)
            .map_err(|error| format!("读取扩展候选代次失败：{error}"))?
            && !self
                .inner
                .runtime
                .extension_candidate_needs_refresh(&project_root)
                .map_err(|error| format!("读取扩展候选刷新状态失败：{error}"))?
        {
            if extension_candidate_matches(&self.inner.paths, &project_root, generation)? {
                return Ok(generation);
            }
            self.inner
                .runtime
                .invalidate_extension_candidate(&project_root)
                .map_err(|error| format!("标记扩展候选失效失败：{error}"))?;
        }
        // 在读取插件、Skill 和 MCP 输入前固定 epoch；构建期间若设置域再次写入，
        // Runtime 会拒绝发布这份旧候选并保留待刷新状态。
        let refresh_epoch = self
            .inner
            .runtime
            .extension_candidate_refresh_epoch(&project_root)
            .map_err(|error| format!("读取扩展候选刷新 epoch 失败：{error}"))?;
        let generation = self
            .inner
            .next_extension_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .checked_add(1)
            .ok_or_else(|| "扩展运行时代次已经耗尽".to_owned())?;
        match build_and_publish_candidate_at_epoch(
            &self.inner.paths,
            &self.inner.runtime,
            &project_root,
            generation,
            refresh_epoch,
        )
        .await
        {
            Ok(generation) => Ok(generation),
            Err(error) if error == EXTENSION_INPUT_CHANGED_DURING_BUILD => {
                let retry_epoch = self
                    .inner
                    .runtime
                    .extension_candidate_refresh_epoch(&project_root)
                    .map_err(|error| format!("读取扩展候选重试 epoch 失败：{error}"))?;
                let retry_generation = self
                    .inner
                    .next_extension_generation
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
                    .checked_add(1)
                    .ok_or_else(|| "扩展运行时代次已经耗尽".to_owned())?;
                build_and_publish_candidate_at_epoch(
                    &self.inner.paths,
                    &self.inner.runtime,
                    &project_root,
                    retry_generation,
                    retry_epoch,
                )
                .await
            }
            Err(error) => Err(error),
        }
    }
}

async fn build_and_publish_candidate_at_epoch(
    paths: &NativePaths,
    runtime: &Arc<AgentRuntime>,
    project_root: &Path,
    generation: u64,
    refresh_epoch: u64,
) -> Result<u64, String> {
    let project_root = canonical_project_root(project_root)?;
    let extension_inputs_before = extension_input_fingerprint(paths, &project_root);
    let snapshot = plugin_snapshot(paths, &project_root)?;
    let disabled_skills = read_disabled_skill_names(&paths.data_root)?;
    let skills = NativeExtensionContributor::discover_skills(
        &paths.data_root,
        &project_root,
        &snapshot,
        disabled_skills,
    )?;
    let skill_reference_catalog = NativeExtensionContributor::skill_reference_catalog(&skills)?;
    let plugin_reference_catalog = NativeExtensionContributor::plugin_reference_catalog(&snapshot);

    let command_catalog = Arc::new(
        PluginCommandCatalog::from_snapshot(&snapshot)
            .map_err(|error| format!("建立插件 command 目录失败：{error}"))?,
    );
    let plugin_command_tool = (!command_catalog.is_empty())
        .then(|| Arc::new(PluginCommandTool::new(command_catalog)) as Arc<dyn AgentTool>);

    let (mut hooks, mut diagnostics) = parse_plugin_hooks(&snapshot);
    let (native_hooks, hook_diagnostics) = runtime_hooks(paths, &project_root);
    hooks.extend(native_hooks);
    diagnostics.extend(hook_diagnostics);
    let (agents, agent_diagnostics) = discover_agents(&paths.data_root, &project_root, &snapshot);
    diagnostics.extend(agent_diagnostics);
    let mcp_inputs = mcp_inputs(paths, &project_root, &snapshot, &mut diagnostics)?;
    let (mcp_tools, mcp_diagnostics, mcp_servers) = prepare_mcp_tools(mcp_inputs).await;
    diagnostics.extend(mcp_diagnostics);

    let (lsp_runtime, lsp_diagnostics) = prepare_lsp_runtime(
        &project_root,
        NativeExtensionContributor::lsp_configs(&snapshot),
    )
    .await?;
    diagnostics.extend(lsp_diagnostics);

    let contributor = NativeExtensionContributor::from_inputs(NativeExtensionInputs {
        project_root: project_root.clone(),
        skills,
        plugin_command_tool,
        mcp_tools,
        mcp_servers,
        lsp_runtime,
        hooks,
        hook_circuits: HookCircuitStore::new(),
        plugin_reference_catalog,
        skill_reference_catalog,
        diagnostics,
        agents,
    })?;
    let candidate = RuntimeExtensionCandidate::new(generation, Arc::new(contributor))
        .map_err(|error| format!("创建扩展候选失败：{error}"))?;
    let extension_inputs_after = extension_input_fingerprint(paths, &project_root);
    if extension_inputs_before != extension_inputs_after {
        runtime
            .invalidate_extension_candidate(&project_root)
            .map_err(|error| format!("标记构建期间扩展输入变化失败：{error}"))?;
        return Err(EXTENSION_INPUT_CHANGED_DURING_BUILD.to_owned());
    }
    let published_generation = runtime
        .publish_extension_candidate_at_epoch(&project_root, candidate, refresh_epoch)
        .map_err(|error| format!("发布扩展候选失败：{error}"))?;
    remember_extension_input(&project_root, published_generation, extension_inputs_after)?;
    Ok(published_generation)
}

fn extension_input_fingerprint(
    paths: &NativePaths,
    project_root: &Path,
) -> ExtensionInputFingerprint {
    ExtensionInputFingerprint {
        native_hooks: input_fingerprint(paths, project_root),
        plugins: plugin_input_fingerprint(paths),
    }
}

fn plugin_input_fingerprint(paths: &NativePaths) -> PluginInputFingerprint {
    let manager = PluginManager::new(paths.data_root.clone());
    let state = match manager.load_state() {
        Ok(state) => state,
        // 错误只让候选变为不可复用，不把状态文件中的路径或配置写入摘要。
        Err(_) => return unreadable_plugin_input_fingerprint("registry"),
    };

    let registry = match crate::storage::read_private_bytes_bounded(
        &manager.storage.state_path,
        crate::plugins::MAX_MANIFEST_BYTES,
        "插件状态",
    ) {
        Ok(Some(bytes)) => (
            serde_json::json!({
                "state": "present",
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
            }),
            true,
        ),
        Ok(None) => (serde_json::json!({"state": "missing"}), true),
        Err(_) => (serde_json::json!({"state": "error"}), false),
    };

    let mut readable = registry.1;
    let mut manifests = Map::new();
    for installed in state.plugins {
        let manifest_path = installed.install_path.join(crate::plugins::PLUGIN_MANIFEST);
        let (manifest, manifest_readable) = plugin_manifest_input_state(&manifest_path);
        readable &= manifest_readable;
        manifests.insert(installed.id.to_string(), manifest);
    }

    let mut payload = Map::new();
    payload.insert("registry".to_owned(), registry.0);
    payload.insert("manifests".to_owned(), Value::Object(manifests));
    PluginInputFingerprint {
        digest: digest_json(&Value::Object(payload)),
        readable,
    }
}

fn unreadable_plugin_input_fingerprint(source: &str) -> PluginInputFingerprint {
    let mut payload = Map::new();
    payload.insert(source.to_owned(), serde_json::json!({"state": "error"}));
    PluginInputFingerprint {
        digest: digest_json(&Value::Object(payload)),
        readable: false,
    }
}

fn plugin_manifest_input_state(path: &Path) -> (Value, bool) {
    match crate::storage::read_private_bytes_bounded(
        path,
        crate::plugins::MAX_MANIFEST_BYTES,
        "插件 manifest",
    ) {
        Ok(Some(bytes)) => (
            serde_json::json!({
                "state": "present",
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
            }),
            // 原始摘要用于检测外部改写；解析失败则不把错误候选当作稳定缓存。
            crate::plugins::parse_plugin_manifest(&bytes).is_ok(),
        ),
        Ok(None) => (serde_json::json!({"state": "missing"}), true),
        Err(_) => (serde_json::json!({"state": "error"}), false),
    }
}

fn digest_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).expect("扩展候选输入摘要必须可序列化");
    format!("{:x}", Sha256::digest(bytes))
}

fn extension_candidate_matches(
    paths: &NativePaths,
    project_root: &Path,
    generation: u64,
) -> Result<bool, String> {
    let current = extension_input_fingerprint(paths, project_root);
    if !current.native_hooks.readable || !current.plugins.readable {
        return Ok(false);
    }
    let states = PUBLISHED_EXTENSION_INPUTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| "扩展候选输入索引已损坏".to_owned())?;
    Ok(states.get(project_root).is_some_and(|published| {
        published.generation == generation && published.fingerprint == current
    }))
}

fn remember_extension_input(
    project_root: &Path,
    generation: u64,
    fingerprint: ExtensionInputFingerprint,
) -> Result<(), String> {
    PUBLISHED_EXTENSION_INPUTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| "扩展候选输入索引已损坏".to_owned())?
        .insert(
            project_root.to_path_buf(),
            PublishedExtensionInput {
                generation,
                fingerprint,
            },
        );
    Ok(())
}

fn plugin_snapshot(
    paths: &NativePaths,
    project_root: &Path,
) -> Result<PluginRuntimeSnapshot, String> {
    let manager = PluginManager::new(paths.data_root.clone());
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let secrets = SystemSecretStore;
    manager
        .runtime_snapshot(project_root, &environment, &secrets)
        .map_err(|error| format!("读取插件运行时快照失败：{error}"))
}

fn read_disabled_skill_names(data_root: &Path) -> Result<Vec<String>, String> {
    let path = data_root.join("ui-presentation.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("读取 Skill 状态失败：{error}")),
    };
    if bytes.len() as u64 > MAX_MCP_CONFIG_BYTES {
        return Err("Skill 状态文件超过大小限制".to_owned());
    }
    let value: Value =
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .map_err(|_| "Skill 状态文件 JSON 无效".to_owned())?;
    Ok(value
        .get("settings")
        .and_then(Value::as_object)
        .and_then(|settings| settings.get("skills"))
        .and_then(Value::as_object)
        .and_then(|skills| skills.get("disabled"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect())
}

fn mcp_inputs(
    paths: &NativePaths,
    project_root: &Path,
    snapshot: &PluginRuntimeSnapshot,
    diagnostics: &mut Vec<crate::agent_runtime::RuntimeExtensionDiagnostic>,
) -> Result<Vec<NativeMcpServerInput>, String> {
    let mut documents = BTreeMap::new();
    read_mcp_document(
        &paths.data_root.join("mcp.json"),
        project_root,
        &mut documents,
    )?;
    read_mcp_document(
        &project_root.join(".agents").join("mcp.json"),
        project_root,
        &mut documents,
    )?;

    let mut result = Vec::new();
    for (name, value) in documents {
        if !mcp_config_enabled(&value) {
            continue;
        }
        match mcp_server_input(name.clone(), &value, project_root) {
            Ok(server) => result.push(server),
            Err(error) => diagnostics.push(mcp_config_diagnostic(name, error)),
        }
    }
    for plugin in &snapshot.plugins {
        let namespace = plugin
            .id
            .runtime_namespace()
            .unwrap_or_else(|_| plugin.id.to_string());
        for (name, value) in &plugin.mcp_servers {
            if !mcp_config_enabled(value) {
                continue;
            }
            let id = format!("{namespace}:{name}");
            match mcp_server_input(id.clone(), value, &plugin.root) {
                Ok(server) => result.push(server),
                Err(error) => diagnostics.push(mcp_config_diagnostic(id, error)),
            }
        }
    }
    Ok(result)
}

fn read_mcp_document(
    path: &Path,
    current_dir: &Path,
    output: &mut BTreeMap<String, Value>,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("读取 MCP 配置失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("MCP 配置必须是普通文件".to_owned());
    }
    if metadata.len() > MAX_MCP_CONFIG_BYTES {
        return Err("MCP 配置超过大小限制".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| format!("读取 MCP 配置失败：{error}"))?;
    let value: Value =
        serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
            .map_err(|_| "MCP 配置 JSON 无效".to_owned())?;
    let servers = value
        .get("mcpServers")
        .and_then(Value::as_object)
        .ok_or_else(|| "MCP 配置缺少 mcpServers 对象".to_owned())?;
    for (name, config) in servers {
        if name.trim().is_empty() {
            return Err("MCP Server 名称不能为空".to_owned());
        }
        let mut config = config.clone();
        if let Some(object) = config.as_object_mut()
            && object.get("command").is_some()
            && object.get("url").is_none()
        {
            object.insert(
                "currentDir".to_owned(),
                Value::String(current_dir.to_string_lossy().into_owned()),
            );
        }
        output.insert(name.clone(), config);
    }
    Ok(())
}

fn mcp_config_enabled(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("disabled").and_then(Value::as_bool) != Some(true)
        && object.get("enabled").and_then(Value::as_bool) != Some(false)
}

fn mcp_server_input(
    name: String,
    value: &Value,
    current_dir: &Path,
) -> Result<NativeMcpServerInput, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("MCP Server {name} 配置必须是对象"))?;
    let config = if let Some(command) = object.get("command").and_then(Value::as_str) {
        let mut config = StdioServerConfig::new(command);
        config.args = string_array(object, "args", &name)?;
        config.current_dir = Some(current_dir.to_path_buf());
        config.environment = string_map(object, "env", &name)?;
        config.inherit_environment = true;
        McpServerConfig::Stdio(config)
    } else if let Some(endpoint) = object.get("url").and_then(Value::as_str) {
        let mut config = StreamableHttpConfig::new(endpoint);
        config.headers = string_map(object, "headers", &name)?;
        config.terminate_session_on_close = true;
        McpServerConfig::StreamableHttp(config)
    } else {
        return Err(format!("MCP Server {name} 缺少 command 或 url"));
    };
    Ok(NativeMcpServerInput {
        name,
        config,
        oauth_status: McpOAuthStatus::NotRequired,
    })
}

fn string_array(
    object: &Map<String, Value>,
    field: &str,
    name: &str,
) -> Result<Vec<String>, String> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("MCP Server {name} 的 {field} 必须是字符串数组"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("MCP Server {name} 的 {field} 只能包含字符串"))
        })
        .collect()
}

fn string_map(
    object: &Map<String, Value>,
    field: &str,
    name: &str,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = object.get(field) else {
        return Ok(BTreeMap::new());
    };
    let values = value
        .as_object()
        .ok_or_else(|| format!("MCP Server {name} 的 {field} 必须是字符串映射"))?;
    values
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| format!("MCP Server {name} 的 {field}.{key} 必须是字符串"))
        })
        .collect()
}

fn mcp_config_diagnostic(
    server: String,
    message: String,
) -> crate::agent_runtime::RuntimeExtensionDiagnostic {
    crate::agent_runtime::RuntimeExtensionDiagnostic {
        source: "mcp".to_owned(),
        server,
        code: "mcp_config_invalid".to_owned(),
        message,
        tool: None,
    }
}

fn canonical_project_root(project_root: &Path) -> Result<PathBuf, String> {
    let metadata =
        fs::metadata(project_root).map_err(|error| format!("无法访问项目目录：{error}"))?;
    if !metadata.is_dir() {
        return Err("项目根必须是目录".to_owned());
    }
    fs::canonicalize(project_root).map_err(|error| format!("无法规范化项目目录：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::AgentRuntimeBuildConfig;
    use crate::native_hooks::{HookDefinition, HookHandler, HookScope, set_trusted, upsert};
    use keencode_agent::HookPhase;
    use keencode_provider::ProviderRegistry;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn clean_project_publishes_first_native_extension_candidate() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        // Windows canonicalize 返回 verbatim 路径，与原生 runner/正式路径发现一致。
        let data_root = fs::canonicalize(storage.path()).unwrap();
        let paths = NativePaths::from_data_root(data_root.clone());
        let runtime = AgentRuntime::build_native(AgentRuntimeBuildConfig {
            storage_root: data_root,
            provider_registry: ProviderRegistry::new(),
            analytics: None,
            default_provider: None,
            memory_service: None,
            local_memories_enabled: false,
            executor_handle: tokio::runtime::Handle::current(),
        })
        .unwrap();
        let refresh_epoch = runtime
            .extension_candidate_refresh_epoch(project.path())
            .unwrap();
        let generation = build_and_publish_candidate_at_epoch(
            &paths,
            &runtime,
            project.path(),
            1,
            refresh_epoch,
        )
        .await
        .expect("干净首次启动必须能发布空扩展候选");
        assert_eq!(generation, 1);
        assert_eq!(
            runtime.extension_generation(project.path()).unwrap(),
            Some(1)
        );
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn external_hook_and_script_changes_reject_cached_candidate() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let data_root = fs::canonicalize(storage.path()).unwrap();
        let project_root = fs::canonicalize(project.path()).unwrap();
        let paths = NativePaths::from_data_root(data_root.clone());
        let runtime = AgentRuntime::build_native(AgentRuntimeBuildConfig {
            storage_root: data_root,
            provider_registry: ProviderRegistry::new(),
            analytics: None,
            default_provider: None,
            memory_service: None,
            local_memories_enabled: false,
            executor_handle: tokio::runtime::Handle::current(),
        })
        .unwrap();

        fs::write(project_root.join("marker.py"), b"one").unwrap();
        let definition = HookDefinition {
            id: "marker".to_owned(),
            name: "Marker".to_owned(),
            scope: HookScope::Project {
                root: project_root.clone(),
            },
            phase: HookPhase::PreToolUse,
            matcher: Some("Bash".to_owned()),
            handler: HookHandler::Command {
                command: "python marker.py".to_owned(),
                args: None,
                shell: None,
                timeout_ms: Some(10_000),
                environment: BTreeMap::new(),
            },
            enabled: true,
        };
        let created = upsert(&paths, definition, 0).unwrap();
        set_trusted(
            &paths,
            &project_root,
            HookScope::Project {
                root: project_root.clone(),
            },
            "marker",
            true,
            created.trust_revision,
        )
        .unwrap();

        let first_epoch = runtime
            .extension_candidate_refresh_epoch(&project_root)
            .unwrap();
        build_and_publish_candidate_at_epoch(&paths, &runtime, &project_root, 1, first_epoch)
            .await
            .unwrap();
        assert!(extension_candidate_matches(&paths, &project_root, 1).unwrap());

        fs::write(project_root.join("marker.py"), b"two").unwrap();
        assert!(!extension_candidate_matches(&paths, &project_root, 1).unwrap());
        runtime
            .invalidate_extension_candidate(&project_root)
            .unwrap();
        let second_epoch = runtime
            .extension_candidate_refresh_epoch(&project_root)
            .unwrap();
        build_and_publish_candidate_at_epoch(&paths, &runtime, &project_root, 2, second_epoch)
            .await
            .unwrap();
        assert!(extension_candidate_matches(&paths, &project_root, 2).unwrap());

        fs::write(
            project_root.join(".keencode").join("hooks.json"),
            b"{invalid",
        )
        .unwrap();
        assert!(!extension_candidate_matches(&paths, &project_root, 2).unwrap());
        runtime.shutdown().await.unwrap();
    }

    #[test]
    fn plugin_registry_and_manifest_changes_reject_cached_input() {
        let storage = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let data_root = fs::canonicalize(storage.path()).unwrap();
        let project_root = fs::canonicalize(project.path()).unwrap();
        let source_root = fs::canonicalize(source.path()).unwrap();
        let paths = NativePaths::from_data_root(data_root.clone());
        fs::create_dir_all(source_root.join(".claude-plugin")).unwrap();
        fs::write(
            source_root.join(crate::plugins::PLUGIN_MANIFEST),
            br#"{"name":"fixture","version":"one"}"#,
        )
        .unwrap();

        let manager = PluginManager::new(data_root);
        let id = crate::plugins::PluginId::parse("fixture@local").unwrap();
        let mut secrets = SystemSecretStore;
        manager
            .install_from_directory(
                crate::plugins::MaterializedPlugin {
                    id: id.clone(),
                    source_root,
                    source: None,
                },
                crate::plugins::UserConfigUpdate::default(),
                &mut secrets,
            )
            .unwrap();

        let first = plugin_input_fingerprint(&paths);
        assert!(first.readable);
        let first_extension = extension_input_fingerprint(&paths, &project_root);
        remember_extension_input(&project_root, 41, first_extension).unwrap();
        assert!(extension_candidate_matches(&paths, &project_root, 41).unwrap());

        manager.set_enabled(&id, false).unwrap();
        let disabled = plugin_input_fingerprint(&paths);
        assert_ne!(first.digest, disabled.digest);
        assert!(!extension_candidate_matches(&paths, &project_root, 41).unwrap());

        manager.set_enabled(&id, true).unwrap();
        let enabled_extension = extension_input_fingerprint(&paths, &project_root);
        remember_extension_input(&project_root, 42, enabled_extension).unwrap();
        let enabled = plugin_input_fingerprint(&paths);
        let installed_path = manager.load_state().unwrap().plugins[0]
            .install_path
            .clone();
        fs::write(
            installed_path.join(crate::plugins::PLUGIN_MANIFEST),
            br#"{"name":"fixture","version":"two"}"#,
        )
        .unwrap();
        let manifest_changed = plugin_input_fingerprint(&paths);
        assert_ne!(enabled.digest, manifest_changed.digest);
        assert!(!extension_candidate_matches(&paths, &project_root, 42).unwrap());
    }
}
