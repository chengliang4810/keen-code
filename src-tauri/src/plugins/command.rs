//! 插件 Slash command 的安全目录、模板解析与 Agent 工具适配。
//!
//! 插件 command 是 Markdown 提示模板，不是宿主 shell 命令。运行时只读取已由
//! [`super::extract_components`] 校验过的文件，并把展开后的模板作为工具结果交给
//! Agent；模板中要求的文件、进程或网络操作仍必须通过正常的 Agent 工具执行。

use super::{PluginError, PluginRuntimeSnapshot, RuntimePlugin};
use keencode_agent::{
    AgentTool, ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolFuture, ToolOutput,
};
use keencode_model::ToolDefinition;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// 注册到 Agent Runtime 的插件命令工具名称。
pub const PLUGIN_COMMAND_TOOL_NAME: &str = "PluginCommand";
/// 插件命令稳定名称允许的最大 UTF-8 字节数。
pub const MAX_PLUGIN_COMMAND_NAME_BYTES: usize = 512;
/// Slash command 参数允许的最大 UTF-8 字节数。
pub const MAX_PLUGIN_COMMAND_ARGUMENT_BYTES: usize = 64 * 1024;
/// 单个插件 command 文件允许读取的最大字节数。
pub const MAX_PLUGIN_COMMAND_BYTES: usize = 256 * 1024;
/// command front matter 允许占用的最大字节数。
const MAX_PLUGIN_COMMAND_FRONT_MATTER_BYTES: usize = 16 * 1024;

/// 一个已通过插件快照绑定的 Markdown command。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCommandEntry {
    /// Agent 与 Slash 面板共用的稳定命令名称。
    pub name: String,
    /// 插件安装根目录的规范绝对路径。
    pub root: PathBuf,
    /// 命令文件的规范绝对路径。
    pub path: PathBuf,
    /// 命令文件相对于插件根目录的展示路径。
    pub relative_path: PathBuf,
}

/// 当前项目启用插件的不可变 command 目录。
#[derive(Clone, Debug, Default)]
pub struct PluginCommandCatalog {
    /// 按 ASCII 大小写折叠后的稳定名称索引命令。
    commands: BTreeMap<String, PluginCommandEntry>,
}

impl PluginCommandCatalog {
    /// 从已经完成插件清单和路径校验的运行时快照构建 command 目录。
    pub fn from_snapshot(snapshot: &PluginRuntimeSnapshot) -> Result<Self, PluginError> {
        let mut commands = BTreeMap::new();
        for plugin in &snapshot.plugins {
            insert_plugin_commands(&mut commands, plugin)?;
        }
        Ok(Self { commands })
    }

    /// 返回当前目录中的插件 command 数量。
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// 判断当前项目是否没有可用插件 command。
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// 按稳定名称查找插件 command；查找对 ASCII 大小写不敏感。
    pub fn get(&self, name: &str) -> Option<&PluginCommandEntry> {
        self.commands.get(&name.to_ascii_lowercase())
    }
}

/// 将启用插件中的所有 command 文件加入目录，并拒绝命名冲突或不可调用名称。
fn insert_plugin_commands(
    commands: &mut BTreeMap<String, PluginCommandEntry>,
    plugin: &RuntimePlugin,
) -> Result<(), PluginError> {
    let namespace = plugin.id.runtime_namespace()?;
    for file in &plugin.commands {
        let name = plugin_command_namespace(&namespace, &file.relative_path);
        validate_plugin_command_name(&name)?;
        let key = name.to_ascii_lowercase();
        let entry = PluginCommandEntry {
            name: name.clone(),
            root: plugin.root.clone(),
            path: file.path.clone(),
            relative_path: file.relative_path.clone(),
        };
        if commands.insert(key, entry).is_some() {
            return Err(PluginError::Invalid(format!(
                "插件 command 名称重复：{name}"
            )));
        }
    }
    Ok(())
}

/// 将插件根相对路径转换为稳定的 `plugin:<marketplace>:<plugin>:...` 名称。
pub fn plugin_command_namespace(plugin_namespace: &str, relative_path: &Path) -> String {
    let mut components = relative_path.components().collect::<Vec<_>>();
    if components
        .first()
        .is_some_and(|component| component.as_os_str() == "commands")
    {
        components.remove(0);
    }
    let Some(file) = components.pop() else {
        return plugin_namespace.to_owned();
    };
    let mut parts = vec![plugin_namespace.to_owned()];
    parts.extend(
        components
            .into_iter()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .filter(|part| !part.is_empty()),
    );
    let filename = file.as_os_str().to_string_lossy();
    let command = filename.strip_suffix(".md").unwrap_or(&filename);
    if !command.is_empty() {
        parts.push(command.to_owned());
    }
    parts.join(":")
}

