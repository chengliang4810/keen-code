//! KeenCode 子 Agent Markdown 定义的安全解析与项目级目录装配。

use super::*;

/// 单个 Agent 定义允许读取的最大字节数。
const MAX_AGENT_DOCUMENT_BYTES: u64 = 512 * 1024;
/// 单个 Agent 工具列表允许包含的最大条目数。
const MAX_AGENT_TOOLS: usize = 128;
/// 单个 Agent 写入目录列表允许包含的最大条目数。
const MAX_AGENT_WRITE_DIRS: usize = 64;

/// Agent 工具字段的三态语义。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum AgentTools {
    /// 定义未声明工具字段，运行时继承父 Agent 的冻结工具集。
    Inherit,
    /// 定义显式声明空工具列表。
    None,
    /// 定义显式声明允许使用的工具名称。
    List(Vec<String>),
}

/// 已通过结构和容量校验的 Agent Markdown 定义。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParsedAgentDocument {
    /// 定义中的可选稳定名称。
    pub name: Option<String>,
    /// 供目录和模型选择使用的简短说明。
    pub description: String,
    /// 可选的 `provider_id::model_id` 精确模型覆盖。
    pub model: Option<String>,
    /// 工具继承或显式过滤规则。
    pub tools: AgentTools,
    /// 从允许工具中排除的名称。
    pub disallowed_tools: Vec<String>,
    /// 当前 Agent 最多运行的模型轮数。
    pub max_turns: Option<u32>,
    /// 当前 Agent 允许写入的额外相对目录。
    pub allowed_write_dirs: Vec<String>,
    /// 追加到 KeenCode 基础提示后的系统说明。
    pub system_prompt: String,
}

/// Agent 定义在当前项目目录中的来源层级。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AgentDefinitionSource {
    /// KeenCode 随应用提供的内置模板。
    Builtin,
    /// 用户数据目录中的全局定义。
    Global,
    /// 当前项目 `.agents/agents` 中的定义。
    Project,
    /// 已启用插件显式声明的定义。
    Plugin,
}

impl AgentDefinitionSource {
    /// 返回设置界面使用的稳定来源标识。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Global => "global",
            Self::Project => "project",
            Self::Plugin => "plugin",
        }
    }
}

/// 一个完成优先级归约的 Agent 目录条目。
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct AgentCatalogEntry {
    /// 运行时和设置界面共同使用的稳定名称。
    pub name: String,
    /// 当前生效定义的来源。
    pub source: AgentDefinitionSource,
    /// 用户可编辑定义的绝对路径；内置模板没有路径。
    pub path: Option<PathBuf>,
    /// 已解析且不再依赖外部文件的定义快照。
    pub document: ParsedAgentDocument,
}

/// 一个项目冻结后的 Agent 定义目录。
#[derive(Clone, Debug, Default)]
pub(super) struct AgentCatalog {
    /// 按不区分 ASCII 大小写的稳定名称保存生效条目。
    entries: BTreeMap<String, AgentCatalogEntry>,
}

impl AgentCatalog {
    /// 返回按稳定名称排序的全部目录条目。
    pub fn entries(&self) -> impl Iterator<Item = &AgentCatalogEntry> {
        self.entries.values()
    }

    /// 按不区分 ASCII 大小写的名称解析一个目录条目。
    pub fn get(&self, name: &str) -> Option<&AgentCatalogEntry> {
        self.entries.get(&name.to_ascii_lowercase())
    }

    /// 以较高优先级定义替换同名条目。
    fn insert(&mut self, entry: AgentCatalogEntry) {
        self.entries.insert(entry.name.to_ascii_lowercase(), entry);
    }
}

/// 校验用户级 Agent 文件名，不允许路径、命名空间或控制字符。
pub(super) fn validate_agent_name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 128 {
        return Err("子智能体名称不能为空且不能超过 128 字节".to_owned());
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("子智能体名称只能包含 ASCII 字母、数字、连字符和下划线".to_owned());
    }
    Ok(value.to_owned())
}

