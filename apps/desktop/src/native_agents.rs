//! Native Agent 模板的受控发现与 Markdown frontmatter 解析。
//!
//! 该模块只产生已经脱离文件依赖的 `RuntimeAgentTemplate`。目录、文件、
//! frontmatter 和正文都在候选发布前完成边界检查；单个坏文件不会阻断同一
//! 候选中的其他合法模板。

use crate::agent_runtime::{RuntimeAgentTemplate, RuntimeExtensionDiagnostic};
use crate::native_extension_contributor::NativeAgentTemplate;
use crate::plugins::{ComponentFile, PluginRuntimeSnapshot};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

const MAX_AGENT_FILE_BYTES: u64 = 512 * 1024;
const MAX_AGENT_FILES: usize = 256;
const MAX_AGENT_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_AGENT_NAME_BYTES: usize = 128;
const MAX_AGENT_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_AGENT_PROMPT_BYTES: usize = 512 * 1024;
const MAX_AGENT_LIST_ITEMS: usize = 128;
const MAX_AGENT_WRITE_DIRS: usize = 64;
const MAX_AGENT_LIST_ITEM_BYTES: usize = 256;
// 与核心 Agent 模板校验保持一致，发现阶段提前拒绝最终无法 spawn 的模板。
const MAX_AGENT_TURNS: u32 = 10_000;

#[derive(Clone, Debug, Eq, PartialEq)]
enum AgentFieldValue {
    Scalar(String),
    List(Vec<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedAgent {
    name: Option<String>,
    description: String,
    model: Option<String>,
    effort: Option<String>,
    inject_agents_md: bool,
    tools: Option<Vec<String>>,
    disallowed_tools: Vec<String>,
    max_turns: Option<u32>,
    allowed_write_dirs: Vec<String>,
    system_prompt: String,
}

#[derive(Default)]
struct DiscoveryBudget {
    files: usize,
    bytes: u64,
}

struct DiscoveryContext<'a> {
    ids: &'a mut BTreeMap<String, RegisteredAgent>,
    templates: &'a mut Vec<NativeAgentTemplate>,
    diagnostics: &'a mut Vec<RuntimeExtensionDiagnostic>,
    budget: &'a mut DiscoveryBudget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AgentScope {
    Global,
    Project,
    Plugin,
}

#[derive(Clone, Copy, Debug)]
struct RegisteredAgent {
    scope: AgentScope,
    template_index: usize,
}

/// 发现当前项目可以显式解析的 Agent 模板，并返回有界安全诊断。
pub(crate) fn discover_agents(
    data_root: &Path,
    project_root: &Path,
    snapshot: &PluginRuntimeSnapshot,
) -> (Vec<NativeAgentTemplate>, Vec<RuntimeExtensionDiagnostic>) {
    let mut templates = Vec::new();
    let mut diagnostics = Vec::new();
    let mut ids = BTreeMap::new();
    let mut budget = DiscoveryBudget::default();

    {
        let mut context = DiscoveryContext {
            ids: &mut ids,
            templates: &mut templates,
            diagnostics: &mut diagnostics,
            budget: &mut budget,
        };
        discover_directory(
            &data_root.join("agents"),
            "global",
            AgentScope::Global,
            &mut context,
        );
        discover_directory(
            &project_root.join(".keencode").join("agents"),
            "project",
            AgentScope::Project,
            &mut context,
        );

        for plugin in &snapshot.plugins {
            let namespace = match plugin.id.runtime_namespace() {
                Ok(namespace) => namespace,
                Err(error) => {
                    push_diagnostic(
                        context.diagnostics,
                        "plugin",
                        plugin.id.to_string(),
                        "agent_plugin_namespace_invalid",
                        error.to_string(),
                    );
                    continue;
                }
            };
            for component in &plugin.agents {
                discover_plugin_component(
                    &namespace,
                    &plugin.root,
                    component,
                    AgentScope::Plugin,
                    &mut context,
                );
            }
        }
    }

    templates.sort_by(|left, right| left.template.name.cmp(&right.template.name));
    diagnostics.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.server.cmp(&right.server))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    (templates, diagnostics)
}

