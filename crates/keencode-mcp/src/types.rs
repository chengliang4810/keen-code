//! Provider 中立的 MCP 领域类型。

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP 客户端或服务端实现的名称与版本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementationInfo {
    /// 稳定的实现名称。
    pub name: String,
    /// 实现版本。
    pub version: String,
    /// 面向用户展示的可选名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// initialize 请求中声明的客户端能力。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    /// 客户端支持的实验能力及其配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
    /// 客户端根目录能力；未启用时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<Value>,
    /// 客户端采样能力；未启用时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Value>,
    /// 客户端 elicitation 能力；未启用时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<Value>,
    /// 客户端任务能力；未启用时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Value>,
    /// 未来协议字段，按原始 JSON 保留。
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// 服务端工具能力。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapabilities {
    /// 服务端是否会发送工具列表变化通知。
    #[serde(default)]
    pub list_changed: bool,
}

/// 服务端资源能力。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceCapabilities {
    /// 服务端是否支持资源订阅。
    #[serde(default)]
    pub subscribe: bool,
    /// 服务端是否会发送资源列表变化通知。
    #[serde(default)]
    pub list_changed: bool,
}

/// initialize 响应中声明的服务端能力。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    /// 服务端支持的实验能力及其配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
    /// 日志能力配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<Value>,
    /// completion 能力配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completions: Option<Value>,
    /// prompt 能力配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Value>,
    /// 资源能力配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceCapabilities>,
    /// 工具能力配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolCapabilities>,
    /// 服务端 elicitation 能力配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<Value>,
    /// 服务端任务能力配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<Value>,
    /// 未来协议字段，按原始 JSON 保留。
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// MCP initialize 成功响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// 服务端选择的 MCP 协议版本。
    pub protocol_version: String,
    /// 服务端能力声明。
    pub capabilities: ServerCapabilities,
    /// 服务端实现信息。
    pub server_info: ImplementationInfo,
    /// 服务端希望向模型或用户展示的可选说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// initialize 结果的可选 MCP 元数据。
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// 完成初始化后固定不变的 MCP 服务端会话信息。
#[derive(Clone, PartialEq)]
pub struct McpServerSession {
    /// 协商完成的 MCP 协议版本。
    pub protocol_version: String,
    /// 服务端实现信息。
    pub server_info: ImplementationInfo,
    /// 服务端能力声明。
    pub capabilities: ServerCapabilities,
    /// 服务端提供的可选使用说明。
    pub instructions: Option<String>,
}

impl fmt::Debug for McpServerSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpServerSession")
            .field("protocol_version", &self.protocol_version)
            .field("server_info", &"<redacted>")
            .field("capabilities", &"<redacted>")
            .field("instructions_present", &self.instructions.is_some())
            .finish()
    }
}

impl From<InitializeResult> for McpServerSession {
    fn from(result: InitializeResult) -> Self {
        Self {
            protocol_version: result.protocol_version,
            server_info: result.server_info,
            capabilities: result.capabilities,
            instructions: result.instructions,
        }
    }
}

/// MCP 图标描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpIcon {
    /// 图标资源地址。
    pub src: String,
    /// 可选 MIME 类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// 可选尺寸提示，例如 `48x48` 或 `any`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sizes: Vec<String>,
    /// 可选明暗主题提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

/// MCP 工具注解；这些字段是服务端提示，不能视为安全边界。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    /// 面向用户展示的工具标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 服务端是否声明工具不会修改环境。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// 服务端是否声明工具可能执行破坏性更新。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// 服务端是否声明使用相同参数重复调用通常安全。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// 服务端是否声明工具会与开放网络或外部实体交互。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// MCP 工具对外部状态的保守影响分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolEffect {
    /// KeenCode 本地可信策略明确认定为只读。
    ReadOnly,
    /// 工具未知、没有本地可信只读策略或明确会修改状态。
    ChangesState,
}

/// MCP 工具对 Tasks 协议的依赖级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTaskSupport {
    /// 工具不能作为 task 启动。
    Forbidden,
    /// 工具既可普通调用，也可作为 task 启动。
    Optional,
    /// 工具只能通过尚未实现的 Tasks 协议调用。
    Required,
}

/// MCP 工具执行方式声明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolExecution {
    /// 工具对 Tasks 协议的依赖级别。
    pub task_support: McpTaskSupport,
    /// 未来执行字段，按原始 JSON 保留。
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// MCP 服务端公布的工具定义。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// 调用工具时使用的稳定名称。
    pub name: String,
    /// 面向用户展示的可选标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 工具行为说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 工具参数的 JSON Schema。
    pub input_schema: Value,
    /// 工具结构化输出的可选 JSON Schema。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// 工具行为提示；不能代替客户端自己的安全策略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
    /// 工具的 Tasks 执行要求；`required` 工具不会暴露给普通调用方。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<McpToolExecution>,
    /// 可供界面展示的图标。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub icons: Vec<McpIcon>,
    /// 工具定义的可选 MCP 元数据。
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

