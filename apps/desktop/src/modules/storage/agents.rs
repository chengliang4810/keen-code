use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::Mutex,
};

static WRITES: Mutex<()> = Mutex::new(());
const MAX_BYTES: usize = 128 * 1024;
const MAX_ROLES: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRole {
    id: String,
    name: String,
    description: String,
    instructions: String,
    icon: String,
    #[serde(default)]
    built_in: bool,
}

impl AgentRole {
    fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
            || self.built_in
        {
            return Err("Invalid custom agent ID.".into());
        }
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.description.len() > 4096
            || self.instructions.len() > 64 * 1024
            || self.instructions.contains('\0')
        {
            return Err("Invalid custom agent content or size.".into());
        }
        if ![
            "coder",
            "architect",
            "reviewer",
            "security",
            "designer",
            "spark",
        ]
        .contains(&self.icon.as_str())
        {
            return Err("Invalid custom agent icon.".into());
        }
        Ok(())
    }

    fn document(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "---\nname: {}\ndescription: {}\nicon: {}\n---\n{}",
            serde_json::to_string(&self.name).map_err(|e| e.to_string())?,
            serde_json::to_string(&self.description).map_err(|e| e.to_string())?,
            self.icon,
            self.instructions
        ))
    }
}

fn parse(id: &str, document: &str) -> Result<AgentRole, String> {
    let body = document
        .strip_prefix("---\r\n")
        .or_else(|| document.strip_prefix("---\n"))
        .ok_or("Agent Markdown requires front matter.")?;
    let (header, instructions) = body
        .split_once("\n---\r\n")
        .or_else(|| body.split_once("\n---\n"))
        .ok_or("Agent front matter is not closed.")?;
    let mut fields = BTreeMap::new();
    for line in header.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(':').ok_or("Invalid agent front matter.")?;
        let key = key.trim();
        if !["name", "description", "icon"].contains(&key) || fields.contains_key(key) {
            return Err("Unknown or duplicate agent field.".into());
        }
        let value = value.trim();
        let value = if value.starts_with('"') {
            serde_json::from_str::<String>(value).map_err(|_| "Invalid quoted agent field.")?
        } else if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
            value[1..value.len() - 1].replace("''", "'")
        } else {
            value.to_owned()
        };
        fields.insert(key, value);
    }
    let role = AgentRole {
        id: id.into(),
        name: fields.remove("name").ok_or("Agent name is missing.")?,
        description: fields.remove("description").unwrap_or_default(),
        icon: fields.remove("icon").unwrap_or_else(|| "spark".into()),
        instructions: instructions.into(),
        built_in: false,
    };
    role.validate()?;
    Ok(role)
}

fn load_at(root: &Path) -> Result<Vec<AgentRole>, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut roles = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 512 {
            return Err("Agent directory exceeds the entry limit.".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        super::reject_link(&path)?;
        let id = path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or("Invalid agent filename.")?;
        let file = File::open(&path).map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.len() > MAX_BYTES as u64 {
            return Err("Invalid agent Markdown file or size.".into());
        }
        let mut text = String::new();
        file.take((MAX_BYTES + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() > MAX_BYTES {
            return Err("Agent Markdown exceeds the size limit.".into());
        }
        roles.push(parse(id, &text).map_err(|e| format!("{}: {e}", path.display()))?);
        if roles.len() > MAX_ROLES {
            return Err("Too many custom agents.".into());
        }
    }
    roles.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(roles)
}

#[tauri::command]
pub async fn storage_agents_read() -> Result<Vec<AgentRole>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        load_at(&super::path("agents")?)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn save_at(root: &Path, roles: &[AgentRole], expected: &[AgentRole]) -> Result<(), String> {
    if roles.len() > MAX_ROLES || expected.len() > MAX_ROLES {
        return Err("Too many custom agents.".into());
    }
    let mut documents = BTreeMap::new();
    for role in roles {
        if documents.insert(&role.id, role.document()?).is_some() {
            return Err("Duplicate custom agent ID.".into());
        }
    }
    let current = load_at(root)?;
    let mut expected = expected.to_vec();
    expected.sort_by(|a, b| a.id.cmp(&b.id));
    if current != expected {
        return Err("Custom agents changed on disk. Reload them before saving.".into());
    }
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    for role in roles {
        if current.iter().any(|old| old == role) {
            continue;
        }
        let path = root.join(format!("{}.md", role.id));
        super::reject_link(&path)?;
        let mut temp = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
        temp.write_all(documents[&role.id].as_bytes())
            .map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist(path).map_err(|e| e.to_string())?;
    }
    for role in current {
        if !documents.contains_key(&role.id) {
            let path = root.join(format!("{}.md", role.id));
            super::reject_link(&path)?;
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn storage_agents_save(
    roles: Vec<AgentRole>,
    expected: Vec<AgentRole>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = WRITES.lock().map_err(|e| e.to_string())?;
        save_at(&super::path("agents")?, &roles, &expected)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_roundtrip_and_external_change_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let role = parse(
            "custom",
            "---\nname: '测试角色'\ndescription: 工程助手\n---\n完整角色指令\n",
        )
        .unwrap();
        assert_eq!(parse(&role.id, &role.document().unwrap()).unwrap(), role);
        let crlf = parse("crlf", "---\r\nname: crlf\r\n---\r\nfirst\r\nsecond").unwrap();
        assert_eq!(parse(&crlf.id, &crlf.document().unwrap()).unwrap(), crlf);
        save_at(dir.path(), std::slice::from_ref(&role), &[]).unwrap();
        assert_eq!(load_at(dir.path()).unwrap(), vec![role.clone()]);
        std::fs::write(
            dir.path().join("custom.md"),
            "---\nname: edited\n---\nexternal",
        )
        .unwrap();
        assert!(save_at(dir.path(), &[], &[role]).is_err());
        assert!(dir.path().join("custom.md").exists());
        let current = load_at(dir.path()).unwrap();
        save_at(dir.path(), &[], &current).unwrap();
        assert!(load_at(dir.path()).unwrap().is_empty());
        assert!(parse("../outside", "---\nname: x\n---\nx").is_err());
    }
}