fn discover_directory(
    directory: &Path,
    source: &str,
    scope: AgentScope,
    context: &mut DiscoveryContext<'_>,
) {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            push_diagnostic(
                context.diagnostics,
                "agent",
                source,
                "agent_directory_read_failed",
                format!("无法读取 Agent 目录：{error}"),
            );
            return;
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() || path_has_symlink(directory) {
        push_diagnostic(
            context.diagnostics,
            "agent",
            source,
            "agent_directory_invalid",
            "Agent 根路径必须是普通目录".to_owned(),
        );
        return;
    }
    let canonical_root = match fs::canonicalize(directory) {
        Ok(path) => path,
        Err(error) => {
            push_diagnostic(
                context.diagnostics,
                "agent",
                source,
                "agent_directory_invalid",
                format!("无法规范化 Agent 目录：{error}"),
            );
            return;
        }
    };
    let mut entries = match fs::read_dir(directory) {
        Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(error) => {
            push_diagnostic(
                context.diagnostics,
                "agent",
                source,
                "agent_directory_read_failed",
                format!("无法扫描 Agent 目录：{error}"),
            );
            return;
        }
    };
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        if context.budget.files >= MAX_AGENT_FILES {
            push_diagnostic(
                context.diagnostics,
                "agent",
                source,
                "agent_file_limit_exceeded",
                format!("Agent 文件数量超过 {MAX_AGENT_FILES} 个"),
            );
            break;
        }
        context.budget.files += 1;
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                push_diagnostic(
                    context.diagnostics,
                    "agent",
                    path.display().to_string(),
                    "agent_file_invalid",
                    format!("无法读取 Agent 文件类型：{error}"),
                );
                continue;
            }
        };
        if file_type.is_symlink() || !file_type.is_file() {
            if file_type.is_symlink() {
                push_diagnostic(
                    context.diagnostics,
                    "agent",
                    path.display().to_string(),
                    "agent_symlink_rejected",
                    "Agent 文件不允许符号链接".to_owned(),
                );
            }
            continue;
        }
        let stem = match path.file_stem().and_then(|value| value.to_str()) {
            Some(stem) => match validate_name(stem) {
                Ok(stem) => stem,
                Err(error) => {
                    push_diagnostic(
                        context.diagnostics,
                        "agent",
                        path.display().to_string(),
                        "agent_name_invalid",
                        error,
                    );
                    continue;
                }
            },
            None => {
                push_diagnostic(
                    context.diagnostics,
                    "agent",
                    path.display().to_string(),
                    "agent_name_invalid",
                    "Agent 文件名必须是有效 UTF-8".to_owned(),
                );
                continue;
            }
        };
        let Some(content) = read_agent_file(
            &path,
            &canonical_root,
            context.budget,
            context.diagnostics,
            "agent",
        ) else {
            continue;
        };
        let parsed = match parse_agent(&content) {
            Ok(parsed) => parsed,
            Err(error) => {
                push_diagnostic(
                    context.diagnostics,
                    "agent",
                    path.display().to_string(),
                    "agent_document_invalid",
                    error,
                );
                continue;
            }
        };
        let name = parsed.name.clone().unwrap_or(stem);
        insert_template(name, parsed, scope, context);
    }
}