/// 校验 command 名称与前端 Slash token 和 Provider 工具输入的共同边界一致。
fn validate_plugin_command_name(name: &str) -> Result<(), PluginError> {
    let bytes = name.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= MAX_PLUGIN_COMMAND_NAME_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'));
    if !valid {
        return Err(PluginError::Invalid(format!(
            "插件 command 名称无效：{name}"
        )));
    }
    Ok(())
}

/// 一个 command 文件中可供 Agent 观察的说明和 Markdown 模板。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCommandDocument {
    /// front matter 中的可选说明；没有说明时为空字符串。
    pub description: String,
    /// 去除 command front matter 后的 Markdown 模板正文。
    pub markdown: String,
}

/// 以与命令工具相同的路径和大小边界读取 command 的简短说明。
///
/// 设置页只需要说明文本，读取失败时返回 `None`，不应因为单个过期或损坏的
/// command 阻断其他扩展的列举；实际执行仍由 `PluginCommandTool` 返回稳定错误。
pub fn plugin_command_description(root: &Path, path: &Path) -> Option<String> {
    let relative_path = path.strip_prefix(root).ok()?.to_path_buf();
    let entry = PluginCommandEntry {
        name: String::new(),
        root: root.to_path_buf(),
        path: path.to_path_buf(),
        relative_path,
    };
    load_plugin_command(&entry)
        .ok()
        .map(|document| document.description)
}

/// 解析插件 command 文档；允许没有 front matter，但拒绝未闭合或重复说明。
pub fn parse_plugin_command_document(
    content: &str,
) -> Result<PluginCommandDocument, PluginCommandDocumentError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !content.starts_with("---") {
        return Ok(PluginCommandDocument {
            description: String::new(),
            markdown: content.to_owned(),
        });
    }
    let Some((first_line, first_end)) = next_line(content, 0) else {
        return Err(PluginCommandDocumentError::InvalidFrontMatter);
    };
    if first_line != "---" {
        return Ok(PluginCommandDocument {
            description: String::new(),
            markdown: content.to_owned(),
        });
    }
    let mut offset = first_end;
    let mut description = None;
    loop {
        if offset > MAX_PLUGIN_COMMAND_FRONT_MATTER_BYTES {
            return Err(PluginCommandDocumentError::FrontMatterTooLarge);
        }
        let Some((line, next_offset)) = next_line(content, offset) else {
            return Err(PluginCommandDocumentError::UnclosedFrontMatter);
        };
        if line == "---" {
            if next_offset > MAX_PLUGIN_COMMAND_FRONT_MATTER_BYTES {
                return Err(PluginCommandDocumentError::FrontMatterTooLarge);
            }
            return Ok(PluginCommandDocument {
                description: description.unwrap_or_default(),
                markdown: content.get(next_offset..).unwrap_or_default().to_owned(),
            });
        }
        if let Some((key, raw_value)) = line.split_once(':')
            && key.trim() == "description"
        {
            if description.is_some() {
                return Err(PluginCommandDocumentError::DuplicateDescription);
            }
            description = Some(parse_front_matter_text(raw_value)?);
        }
        offset = next_offset;
    }
}

/// command 文档解析阶段不回显原始内容的稳定错误。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginCommandDocumentError {
    /// front matter 没有在有界范围内闭合。
    UnclosedFrontMatter,
    /// front matter 超过有界读取区域。
    FrontMatterTooLarge,
    /// front matter 起始形状无法解析。
    InvalidFrontMatter,
    /// description 被声明多次。
    DuplicateDescription,
    /// description 使用了不支持的空值或引号形式。
    InvalidDescription,
}

impl std::fmt::Display for PluginCommandDocumentError {
    /// 输出不包含 command 正文的稳定解析错误。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnclosedFrontMatter => "插件 command front matter 未闭合",
            Self::FrontMatterTooLarge => "插件 command front matter 超过大小上限",
            Self::InvalidFrontMatter => "插件 command front matter 格式无效",
            Self::DuplicateDescription => "插件 command description 重复声明",
            Self::InvalidDescription => "插件 command description 格式无效",
        })
    }
}