/// 解析 KeenCode Agent Markdown 的 YAML 前置元数据和正文。
pub(super) fn parse_agent_document(content: &str) -> Result<ParsedAgentDocument, String> {
    if content.len() as u64 > MAX_AGENT_DOCUMENT_BYTES {
        return Err(format!("子智能体定义超过 {MAX_AGENT_DOCUMENT_BYTES} 字节"));
    }
    let content = content.trim_start_matches('\u{feff}');
    let (front_matter, body) = split_front_matter(content)?;
    let fields = parse_agent_fields(front_matter)?;
    validate_agent_field_names(&fields)?;
    let description = required_non_empty_scalar(&fields, "description", "子智能体说明")?;
    let name = optional_scalar(&fields, "name")?
        .map(|name| validate_agent_name(&name))
        .transpose()?;
    let model = optional_scalar(&fields, "model")?
        .map(|model| normalize_model_reference(&model))
        .transpose()?;
    let tools = match fields.get("tools") {
        None => AgentTools::Inherit,
        Some(value) => {
            let tools = parse_string_list(value, "tools", MAX_AGENT_TOOLS)?;
            if tools.is_empty() {
                AgentTools::None
            } else {
                AgentTools::List(tools)
            }
        }
    };
    let disallowed_tools = optional_list(&fields, "disallowedTools", MAX_AGENT_TOOLS)?;
    let allowed_write_dirs = optional_list(&fields, "allowedWriteDirs", MAX_AGENT_WRITE_DIRS)?;
    for directory in &allowed_write_dirs {
        validate_relative_write_directory(directory)?;
    }
    let max_turns = optional_scalar(&fields, "maxTurns")?
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|_| "maxTurns 必须是正整数".to_owned())
                .and_then(|value| {
                    if value == 0 {
                        Err("maxTurns 必须大于 0".to_owned())
                    } else {
                        Ok(value)
                    }
                })
        })
        .transpose()?;
    let system_prompt = body.trim().to_owned();
    if system_prompt.is_empty() {
        return Err("子智能体系统提示不能为空".to_owned());
    }
    Ok(ParsedAgentDocument {
        name,
        description,
        model,
        tools,
        disallowed_tools,
        max_turns,
        allowed_write_dirs,
        system_prompt,
    })
}

/// 拒绝 KeenCode 唯一 Agent Schema 之外的字段，避免拼写错误被静默忽略。
fn validate_agent_field_names(fields: &BTreeMap<String, AgentFieldValue>) -> Result<(), String> {
    /// 当前唯一 Agent 前置元数据 Schema 允许的字段。
    const ALLOWED_FIELDS: [&str; 7] = [
        "name",
        "description",
        "model",
        "tools",
        "disallowedTools",
        "maxTurns",
        "allowedWriteDirs",
    ];
    if let Some(field) = fields
        .keys()
        .find(|field| !ALLOWED_FIELDS.contains(&field.as_str()))
    {
        return Err(format!("前置元数据包含未知字段：{field}"));
    }
    Ok(())
}

/// 校验额外写目录是可跨平台解释、不会越出项目边界的非空相对路径。
fn validate_relative_write_directory(directory: &str) -> Result<(), String> {
    let invalid = || "allowedWriteDirs 只能包含不越界的相对目录".to_owned();
    if directory.is_empty()
        || directory.contains(':')
        || directory
            .split(['/', '\\'])
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(invalid());
    }
    let path = Path::new(directory);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(invalid());
    }
    Ok(())
}