fn discover_plugin_component(
    namespace: &str,
    plugin_root: &Path,
    component: &ComponentFile,
    scope: AgentScope,
    context: &mut DiscoveryContext<'_>,
) {
    let root_metadata = match fs::symlink_metadata(plugin_root) {
        Ok(metadata) => metadata,
        Err(error) => {
            push_diagnostic(
                context.diagnostics,
                "plugin",
                namespace,
                "agent_plugin_root_invalid",
                format!("无法读取插件 Agent 根目录：{error}"),
            );
            return;
        }
    };
    if root_metadata.file_type().is_symlink()
        || !root_metadata.is_dir()
        || path_has_symlink(plugin_root)
    {
        push_diagnostic(
            context.diagnostics,
            "plugin",
            namespace,
            "agent_plugin_root_invalid",
            "插件 Agent 根路径必须是普通目录".to_owned(),
        );
        return;
    }
    let canonical_root = match fs::canonicalize(plugin_root) {
        Ok(path) => path,
        Err(error) => {
            push_diagnostic(
                context.diagnostics,
                "plugin",
                namespace,
                "agent_plugin_root_invalid",
                format!("无法规范化插件 Agent 根目录：{error}"),
            );
            return;
        }
    };
    if context.budget.files >= MAX_AGENT_FILES {
        push_diagnostic(
            context.diagnostics,
            "plugin",
            namespace,
            "agent_file_limit_exceeded",
            format!("Agent 文件数量超过 {MAX_AGENT_FILES} 个"),
        );
        return;
    }
    context.budget.files += 1;
    let path = &component.path;
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        push_diagnostic(
            context.diagnostics,
            "plugin",
            path.display().to_string(),
            "agent_name_invalid",
            "插件 Agent 文件名必须是有效 UTF-8".to_owned(),
        );
        return;
    };
    let stem = match validate_name(stem) {
        Ok(stem) => stem,
        Err(error) => {
            push_diagnostic(
                context.diagnostics,
                "plugin",
                path.display().to_string(),
                "agent_name_invalid",
                error,
            );
            return;
        }
    };
    let Some(content) = read_agent_file(
        path,
        &canonical_root,
        context.budget,
        context.diagnostics,
        "plugin",
    ) else {
        return;
    };
    let parsed = match parse_agent(&content) {
        Ok(parsed) => parsed,
        Err(error) => {
            push_diagnostic(
                context.diagnostics,
                "plugin",
                path.display().to_string(),
                "agent_document_invalid",
                error,
            );
            return;
        }
    };
    if parsed.name.as_deref() != Some(stem.as_str()) {
        push_diagnostic(
            context.diagnostics,
            "plugin",
            path.display().to_string(),
            "agent_name_mismatch",
            "插件 Agent 的 frontmatter name 必须与文件名一致".to_owned(),
        );
        return;
    }
    insert_template(format!("{namespace}:{stem}"), parsed, scope, context);
}

fn insert_template(
    name: String,
    parsed: ParsedAgent,
    scope: AgentScope,
    context: &mut DiscoveryContext<'_>,
) {
    let key = name.to_ascii_lowercase();
    if let Some(existing) = context.ids.get(&key).copied() {
        if existing.scope == AgentScope::Global && scope == AgentScope::Project {
            // 项目模板覆盖同名全局模板；同一作用域仍由下方分支报告重复，避免
            // 项目目录中的第二份配置静默改变最终运行时模板。
        } else if existing.scope == AgentScope::Project && scope == AgentScope::Global {
            // 全局模板已经被项目模板覆盖，保留项目版本且不把合法覆盖误报为重复。
            return;
        } else {
            push_diagnostic(
                context.diagnostics,
                "agent",
                name,
                "agent_duplicate_id",
                "重复的 Agent ID 已跳过".to_owned(),
            );
            return;
        }
    }
    // 核心校验与 Runtime 会以项目根解释这些相对目录；发现阶段不能改成绝对路径。
    let allowed_write_dirs = parsed
        .allowed_write_dirs
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let template = NativeAgentTemplate {
        description: parsed.description.clone(),
        template: RuntimeAgentTemplate {
            name,
            inject_agents_md: parsed.inject_agents_md,
            system_prompt: parsed.system_prompt,
            model: parsed.model,
            reasoning_effort: parsed.effort,
            tool_names: parsed.tools,
            disallowed_tool_names: parsed.disallowed_tools,
            max_turns: parsed.max_turns,
            allowed_write_dirs,
        },
    };
    if let Some(existing) = context.ids.get_mut(&key) {
        // 只有项目覆盖全局会走到这里；插件 ID 带命名空间，不参与该优先级。
        context.templates[existing.template_index] = template;
        existing.scope = scope;
    } else {
        let template_index = context.templates.len();
        context.templates.push(template);
        context.ids.insert(
            key,
            RegisteredAgent {
                scope,
                template_index,
            },
        );
    }
}