impl std::error::Error for PluginCommandDocumentError {}

/// 读取下一行并返回去除 CR 的行文本及下一行字节偏移。
fn next_line(content: &str, offset: usize) -> Option<(&str, usize)> {
    if offset >= content.len() {
        return None;
    }
    let remainder = content.get(offset..)?;
    let end = remainder
        .find('\n')
        .map_or(remainder.len(), |index| index + 1);
    let next_offset = offset + end;
    let line = remainder[..end]
        .strip_suffix('\n')
        .unwrap_or(&remainder[..end]);
    Some((line.strip_suffix('\r').unwrap_or(line), next_offset))
}

/// 解析 command front matter 中的常用纯文本、单引号或双引号标量。
fn parse_front_matter_text(raw_value: &str) -> Result<String, PluginCommandDocumentError> {
    let value = raw_value.trim();
    if value.is_empty() {
        return Err(PluginCommandDocumentError::InvalidDescription);
    }
    if value.starts_with('\'') {
        if value.len() < 2 || !value.ends_with('\'') {
            return Err(PluginCommandDocumentError::InvalidDescription);
        }
        return Ok(value[1..value.len() - 1].replace("''", "'"));
    }
    if value.starts_with('"') {
        if value.len() < 2 || !value.ends_with('"') {
            return Err(PluginCommandDocumentError::InvalidDescription);
        }
        let mut output = String::new();
        let mut escaped = false;
        for character in value[1..value.len() - 1].chars() {
            if escaped {
                output.push(match character {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '"' => '"',
                    _ => return Err(PluginCommandDocumentError::InvalidDescription),
                });
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else {
                output.push(character);
            }
        }
        if escaped {
            return Err(PluginCommandDocumentError::InvalidDescription);
        }
        return Ok(output);
    }
    Ok(value.to_owned())
}

/// 在 Markdown 模板中展开完整参数和最多九个位置参数。
pub fn render_plugin_command(
    document: &PluginCommandDocument,
    arguments: &str,
) -> Result<String, PluginCommandDocumentError> {
    if arguments.len() > MAX_PLUGIN_COMMAND_ARGUMENT_BYTES
        || arguments.chars().any(|character| character == '\0')
    {
        return Err(PluginCommandDocumentError::InvalidDescription);
    }
    let words = arguments.split_whitespace().collect::<Vec<_>>();
    let mut rendered = String::with_capacity(document.markdown.len() + arguments.len());
    let mut chars = document.markdown.char_indices().peekable();
    let mut substituted = false;
    while let Some((index, character)) = chars.next() {
        if character != '$' {
            rendered.push(character);
            continue;
        }
        let remainder = document
            .markdown
            .get(index + character.len_utf8()..)
            .unwrap_or_default();
        let (replacement, consumed) = command_placeholder(remainder, arguments, &words);
        if let Some(replacement) = replacement {
            rendered.push_str(replacement);
            substituted = true;
            for _ in 0..consumed {
                chars.next();
            }
        } else {
            rendered.push(character);
        }
    }
    if !arguments.trim().is_empty() && !substituted {
        rendered.push_str("\n\n用户参数：\n");
        rendered.push_str(arguments);
    }
    if rendered.len() > MAX_PLUGIN_COMMAND_BYTES + MAX_PLUGIN_COMMAND_ARGUMENT_BYTES {
        return Err(PluginCommandDocumentError::FrontMatterTooLarge);
    }
    Ok(rendered)
}

/// 解析 `$ARGUMENTS`、`${ARGUMENTS}`、`$1..$9` 与对应的大括号占位符。
fn command_placeholder<'a>(
    remainder: &'a str,
    arguments: &'a str,
    words: &[&'a str],
) -> (Option<&'a str>, usize) {
    for (token, replacement) in [("{ARGUMENTS}", arguments), ("ARGUMENTS", arguments)] {
        if let Some(value) = remainder.strip_prefix(token) {
            let _ = value;
            return (Some(replacement), token.len());
        }
    }
    if let Some(digit) = remainder.chars().next()
        && ('1'..='9').contains(&digit)
    {
        let index = (digit as usize) - ('1' as usize);
        let replacement = words.get(index).copied().unwrap_or_default();
        return (Some(replacement), digit.len_utf8());
    }
    if let Some(remainder) = remainder.strip_prefix('{')
        && let Some(digit) = remainder.chars().next()
        && ('1'..='9').contains(&digit)
        && remainder
            .strip_prefix(&digit.to_string())
            .is_some_and(|tail| tail.starts_with('}'))
    {
        let index = (digit as usize) - ('1' as usize);
        let replacement = words.get(index).copied().unwrap_or_default();
        return (Some(replacement), 3);
    }
    (None, 0)
}

/// 读取并验证 command 文件，拒绝目录边界变化、符号链接和超大正文。
fn load_plugin_command(
    entry: &PluginCommandEntry,
) -> Result<PluginCommandDocument, PluginCommandLoadError> {
    let root_metadata =
        fs::symlink_metadata(&entry.root).map_err(|_| PluginCommandLoadError::Stale)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(PluginCommandLoadError::Stale);
    }
    let root = fs::canonicalize(&entry.root).map_err(|_| PluginCommandLoadError::Stale)?;
    if root != entry.root {
        return Err(PluginCommandLoadError::Stale);
    }
    let metadata =
        fs::symlink_metadata(&entry.path).map_err(|_| PluginCommandLoadError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(PluginCommandLoadError::UnsafePath);
    }
    let canonical =
        fs::canonicalize(&entry.path).map_err(|_| PluginCommandLoadError::Unavailable)?;
    if canonical != entry.path || !canonical.starts_with(&root) {
        return Err(PluginCommandLoadError::UnsafePath);
    }
    for component in entry
        .path
        .strip_prefix(&root)
        .map_err(|_| PluginCommandLoadError::UnsafePath)?
        .components()
    {
        if !matches!(component, Component::Normal(_)) {
            return Err(PluginCommandLoadError::UnsafePath);
        }
    }
    let mut file = fs::File::open(&canonical).map_err(|_| PluginCommandLoadError::Unavailable)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_PLUGIN_COMMAND_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| PluginCommandLoadError::Unavailable)?;
    if bytes.len() > MAX_PLUGIN_COMMAND_BYTES {
        return Err(PluginCommandLoadError::TooLarge);
    }
    let content = String::from_utf8(bytes).map_err(|_| PluginCommandLoadError::InvalidUtf8)?;
    parse_plugin_command_document(&content).map_err(PluginCommandLoadError::InvalidDocument)
}

