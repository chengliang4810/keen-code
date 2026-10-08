use crate::modules::storage;
use rcode_runtime::security::check_file_path;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

pub(super) const MAX_BYTES: usize = 64 * 1024;
pub(super) const MAX_COMMANDS: usize = 128;
pub(super) const ENTRY_LIMIT: usize = 1024;
const CONFLICT: &str = "Command changed on disk. Reload it before saving.";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommandScope {
    User,
    Project,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandConfig {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub prompt: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub enabled: bool,
    pub scope: CommandScope,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandFile {
    #[serde(flatten)]
    pub config: CommandConfig,
    pub content: String,
}

#[derive(Default, Serialize)]
pub struct CommandCatalog {
    pub commands: Vec<CommandEntry>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone)]
pub(super) struct Directory {
    pub boundary: PathBuf,
    pub relative: &'static str,
    pub scope: CommandScope,
}

impl Directory {
    pub fn user(home: &Path) -> Self {
        Self {
            boundary: home.to_path_buf(),
            relative: ".rcode/commands",
            scope: CommandScope::User,
        }
    }

    pub fn project(root: &Path) -> Self {
        Self {
            boundary: root.to_path_buf(),
            relative: ".rcode/commands",
            scope: CommandScope::Project,
        }
    }

    pub fn path(&self, name: Option<&str>) -> Result<PathBuf, String> {
        storage::reject_link(&self.boundary)?;
        let mut directory = self.boundary.clone();
        for part in self.relative.split('/') {
            directory.push(part);
            storage::reject_link(&directory)?;
            match fs::symlink_metadata(&directory) {
                Ok(metadata) if !metadata.is_dir() => {
                    return Err("Command root must be a directory.".into());
                }
                Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.to_string()),
                _ => {}
            }
        }
        let relative = match name {
            Some(name) => {
                validate_name(name)?;
                format!("{}/{}.md", self.relative, name)
            }
            None => self.relative.to_owned(),
        };
        let path = storage::path_at(&self.boundary, &relative)?;
        check_file_path(&self.boundary, &path.to_string_lossy())
    }

    pub fn list(&self) -> Result<CommandCatalog, String> {
        let root = self.path(None)?;
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(CommandCatalog::default()),
            Err(e) => return Err(e.to_string()),
        };
        let mut catalog = CommandCatalog::default();
        let mut names = BTreeSet::new();
        for (index, entry) in entries.enumerate() {
            if index >= ENTRY_LIMIT {
                return Err("Command directory exceeds the entry limit.".into());
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "md") {
                continue;
            }
            if catalog.commands.len() >= MAX_COMMANDS {
                return Err("Too many commands.".into());
            }
            let result: Result<CommandEntry, String> = (|| {
                let name = path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .ok_or("Invalid command name.")?;
                validate_name(name)?;
                if !names.insert(name.to_ascii_lowercase()) {
                    return Err("Duplicate command name.".into());
                }
                let file = self.read(name)?.ok_or("Command file no longer exists.")?;
                Ok(CommandEntry {
                    name: name.to_owned(),
                    description: file.config.description,
                    argument_hint: file.config.argument_hint,
                    enabled: file.config.enabled,
                    scope: self.scope,
                    path: path.to_string_lossy().into_owned(),
                })
            })();
            match result {
                Ok(command) => catalog.commands.push(command),
                Err(error) => catalog
                    .diagnostics
                    .push(format!("{}: {error}", path.display())),
            }
            if catalog.diagnostics.len() > 32 {
                return Err("Too many invalid command files.".into());
            }
        }
        catalog
            .commands
            .sort_by_key(|entry| entry.name.to_ascii_lowercase());
        Ok(catalog)
    }

    pub fn read(&self, name: &str) -> Result<Option<CommandFile>, String> {
        let path = self.path(Some(name))?;
        read_text(&self.boundary, &path)?
            .map(|content| {
                Ok(CommandFile {
                    config: parse(name, &content)?,
                    content,
                })
            })
            .transpose()
    }

    pub fn names(&self) -> Result<Vec<String>, String> {
        let entries = match fs::read_dir(self.path(None)?) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.to_string()),
        };
        let mut names = Vec::new();
        for (index, entry) in entries.enumerate() {
            if index >= ENTRY_LIMIT {
                return Err("Command directory exceeds the entry limit.".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_some_and(|extension| extension == "md") {
                names.push(
                    path.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        Ok(names)
    }

    pub fn find(&self, name: &str) -> Result<Option<CommandFile>, String> {
        validate_name(name)?;
        let mut found = None;
        for existing in self.names()? {
            if !existing.eq_ignore_ascii_case(name) {
                continue;
            }
            if found.is_some() {
                return Err("Duplicate command name.".into());
            }
            found = self.read(&existing)?;
        }
        Ok(found)
    }

    pub fn save(
        &self,
        config: &CommandConfig,
        expected: Option<&str>,
    ) -> Result<CommandFile, String> {
        if expected.is_some_and(|content| content.len() > MAX_BYTES) {
            return Err("Invalid command content or size.".into());
        }
        let document = document(config)?;
        let names = self.names()?;
        if names
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&config.name) && name != &config.name)
        {
            return Err("Duplicate command name.".into());
        }
        if expected.is_none() && names.len() >= MAX_COMMANDS {
            return Err("Too many commands.".into());
        }
        let path = self.path(Some(&config.name))?;
        if read_text(&self.boundary, &path)?.as_deref() != expected {
            return Err(CONFLICT.into());
        }
        let directory = self.path(None)?;
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path = self.path(Some(&config.name))?;
        #[cfg(unix)]
        if self.scope == CommandScope::User {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                self.boundary.join(".rcode"),
                fs::Permissions::from_mode(0o700),
            )
            .map_err(|e| e.to_string())?;
        }
        let mut temp = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
        if let Ok(metadata) = fs::metadata(&path) {
            temp.as_file()
                .set_permissions(metadata.permissions())
                .map_err(|e| e.to_string())?;
        }
        temp.write_all(document.as_bytes())
            .map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        if read_text(&self.boundary, &path)?.as_deref() != expected {
            return Err(CONFLICT.into());
        }
        if expected.is_some() {
            temp.persist(&path).map_err(|e| e.to_string())?;
        } else {
            temp.persist_noclobber(&path).map_err(|e| e.to_string())?;
        }
        self.read(&config.name)?
            .ok_or_else(|| "Command file no longer exists.".into())
    }

    pub fn delete(&self, name: &str, expected: &str) -> Result<(), String> {
        if expected.len() > MAX_BYTES {
            return Err("Invalid command content or size.".into());
        }
        let path = self.path(Some(name))?;
        if read_text(&self.boundary, &path)?.as_deref() != Some(expected) {
            return Err(CONFLICT.into());
        }
        fs::remove_file(self.path(Some(name))?).map_err(|e| e.to_string())
    }
}