fn read_agent_file(
    path: &Path,
    canonical_root: &Path,
    budget: &mut DiscoveryBudget,
    diagnostics: &mut Vec<RuntimeExtensionDiagnostic>,
    source: &str,
) -> Option<String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return rejected_file(
                diagnostics,
                source,
                path,
                "agent_file_invalid",
                format!("无法读取 Agent 文件：{error}"),
            );
        }
    };
    if metadata.file_type().is_symlink() {
        return rejected_file(
            diagnostics,
            source,
            path,
            "agent_symlink_rejected",
            "Agent 文件不允许符号链接".to_owned(),
        );
    }
    if !metadata.is_file() {
        return rejected_file(
            diagnostics,
            source,
            path,
            "agent_file_invalid",
            "Agent 定义必须是普通文件".to_owned(),
        );
    }
    if metadata.len() > MAX_AGENT_FILE_BYTES {
        return rejected_file(
            diagnostics,
            source,
            path,
            "agent_file_too_large",
            format!("Agent 定义超过 {MAX_AGENT_FILE_BYTES} 字节"),
        );
    }
    if budget.bytes.saturating_add(metadata.len()) > MAX_AGENT_TOTAL_BYTES {
        return rejected_file(
            diagnostics,
            source,
            path,
            "agent_total_bytes_exceeded",
            format!("Agent 文件总大小超过 {MAX_AGENT_TOTAL_BYTES} 字节"),
        );
    }
    let canonical_path = match fs::canonicalize(path) {
        Ok(path) => path,
        Err(error) => {
            return rejected_file(
                diagnostics,
                source,
                path,
                "agent_file_invalid",
                format!("无法规范化 Agent 文件：{error}"),
            );
        }
    };
    if !canonical_path.starts_with(canonical_root) || path_has_symlink(path) {
        return rejected_file(
            diagnostics,
            source,
            path,
            "agent_path_escape",
            "Agent 文件越出受控目录或路径包含符号链接".to_owned(),
        );
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return rejected_file(
                diagnostics,
                source,
                path,
                "agent_file_invalid",
                format!("无法读取 Agent 文件：{error}"),
            );
        }
    };
    let final_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return rejected_file(
                diagnostics,
                source,
                path,
                "agent_file_changed",
                format!("Agent 文件读取期间发生变化：{error}"),
            );
        }
    };
    if final_metadata.file_type().is_symlink()
        || !final_metadata.is_file()
        || final_metadata.len() != metadata.len()
        || bytes.len() as u64 != metadata.len()
    {
        return rejected_file(
            diagnostics,
            source,
            path,
            "agent_file_changed",
            "Agent 文件读取期间发生变化".to_owned(),
        );
    }
    budget.bytes = budget.bytes.saturating_add(metadata.len());
    match String::from_utf8(bytes) {
        Ok(content) => Some(content),
        Err(_) => rejected_file(
            diagnostics,
            source,
            path,
            "agent_document_invalid",
            "Agent 文件必须是 UTF-8".to_owned(),
        ),
    }
}

fn rejected_file(
    diagnostics: &mut Vec<RuntimeExtensionDiagnostic>,
    source: &str,
    path: &Path,
    code: &str,
    message: String,
) -> Option<String> {
    push_diagnostic(
        diagnostics,
        source,
        path.display().to_string(),
        code,
        message,
    );
    None
}

fn path_has_symlink(path: &Path) -> bool {
    let mut current = path.to_path_buf();
    loop {
        if fs::symlink_metadata(&current)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            return true;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current.as_path() {
            break;
        }
        current = parent.to_path_buf();
    }
    false
}

