//! KeenCode Agent 定义文件解析器。
//!
//! 项目级 Agent 使用带 YAML frontmatter 的 Markdown；未知字段和非法类型直接拒绝。

use serde::{de::Error, Deserialize};

use crate::claude_agent_parser::{
    ClaudeAgent, ClaudeAgentFrontmatter, ToolsValue as ClaudeToolsValue,
};

/// KeenCode Agent YAML frontmatter 定义。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentFrontmatter {
    /// 使用小写字母、数字和连字符的唯一标识符。
    pub name: String,
    /// 主 Agent 何时应委托给此子 Agent。
    pub description: String,
    /// 子 Agent 可以使用的工具列表。
    #[serde(default)]
    pub tools: ToolsValue,
    /// 子 Agent 不允许使用的工具列表。
    #[serde(default)]
    pub disallowed_tools: ToolsValue,
    /// 输出风格覆盖。
    #[serde(default)]
    pub tone: Option<String>,
    /// 主动性覆盖。
    #[serde(default)]
    pub proactiveness: Option<String>,
    /// 子 Agent 停止前的最大代理轮数。
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// 启动时加载的 Skills 列表。
    #[serde(default)]
    pub skills: Vec<String>,
    /// Prompt 模式：`extend`（默认）或 `full`。
    #[serde(default)]
    pub prompt_mode: Option<String>,
    /// 沙箱写目录白名单。
    #[serde(default)]
    pub allowed_write_dirs: Vec<String>,
    /// 模型覆盖，KeenCode 原生格式为 `provider_id::model`。
    #[serde(default)]
    pub model: Option<String>,
}

/// 工具列表；字段缺失与显式空数组具有不同权限语义。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ToolsValue {
    /// 字段缺失：继承父工具。
    #[default]
    Inherit,
    /// 显式空数组：不继承任何父工具。
    NoTools,
    /// 显式列出的工具。
    List(Vec<String>),
}

impl<'de> Deserialize<'de> for ToolsValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        match value {
            serde_yaml::Value::Sequence(values) => {
                let tools = values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(|tool| tool.trim().to_string())
                            .ok_or_else(|| D::Error::custom("tools 数组只能包含字符串"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if tools.iter().any(|tool| tool.is_empty()) {
                    return Err(D::Error::custom("tools 数组不能包含空工具名"));
                }
                if tools.is_empty() {
                    Ok(Self::NoTools)
                } else {
                    Ok(Self::List(tools))
                }
            }
            value => Err(D::Error::custom(format!(
                "tools 必须是字符串数组，收到 {value:?}"
            ))),
        }
    }
}

impl ToolsValue {
    /// 返回显式声明的工具名；继承与零工具都返回空列表。
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            Self::Inherit | Self::NoTools => Vec::new(),
            Self::List(values) => values.clone(),
        }
    }

    /// 转换为 Peri 的 Claude/插件 Agent 工具语义。
    fn into_claude(self) -> ClaudeToolsValue {
        match self {
            Self::Inherit => ClaudeToolsValue::Empty,
            Self::NoTools => ClaudeToolsValue::NoTools,
            Self::List(values) => ClaudeToolsValue::List(values),
        }
    }
}

/// KeenCode Agent 定义。
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// Frontmatter 配置。
    pub frontmatter: AgentFrontmatter,
    /// Markdown 正文（系统提示）。
    pub system_prompt: String,
}

impl AgentDefinition {
    /// 获取显式声明的工具列表。
    pub fn tools(&self) -> Vec<String> {
        self.frontmatter.tools.to_vec()
    }

    /// 获取显式拒绝的工具列表。
    pub fn disallowed_tools(&self) -> Vec<String> {
        self.frontmatter.disallowed_tools.to_vec()
    }

    /// 转为上游 SubAgent 装配使用的统一结构。
    pub fn into_claude_agent(self) -> ClaudeAgent {
        let frontmatter = self.frontmatter;
        ClaudeAgent {
            frontmatter: ClaudeAgentFrontmatter {
                name: frontmatter.name,
                description: frontmatter.description,
                tools: frontmatter.tools.into_claude(),
                disallowed_tools: frontmatter.disallowed_tools.into_claude(),
                model: frontmatter.model,
                tone: frontmatter.tone,
                proactiveness: frontmatter.proactiveness,
                permission_mode: None,
                max_turns: frontmatter.max_turns,
                skills: frontmatter.skills,
                mcp_servers: Vec::new(),
                hooks: serde_yaml::Value::Null,
                memory: None,
                background: false,
                prompt_mode: frontmatter.prompt_mode,
                isolation: None,
                allowed_write_dirs: frontmatter.allowed_write_dirs,
            },
            system_prompt: self.system_prompt,
        }
    }
}

