pub(super) use super::client_tools::validate_client_tools;
use super::{client_tools::register_client_tools, extensions};
use rcode_agent::{ToolRegistry, TurnCancellation};
use rcode_runtime::{register_workspace_tools, AgentRuntime, EventBridge};
#[cfg(test)]
use rcode_tools::ToolEnvironment;
use serde_json::json;
use std::{collections::HashSet, path::Path, sync::Arc};
use tauri::AppHandle;

pub(super) fn validate_subagent_tools(
    names: &[String],
    plan_mode: bool,
    clients: &[rcode_model::ToolDefinition],
) -> Result<(), String> {
    if !plan_mode
        || names.len() > 4
        || names.iter().collect::<HashSet<_>>().len() != names.len()
        || names
            .iter()
            .any(|name| !matches!(name.as_str(), "Read" | "Glob" | "Grep" | "list_directory"))
        || clients
            .iter()
            .any(|tool| tool.name != "list_directory" || !names.contains(&tool.name))
    {
        return Err("子智能体工具必须为只读文件工具且不可递归委派".into());
    }
    Ok(())
}

pub(super) async fn prepare_tools(
    app: &AppHandle,
    cwd: Option<&Path>,
    events: Arc<EventBridge>,
    cancellation: &TurnCancellation,
    state: AgentRuntime,
    request: &super::NativeAgentRequest,
) -> Result<(ToolRegistry, Vec<rcode_mcp::McpClient>), String> {
    let mut clients = Vec::new();
    let run_id = request.run_id.as_str();
    let session_id = request.session_id.as_str();
    let client_tools = &request.client_tools;
    let prepared = async {
        let mut registry = ToolRegistry::new();
        register_client_tools(&mut registry, client_tools, run_id, state.clone(), events.clone())?;
        let mut servers = Vec::new();
        if let Some(root) = cwd {
            let artifacts = crate::modules::storage::directory("outputs")?;
            let env = state.environment(session_id, root, Some(&artifacts))?;
            register_workspace_tools(&mut registry, env, root, request.plan_mode)?;
            if let Some(names) = &request.subagent_tools {
                return registry.select_exact(names).map_err(|error| error.to_string());
            }
            let manager = rcode_plugins::PluginManager::new(extensions::data_root(app)?);
            let snapshot = manager.runtime_snapshot(root, &Default::default(), &extensions::extension_secrets(app)).map_err(|e| e.to_string())?;
            let commands = rcode_plugins::PluginCommandCatalog::from_snapshot(&snapshot).map_err(|e| e.to_string())?;
            if !commands.is_empty() { registry.register(Arc::new(rcode_plugins::PluginCommandTool::new(Arc::new(commands)))).map_err(|e| e.to_string())?; }
            let skill_config = super::resources::skill_config(extensions::data_root(app)?, Some(root), &snapshot)?;
            for plugin in &snapshot.plugins {
                for (id, server) in &plugin.mcp_servers {
                    servers.push((format!("{}-{}-{id}", plugin.id.plugin, plugin.id.marketplace.as_deref().unwrap_or("local")), extensions::plugin_connection(server, cwd)?));
                }
            }
            let catalog = rcode_skills::discover_skills(&skill_config).map_err(|e| e.to_string())?;
            for diagnostic in catalog.diagnostics() { events.emit(json!({"type":"diagnostic", "message":diagnostic.message}))?; }
            if !catalog.entries().is_empty() {
                registry.register(Arc::new(rcode_tools::SkillTool::new(Arc::new(catalog)))).map_err(|e| e.to_string())?;
            }
        }
        servers.extend(extensions::load_config(app)?.mcp_servers.into_iter()
            .filter(|server| server.enabled).map(|server| Ok((server.id.clone(), server.connection(app, cwd)?)))
            .collect::<Result<Vec<_>, String>>()?);
        if servers.len() > 64 { return Err("启用的 MCP Server 总数超过 64".into()); }
        for (id, mut config) in servers {
            if cancellation.is_cancelled() { break; }
            if let rcode_mcp::McpServerConfig::StreamableHttp(http) = &mut config {
                http.http_client = Some(super::super::net::agent_http_client(&http.endpoint, true).await?);
            }
            let report = tokio::select! {
                _ = cancellation.cancelled() => break,
                report = rcode_tools::prepare_mcp_server_tools(&id, config, rcode_mcp::McpClientOptions::default()) => report,
            };
            let (tools, diagnostics, client) = report.into_parts();
            if let Some(client) = client { clients.push(client); }
            for diagnostic in diagnostics { events.emit(json!({"type":"diagnostic", "message":diagnostic.message}))?; }
            for tool in tools { registry.register(tool).map_err(|e| e.to_string())?; }
        }
        Ok(registry)
    }.await;
    match prepared {
        Ok(registry) => Ok((registry, clients)),
        Err(error) => {
            for client in clients {
                let _ = client.close().await;
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcode_model::ToolDefinition;

    #[test]
    fn subagent_selection_cannot_expand_permissions_or_delegate() {
        for names in [
            vec!["Write"],
            vec!["Bash"],
            vec!["run_subagent"],
            vec!["Skill"],
            vec!["Grep", "Grep"],
        ] {
            let names: Vec<String> = names.into_iter().map(String::from).collect();
            assert!(validate_subagent_tools(&names, true, &[]).is_err());
        }
        assert!(validate_subagent_tools(&[], false, &[]).is_err());
        assert!(validate_subagent_tools(&[], true, &[]).is_ok());
        let client = ToolDefinition::new("run_subagent", "delegate", json!({"type":"object"}));
        assert!(validate_subagent_tools(&[], true, &[client]).is_err());
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let env = Arc::new(ToolEnvironment::new(&root).unwrap());
        let mut registry = ToolRegistry::new();
        register_workspace_tools(&mut registry, env, &root, true).unwrap();
        let selected = registry.select_exact(&["Grep".into()]).unwrap();
        assert_eq!(
            selected
                .definitions()
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["Grep"]
        );
        assert!(selected.select_exact(&["Read".into()]).is_err());
    }
}