fn parse_agent(content: &str) -> Result<ParsedAgent, String> {
    if content.len() as u64 > MAX_AGENT_FILE_BYTES {
        return Err(format!("Agent 定义超过 {MAX_AGENT_FILE_BYTES} 字节"));
    }
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let (front_matter, body) = split_front_matter(content)?;
    let fields = parse_fields(front_matter)?;
    const ALLOWED: [&str; 9] = [
        "name",
        "description",
        "model",
        "effort",
        "injectAgentsMd",
        "tools",
        "disallowedTools",
        "maxTurns",
        "allowedWriteDirs",
    ];
    if let Some(field) = fields
        .keys()
        .find(|field| !ALLOWED.contains(&field.as_str()))
    {
        return Err(format!("前置元数据包含未知字段：{field}"));
    }
    let name = optional_scalar(&fields, "name")?
        .map(|value| validate_name(&value))
        .transpose()?;
    let description = optional_scalar(&fields, "description")?
        .ok_or_else(|| "description 不能为空".to_owned())
        .and_then(|value| bounded_text(&value, "description", MAX_AGENT_DESCRIPTION_BYTES))?;
    let model = match optional_scalar(&fields, "model")? {
        None => None,
        Some(value) if value.trim() == "inherit" => None,
        Some(value) => Some(validate_model_reference(&value)?),
    };
    let effort = optional_scalar(&fields, "effort")?
        .map(|value| {
            let value = value.trim();
            if matches!(
                value,
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
            ) {
                Ok(value.to_owned())
            } else {
                Err("effort 必须是 none、minimal、low、medium、high、xhigh 或 max".to_owned())
            }
        })
        .transpose()?;
    let inject_agents_md = optional_bool(&fields, "injectAgentsMd", true)?;
    let tools = fields
        .get("tools")
        .map(|value| parse_list(value, "tools", MAX_AGENT_LIST_ITEMS))
        .transpose()?;
    let disallowed_tools = fields
        .get("disallowedTools")
        .map(|value| parse_list(value, "disallowedTools", MAX_AGENT_LIST_ITEMS))
        .transpose()?
        .unwrap_or_default();
    let allowed_write_dirs = fields
        .get("allowedWriteDirs")
        .map(|value| parse_list(value, "allowedWriteDirs", MAX_AGENT_WRITE_DIRS))
        .transpose()?
        .unwrap_or_default();
    for directory in &allowed_write_dirs {
        validate_relative_directory(directory)?;
    }
    let max_turns = optional_scalar(&fields, "maxTurns")?
        .map(|value| {
            let turns = value
                .trim()
                .parse::<u32>()
                .map_err(|_| "maxTurns 必须是正整数".to_owned())?;
            if turns == 0 || turns > MAX_AGENT_TURNS {
                Err(format!("maxTurns 必须在 1 到 {MAX_AGENT_TURNS} 之间"))
            } else {
                Ok(turns)
            }
        })
        .transpose()?;
    let system_prompt = bounded_text(body, "system prompt", MAX_AGENT_PROMPT_BYTES)?;
    Ok(ParsedAgent {
        name,
        description,
        model,
        effort,
        inject_agents_md,
        tools,
        disallowed_tools,
        max_turns,
        allowed_write_dirs,
        system_prompt,
    })
}

fn split_front_matter(content: &str) -> Result<(&str, &str), String> {
    let Some(after_open) = content.strip_prefix("---") else {
        return Err("缺少 YAML 前置元数据".to_owned());
    };
    let after_open = after_open
        .strip_prefix("\r\n")
        .or_else(|| after_open.strip_prefix('\n'))
        .ok_or_else(|| "YAML 起始分隔符必须独占一行".to_owned())?;
    let mut offset = 0;
    while offset <= after_open.len() {
        let remainder = &after_open[offset..];
        let Some(newline) = remainder.find('\n') else {
            break;
        };
        let line = remainder[..newline]
            .strip_suffix('\r')
            .unwrap_or(&remainder[..newline]);
        if line == "---" {
            return Ok((&after_open[..offset], &remainder[newline + 1..]));
        }
        offset += newline + 1;
    }
    Err("YAML 前置元数据未闭合".to_owned())
}

