use std::collections::HashSet;
use std::path::Path;

/// 错误上下文，包含建议器做决策所需的全部信息
pub struct ErrorContext<'a> {
    pub tool_name: &'a str,
    pub tool_input: &'a serde_json::Value,
    pub error_message: &'a str,
    pub cwd: &'a Path,
    pub tool_registry: &'a ToolRegistrySnapshot,
}

impl<'a> ErrorContext<'a> {
    pub fn new(
        tool_name: &'a str,
        tool_input: &'a serde_json::Value,
        error_message: &'a str,
        cwd: &'a Path,
        tool_registry: &'a ToolRegistrySnapshot,
    ) -> Self {
        Self {
            tool_name,
            tool_input,
            error_message,
            cwd,
            tool_registry,
        }
    }
}

/// 工具名 + subagent 类型快照，每轮 ReAct 构造期填充
#[derive(Clone, Default, Debug)]
pub struct ToolRegistrySnapshot {
    pub all_tool_names: HashSet<String>,
    pub subagent_types: HashSet<String>,
}
