//! KeenCode Agent 定义文件解析器。
//!
//! 当前格式为带 YAML frontmatter 的 Markdown；未知字段和非法类型直接拒绝。
//!
//! 文件格式示例：
//! ```markdown
//! ---
//! name: code-reviewer
//! description: Reviews code for quality and best practices
//! tools: [Read, Glob, Grep]
//! ---
//!
//! You are a code reviewer...
//! ```

use serde::{de::Error, Deserialize};

/// KeenCode Agent YAML frontmatter 定义。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentFrontmatter {
    /// 使用小写字母和连字符的唯一标识符
    pub name: String,
    /// 主 Agent 何时应委托给此子 Agent。
    pub description: String,
    /// 子 Agent 可以使用的工具列表。
    #[serde(default)]
    pub tools: ToolsValue,
    /// 要拒绝的工具列表
    #[serde(default)]
    pub disallowed_tools: ToolsValue,
    /// 输出风格覆盖（替换默认的 Tone and style 章节）
    #[serde(default)]
    pub tone: Option<String>,
    /// 主动性覆盖（替换默认的 Proactiveness 章节）
    #[serde(default)]
    pub proactiveness: Option<String>,
    /// subagent 停止前的最大代理轮数
    #[serde(default)]
    pub max_turns: Option<u32>,
    /// 在启动时加载的 skills 列表
    #[serde(default)]
    pub skills: Vec<String>,
    /// prompt 模式："extend"（默认）或 "full"
    #[serde(default)]
    pub prompt_mode: Option<String>,
    /// 沙箱写目录白名单——声明后 subagent 可获得 WriteSandbox 工具，
    /// 只能写入这些相对目录（基于 cwd），不能碰项目代码。
    /// 不影响 can_mutate 推断（agent 仍视为 readonly）。
    #[serde(default)]
    pub allowed_write_dirs: Vec<String>,
    /// 子 Agent 使用的模型覆盖：编码为 `"{provider_id}::{model}"`，
    /// 省略或为空表示跟随会话 provider（Q2 决策）。运行时实际读取的是
    /// `claude_agent_parser::ClaudeAgentFrontmatter.model`（同一份文件按
    /// 该 parser 重新解析后驱动 `llm_factory`）；此字段仅用于本 parser
    /// 的写入校验，防止 `deny_unknown_fields` 拒绝 `agent_update` 写入的
    /// `model:` 键。
    #[serde(default)]
    pub model: Option<String>,
}

/// 工具列表；字段缺失与显式空数组具有不同权限语义。
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ToolsValue {
    /// `tools` 字段缺失：继承父工具。
    #[default]
    Inherit,
    /// 显式 `tools: []`：不继承任何父工具。
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
            serde_yaml::Value::Sequence(arr) => {
                let tools = arr
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
                    Ok(ToolsValue::NoTools)
                } else {
                    Ok(ToolsValue::List(tools))
                }
            }
            value => Err(D::Error::custom(format!(
                "tools 必须是字符串数组，收到 {value:?}"
            ))),
        }
    }
}

impl ToolsValue {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            ToolsValue::Inherit | ToolsValue::NoTools => Vec::new(),
            ToolsValue::List(v) => v.clone(),
        }
    }
}

impl AgentDefinition {
    /// 获取工具列表
    pub fn tools(&self) -> Vec<String> {
        self.frontmatter.tools.to_vec()
    }

    /// 获取被拒绝的工具列表
    pub fn disallowed_tools(&self) -> Vec<String> {
        self.frontmatter.disallowed_tools.to_vec()
    }
}

/// KeenCode Agent 定义。
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// Frontmatter 配置
    pub frontmatter: AgentFrontmatter,
    /// Markdown 正文（系统提示）
    pub system_prompt: String,
}

/// 将 agent_id（kebab-case 或 snake_case）格式化为友好显示名称
///
/// 例：`"code-reviewer"` → `"Code Reviewer"`，`"security_auditor"` → `"Security Auditor"`
pub fn format_agent_id(id: &str) -> String {
    id.split(['-', '_'])
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
        .map_err(|e| format!("YAML frontmatter 解析失败: {e}"))?;

    frontmatter.name = frontmatter.name.trim().to_string();
    frontmatter.description = frontmatter.description.trim().to_string();
    validate_agent_id(&frontmatter.name)?;
    if frontmatter.description.is_empty() {
        return Err("description 不能为空".to_string());
    }
    validate_unique_values("tools", &frontmatter.tools.to_vec())?;
    validate_unique_values("disallowedTools", &frontmatter.disallowed_tools.to_vec())?;
    validate_unique_values("skills", &frontmatter.skills)?;
    validate_unique_values("allowedWriteDirs", &frontmatter.allowed_write_dirs)?;

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

fn validate_unique_values(field: &str, values: &[String]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(format!("{field} 包含重复值 '{value}'"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "agent_parser_test.rs"]
mod tests;