fn parse_fields(front_matter: &str) -> Result<BTreeMap<String, AgentFieldValue>, String> {
    let lines = front_matter.lines().collect::<Vec<_>>();
    let mut fields = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        index += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err("前置元数据包含没有所属字段的缩进行".to_owned());
        }
        let (key, raw) = line
            .split_once(':')
            .ok_or_else(|| "前置元数据顶层字段必须使用 key: value".to_owned())?;
        let key = key.trim();
        if key.is_empty() || fields.contains_key(key) {
            return Err(format!("前置元数据字段为空或重复：{key}"));
        }
        let raw = raw.trim();
        let value = if matches!(raw, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            let folded = raw.starts_with('>');
            let mut block = Vec::new();
            while index < lines.len()
                && (lines[index].trim().is_empty()
                    || lines[index].starts_with(' ')
                    || lines[index].starts_with('\t'))
            {
                block.push(lines[index].trim().to_owned());
                index += 1;
            }
            AgentFieldValue::Scalar(if folded {
                block.join(" ").trim().to_owned()
            } else {
                block.join("\n").trim().to_owned()
            })
        } else if raw.is_empty() {
            let mut values = Vec::new();
            while index < lines.len()
                && (lines[index].trim().is_empty()
                    || lines[index].starts_with(' ')
                    || lines[index].starts_with('\t'))
            {
                let candidate = lines[index].trim();
                index += 1;
                if candidate.is_empty() || candidate.starts_with('#') {
                    continue;
                }
                let item = candidate
                    .strip_prefix('-')
                    .map(str::trim)
                    .ok_or_else(|| format!("字段 {key} 的缩进行必须是列表项"))?;
                values.push(parse_scalar(item)?);
            }
            AgentFieldValue::List(values)
        } else if raw.starts_with('[') {
            AgentFieldValue::List(
                serde_json::from_str::<Vec<String>>(raw)
                    .map_err(|_| format!("字段 {key} 的内联列表必须是字符串 JSON 数组"))?,
            )
        } else {
            AgentFieldValue::Scalar(parse_scalar(raw)?)
        };
        fields.insert(key.to_owned(), value);
    }
    Ok(fields)
}

fn parse_scalar(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.starts_with('"') {
        return serde_json::from_str(value)
            .map_err(|_| "双引号 YAML 标量必须是有效 JSON 字符串".to_owned());
    }
    if value.starts_with('\'') {
        if value.len() < 2 || !value.ends_with('\'') {
            return Err("单引号 YAML 标量未闭合".to_owned());
        }
        return Ok(value[1..value.len() - 1].replace("''", "'"));
    }
    Ok(value.to_owned())
}

fn optional_scalar(
    fields: &BTreeMap<String, AgentFieldValue>,
    key: &str,
) -> Result<Option<String>, String> {
    match fields.get(key) {
        Some(AgentFieldValue::Scalar(value)) => Ok(Some(value.clone())),
        Some(AgentFieldValue::List(_)) => Err(format!("字段 {key} 必须是字符串")),
        None => Ok(None),
    }
}

fn optional_bool(
    fields: &BTreeMap<String, AgentFieldValue>,
    key: &str,
    default: bool,
) -> Result<bool, String> {
    let Some(value) = optional_scalar(fields, key)? else {
        return Ok(default);
    };
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("字段 {key} 必须是 true 或 false")),
    }
}

fn parse_list(value: &AgentFieldValue, label: &str, maximum: usize) -> Result<Vec<String>, String> {
    let values = match value {
        AgentFieldValue::List(values) => values.clone(),
        AgentFieldValue::Scalar(value) if value.trim().is_empty() => Vec::new(),
        AgentFieldValue::Scalar(value) => {
            value.split(',').map(str::trim).map(str::to_owned).collect()
        }
    };
    if values.len() > maximum {
        return Err(format!("字段 {label} 超过 {maximum} 个条目"));
    }
    let mut result = Vec::with_capacity(values.len());
    let mut seen = BTreeSet::new();
    for value in values {
        let value = bounded_text(&value, label, MAX_AGENT_LIST_ITEM_BYTES)?;
        if !seen.insert(value.to_ascii_lowercase()) {
            continue;
        }
        result.push(value);
    }
    Ok(result)
}

fn validate_name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_AGENT_NAME_BYTES {
        return Err(format!(
            "Agent name 不能为空且不能超过 {MAX_AGENT_NAME_BYTES} 字节"
        ));
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("Agent name 只能包含 ASCII 字母、数字、连字符和下划线".to_owned());
    }
    Ok(value.to_owned())
}