/// command 文件读取失败时的安全分类，不携带文件正文。
#[derive(Debug)]
enum PluginCommandLoadError {
    /// 插件根或文件在候选发布后发生变化。
    Stale,
    /// 文件已消失或无法读取。
    Unavailable,
    /// 文件路径越过插件根或使用了符号链接。
    UnsafePath,
    /// 文件超过固定大小上限。
    TooLarge,
    /// 文件不是 UTF-8。
    InvalidUtf8,
    /// command front matter 或正文结构无效。
    InvalidDocument(PluginCommandDocumentError),
}

/// 将插件 command 读取失败映射为 Agent 可稳定识别的错误码。
fn map_command_load_error(error: PluginCommandLoadError) -> ToolError {
    match error {
        PluginCommandLoadError::Stale => ToolError::retryable(
            "plugin_command_catalog_stale",
            "插件 command 目录已经变化，请重试",
        ),
        PluginCommandLoadError::Unavailable => ToolError::retryable(
            "plugin_command_unavailable",
            "插件 command 文件当前不可读取",
        ),
        PluginCommandLoadError::UnsafePath => {
            ToolError::permanent("plugin_command_unsafe_path", "插件 command 文件路径不安全")
        }
        PluginCommandLoadError::TooLarge => {
            ToolError::permanent("plugin_command_too_large", "插件 command 文件超过大小上限")
        }
        PluginCommandLoadError::InvalidUtf8 => ToolError::permanent(
            "plugin_command_invalid_utf8",
            "插件 command 文件不是有效 UTF-8",
        ),
        PluginCommandLoadError::InvalidDocument(error) => {
            ToolError::permanent("plugin_command_invalid_document", error.to_string())
        }
    }
}

/// 向 Agent 暴露当前启用插件 command 的统一只读工具。
pub struct PluginCommandTool {
    /// 本次扩展候选冻结的插件 command 目录。
    catalog: Arc<PluginCommandCatalog>,
}

impl PluginCommandTool {
    /// 创建绑定到一个不可变插件 command 目录的工具。
    pub fn new(catalog: Arc<PluginCommandCatalog>) -> Self {
        Self { catalog }
    }
}

