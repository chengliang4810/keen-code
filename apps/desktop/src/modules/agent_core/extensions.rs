use rcode_mcp::{McpServerConfig, StdioServerConfig, StreamableHttpConfig};
use rcode_plugins::{
    MaterializedPlugin, PluginId, PluginManager, PluginState, SecretStore, UserConfigUpdate,
};
use rcode_runtime::security::deny_secret_path;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Manager};

pub(super) static EXTENSION_WRITES: Mutex<()> = Mutex::new(());
const MAX_CONFIG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpServerEntry {
    pub id: String,
    pub enabled: bool,
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: String,
}

impl McpServerEntry {
    fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        if self.args.len() > 128
            || self
                .args
                .iter()
                .any(|arg| arg.len() > 8192 || arg.contains('\0'))
        {
            return Err("MCP 参数超出上限或包含无效字符".into());
        }
        match self.transport.as_str() {
            "stdio"
                if !self.command.trim().is_empty()
                    && self.command.len() <= 4096
                    && !self.command.contains('\0') =>
            {
                Ok(())
            }
            "http" => {
                super::validate_network_endpoint(&self.url)?;
                Ok(())
            }
            _ => Err("MCP 传输必须为 stdio 或 http，并提供对应命令或地址".into()),
        }
    }

    pub(super) fn connection(
        &self,
        app: &AppHandle,
        cwd: Option<&Path>,
    ) -> Result<McpServerConfig, String> {
        self.validate()?;
        let secrets = extension_secrets(app);
        let stored = secrets
            .get_json(&format!("mcp:{}", self.id))
            .map_err(|e| e.to_string())?;
        if self.transport == "stdio" {
            let mut config = StdioServerConfig::new(&self.command);
            config.args = self.args.clone();
            config.current_dir = cwd.map(Path::to_path_buf);
            if let Some(env) = stored
                .as_ref()
                .and_then(|v| v.get("environment"))
                .and_then(Value::as_object)
            {
                for (name, value) in env {
                    config.environment.insert(
                        name.clone(),
                        value.as_str().ok_or("MCP 环境变量必须为文本")?.into(),
                    );
                }
            }
            Ok(McpServerConfig::Stdio(config))
        } else {
            let mut config = StreamableHttpConfig::new(&self.url);
            if let Some(token) = stored
                .as_ref()
                .and_then(|v| v["token"].as_str())
                .filter(|v| !v.is_empty())
            {
                config
                    .headers
                    .insert("Authorization".into(), format!("Bearer {token}"));
            }
            Ok(McpServerConfig::StreamableHttp(config))
        }
    }
}

// 插件运行时已完成变量插值，保留 env 与 headers，绝不能投影成会丢失凭据的公开配置。
pub(super) fn plugin_connection(
    value: &Value,
    cwd: Option<&Path>,
) -> Result<McpServerConfig, String> {
    let object = value.as_object().ok_or("插件 MCP 配置必须为对象")?;
    let strings = |key: &str| -> Result<BTreeMap<String, String>, String> {
        object
            .get(key)
            .map(|v| {
                serde_json::from_value(v.clone())
                    .map_err(|_| format!("插件 MCP {key} 必须为文本映射"))
            })
            .transpose()
            .map(|v| v.unwrap_or_default())
    };
    if let Some(url) = object.get("url").and_then(Value::as_str) {
        super::validate_network_endpoint(url)?;
        let mut config = StreamableHttpConfig::new(url);
        config.headers = strings("headers")?;
        Ok(McpServerConfig::StreamableHttp(config))
    } else {
        let command = object
            .get("command")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or("插件 MCP 缺少命令或 URL")?;
        let mut config = StdioServerConfig::new(command);
        config.args = object
            .get("args")
            .map(|v| serde_json::from_value(v.clone()).map_err(|_| "插件 MCP args 必须为文本数组"))
            .transpose()?
            .unwrap_or_default();
        config.environment = strings("env")?;
        config.current_dir = cwd.map(Path::to_path_buf);
        Ok(McpServerConfig::Stdio(config))
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionConfig {
    pub mcp_servers: Vec<McpServerEntry>,
}

pub(super) fn data_root(_app: &AppHandle) -> Result<PathBuf, String> {
    crate::modules::storage::root()
}

pub(super) fn load_config(_app: &AppHandle) -> Result<ExtensionConfig, String> {
    let path = crate::modules::storage::path("mcp/servers.json")?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExtensionConfig::default());
        }
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() > MAX_CONFIG_BYTES as u64 =>
        {
            return Err("扩展配置不是普通文件或超出大小上限".into());
        }
        Err(error) => return Err(error.to_string()),
        _ => {}
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let config: ExtensionConfig = serde_json::from_slice(&bytes).map_err(|_| "扩展配置格式无效")?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("扩展 ID 只能包含字母、数字、横线和下划线，最多 64 字符".into());
    }
    Ok(())
}