fn validate_model_reference(value: &str) -> Result<String, String> {
    if value.contains("::") && value.trim() != value {
        return Err("model 的 provider::model 引用格式无效".to_owned());
    }
    let bounded = bounded_text(value, "model", MAX_AGENT_LIST_ITEM_BYTES)?;
    if bounded.chars().any(char::is_control) {
        return Err("model 不能包含控制字符".to_owned());
    }
    if let Some((provider, model)) = bounded.split_once("::")
        && (provider.is_empty()
            || model.is_empty()
            || provider.trim() != provider
            || model.trim() != model
            || model.contains("::"))
    {
        return Err("model 的 provider::model 引用格式无效".to_owned());
    }
    Ok(bounded)
}

fn bounded_text(value: &str, label: &str, maximum: usize) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{label} 不能为空"));
    }
    if value.len() > maximum {
        return Err(format!("{label} 超过 {maximum} 字节"));
    }
    if value.chars().any(|character| {
        character == '\0' || (character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    }) {
        return Err(format!("{label} 包含控制字符"));
    }
    Ok(value.to_owned())
}

fn validate_relative_directory(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.contains(':')
        || value
            .split(['/', '\\'])
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err("allowedWriteDirs 只能包含安全相对目录".to_owned());
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("allowedWriteDirs 只能包含安全相对目录".to_owned());
    }
    Ok(())
}

fn push_diagnostic(
    diagnostics: &mut Vec<RuntimeExtensionDiagnostic>,
    source: &str,
    server: impl Into<String>,
    code: &str,
    message: String,
) {
    diagnostics.push(RuntimeExtensionDiagnostic {
        source: source.to_owned(),
        server: bounded_diagnostic_text(&server.into()),
        code: code.to_owned(),
        message: bounded_diagnostic_text(&message),
        tool: None,
    });
}