impl AgentTool for PluginCommandTool {
    /// 返回严格的 command 名称与参数输入 Schema。
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            PLUGIN_COMMAND_TOOL_NAME,
            "加载并展开当前项目启用插件的 Slash command Markdown 模板。用户消息以 /plugin:... 开始时应先调用本工具；模板不会直接执行 shell，模板要求的操作必须继续通过正常工具完成。",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_PLUGIN_COMMAND_NAME_BYTES,
                        "description": "用户 Slash 消息中的完整插件 command 名称"
                    },
                    "arguments": {
                        "type": "string",
                        "maxLength": MAX_PLUGIN_COMMAND_ARGUMENT_BYTES,
                        "description": "Slash command 名称之后的原始参数文本"
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        )
    }

    /// command 只读取插件模板，不直接产生外部状态变更。
    fn effect(&self, input: &Value) -> Result<ToolEffect, ToolError> {
        parse_plugin_command_input(input)?;
        Ok(ToolEffect::ReadOnly)
    }

    /// 不同 command 文件的读取可以与相邻只读工具并行。
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::ParallelReadOnly
    }

    /// 在当前不可变目录中读取并展开指定 command。
    fn execute(&self, context: ToolContext, input: Value) -> ToolFuture<'_> {
        let catalog = Arc::clone(&self.catalog);
        Box::pin(async move {
            if context.cancellation.is_cancelled() {
                return Err(ToolError::permanent(
                    "plugin_command_cancelled",
                    "插件 command 加载因当前 Turn 取消而停止",
                ));
            }
            let input = parse_plugin_command_input(&input)?;
            let Some(entry) = catalog.get(&input.name) else {
                return Err(ToolError::permanent(
                    "plugin_command_not_found",
                    "指定的插件 command 不存在、未启用或不适用于当前项目",
                ));
            };
            let document = load_plugin_command(entry).map_err(map_command_load_error)?;
            let markdown =
                render_plugin_command(&document, input.arguments.as_deref().unwrap_or(""))
                    .map_err(|error| {
                        ToolError::permanent("plugin_command_invalid_document", error.to_string())
                    })?;
            if context.cancellation.is_cancelled() {
                return Err(ToolError::permanent(
                    "plugin_command_cancelled",
                    "插件 command 加载因当前 Turn 取消而停止",
                ));
            }
            let output = serde_json::to_string(&json!({
                "name": entry.name,
                "description": document.description,
                "markdown": markdown,
            }))
            .map_err(|_| {
                ToolError::permanent(
                    "plugin_command_output_failed",
                    "插件 command 结果无法序列化",
                )
            })?;
            Ok(ToolOutput::text(output))
        })
    }
}

/// 插件 command 工具的严格输入。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginCommandInput {
    /// 当前启用插件目录中的完整稳定 command 名称。
    name: String,
    /// Slash 名称之后的原始参数文本。
    arguments: Option<String>,
}