/// 构造当前项目的 Agent 目录；项目定义覆盖全局定义，内置名称不可被外部覆盖。
///
/// 插件定义始终使用 `plugin:<marketplace>:<plugin>:<name>` 命名空间，不与
/// 全局或项目定义争用名称。
pub(super) fn build_agent_catalog(
    data_root: &Path,
    project_root: &Path,
    snapshot: &PluginRuntimeSnapshot,
    model_overrides: &BTreeMap<String, String>,
) -> Result<AgentCatalog, String> {
    let mut catalog = AgentCatalog::default();
    let builtin_entries = builtin_agents(model_overrides)?;
    let reserved_names = builtin_entries
        .iter()
        .map(|entry| entry.name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    for entry in builtin_entries {
        catalog.insert(entry);
    }
    for entry in scan_agent_directory(
        &data_root.join("agents"),
        AgentDefinitionSource::Global,
        &reserved_names,
    )? {
        catalog.insert(entry);
    }
    for entry in scan_agent_directory(
        &project_root.join(".agents").join("agents"),
        AgentDefinitionSource::Project,
        &reserved_names,
    )? {
        catalog.insert(entry);
    }
    for plugin in &snapshot.plugins {
        if plugin.agents.is_empty() {
            continue;
        }
        let plugin_namespace = plugin
            .id
            .runtime_namespace()
            .map_err(|error| error.to_string())?;
        let plugin_root_metadata = fs::symlink_metadata(&plugin.root).map_err(|error| {
            format!(
                "无法读取插件 Agent 根目录 {}：{error}",
                plugin.root.display()
            )
        })?;
        if plugin_root_metadata.file_type().is_symlink() || !plugin_root_metadata.is_dir() {
            return Err(format!(
                "插件 Agent 根路径必须是普通目录：{}",
                plugin.root.display()
            ));
        }
        let canonical_plugin_root = fs::canonicalize(&plugin.root).map_err(|error| {
            format!(
                "无法规范化插件 Agent 根目录 {}：{error}",
                plugin.root.display()
            )
        })?;
        for component in &plugin.agents {
            let path = fs::canonicalize(&component.path)
                .map_err(|error| format!("无法规范化插件 Agent 文件：{error}"))?;
            if !path.starts_with(&canonical_plugin_root) {
                return Err(format!("插件 Agent 文件越出插件目录：{}", path.display()));
            }
            let content = read_agent_file(&component.path)?;
            let stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| format!("插件 Agent 文件名不是有效 UTF-8：{}", path.display()))?;
            let stem = validate_agent_name(stem)?;
            let name = format!("{plugin_namespace}:{stem}");
            let document = parse_agent_document(&content)
                .map_err(|error| format!("插件 Agent {name} 无效：{error}"))?;
            if let Some(declared_name) = document.name.as_deref()
                && !declared_name.eq_ignore_ascii_case(&stem)
            {
                return Err(format!(
                    "插件 Agent 文件名 {stem} 与定义 name {declared_name} 不一致"
                ));
            }
            catalog.insert(AgentCatalogEntry {
                name,
                source: AgentDefinitionSource::Plugin,
                path: Some(path),
                document,
            });
        }
    }
    Ok(catalog)
}

/// 返回 KeenCode 自有的只读规划 Agent 定义。
fn builtin_agents(
    model_overrides: &BTreeMap<String, String>,
) -> Result<Vec<AgentCatalogEntry>, String> {
    let mut plan = ParsedAgentDocument {
        name: Some("plan".to_owned()),
        description: "只读分析代码库并给出可执行实施计划".to_owned(),
        model: None,
        tools: AgentTools::List(vec![
            "Read".to_owned(),
            "Glob".to_owned(),
            "Grep".to_owned(),
        ]),
        disallowed_tools: Vec::new(),
        max_turns: None,
        allowed_write_dirs: Vec::new(),
        system_prompt: "你是 KeenCode 的只读规划 Agent。先核对实际代码、配置和测试，再输出具体、可验证的实施计划。不得修改文件、执行有副作用的命令或扩大任务范围。".to_owned(),
    };
    if let Some(model) = model_overrides.get("plan") {
        plan.model = Some(normalize_model_reference(model)?);
    }
    Ok(vec![AgentCatalogEntry {
        name: "plan".to_owned(),
        source: AgentDefinitionSource::Builtin,
        path: None,
        document: plan,
    }])
}