impl McpTool {
    /// 返回该工具是否强制要求尚未实现的 MCP Tasks 协议。
    pub fn requires_task(&self) -> bool {
        self.execution
            .as_ref()
            .is_some_and(|execution| execution.task_support == McpTaskSupport::Required)
    }
}

/// 已读取完整分页结果的 MCP 工具集合。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpToolSet {
    tools: Vec<McpTool>,
    local_effects: BTreeMap<String, McpToolEffect>,
}

impl McpToolSet {
    /// 从工具列表创建集合。
    pub fn new(tools: Vec<McpTool>) -> Self {
        Self {
            tools,
            local_effects: BTreeMap::new(),
        }
    }

    /// 返回所有工具。
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// 按名称查找工具。
    pub fn get(&self, name: &str) -> Option<&McpTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    /// 设置由 KeenCode 本地可信策略给出的工具影响分类；未知工具不会写入策略。
    pub fn set_local_effect(&mut self, name: &str, effect: McpToolEffect) -> bool {
        if self.get(name).is_none() {
            return false;
        }
        self.local_effects.insert(name.to_owned(), effect);
        true
    }

    /// 返回指定工具的本地影响分类；服务端注解和未知名称始终视为会修改状态。
    pub fn effect_for(&self, name: &str) -> McpToolEffect {
        self.local_effects
            .get(name)
            .copied()
            .unwrap_or(McpToolEffect::ChangesState)
    }

    /// 消费集合并返回底层工具列表。
    pub fn into_tools(self) -> Vec<McpTool> {
        self.tools
    }
}

/// MCP 内容块；未知内容类型及扩展字段会原样保留。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpContent {
    /// 内容类型，例如 `text`、`image`、`audio`、`resource_link` 或 `resource`。
    #[serde(rename = "type")]
    pub content_type: String,
    /// 文本内容块的正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 图像或音频内容块的 Base64 数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// 图像、音频或资源的 MIME 类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// 资源链接内容块的 URI。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// 资源链接内容块的名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 资源链接内容块的可选标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 资源链接内容块的可选说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 资源链接内容块的可选字节大小。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 内嵌资源内容块。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<McpResourceContent>,
    /// 内容块的可选注解。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpAnnotations>,
    /// 内容块的可选 MCP 元数据。
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// 未来协议字段，按原始 JSON 保留。
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// MCP 工具调用结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    /// 工具返回的内容块。
    #[serde(default)]
    pub content: Vec<McpContent>,
    /// 与工具 outputSchema 对应的可选结构化结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    /// 工具是否把本次调用标记为业务错误。
    #[serde(default)]
    pub is_error: bool,
    /// 调用结果的可选 MCP 元数据。
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// MCP 资源或内容的受众与排序注解。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpAnnotations {
    /// 内容面向的角色名称。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audience: Vec<String>,
    /// 0 到 1 之间的可选优先级提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
    /// 资源最后修改时间的 RFC 3339 文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

/// MCP 服务端公布的具体资源。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResource {
    /// 读取资源时使用的 URI。
    pub uri: String,
    /// 服务端定义的资源名称。
    pub name: String,
    /// 面向用户展示的可选标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 资源说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 资源 MIME 类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// 资源字节大小。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 资源注解。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpAnnotations>,
    /// 可供界面展示的图标。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub icons: Vec<McpIcon>,
    /// 资源的可选 MCP 元数据。
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// MCP 服务端公布的参数化资源模板。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceTemplate {
    /// 包含 RFC 6570 模板变量的资源 URI 模板。
    pub uri_template: String,
    /// 服务端定义的模板名称。
    pub name: String,
    /// 面向用户展示的可选标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 模板说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 模板生成资源的 MIME 类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// 模板注解。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpAnnotations>,
    /// 可供界面展示的图标。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub icons: Vec<McpIcon>,
    /// 模板的可选 MCP 元数据。
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
}

/// `resources/read` 返回的文本或二进制资源内容。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpResourceContent {
    /// 实际读取的资源 URI。
    pub uri: String,
    /// 可选 MIME 类型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// 文本资源正文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// 二进制资源的 Base64 数据。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    /// 资源内容的可选 MCP 元数据。
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    /// 未来协议字段，按原始 JSON 保留。
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListToolsResult {
    pub(crate) tools: Vec<McpTool>,
    #[serde(default)]
    pub(crate) next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListResourcesResult {
    pub(crate) resources: Vec<McpResource>,
    #[serde(default)]
    pub(crate) next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListResourceTemplatesResult {
    pub(crate) resource_templates: Vec<McpResourceTemplate>,
    #[serde(default)]
    pub(crate) next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReadResourceResult {
    pub(crate) contents: Vec<McpResourceContent>,
}