/// 解析并验证插件 command 工具输入。
fn parse_plugin_command_input(input: &Value) -> Result<PluginCommandInput, ToolError> {
    let input: PluginCommandInput = serde_json::from_value(input.clone()).map_err(|error| {
        ToolError::permanent("invalid_input", format!("PluginCommand 输入无效：{error}"))
    })?;
    validate_plugin_command_name(&input.name)
        .map_err(|error| ToolError::permanent("invalid_input", error.to_string()))?;
    if input.arguments.as_deref().is_some_and(|arguments| {
        arguments.len() > MAX_PLUGIN_COMMAND_ARGUMENT_BYTES
            || arguments.chars().any(|character| character == '\0')
    }) {
        return Err(ToolError::permanent(
            "invalid_input",
            "PluginCommand arguments 超过大小上限或包含空字符",
        ));
    }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{ComponentFile, PluginId, RuntimePlugin};
    use keencode_agent::{AgentId, SessionId, ToolCallId, TurnCancellation, TurnId};
    use keencode_model::ToolResultContent;
    use std::collections::BTreeMap;

    /// 构造一个只包含 command 文件的运行时插件快照。
    fn snapshot_with_command(relative_path: &str) -> PluginRuntimeSnapshot {
        PluginRuntimeSnapshot {
            plugins: vec![RuntimePlugin {
                id: PluginId::parse("demo@official").expect("插件 ID 应有效"),
                root: PathBuf::from("C:/plugins/demo"),
                commands: vec![ComponentFile {
                    path: PathBuf::from("C:/plugins/demo").join(relative_path),
                    relative_path: PathBuf::from(relative_path),
                }],
                skills: Vec::new(),
                agents: Vec::new(),
                hooks: None,
                unsupported_hooks: Vec::new(),
                mcp_servers: BTreeMap::new(),
                lsp_servers: Vec::new(),
            }],
        }
    }

    /// 插件 command 名称必须移除 commands 目录但保留嵌套命名空间。
    #[test]
    fn namespace_uses_stable_nested_command_path() {
        assert_eq!(
            plugin_command_namespace("plugin:official:demo", Path::new("commands/review.md")),
            "plugin:official:demo:review"
        );
        assert_eq!(
            plugin_command_namespace("plugin:official:demo", Path::new("commands/admin/check.md")),
            "plugin:official:demo:admin:check"
        );
    }

    /// command 目录必须按稳定名称折叠大小写并暴露精确查找结果。
    #[test]
    fn catalog_builds_case_insensitive_lookup() {
        let catalog =
            PluginCommandCatalog::from_snapshot(&snapshot_with_command("commands/Review.md"))
                .expect("command 目录应构建成功");
        assert_eq!(catalog.len(), 1);
        assert_eq!(
            catalog
                .get("PLUGIN:OFFICIAL:DEMO:review")
                .map(|entry| entry.name.as_str()),
            Some("plugin:official:demo:Review")
        );
    }

    /// 不可由当前 Slash token 表达的命令路径必须在候选构建阶段失败关闭。
    #[test]
    fn catalog_rejects_command_names_with_spaces() {
        let error =
            PluginCommandCatalog::from_snapshot(&snapshot_with_command("commands/bad name.md"))
                .expect_err("包含空格的 command 名称必须拒绝");
        assert!(error.to_string().contains("command 名称无效"));
    }

    /// 无 front matter 的 command 也应作为完整 Markdown 模板读取。
    #[test]
    fn parser_accepts_plain_markdown() {
        let document = parse_plugin_command_document("请检查 $ARGUMENTS").expect("正文应可解析");
        assert_eq!(document.description, "");
        assert_eq!(document.markdown, "请检查 $ARGUMENTS");
    }

    /// 设置页读取 command 说明时必须复用命令工具的路径与大小边界。
    #[test]
    fn description_reader_reuses_bounded_command_loader() {
        let directory = tempfile::tempdir().expect("应创建插件测试目录");
        let root = directory.path().to_path_buf();
        let command_path = root.join("commands/review.md");
        fs::create_dir_all(command_path.parent().expect("command 应有父目录"))
            .expect("应创建 command 目录");
        fs::write(&command_path, "---\ndescription: 审查变更\n---\n请检查变更")
            .expect("应写入 command 文件");
        let root = fs::canonicalize(root).expect("插件根应可规范化");
        let command_path = fs::canonicalize(command_path).expect("command 应可规范化");
        assert_eq!(
            plugin_command_description(&root, &command_path).as_deref(),
            Some("审查变更")
        );

        fs::write(&command_path, vec![b'x'; MAX_PLUGIN_COMMAND_BYTES + 1])
            .expect("应写入超限 command 文件");
        assert!(plugin_command_description(&root, &command_path).is_none());
    }

    /// parser 应去掉 front matter 并读取 description 的常见引号形式。
    #[test]
    fn parser_extracts_description_and_body() {
        let document = parse_plugin_command_document(
            "---\ndescription: \"审查变更\"\nargument-hint: [path]\n---\n请审查 $1",
        )
        .expect("front matter 应可解析");
        assert_eq!(document.description, "审查变更");
        assert_eq!(document.markdown, "请审查 $1");
    }

    /// 未闭合和重复 description 的 front matter 必须失败且不泄露正文。
    #[test]
    fn parser_rejects_unclosed_or_duplicate_description() {
        assert_eq!(
            parse_plugin_command_document("---\ndescription: demo\n正文"),
            Err(PluginCommandDocumentError::UnclosedFrontMatter)
        );
        assert_eq!(
            parse_plugin_command_document("---\ndescription: one\ndescription: two\n---\n正文"),
            Err(PluginCommandDocumentError::DuplicateDescription)
        );
    }

    /// command 参数应展开完整参数、位置参数，并在没有占位符时显式附加参数。
    #[test]
    fn renderer_expands_arguments_and_positional_values() {
        let document = PluginCommandDocument {
            description: String::new(),
            markdown: "全部=$ARGUMENTS\n首个=${1}\n第二个=$2".to_owned(),
        };
        assert_eq!(
            render_plugin_command(&document, "one two").expect("参数应展开"),
            "全部=one two\n首个=one\n第二个=two"
        );

        let no_placeholder = PluginCommandDocument {
            description: String::new(),
            markdown: "请检查变更".to_owned(),
        };
        assert_eq!(
            render_plugin_command(&no_placeholder, "src/lib.rs").expect("参数应追加"),
            "请检查变更\n\n用户参数：\nsrc/lib.rs"
        );
    }

    /// 工具定义必须是 Provider 可移植名称，并只接受 name/arguments 两个字段。
    #[test]
    fn tool_definition_is_portable_and_strict() {
        let tool = PluginCommandTool::new(Arc::new(PluginCommandCatalog::default()));
        let definition = tool.definition();
        assert_eq!(definition.name, PLUGIN_COMMAND_TOOL_NAME);
        definition
            .validate()
            .expect("工具定义应满足 Provider Schema");
        assert!(
            tool.effect(&json!({"name":"plugin:official:demo:review"}))
                .is_ok()
        );
        assert!(
            tool.effect(&json!({"name":"plugin:official:demo:review","extra":true}))
                .is_err()
        );
    }

    /// 读取错误枚举不应实现 Send 之外的隐式文本格式，确保映射接口保持封闭。
    #[test]
    fn load_error_mapping_is_stable() {
        let error = map_command_load_error(PluginCommandLoadError::TooLarge);
        assert_eq!(error.code, "plugin_command_too_large");
        assert!(!error.retryable);
        let error = map_command_load_error(PluginCommandLoadError::Stale);
        assert_eq!(error.code, "plugin_command_catalog_stale");
        assert!(error.retryable);
    }

    /// 真实读取、参数展开和工具输出必须沿 AgentTool 执行边界完成。
    #[tokio::test]
    async fn tool_executes_command_template_from_frozen_catalog() {
        let root = tempfile::tempdir().expect("应创建插件测试目录");
        let command_path = root.path().join("commands").join("review.md");
        fs::create_dir_all(command_path.parent().expect("command 应有父目录"))
            .expect("应创建 command 目录");
        fs::write(
            &command_path,
            "---\ndescription: 审查变更\n---\n请检查 $1，并按需继续调用工具。",
        )
        .expect("应写入 command 模板");
        let snapshot = PluginRuntimeSnapshot {
            plugins: vec![RuntimePlugin {
                id: PluginId::parse("demo@official").expect("插件 ID 应有效"),
                root: fs::canonicalize(root.path()).expect("插件根应可规范化"),
                commands: vec![ComponentFile {
                    path: fs::canonicalize(&command_path).expect("command 应可规范化"),
                    relative_path: PathBuf::from("commands/review.md"),
                }],
                skills: Vec::new(),
                agents: Vec::new(),
                hooks: None,
                unsupported_hooks: Vec::new(),
                mcp_servers: BTreeMap::new(),
                lsp_servers: Vec::new(),
            }],
        };
        let catalog = Arc::new(
            PluginCommandCatalog::from_snapshot(&snapshot).expect("command 目录应冻结成功"),
        );
        let tool = PluginCommandTool::new(catalog);
        let output = tool
            .execute(
                ToolContext {
                    session_id: SessionId::new("session-command").expect("Session ID 应有效"),
                    turn_id: TurnId::new("turn-command").expect("Turn ID 应有效"),
                    source_agent_id: AgentId::new("agent-command").expect("Agent ID 应有效"),
                    tool_call_id: ToolCallId::new("call-command").expect("Tool Call ID 应有效"),
                    cancellation: TurnCancellation::new(),
                },
                json!({
                    "name": "PLUGIN:OFFICIAL:DEMO:REVIEW",
                    "arguments": "src/lib.rs"
                }),
            )
            .await
            .expect("command 工具应执行成功");
        let [ToolResultContent::Text { text }] = output.content.as_slice() else {
            panic!("command 工具应返回唯一文本结果");
        };
        let value: Value = serde_json::from_str(text).expect("command 输出应为 JSON");
        assert_eq!(value["name"], "plugin:official:demo:review");
        assert_eq!(value["description"], "审查变更");
        assert_eq!(value["markdown"], "请检查 src/lib.rs，并按需继续调用工具。");
    }
}