/// 扫描一个受控目录中的顶层 Markdown Agent 定义。
fn scan_agent_directory(
    directory: &Path,
    source: AgentDefinitionSource,
    reserved_names: &BTreeSet<String>,
) -> Result<Vec<AgentCatalogEntry>, String> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(format!(
                "无法读取 Agent 目录 {}：{error}",
                directory.display()
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "Agent 根路径必须是普通目录：{}",
            directory.display()
        ));
    }
    let canonical_root = fs::canonicalize(directory)
        .map_err(|error| format!("无法规范化 Agent 目录 {}：{error}", directory.display()))?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("无法扫描 Agent 目录 {}：{error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("无法读取 Agent 目录项：{error}"))?;
        let entry_path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("无法读取 Agent 目录项类型：{error}"))?;
        if entry_path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let file_name = entry_path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| format!("Agent 文件名不是有效 UTF-8：{}", entry_path.display()))?;
        let name = validate_agent_name(file_name)?;
        // 内置名称由应用独占。即使外部同名文件损坏或是符号链接，也直接忽略，
        // 避免不可信项目阻断或替换只读规划 Agent。
        if reserved_names.contains(&name.to_ascii_lowercase()) {
            continue;
        }
        if file_type.is_symlink() {
            return Err(format!(
                "Agent 目录不允许符号链接：{}",
                entry_path.display()
            ));
        }
        if !file_type.is_file() {
            continue;
        }
        let path = fs::canonicalize(entry_path)
            .map_err(|error| format!("无法规范化 Agent 文件：{error}"))?;
        if !path.starts_with(&canonical_root) {
            return Err(format!("Agent 文件越出受控目录：{}", path.display()));
        }
        let content = read_agent_file(&path)?;
        let document = parse_agent_document(&content)
            .map_err(|error| format!("Agent {name} 无效：{error}"))?;
        if let Some(declared_name) = document.name.as_deref()
            && !declared_name.eq_ignore_ascii_case(&name)
        {
            return Err(format!(
                "Agent 文件名 {name} 与定义 name {declared_name} 不一致"
            ));
        }
        entries.push(AgentCatalogEntry {
            name,
            source,
            path: Some(path),
            document,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

/// 有界读取一个普通 UTF-8 Agent 文件。
fn read_agent_file(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法读取 Agent 文件 {}：{error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("Agent 定义必须是普通文件：{}", path.display()));
    }
    if metadata.len() > MAX_AGENT_DOCUMENT_BYTES {
        return Err(format!("Agent 定义超过 {MAX_AGENT_DOCUMENT_BYTES} 字节"));
    }
    fs::read_to_string(path)
        .map_err(|error| format!("无法读取 Agent 文件 {}：{error}", path.display()))
}

/// 将前置元数据与 Markdown 正文切开。
fn split_front_matter(content: &str) -> Result<(&str, &str), String> {
    let Some(after_open) = content.strip_prefix("---") else {
        return Err("缺少 YAML 前置元数据".to_owned());
    };
    let after_open = after_open
        .strip_prefix("\r\n")
        .or_else(|| after_open.strip_prefix('\n'))
        .ok_or_else(|| "YAML 起始分隔符必须独占一行".to_owned())?;
    let mut line_start = 0usize;
    loop {
        let remainder = &after_open[line_start..];
        let (line, consumed, has_line_ending) = match remainder.find('\n') {
            Some(newline) => (
                remainder[..newline]
                    .strip_suffix('\r')
                    .unwrap_or(&remainder[..newline]),
                newline + 1,
                true,
            ),
            None => (
                remainder.strip_suffix('\r').unwrap_or(remainder),
                remainder.len(),
                false,
            ),
        };
        if line == "---" {
            let body_start = line_start + consumed;
            return Ok((&after_open[..line_start], &after_open[body_start..]));
        }
        if !has_line_ending {
            break;
        }
        line_start += consumed;
    }
    Err("YAML 前置元数据未闭合".to_owned())
}

/// 一个前置元数据字段的标量或字符串列表表示。
#[derive(Clone, Debug, Eq, PartialEq)]
enum AgentFieldValue {
    /// 单个 YAML 标量。
    Scalar(String),
    /// 缩进短横线或 JSON 数组表示的字符串列表。
    List(Vec<String>),
}

/// 解析 Agent 定义使用的受限顶层 YAML 字段。
fn parse_agent_fields(front_matter: &str) -> Result<BTreeMap<String, AgentFieldValue>, String> {
    let lines = front_matter.lines().collect::<Vec<_>>();
    let mut fields = BTreeMap::new();
    let mut index = 0usize;
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
                values.push(parse_yaml_scalar(item)?);
            }
            AgentFieldValue::List(values)
        } else if raw.starts_with('[') {
            let values = serde_json::from_str::<Vec<String>>(raw)
                .map_err(|_| format!("字段 {key} 的内联列表必须是字符串 JSON 数组"))?;
            AgentFieldValue::List(values)
        } else {
            AgentFieldValue::Scalar(parse_yaml_scalar(raw)?)
        };
        fields.insert(key.to_owned(), value);
    }
    Ok(fields)
}

/// 解析双引号 JSON 字符串、单引号 YAML 字符串或裸标量。
fn parse_yaml_scalar(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value)
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

