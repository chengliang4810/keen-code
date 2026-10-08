//! 本地 skills 发现和 prompt 读取。

use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::native_paths::NativePaths;

const MAX_PROMPT_BYTES: u64 = 512 * 1024;
const MAX_SKILLS: usize = 4_096;

type SkillFrontmatter = (Option<String>, Option<String>, Option<String>);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSkill {
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub path: PathBuf,
    pub source: String,
}

#[derive(Clone)]
pub struct SkillService {
    paths: Arc<NativePaths>,
}

impl SkillService {
    pub fn new(paths: Arc<NativePaths>) -> Self {
        Self { paths }
    }

    pub fn list(&self, extra_roots: &[PathBuf]) -> Result<Vec<NativeSkill>, String> {
        let mut roots = vec![
            self.paths.home_dir.join(".agents").join("skills"),
            self.paths.home_dir.join(".codex").join("skills"),
        ];
        roots.extend(extra_roots.iter().cloned());
        let mut skills = Vec::new();
        for root in roots {
            if !root.is_dir() {
                continue;
            }
            collect_skills(&root, &mut skills)?;
            if skills.len() >= MAX_SKILLS {
                break;
            }
        }
        skills.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
        skills.dedup_by(|left, right| left.path == right.path);
        Ok(skills)
    }

    pub fn prompt(&self, path: &Path) -> Result<String, String> {
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("Skill 路径不可访问：{error}"))?;
        if !canonical.file_name().is_some_and(|name| name == "SKILL.md") {
            return Err("Skill prompt 必须来自 SKILL.md".to_owned());
        }
        let allowed = canonical.starts_with(self.paths.home_dir.join(".agents").join("skills"))
            || canonical.starts_with(self.paths.home_dir.join(".codex").join("skills"));
        if !allowed {
            return Err("Skill prompt 不属于受控本地 skill 根".to_owned());
        }
        let metadata = fs::metadata(&canonical).map_err(|error| error.to_string())?;
        if metadata.len() > MAX_PROMPT_BYTES {
            return Err("Skill prompt 超过 512 KiB 限制".to_owned());
        }
        let text = fs::read_to_string(&canonical).map_err(|error| error.to_string())?;
        Ok(text)
    }
}

fn collect_skills(root: &Path, result: &mut Vec<NativeSkill>) -> Result<(), String> {
    let entries = fs::read_dir(root).map_err(|error| format!("读取 Skill 目录失败：{error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if !path.is_dir() || result.len() >= MAX_SKILLS {
            continue;
        }
        let prompt = path.join("SKILL.md");
        if !prompt.is_file() {
            continue;
        }
        let (name, display_name, description) = parse_frontmatter(&prompt)?;
        result.push(NativeSkill {
            name: name.unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned()),
            display_name: display_name
                .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned()),
            description,
            path: prompt,
            source: root.to_string_lossy().into_owned(),
        });
    }
    Ok(())
}

fn parse_frontmatter(path: &Path) -> Result<SkillFrontmatter, String> {
    let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Ok((None, None, None));
    }
    let mut name = None;
    let mut display = None;
    let mut description = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']).to_owned();
        match key.trim() {
            "name" => name = Some(value),
            "displayName" | "title" => display = Some(value),
            "description" => description = Some(value),
            _ => {}
        }
    }
    Ok((name, display, description))
}