/// 解析 KeenCode Agent 文件内容；任何格式错误都返回可定位的显式错误。
pub fn parse_agent_file(content: &str) -> Result<AgentDefinition, String> {
    let content = content.replace("\r\n", "\n");
    let mut lines = content.lines();
    if lines.next() != Some("---") {
        return Err("Agent 文件第一行必须是精确的 '---'".to_string());
    }

    let mut yaml_lines = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line == "---" {
            closed = true;
            break;
        }
        yaml_lines.push(line);
    }
    if !closed {
        return Err("Agent 文件缺少闭合的 '---' 分隔符".to_string());
    }
    if yaml_lines.is_empty() {
        return Err("Agent YAML frontmatter 不能为空".to_string());
    }

    let mut frontmatter: AgentFrontmatter = serde_yaml::from_str(&yaml_lines.join("\n"))
        .map_err(|error| format!("YAML frontmatter 解析失败: {error}"))?;

    frontmatter.name = frontmatter.name.trim().to_string();
    frontmatter.description = frontmatter.description.trim().to_string();
    frontmatter.model = match frontmatter.model.take() {
        Some(model) if model.trim().is_empty() => None,
        Some(model) => peri_acp_types::agents::normalize_agent_model(&model)
            .map_err(|error| format!("model 无效: {error}"))?,
        None => None,
    };
    validate_agent_id(&frontmatter.name)?;
    if frontmatter.description.is_empty() {
        return Err("description 不能为空".to_string());
    }
    validate_unique_values("tools", &frontmatter.tools.to_vec())?;
    validate_unique_values("disallowedTools", &frontmatter.disallowed_tools.to_vec())?;
    validate_unique_values("skills", &frontmatter.skills)?;
    validate_unique_values("allowedWriteDirs", &frontmatter.allowed_write_dirs)?;
    for directory in &frontmatter.allowed_write_dirs {
        validate_allowed_write_dir(directory)?;
    }
    if matches!(frontmatter.max_turns, Some(0)) {
        return Err("maxTurns 必须大于 0".to_string());
    }
    if let Some(mode) = frontmatter.prompt_mode.as_deref() {
        if mode != "extend" && mode != "full" {
            return Err("promptMode 只允许 extend 或 full".to_string());
        }
    }

    Ok(AgentDefinition {
        frontmatter,
        system_prompt: lines.collect::<Vec<_>>().join("\n").trim().to_string(),
    })
}

/// 解析项目 Agent，并校验文件名 ID 与 frontmatter `name` 完全一致。
pub fn parse_project_agent(agent_id: &str, content: &str) -> Result<AgentDefinition, String> {
    validate_agent_id(agent_id)?;
    let definition = parse_agent_file(content)?;
    if definition.frontmatter.name != agent_id {
        return Err(format!(
            "Agent 文件名 ID '{agent_id}' 与 frontmatter name '{}' 不一致",
            definition.frontmatter.name
        ));
    }
    Ok(definition)
}

/// 校验 Agent ID 的唯一当前格式。
pub fn validate_agent_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Agent name '{id}' 只允许小写 ASCII 字母、数字和非首尾连字符"
        ))
    }
}

/// 校验列表字段不存在重复值。
fn validate_unique_values(field: &str, values: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(format!("{field} 包含重复值 '{value}'"));
        }
    }
    Ok(())
}

/// 校验沙箱写目录是位于项目根下的非空规范相对目录。
fn validate_allowed_write_dir(directory: &str) -> Result<(), String> {
    let normalized = directory.replace('\\', "/");
    let has_windows_prefix = normalized
        .as_bytes()
        .get(1)
        .is_some_and(|separator| *separator == b':');
    let valid = !normalized.is_empty()
        && !normalized.starts_with('/')
        && !has_windows_prefix
        && normalized
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    if valid {
        Ok(())
    } else {
        Err(format!(
            "allowedWriteDirs 只能包含项目根下的规范相对目录，收到 '{directory}'"
        ))
    }
}

#[cfg(test)]
#[path = "agent_parser_test.rs"]
mod tests;