/// 读取必填且非空的字符串字段。
fn required_non_empty_scalar(
    fields: &BTreeMap<String, AgentFieldValue>,
    key: &str,
    label: &str,
) -> Result<String, String> {
    optional_scalar(fields, key)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{label}不能为空"))
}

/// 读取可选字符串字段，并拒绝把列表静默当成字段缺失。
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

/// 读取一个可选字符串列表。
fn optional_list(
    fields: &BTreeMap<String, AgentFieldValue>,
    key: &str,
    maximum: usize,
) -> Result<Vec<String>, String> {
    let Some(value) = fields.get(key) else {
        return Ok(Vec::new());
    };
    parse_string_list(value, key, maximum)
}

/// 将字段归一为去重、非空且有界的字符串列表。
fn parse_string_list(
    value: &AgentFieldValue,
    label: &str,
    maximum: usize,
) -> Result<Vec<String>, String> {
    let values = match value {
        AgentFieldValue::List(values) => values.clone(),
        AgentFieldValue::Scalar(value) if value.trim().is_empty() => Vec::new(),
        AgentFieldValue::Scalar(_) => return Err(format!("字段 {label} 必须是字符串列表")),
    };
    if values.len() > maximum {
        return Err(format!("字段 {label} 超过 {maximum} 个条目"));
    }
    let mut normalized = Vec::with_capacity(values.len());
    let mut seen = BTreeSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(format!("字段 {label} 包含无效名称"));
        }
        if !seen.insert(value.to_ascii_lowercase()) {
            return Err(format!("字段 {label} 包含重复名称：{value}"));
        }
        normalized.push(value.to_owned());
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一份最小且合法的 Agent Markdown 文本。
    fn agent_markdown(name: &str, description: &str, prompt: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n{prompt}")
    }

    /// 在指定路径创建 Agent 文件及其父目录。
    fn write_agent(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().expect("Agent 文件应有父目录")).expect("创建 Agent 目录");
        fs::write(path, content).expect("写入 Agent 定义");
    }

    /// 工具缺失、空列表和显式列表必须保留三态语义。
    #[test]
    fn parser_preserves_tool_inheritance_states() {
        let inherited = parse_agent_document("---\ndescription: inherited\n---\nRead only")
            .expect("缺失 tools 应可解析");
        assert_eq!(inherited.tools, AgentTools::Inherit);

        let none = parse_agent_document("---\ndescription: none\ntools: []\n---\nNo tools")
            .expect("空 tools 应可解析");
        assert_eq!(none.tools, AgentTools::None);

        let listed = parse_agent_document(
            "---\ndescription: listed\ntools:\n  - Read\n  - Grep\n---\nInspect",
        )
        .expect("块列表 tools 应可解析");
        assert_eq!(
            listed.tools,
            AgentTools::List(vec!["Read".to_owned(), "Grep".to_owned()])
        );
    }

    /// 结构字段必须完整保留模型、轮数、写目录和工具排除规则。
    #[test]
    fn parser_preserves_runtime_policy_fields() {
        let document = parse_agent_document(
            "---\n\
             name: reviewer\n\
             description: Review changes\n\
             model: \"provider-a::model-a\"\n\
             tools: [\"Read\", \"Grep\", \"Bash\"]\n\
             disallowedTools: [\"Bash\", \"Write\"]\n\
             maxTurns: 17\n\
             allowedWriteDirs: [\"reports/generated\", \"scratch/agent\"]\n\
             ---\n\
             Inspect the actual changes.",
        )
        .expect("完整 Agent 字段应可解析");

        assert_eq!(document.name.as_deref(), Some("reviewer"));
        assert_eq!(document.description, "Review changes");
        assert_eq!(document.model.as_deref(), Some("provider-a::model-a"));
        assert_eq!(
            document.tools,
            AgentTools::List(vec![
                "Read".to_owned(),
                "Grep".to_owned(),
                "Bash".to_owned(),
            ])
        );
        assert_eq!(
            document.disallowed_tools,
            vec!["Bash".to_owned(), "Write".to_owned()]
        );
        assert_eq!(document.max_turns, Some(17));
        assert_eq!(
            document.allowed_write_dirs,
            vec!["reports/generated".to_owned(), "scratch/agent".to_owned()]
        );
        assert_eq!(document.system_prompt, "Inspect the actual changes.");
    }

    /// 唯一新 Schema 必须拒绝未知字段、历史别名和无引号内联列表。
    #[test]
    fn parser_rejects_noncanonical_front_matter() {
        for content in [
            "---\ndescription: typo\ndisalowedTools: [\"Write\"]\n---\nInspect",
            "---\ndescription: alias\ndisallowed_tools: [\"Write\"]\n---\nInspect",
            "---\ndescription: alias\nmax-turns: 3\n---\nInspect",
            "---\ndescription: yaml\ntools: [Read, Grep]\n---\nInspect",
        ] {
            assert!(
                parse_agent_document(content).is_err(),
                "非 canonical 定义必须被拒绝：{content}"
            );
        }
    }

    /// Agent 定义必须拒绝越界、平台前缀、空段和零轮次。
    #[test]
    fn parser_rejects_unsafe_limits() {
        for directory in [
            "",
            ".",
            "..",
            "../outside",
            "safe/../outside",
            "/outside",
            r"C:\outside",
            r"\\server\share",
            "safe//nested",
            r"safe\..\outside",
            "safe:stream",
        ] {
            let directory = serde_json::to_string(directory).expect("序列化测试路径");
            let content =
                format!("---\ndescription: bad\nallowedWriteDirs: [{directory}]\n---\nBad");
            assert!(
                parse_agent_document(&content).is_err(),
                "应拒绝危险写目录 {directory}"
            );
        }
        assert!(parse_agent_document("---\ndescription: bad\nmaxTurns: 0\n---\nBad").is_err());
        assert!(
            parse_agent_document("---\ndescription: bad\nmodel: []\n---\nBad").is_err(),
            "可选标量不得把错误的列表类型当成字段缺失"
        );
    }

    /// 结束分隔符必须同时支持 LF、CRLF 和文件末尾无换行的形式。
    #[test]
    fn front_matter_accepts_closing_delimiter_without_trailing_newline() {
        assert_eq!(
            split_front_matter("---\ndescription: value\n---")
                .expect("无尾换行的结束分隔符应可识别"),
            ("description: value\n", "")
        );
        assert_eq!(
            split_front_matter("---\r\ndescription: value\r\n---\r\nBody")
                .expect("CRLF 文档应可识别"),
            ("description: value\r\n", "Body")
        );
        assert!(split_front_matter("---\ndescription: value\n----").is_err());
        assert!(split_front_matter("--- description: value\n---").is_err());
    }

    /// 内置 plan 必须保持只读工具集并接受严格的模型覆盖。
    #[test]
    fn builtin_plan_is_read_only_and_accepts_model_override() {
        let entries = builtin_agents(&BTreeMap::from([(
            "plan".to_owned(),
            " provider-a :: model-a ".to_owned(),
        )]))
        .expect("内置 plan 应可构造");
        assert_eq!(entries.len(), 1);
        let plan = &entries[0];
        assert_eq!(plan.name, "plan");
        assert_eq!(plan.source, AgentDefinitionSource::Builtin);
        assert_eq!(plan.path, None);
        assert_eq!(plan.document.model.as_deref(), Some("provider-a::model-a"));
        assert_eq!(plan.document.max_turns, None);
        assert!(plan.document.allowed_write_dirs.is_empty());
        let AgentTools::List(tools) = &plan.document.tools else {
            panic!("内置 plan 应使用显式只读工具集");
        };
        for forbidden in ["Agent", "Bash", "PowerShell", "Git", "Write", "Edit"] {
            assert!(!tools.iter().any(|tool| tool == forbidden), "{forbidden}");
        }
        assert_eq!(tools, &["Read", "Glob", "Grep"]);
    }

    /// 项目定义覆盖全局同名定义，插件定义则必须保持命名空间隔离。
    #[test]
    fn catalog_applies_project_priority_and_plugin_namespace() {
        let temporary = tempfile::tempdir().expect("创建临时目录");
        let data_root = temporary.path().join("data");
        let project_root = temporary.path().join("project");
        let plugin_root = temporary.path().join("plugin");
        let global_agent = data_root.join("agents/reviewer.md");
        let project_agent = project_root.join(".agents/agents/reviewer.md");
        let plugin_agent = plugin_root.join("agents/reviewer.md");
        write_agent(
            &global_agent,
            &agent_markdown("reviewer", "global definition", "Global prompt"),
        );
        write_agent(
            &project_agent,
            &agent_markdown("reviewer", "project definition", "Project prompt"),
        );
        write_agent(
            &plugin_agent,
            &agent_markdown("reviewer", "plugin definition", "Plugin prompt"),
        );
        let snapshot = PluginRuntimeSnapshot {
            plugins: vec![crate::plugins::RuntimePlugin {
                id: PluginId {
                    plugin: "demo".to_owned(),
                    marketplace: Some("local".to_owned()),
                },
                root: plugin_root,
                commands: Vec::new(),
                skills: Vec::new(),
                agents: vec![crate::plugins::ComponentFile {
                    path: plugin_agent,
                    relative_path: PathBuf::from("agents/reviewer.md"),
                }],
                hooks: None,
                unsupported_hooks: Vec::new(),
                mcp_servers: BTreeMap::new(),
                lsp_servers: Vec::new(),
            }],
        };

        let catalog = build_agent_catalog(&data_root, &project_root, &snapshot, &BTreeMap::new())
            .expect("应构造 Agent 目录");

        let reviewer = catalog.get("REVIEWER").expect("应保留 reviewer");
        assert_eq!(reviewer.source, AgentDefinitionSource::Project);
        assert_eq!(reviewer.document.description, "project definition");
        let plugin = catalog
            .get("plugin:local:demo:reviewer")
            .expect("应保留插件命名空间定义");
        assert_eq!(plugin.source, AgentDefinitionSource::Plugin);
        assert_eq!(plugin.document.description, "plugin definition");
        assert_eq!(catalog.entries().count(), 3);
    }

    /// 插件快照中的 Agent 路径即使指向普通文件，也不得越出声明的插件根目录。
    #[test]
    fn catalog_rejects_plugin_agent_outside_plugin_root() {
        let temporary = tempfile::tempdir().expect("创建临时目录");
        let data_root = temporary.path().join("data");
        let project_root = temporary.path().join("project");
        let plugin_root = temporary.path().join("plugin");
        let outside_agent = temporary.path().join("outside/escape.md");
        fs::create_dir_all(&plugin_root).expect("创建插件根目录");
        write_agent(
            &outside_agent,
            &agent_markdown("escape", "outside definition", "Outside prompt"),
        );
        let snapshot = PluginRuntimeSnapshot {
            plugins: vec![crate::plugins::RuntimePlugin {
                id: PluginId {
                    plugin: "demo".to_owned(),
                    marketplace: Some("local".to_owned()),
                },
                root: plugin_root,
                commands: Vec::new(),
                skills: Vec::new(),
                agents: vec![crate::plugins::ComponentFile {
                    path: outside_agent,
                    relative_path: PathBuf::from("agents/escape.md"),
                }],
                hooks: None,
                unsupported_hooks: Vec::new(),
                mcp_servers: BTreeMap::new(),
                lsp_servers: Vec::new(),
            }],
        };

        let error = build_agent_catalog(&data_root, &project_root, &snapshot, &BTreeMap::new())
            .expect_err("越出插件根目录的 Agent 必须被拒绝");
        assert!(error.contains("越出插件目录"), "{error}");
    }

    /// 全局与项目的同名文件不得覆盖或阻断内置 plan。
    #[test]
    fn catalog_ignores_external_files_with_builtin_names() {
        let temporary = tempfile::tempdir().expect("创建临时目录");
        let data_root = temporary.path().join("data");
        let project_root = temporary.path().join("project");
        write_agent(&data_root.join("agents/PLAN.md"), "not valid front matter");
        write_agent(
            &project_root.join(".agents/agents/plan.md"),
            &agent_markdown("plan", "project override", "Unsafe override"),
        );

        let catalog = build_agent_catalog(
            &data_root,
            &project_root,
            &PluginRuntimeSnapshot::default(),
            &BTreeMap::new(),
        )
        .expect("外部 plan 文件不应阻断目录构造");

        let plan = catalog.get("plan").expect("内置 plan 应始终存在");
        assert_eq!(plan.source, AgentDefinitionSource::Builtin);
        assert_eq!(catalog.entries().count(), 1);
    }
}
