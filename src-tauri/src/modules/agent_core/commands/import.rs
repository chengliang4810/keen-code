use super::files::{self, CommandScope, Directory, ENTRY_LIMIT, MAX_COMMANDS};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

struct Source {
    id: &'static str,
    label: &'static str,
    project: &'static str,
    user: &'static str,
}

const SOURCES: &[Source] = &[
    Source {
        id: "claudeCode",
        label: "Claude Code",
        project: ".claude/commands",
        user: ".claude/commands",
    },
    Source {
        id: "codexCli",
        label: "Codex CLI",
        project: ".codex/commands",
        user: ".codex/commands",
    },
    Source {
        id: "openCode",
        label: "OpenCode",
        project: ".opencode/commands",
        user: ".config/opencode/commands",
    },
    Source {
        id: "openClaw",
        label: "OpenClaw",
        project: "commands",
        user: ".openclaw/commands",
    },
    Source {
        id: "augment",
        label: "Augment",
        project: ".augment/commands",
        user: ".augment/commands",
    },
    Source {
        id: "continue",
        label: "Continue",
        project: ".continue/commands",
        user: ".continue/commands",
    },
    Source {
        id: "goose",
        label: "Goose",
        project: ".goose/commands",
        user: ".config/goose/commands",
    },
    Source {
        id: "qwenCode",
        label: "Qwen Code",
        project: ".qwen/commands",
        user: ".qwen/commands",
    },
    Source {
        id: "qode",
        label: "Qoder",
        project: ".qoder/commands",
        user: ".qoder/commands",
    },
    Source {
        id: "qodeCn",
        label: "Qoder CN",
        project: ".qoder/commands",
        user: ".qoder-cn/commands",
    },
    Source {
        id: "windsurf",
        label: "Windsurf",
        project: ".windsurf/commands",
        user: ".codeium/windsurf/commands",
    },
    Source {
        id: "trae",
        label: "Trae",
        project: ".trae/commands",
        user: ".trae/commands",
    },
    Source {
        id: "kiroCli",
        label: "Kiro CLI",
        project: ".kiro/commands",
        user: ".kiro/commands",
    },
    Source {
        id: "roo",
        label: "Roo Code",
        project: ".roo/commands",
        user: ".roo/commands",
    },
    Source {
        id: "codeBuddy",
        label: "CodeBuddy",
        project: ".codebuddy/commands",
        user: ".codebuddy/commands",
    },
];

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportSelection {
    agent: String,
    scope: CommandScope,
    relative_path: String,
    expected_content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalCommand {
    pub name: String,
    pub description: String,
    pub agent_label: String,
    pub path: String,
    pub selection: ImportSelection,
}

#[derive(Default, Serialize)]
pub struct ImportCatalog {
    pub commands: Vec<ExternalCommand>,
    pub diagnostics: Vec<String>,
}

#[derive(Serialize)]
pub struct ImportResult {
    id: String,
    name: String,
    status: &'static str,
    error: Option<String>,
}

fn source_root<'a>(
    home: &'a Path,
    project: Option<&'a Path>,
    agent: &str,
    scope: CommandScope,
) -> Result<(&'a Path, PathBuf), String> {
    let source = SOURCES
        .iter()
        .find(|source| source.id == agent)
        .ok_or("Invalid command import source.")?;
    let (boundary, relative) = match scope {
        CommandScope::User => (home, source.user),
        CommandScope::Project => (project.ok_or("Select a project.")?, source.project),
    };
    let root = crate::modules::storage::path_at(boundary, relative)?;
    super::super::security::check_file_path(boundary, &root.to_string_lossy())?;
    Ok((boundary, root))
}

fn command_name(relative: &str) -> Result<String, String> {
    if relative.contains(['\\', ':', '\0']) {
        return Err("Invalid command import path.".into());
    }
    let path = Path::new(relative);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
        || path.extension().is_none_or(|extension| extension != "md")
    {
        return Err("Invalid command import path.".into());
    }
    let name = relative
        .strip_suffix(".md")
        .ok_or("Invalid command import path.")?
        .replace('/', "-");
    files::validate_name(&name)?;
    Ok(name)
}