pub(super) fn validate_name(name: &str) -> Result<(), String> {
    let key = name.to_ascii_lowercase();
    if name.is_empty()
        || name.len() > 50
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        || matches!(
            key.as_str(),
            "con" | "prn" | "aux" | "nul" | "init" | "plan" | "claude-code"
        )
        || (key.len() == 4
            && (key.starts_with("com") || key.starts_with("lpt"))
            && matches!(key.as_bytes()[3], b'1'..=b'9'))
    {
        return Err("Invalid command name.".into());
    }
    Ok(())
}

pub(super) fn read_text(boundary: &Path, path: &Path) -> Result<Option<String>, String> {
    let relative = path
        .strip_prefix(boundary)
        .map_err(|_| "Command path is outside its directory.")?;
    storage::reject_link(boundary)?;
    storage::path_at(boundary, &relative.to_string_lossy().replace('\\', "/"))?;
    check_file_path(boundary, &path.to_string_lossy())?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("Invalid command file or size.".into());
    }
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Command must be a regular file.".into());
    }
    let mut content = String::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_string(&mut content)
        .map_err(|e| e.to_string())?;
    if content.len() > MAX_BYTES || content.contains('\0') {
        return Err("Invalid command file or size.".into());
    }
    storage::path_at(boundary, &relative.to_string_lossy().replace('\\', "/"))?;
    Ok(Some(content))
}

fn text_field(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.starts_with('"') {
        serde_json::from_str(value).map_err(|_| "Invalid command field.".into())
    } else if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        Ok(value[1..value.len() - 1].replace("''", "'"))
    } else {
        Ok(value.to_owned())
    }
}

pub(super) fn parse(name: &str, content: &str) -> Result<CommandConfig, String> {
    validate_name(name)?;
    let parsed =
        rcode_plugins::parse_plugin_command_document(content).map_err(|e| e.to_string())?;
    let mut argument_hint = String::new();
    let mut enabled = true;
    let text = content.strip_prefix('\u{feff}').unwrap_or(content);
    if text.lines().next() == Some("---") {
        let mut seen = BTreeSet::new();
        for line in text.lines().skip(1).take_while(|line| *line != "---") {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.trim();
            if !["argument-hint", "enabled"].contains(&key) {
                continue;
            }
            if !seen.insert(key) {
                return Err("Duplicate command field.".into());
            }
            match key {
                "argument-hint" => argument_hint = text_field(value)?,
                "enabled" => {
                    enabled = match value.trim() {
                        "true" => true,
                        "false" => false,
                        _ => return Err("Invalid command enabled state.".into()),
                    }
                }
                _ => unreachable!(),
            }
        }
    }
    let config = CommandConfig {
        name: name.to_owned(),
        description: parsed.description,
        argument_hint,
        prompt: parsed.markdown,
        enabled,
    };
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &CommandConfig) -> Result<(), String> {
    validate_name(&config.name)?;
    if config.prompt.trim().is_empty()
        || config.prompt.len() > MAX_BYTES
        || config.description.len() > 4096
        || config.argument_hint.len() > 256
        || [&config.prompt, &config.description, &config.argument_hint]
            .iter()
            .any(|value| value.contains('\0'))
    {
        return Err("Invalid command content or size.".into());
    }
    Ok(())
}

fn document(config: &CommandConfig) -> Result<String, String> {
    validate_config(config)?;
    let content = format!(
        "---\ndescription: {}\nargument-hint: {}\nenabled: {}\n---\n{}",
        serde_json::to_string(&config.description).map_err(|e| e.to_string())?,
        serde_json::to_string(&config.argument_hint).map_err(|e| e.to_string())?,
        config.enabled,
        config.prompt
    );
    if content.len() > MAX_BYTES {
        return Err("Invalid command content or size.".into());
    }
    Ok(content)
}