fn bounded_diagnostic_text(value: &str) -> String {
    let value = value.replace(['\r', '\n'], " ");
    let mut end = value.len().min(1024);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{PluginId, RuntimePlugin};
    use std::collections::BTreeMap;

    fn plugin_snapshot(root: &Path, file: &Path) -> PluginRuntimeSnapshot {
        PluginRuntimeSnapshot {
            plugins: vec![RuntimePlugin {
                id: PluginId::from_components("demo", Some("official")).unwrap(),
                root: root.to_path_buf(),
                commands: Vec::new(),
                skills: Vec::new(),
                agents: vec![ComponentFile {
                    path: file.to_path_buf(),
                    relative_path: PathBuf::from("agents/reviewer.md"),
                }],
                hook_environment: BTreeMap::new(),
                hooks: None,
                mcp_servers: BTreeMap::new(),
                lsp_servers: Vec::new(),
            }],
        }
    }

    #[test]
    fn discovers_plugin_agent_with_stable_id() {
        let project = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        let file = plugin.path().join("agents").join("reviewer.md");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "---\nname: reviewer\ndescription: Review changes\nmodel: provider::model\neffort: high\ntools: []\nallowedWriteDirs: [\"workspace\"]\n---\nInspect the change.",
        )
        .unwrap();

        let (templates, diagnostics) = discover_agents(
            project.path(),
            project.path(),
            &plugin_snapshot(plugin.path(), &file),
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].template.name, "plugin:official:demo:reviewer");
        assert_eq!(templates[0].template.tool_names, Some(Vec::new()));
        assert_eq!(
            templates[0].template.model.as_deref(),
            Some("provider::model")
        );
        assert_eq!(
            templates[0].template.allowed_write_dirs,
            vec![PathBuf::from("workspace")]
        );
    }

    #[test]
    fn project_agent_overrides_global_agent_without_duplicate_diagnostic() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("agents");
        let project = root.path().join(".keencode").join("agents");
        fs::create_dir_all(&global).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(
            global.join("reviewer.md"),
            "---\ndescription: Global reviewer\n---\nUse the global instructions.",
        )
        .unwrap();
        fs::write(
            project.join("reviewer.md"),
            "---\ndescription: Project reviewer\n---\nUse the project instructions.",
        )
        .unwrap();

        let (templates, diagnostics) =
            discover_agents(root.path(), root.path(), &PluginRuntimeSnapshot::default());

        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].template.name, "reviewer");
        assert_eq!(templates[0].description, "Project reviewer");
        assert_eq!(
            templates[0].template.system_prompt,
            "Use the project instructions."
        );
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "agent_duplicate_id"),
            "合法的项目覆盖不应报告重复：{diagnostics:?}"
        );
    }

    #[test]
    fn duplicate_agents_in_the_same_scope_report_diagnostic() {
        let root = tempfile::tempdir().unwrap();
        let agents = root.path().join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(
            agents.join("first.md"),
            "---\nname: reviewer\ndescription: First reviewer\n---\nFirst instructions.",
        )
        .unwrap();
        fs::write(
            agents.join("second.md"),
            "---\nname: reviewer\ndescription: Second reviewer\n---\nSecond instructions.",
        )
        .unwrap();

        let (templates, diagnostics) =
            discover_agents(root.path(), root.path(), &PluginRuntimeSnapshot::default());

        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].description, "First reviewer");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "agent_duplicate_id"),
            "同一作用域的重复 Agent 必须报告诊断：{diagnostics:?}"
        );
    }

    #[test]
    fn plugin_agent_name_stays_namespaced_from_global_agent() {
        let root = tempfile::tempdir().unwrap();
        let plugin = tempfile::tempdir().unwrap();
        let global = root.path().join("agents");
        let plugin_file = plugin.path().join("agents").join("reviewer.md");
        fs::create_dir_all(&global).unwrap();
        fs::create_dir_all(plugin_file.parent().unwrap()).unwrap();
        fs::write(
            global.join("reviewer.md"),
            "---\ndescription: Global reviewer\n---\nGlobal instructions.",
        )
        .unwrap();
        fs::write(
            &plugin_file,
            "---\nname: reviewer\ndescription: Plugin reviewer\n---\nPlugin instructions.",
        )
        .unwrap();

        let (templates, diagnostics) = discover_agents(
            root.path(),
            root.path(),
            &plugin_snapshot(plugin.path(), &plugin_file),
        );

        assert_eq!(templates.len(), 2);
        assert!(templates.iter().any(|template| {
            template.template.name == "reviewer" && template.description == "Global reviewer"
        }));
        assert!(templates.iter().any(|template| {
            template.template.name == "plugin:official:demo:reviewer"
                && template.description == "Plugin reviewer"
        }));
        assert!(
            diagnostics.is_empty(),
            "命名空间隔离不应产生重复：{diagnostics:?}"
        );
    }

    #[test]
    fn skips_invalid_file_but_keeps_valid_file() {
        let project = tempfile::tempdir().unwrap();
        let agents = project.path().join(".keencode").join("agents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(agents.join("bad.md"), "---\ndescription: missing close\n").unwrap();
        fs::write(
            agents.join("good.md"),
            "---\ndescription: Valid\n---\nDo the work.",
        )
        .unwrap();

        let (templates, diagnostics) = discover_agents(
            project.path(),
            project.path(),
            &PluginRuntimeSnapshot::default(),
        );
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].template.name, "good");
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "agent_document_invalid")
        );
    }

    #[test]
    fn validates_turn_limit_and_model_references() {
        let document = |field: &str| format!("---\ndescription: Valid\n{field}\n---\nDo the work.");

        assert!(parse_agent(&document("maxTurns: 10001")).is_err());
        assert_eq!(
            parse_agent(&document("maxTurns: 10000")).unwrap().max_turns,
            Some(10000)
        );

        for model in [
            "provider::",
            "::model",
            "provider::model::extra",
            "\"provider::model \"",
        ] {
            assert!(
                parse_agent(&document(&format!("model: {model}"))).is_err(),
                "应拒绝无效模型引用：{model}"
            );
        }
        assert_eq!(
            parse_agent(&document("model: catalog-model"))
                .unwrap()
                .model
                .as_deref(),
            Some("catalog-model")
        );
        assert_eq!(
            parse_agent(&document("model: provider::catalog-model"))
                .unwrap()
                .model
                .as_deref(),
            Some("provider::catalog-model")
        );
    }
}