pub(super) fn discover(home: &Path, project: Option<&Path>) -> ImportCatalog {
    let mut catalog = ImportCatalog::default();
    let mut scanned = 0usize;
    for source in SOURCES {
        for scope in [CommandScope::User, CommandScope::Project] {
            if scope == CommandScope::Project && project.is_none() {
                continue;
            }
            if source.id == "qodeCn" && scope == CommandScope::Project {
                continue;
            }
            let result: Result<(), String> = (|| {
                let (boundary, root) = source_root(home, project, source.id, scope)?;
                let mut directories = vec![(root.clone(), 0usize)];
                while let Some((directory, depth)) = directories.pop() {
                    let entries = match fs::read_dir(&directory) {
                        Ok(entries) => entries,
                        Err(e) if e.kind() == ErrorKind::NotFound => continue,
                        Err(e) => return Err(e.to_string()),
                    };
                    for entry in entries {
                        scanned += 1;
                        if scanned > ENTRY_LIMIT {
                            return Err("Command import exceeds the entry limit.".into());
                        }
                        let path = entry.map_err(|e| e.to_string())?.path();
                        crate::modules::storage::reject_link(&path)?;
                        super::super::security::check_file_path(boundary, &path.to_string_lossy())?;
                        let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
                        if metadata.is_dir() && depth < 4 {
                            directories.push((path, depth + 1));
                            continue;
                        }
                        if !metadata.is_file()
                            || path.extension().is_none_or(|extension| extension != "md")
                        {
                            continue;
                        }
                        if catalog.commands.len() >= MAX_COMMANDS {
                            return Err("Too many commands to import.".into());
                        }
                        let relative_path = path
                            .strip_prefix(&root)
                            .map_err(|e| e.to_string())?
                            .to_string_lossy()
                            .replace('\\', "/");
                        let result: Result<ExternalCommand, String> = (|| {
                            let name = command_name(&relative_path)?;
                            let content = files::read_text(boundary, &path)?
                                .ok_or("Command file no longer exists.")?;
                            let config = files::parse(&name, &content)?;
                            Ok(ExternalCommand {
                                name,
                                description: config.description,
                                agent_label: source.label.to_owned(),
                                path: path.to_string_lossy().into_owned(),
                                selection: ImportSelection {
                                    agent: source.id.into(),
                                    scope,
                                    relative_path,
                                    expected_content: content,
                                },
                            })
                        })();
                        match result {
                            Ok(command) => catalog.commands.push(command),
                            Err(error) => {
                                if catalog.diagnostics.len() < 32 {
                                    catalog
                                        .diagnostics
                                        .push(format!("{}: {error}", path.display()));
                                }
                            }
                        }
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                if catalog.diagnostics.len() < 32 {
                    catalog
                        .diagnostics
                        .push(format!("{}: {error}", source.label));
                }
            }
            if scanned > ENTRY_LIMIT || catalog.commands.len() >= MAX_COMMANDS {
                return catalog;
            }
        }
    }
    catalog.commands.sort_by(|a, b| {
        (&a.agent_label, &a.name, &a.path).cmp(&(&b.agent_label, &b.name, &b.path))
    });
    catalog
}

pub(super) fn copy_selected(
    home: &Path,
    project: Option<&Path>,
    target: &Directory,
    selections: &[ImportSelection],
) -> Result<Vec<ImportResult>, String> {
    if selections.is_empty() || selections.len() > MAX_COMMANDS {
        return Err("Invalid command import selection.".into());
    }
    for selection in selections {
        command_name(&selection.relative_path)?;
        source_root(home, project, &selection.agent, selection.scope)?;
        if selection.expected_content.len() > files::MAX_BYTES {
            return Err("Invalid command import selection.".into());
        }
    }
    let mut results = Vec::new();
    for selection in selections {
        let name = command_name(&selection.relative_path)?;
        let id =
            serde_json::to_string(&(&selection.agent, selection.scope, &selection.relative_path))
                .map_err(|e| e.to_string())?;
        let result = (|| {
            let (boundary, root) = source_root(home, project, &selection.agent, selection.scope)?;
            let path = crate::modules::storage::path_at(&root, &selection.relative_path)?;
            let content =
                files::read_text(boundary, &path)?.ok_or("Command file no longer exists.")?;
            if content != selection.expected_content {
                return Err("Command changed on disk. Reload it before saving.".into());
            }
            if target
                .names()?
                .iter()
                .any(|entry| entry.eq_ignore_ascii_case(&name))
                || target.path(Some(&name))?.exists()
            {
                return Ok(ImportResult {
                    id: id.clone(),
                    name: name.clone(),
                    status: "skipped",
                    error: None,
                });
            }
            let mut config = files::parse(&name, &content)?;
            config.enabled = true;
            target.save(&config, None)?;
            Ok(ImportResult {
                id: id.clone(),
                name: name.clone(),
                status: "imported",
                error: None,
            })
        })();
        results.push(result.unwrap_or_else(|error| ImportResult {
            id,
            name,
            status: "failed",
            error: Some(error),
        }));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_imports_never_discover_or_copy_project_commands() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let project = home.join("project");
        for (root, prompt) in [(&home, "User command"), (&project, "Project command")] {
            let source = root.join(".claude/commands");
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("review.md"), prompt).unwrap();
        }
        let global = discover(&home, None);
        assert!(global.diagnostics.is_empty());
        assert_eq!(global.commands.len(), 1);
        assert_eq!(global.commands[0].selection.scope, CommandScope::User);
        let scoped = discover(&home, Some(&project));
        let selection = &scoped
            .commands
            .iter()
            .find(|entry| entry.selection.scope == CommandScope::Project)
            .unwrap()
            .selection;
        let target = Directory::user(&home);
        assert!(copy_selected(&home, None, &target, std::slice::from_ref(selection)).is_err());
        assert!(!home.join(".rcode").exists());
        copy_selected(
            &home,
            None,
            &target,
            &[global.commands[0].selection.clone()],
        )
        .unwrap();
        assert_eq!(
            target.read("review").unwrap().unwrap().config.prompt,
            "User command"
        );
        assert_eq!(
            fs::read_to_string(project.join(".claude/commands/review.md")).unwrap(),
            "Project command"
        );
    }

    #[test]
    fn imports_are_explicit_copies_and_do_not_overwrite_or_accept_arbitrary_sources() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path().canonicalize().unwrap();
        let source = home.join(".claude/commands");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("review.md"), "Review $ARGUMENTS").unwrap();
        let catalog = discover(&home, None);
        assert_eq!(catalog.commands.len(), 1);
        let selection = catalog.commands[0].selection.clone();
        let target = Directory::user(&home);
        assert!(!home.join(".rcode").exists());
        let result = copy_selected(&home, None, &target, std::slice::from_ref(&selection)).unwrap();
        assert_eq!(result[0].status, "imported");
        assert_eq!(
            fs::read_to_string(source.join("review.md")).unwrap(),
            "Review $ARGUMENTS"
        );
        let result = copy_selected(&home, None, &target, std::slice::from_ref(&selection)).unwrap();
        assert_eq!(result[0].status, "skipped");
        fs::write(source.join("review.md"), "Changed").unwrap();
        assert_eq!(
            copy_selected(&home, None, &target, std::slice::from_ref(&selection)).unwrap()[0]
                .status,
            "failed"
        );
        assert!(command_name("../secret.md").is_err());
        assert!(command_name("review.md:secret").is_err());
        assert!(source_root(&home, None, "arbitrary", CommandScope::User).is_err());
        assert!(source_root(&home, None, "claudeCode", CommandScope::Project).is_err());
    }
}