fn validate_config(config: &ExtensionConfig) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    if config.mcp_servers.len() > 32 {
        return Err("MCP Server 数量超过上限 32".into());
    }
    for server in &config.mcp_servers {
        server.validate()?;
        if !ids.insert(&server.id) {
            return Err("MCP Server ID 重复".into());
        }
    }
    Ok(())
}

pub(super) struct ExtensionSecrets {
    app: AppHandle,
}

pub(super) fn extension_secrets(app: &AppHandle) -> ExtensionSecrets {
    ExtensionSecrets { app: app.clone() }
}

impl SecretStore for ExtensionSecrets {
    fn set_json(&mut self, key: &str, value: &Value) -> rcode_plugins::Result<()> {
        super::super::secrets::write_secret(
            &self.app,
            &self.app.state(),
            "rcode-ai",
            &format!("extension:{key}"),
            &value.to_string(),
        )
        .map_err(rcode_plugins::PluginError::Invalid)
    }
    fn get_json(&self, key: &str) -> rcode_plugins::Result<Option<Value>> {
        let value = super::super::secrets::read_secret(
            &self.app,
            &self.app.state(),
            "rcode-ai",
            &format!("extension:{key}"),
        )
        .map_err(rcode_plugins::PluginError::Invalid)?;
        value
            .map(|text| serde_json::from_str(&text).map_err(rcode_plugins::PluginError::Json))
            .transpose()
    }
    fn delete(&mut self, key: &str) -> rcode_plugins::Result<()> {
        super::super::secrets::remove_secret(
            &self.app,
            &self.app.state(),
            "rcode-ai",
            &format!("extension:{key}"),
        )
        .map_err(rcode_plugins::PluginError::Invalid)
    }
}

