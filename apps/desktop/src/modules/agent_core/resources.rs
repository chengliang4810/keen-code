use super::extensions;
use rcode_plugins::PluginRuntimeSnapshot;
use rcode_skills::{SkillDiscoveryConfig, SkillRoot, SkillSource};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};

const RESOURCE_LIMIT: usize = 500;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentResource {
    name: String,
    description: String,
    source: String,
    path: Option<String>,
    enabled: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentResources {
    skills: Vec<AgentResource>,
    commands: Vec<AgentResource>,
    hooks: Vec<AgentResource>,
    diagnostics: Vec<String>,
    truncated: bool,
}

pub(super) fn skill_config(
    data: PathBuf,
    root: Option<&Path>,
    snapshot: &PluginRuntimeSnapshot,
) -> Result<SkillDiscoveryConfig, String> {
    let mut config = match root {
        Some(root) => SkillDiscoveryConfig::new(data, root),
        None => SkillDiscoveryConfig::for_user(data),
    };
    for plugin in &snapshot.plugins {
        for skill in &plugin.skills {
            config.additional_roots.push(SkillRoot {
                plugin_root: Some(plugin.root.clone()),
                namespace: Some(plugin.id.plugin.clone()),
                path: skill
                    .path
                    .parent()
                    .ok_or("插件 Skill 缺少父目录")?
                    .to_path_buf(),
                source: SkillSource::Plugin,
                recursive: false,
            });
        }
    }
    Ok(config)
}

/// 只投影事件名，hooks 的命令、环境变量及插值后的敏感配置不得离开 Rust。
fn hook_resources(snapshot: &PluginRuntimeSnapshot) -> Vec<AgentResource> {
    snapshot
        .plugins
        .iter()
        .filter_map(|plugin| {
            let object = plugin.hooks.as_ref()?.as_object()?;
            let events = object
                .get("hooks")
                .and_then(|v| v.as_object())
                .unwrap_or(object);
            let description = events
                .keys()
                .take(32)
                .map(|key| key.chars().take(128).collect::<String>())
                .collect::<Vec<_>>()
                .join(", ");
            Some(AgentResource {
                name: plugin.id.plugin.clone(),
                description,
                source: "plugin".into(),
                path: None,
                enabled: false,
            })
        })
        .collect()
}

#[tauri::command]
pub async fn agent_resources_list(
    app: AppHandle,
    cwd: Option<String>,
) -> Result<AgentResources, String> {
    let root = match cwd {
        Some(cwd) => Some(
            super::super::workspace::authorize_spawn_cwd(
                &app.state(),
                Some(&cwd),
                &super::super::workspace::WorkspaceEnv::Local,
            )?
            .ok_or("请先选择项目")?,
        ),
        None => None,
    };
    let data = extensions::data_root(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let snapshot = rcode_plugins::PluginManager::new(&data)
            .runtime_snapshot(
                root.as_deref()
                    .unwrap_or(data.parent().ok_or("User home directory is unavailable.")?),
                &Default::default(),
                &extensions::extension_secrets(&app),
            )
            .map_err(|e| e.to_string())?;
        list_resources(&data, root.as_deref(), &snapshot)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn list_resources(
    data: &Path,
    root: Option<&Path>,
    snapshot: &PluginRuntimeSnapshot,
) -> Result<AgentResources, String> {
    let custom = super::commands::list_at(
        data.parent().ok_or("User home directory is unavailable.")?,
        root,
    )?;
    let catalog = rcode_skills::discover_skills(&skill_config(data.to_path_buf(), root, snapshot)?)
        .map_err(|e| e.to_string())?;
    let commands =
        rcode_plugins::PluginCommandCatalog::from_snapshot(snapshot).map_err(|e| e.to_string())?;
    let mut hooks = hook_resources(snapshot);
    let command_count = commands.entries().count();
    let truncated = catalog.entries().len() > RESOURCE_LIMIT
        || command_count + custom.commands.len() > RESOURCE_LIMIT
        || hooks.len() > RESOURCE_LIMIT;
    hooks.truncate(RESOURCE_LIMIT);
    Ok(AgentResources {
        skills: catalog
            .entries()
            .iter()
            .take(RESOURCE_LIMIT)
            .map(|entry| AgentResource {
                name: entry.name.clone(),
                description: entry.description.clone(),
                source: match entry.source {
                    SkillSource::Project => "project",
                    SkillSource::Data => "user",
                    SkillSource::Plugin => "plugin",
                }
                .into(),
                path: catalog
                    .source_path(&entry.name)
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned()),
                enabled: entry.enabled,
            })
            .collect(),
        commands: custom
            .commands
            .into_iter()
            .map(|entry| AgentResource {
                name: entry.name,
                description: entry.description,
                source: match entry.scope {
                    super::commands::CommandScope::User => "user",
                    super::commands::CommandScope::Project => "project",
                }
                .into(),
                path: Some(entry.path),
                enabled: entry.enabled,
            })
            .chain(commands.entries().map(|entry| {
                AgentResource {
                    name: entry.name.clone(),
                    description: rcode_plugins::plugin_command_description(
                        &entry.root,
                        &entry.path,
                    )
                    .unwrap_or_default(),
                    source: "plugin".into(),
                    path: Some(entry.path.to_string_lossy().into_owned()),
                    enabled: true,
                }
            }))
            .take(RESOURCE_LIMIT)
            .collect(),
        hooks,
        diagnostics: custom
            .diagnostics
            .into_iter()
            .chain(
                catalog
                    .diagnostics()
                    .iter()
                    .take(32)
                    .map(|d| d.message.clone()),
            )
            .take(32)
            .collect(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::ipc::{CommandArg, CommandItem, InvokeError};
    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};

    fn decode_resource_cwd<'de, Cwd, Output>(
        item: CommandItem<'de, MockRuntime>,
        _command: fn(AppHandle, Cwd) -> Output,
    ) -> Result<Cwd, InvokeError>
    where
        Cwd: CommandArg<'de, MockRuntime>,
    {
        Cwd::from_command(item)
    }

    fn invoke_resource_cwd(body: serde_json::Value) -> Result<Option<String>, serde_json::Value> {
        let app = mock_builder()
            .invoke_handler(|invoke| {
                if invoke.message.command() != "agent_resources_list" {
                    return false;
                }
                let cwd = decode_resource_cwd(
                    CommandItem {
                        plugin: None,
                        name: "agent_resources_list",
                        key: "cwd",
                        message: &invoke.message,
                        acl: &invoke.acl,
                    },
                    agent_resources_list,
                );
                invoke.resolver.respond(cwd);
                true
            })
            .build(mock_context(noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "agent_resources_list".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(any(windows, target_os = "android")) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|response| response.deserialize().unwrap())
    }

    #[test]
    fn resources_ipc_accepts_null_or_omitted_cwd() {
        for body in [serde_json::json!({ "cwd": null }), serde_json::json!({})] {
            assert_eq!(invoke_resource_cwd(body), Ok(None));
        }
    }

    #[test]
    fn resources_ipc_project_cwd_requires_workspace_authorization() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().to_string_lossy().into_owned();
        let cwd = invoke_resource_cwd(serde_json::json!({ "cwd": path })).unwrap();
        assert_eq!(cwd.as_deref(), Some(path.as_str()));
        let registry = crate::modules::workspace::WorkspaceRegistry::default();
        let resolve = || {
            crate::modules::workspace::authorize_spawn_cwd(
                &registry,
                cwd.as_deref(),
                &crate::modules::workspace::WorkspaceEnv::Local,
            )
        };
        assert!(resolve()
            .unwrap_err()
            .contains("cwd is outside the authorized workspace"));
        let authorized = registry.authorize(project.path()).unwrap();
        assert_eq!(resolve().unwrap(), Some(authorized));
    }

    #[test]
    fn resources_ipc_rejects_non_string_cwd() {
        for cwd in [
            serde_json::json!(false),
            serde_json::json!(42),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let error = invoke_resource_cwd(serde_json::json!({ "cwd": cwd })).unwrap_err();
            let message = error.as_str().unwrap();
            assert!(message.contains("invalid args `cwd` for command `agent_resources_list`"));
            assert!(message.contains("expected a string"));
        }
    }

    #[test]
    fn user_resources_exclude_project_files_and_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let data = home.join(".rcode");
        let project = home.join("project");
        let skill = "---\nname: example\ndescription: An example skill\n---\nInstructions\n";
        for root in [
            &data.join("skills/example"),
            &project.join(".agents/skills/example"),
        ] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("SKILL.md"), skill).unwrap();
        }
        for root in [&data.join("commands"), &project.join(".rcode/commands")] {
            std::fs::create_dir_all(root).unwrap();
            std::fs::write(root.join("review.md"), "Review code").unwrap();
        }
        std::fs::write(project.join(".rcode/commands/broken.md"), "---\ninvalid").unwrap();
        std::fs::write(home.join(".agents"), "Not a skills root").unwrap();
        std::fs::write(data.join(".agents"), "Not a skills root").unwrap();
        let snapshot = PluginRuntimeSnapshot { plugins: vec![] };

        let global = list_resources(&data, None, &snapshot).unwrap();
        assert!(global.diagnostics.is_empty());
        assert_eq!(global.skills.len(), 1);
        assert_eq!(global.skills[0].source, "user");
        assert_eq!(global.commands.len(), 1);
        assert_eq!(global.commands[0].source, "user");

        let runtime = list_resources(&data, Some(&project), &snapshot).unwrap();
        assert_eq!(runtime.skills[0].source, "project");
        assert_eq!(runtime.commands.len(), 2);
        assert!(!runtime.diagnostics.is_empty());
        std::fs::remove_dir_all(&project).unwrap();
        let global = list_resources(&data, None, &snapshot).unwrap();
        assert!(global.diagnostics.is_empty());
        assert_eq!(global.skills[0].source, "user");
        assert_eq!(global.commands.len(), 1);
    }

    #[test]
    fn empty_user_resources_do_not_create_directories() {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().canonicalize().unwrap().join(".rcode");
        let resources =
            list_resources(&data, None, &PluginRuntimeSnapshot { plugins: vec![] }).unwrap();
        assert!(resources.skills.is_empty());
        assert!(resources.commands.is_empty());
        assert!(resources.hooks.is_empty());
        assert!(resources.diagnostics.is_empty());
        assert!(!data.exists());
    }

    #[test]
    fn user_resources_include_installed_plugin_components_without_a_project() {
        let home = tempfile::tempdir().unwrap();
        let data = home.path().canonicalize().unwrap().join(".rcode");
        let root = data.join("plugins/example");
        let skill = root.join("skills/reviewer/SKILL.md");
        let command = root.join("commands/review.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::create_dir_all(command.parent().unwrap()).unwrap();
        std::fs::write(
            &skill,
            "---\nname: reviewer\ndescription: Review code\n---\nInstructions\n",
        )
        .unwrap();
        std::fs::write(&command, "Review code").unwrap();
        let snapshot = PluginRuntimeSnapshot {
            plugins: vec![rcode_plugins::RuntimePlugin {
                id: rcode_plugins::PluginId {
                    plugin: "example".into(),
                    marketplace: Some("local".into()),
                },
                root,
                commands: vec![rcode_plugins::ComponentFile {
                    path: command,
                    relative_path: "commands/review.md".into(),
                }],
                skills: vec![rcode_plugins::ComponentFile {
                    path: skill,
                    relative_path: "skills/reviewer/SKILL.md".into(),
                }],
                agents: vec![],
                hook_environment: [("API_KEY".into(), "private-value".into())].into(),
                hooks: Some(
                    serde_json::json!({"hooks":{"Stop":[{"command":"echo private-value"}]}}),
                ),
                mcp_servers: Default::default(),
                lsp_servers: vec![],
            }],
        };

        let resources = list_resources(&data, None, &snapshot).unwrap();
        assert!(resources.diagnostics.is_empty());
        assert_eq!(resources.skills.len(), 1);
        assert_eq!(resources.skills[0].source, "plugin");
        assert_eq!(resources.commands.len(), 1);
        assert_eq!(resources.commands[0].source, "plugin");
        assert_eq!(resources.hooks.len(), 1);
        assert_eq!(resources.hooks[0].description, "Stop");
        assert!(!resources.hooks[0].enabled);
        assert!(!serde_json::to_string(&resources)
            .unwrap()
            .contains("private-value"));
    }

    #[test]
    fn user_skills_use_the_rcode_root_with_project_precedence() {
        let home = tempfile::tempdir().unwrap();
        let data = rcode_control_protocol::paths::user_root(home.path());
        let project = home.path().join("project");
        let user_skill = data.join("skills/example");
        let project_skill = project.join(".agents/skills/example");
        std::fs::create_dir_all(&user_skill).unwrap();
        std::fs::create_dir_all(&project_skill).unwrap();
        for directory in [&user_skill, &project_skill] {
            std::fs::write(
                directory.join("SKILL.md"),
                "---\nname: example\ndescription: An example skill\n---\nInstructions\n",
            )
            .unwrap();
        }
        let config = skill_config(
            data,
            Some(&project),
            &PluginRuntimeSnapshot { plugins: vec![] },
        )
        .unwrap();
        let catalog = rcode_skills::discover_skills(&config).unwrap();
        assert_eq!(catalog.entries().len(), 1);
        assert_eq!(catalog.entries()[0].source, SkillSource::Project);
        std::fs::remove_file(project_skill.join("SKILL.md")).unwrap();
        let catalog = rcode_skills::discover_skills(&config).unwrap();
        assert_eq!(catalog.entries()[0].source, SkillSource::Data);
        assert_eq!(
            catalog.source_path("example").unwrap(),
            user_skill.join("SKILL.md").canonicalize().unwrap()
        );
    }
    #[test]
    fn hook_projection_never_exposes_commands_or_secrets() {
        let plugin = rcode_plugins::RuntimePlugin {
            id: rcode_plugins::PluginId {
                plugin: "example".into(),
                marketplace: None,
            },
            root: PathBuf::from("plugin"),
            commands: vec![],
            skills: vec![],
            agents: vec![],
            hook_environment: [("API_KEY".into(), "private-value".into())].into(),
            hooks: Some(serde_json::json!({"hooks":{"Stop":[{"command":"echo private-value"}]}})),
            mcp_servers: Default::default(),
            lsp_servers: vec![],
        };
        let resources = hook_resources(&PluginRuntimeSnapshot {
            plugins: vec![plugin],
        });
        let output = serde_json::to_string(&resources).unwrap();
        assert!(output.contains("Stop"));
        assert!(!output.contains("private-value"));
        assert!(!resources[0].enabled);
    }
}