#[tauri::command]
pub async fn agent_extensions_get(app: AppHandle) -> Result<ExtensionConfig, String> {
    tauri::async_runtime::spawn_blocking(move || load_config(&app))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_extensions_save(app: AppHandle, config: ExtensionConfig) -> Result<(), String> {
    validate_config(&config)?;
    let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err("扩展配置超过大小上限".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        let previous = load_config(&app)?;
        atomic_write(&crate::modules::storage::path("mcp/servers.json")?, &bytes)?;
        for server in previous.mcp_servers {
            if !config.mcp_servers.iter().any(|entry| entry.id == server.id) {
                extension_secrets(&app)
                    .delete(&format!("mcp:{}", server.id))
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("配置路径缺少父目录")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_symlink() || !meta.is_file())
    {
        return Err("配置目标不是普通文件".into());
    }
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.error.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn agent_mcp_set_secrets(
    app: AppHandle,
    server_id: String,
    token: Option<String>,
    environment: Option<BTreeMap<String, String>>,
) -> Result<(), String> {
    validate_id(&server_id)?;
    if token
        .as_ref()
        .is_some_and(|v| v.len() > 16_384 || v.contains(['\r', '\n', '\0']))
        || environment.as_ref().is_some_and(|env| {
            env.len() > 64
                || env.iter().any(|(k, v)| {
                    k.is_empty()
                        || k.len() > 256
                        || k.contains(['=', '\0'])
                        || v.contains('\0')
                        || v.len() > 16_384
                })
        })
    {
        return Err("MCP 凭据或环境变量无效".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        if !load_config(&app)?
            .mcp_servers
            .iter()
            .any(|server| server.id == server_id)
        {
            return Err("MCP Server 尚未保存".into());
        }
        let mut secrets = extension_secrets(&app);
        let key = format!("mcp:{server_id}");
        let mut stored = secrets
            .get_json(&key)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(token) = token {
            stored["token"] = Value::String(token);
        }
        if let Some(environment) = environment {
            stored["environment"] = serde_json::to_value(environment).map_err(|e| e.to_string())?;
        }
        secrets.set_json(&key, &stored).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_plugins_list(app: AppHandle) -> Result<PluginState, String> {
    tauri::async_runtime::spawn_blocking(move || {
        PluginManager::new(data_root(&app)?)
            .load_state()
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_plugins_install(
    app: AppHandle,
    source_root: String,
    plugin_id: String,
    values: Option<BTreeMap<String, Value>>,
) -> Result<(), String> {
    deny_secret_path(Path::new(&source_root))?;
    let canonical = std::fs::canonicalize(&source_root).map_err(|e| e.to_string())?;
    deny_secret_path(&canonical)?;
    if !app
        .state::<super::super::workspace::WorkspaceRegistry>()
        .is_authorized(&canonical)
    {
        return Err("请先选择并授权插件来源目录".into());
    }
    let id = PluginId::parse(&plugin_id).map_err(|e| e.to_string())?;
    let values = values.unwrap_or_default();
    if !values.is_empty() {
        return Err("请安装后单独保存插件配置".into());
    }
    if serde_json::to_vec(&values)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_CONFIG_BYTES
    {
        return Err("插件配置超过大小上限".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        super::marketplace::validate_plugin_tree(&canonical)?;
        let _guard = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        PluginManager::new(data_root(&app)?)
            .install_from_directories(
                vec![MaterializedPlugin {
                    id,
                    source_root: canonical.clone(),
                    source: Some(rcode_plugins::PluginInstallSource::Local {
                        path: canonical.clone(),
                    }),
                }],
                UserConfigUpdate {
                    values,
                    replace: false,
                },
                &mut extension_secrets(&app),
            )
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_plugins_enable(
    app: AppHandle,
    plugin_id: String,
    enabled: bool,
) -> Result<(), String> {
    let id = PluginId::parse(&plugin_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        PluginManager::new(data_root(&app)?)
            .set_enabled(&id, enabled)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_plugins_uninstall(app: AppHandle, plugin_id: String) -> Result<(), String> {
    let id = PluginId::parse(&plugin_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        PluginManager::new(data_root(&app)?)
            .uninstall(&id, &mut extension_secrets(&app))
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn agent_plugins_configure(
    app: AppHandle,
    plugin_id: String,
    values: BTreeMap<String, Value>,
) -> Result<(), String> {
    if serde_json::to_vec(&values)
        .map_err(|e| e.to_string())?
        .len()
        > MAX_CONFIG_BYTES
    {
        return Err("插件配置超过大小上限".into());
    }
    let id = PluginId::parse(&plugin_id).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = EXTENSION_WRITES.lock().map_err(|_| "扩展配置锁不可用")?;
        PluginManager::new(data_root(&app)?)
            .update_user_config(
                &id,
                UserConfigUpdate {
                    values,
                    replace: false,
                },
                &mut extension_secrets(&app),
            )
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn public_config_rejects_secrets_and_duplicate_ids() {
        assert!(serde_json::from_value::<ExtensionConfig>(json!({"mcpServers":[{"id":"x","enabled":true,"transport":"stdio","command":"server","token":"secret"}]})).is_err());
        let config = ExtensionConfig {
            mcp_servers: vec![
                McpServerEntry {
                    id: "x".into(),
                    enabled: true,
                    transport: "stdio".into(),
                    command: "server".into(),
                    args: vec![],
                    url: String::new()
                };
                2
            ],
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn plugin_mcp_keeps_interpolated_headers_and_environment() {
        let McpServerConfig::Stdio(stdio) = plugin_connection(
            &json!({"command":"server","args":["--stdio"],"env":{"TOKEN":"value"}}),
            None,
        )
        .unwrap() else {
            panic!("expected stdio")
        };
        assert_eq!(stdio.environment["TOKEN"], "value");
        let McpServerConfig::StreamableHttp(http) = plugin_connection(
            &json!({"url":"https://example.com/mcp","headers":{"X-API-Key":"value"}}),
            None,
        )
        .unwrap() else {
            panic!("expected HTTP")
        };
        assert_eq!(http.headers["X-API-Key"], "value");
        assert!(!format!("{http:?}").contains("value"));
        assert!(plugin_connection(&json!({"command":"server","env":{"TOKEN":42}}), None).is_err());
    }
}
